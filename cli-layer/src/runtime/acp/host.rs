//! ACP host: spawn one CLI adapter per connection, keep the session alive, and
//! route updates/permissions into the event bus.
//!
//! This is deliberately a small host, not a copy of codeg's 127k-line ACP
//! module. The shape follows the official `agent-client-protocol` client
//! example; the permission broker and per-role env are ours.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, InitializeRequest, NewSessionRequest,
    PromptRequest, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionNotification, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Agent, Client, ConnectionTo};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::config::Config;
use crate::infra::skill_layer::SkillLayerClient;
use crate::infra::store::Store;
use crate::runtime::acp::events::{self, EventBus, EventEnvelope};
use crate::runtime::engine::registry::Engines;
use crate::runtime::overlay;

#[derive(Clone)]
pub struct EventSink {
    bus: EventBus,
    store: Store,
}

impl EventSink {
    pub fn emit(&self, event: EventEnvelope) {
        let event = self.bus.publish(event);
        let value = serde_json::to_value(&event).unwrap_or(Value::Null);
        let _ = self.store.append_event(&event.connection_id, None, &value);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.bus.subscribe()
    }
}

#[derive(Clone, Debug)]
pub enum PermissionDecision {
    Selected(String),
    Cancelled,
}

#[derive(Clone, Default)]
pub struct PermissionBroker {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<PermissionDecision>>>>,
}

impl PermissionBroker {
    fn open(&self) -> (String, oneshot::Receiver<PermissionDecision>) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(request_id.clone(), tx);
        (request_id, rx)
    }

    pub fn resolve(&self, request_id: &str, decision: PermissionDecision) -> bool {
        let sender = self.pending.lock().unwrap().remove(request_id);
        match sender {
            Some(sender) => sender.send(decision).is_ok(),
            None => false,
        }
    }
}

#[derive(Clone)]
pub struct ConnectionInfo {
    pub id: String,
    pub agent_type: String,
    pub session_id: String,
    #[allow(dead_code)]
    pub conversation_id: Option<String>,
    pub model_label: String,
}

struct ConnectionHandle {
    info: ConnectionInfo,
    tx: mpsc::UnboundedSender<Command>,
    task: tokio::task::JoinHandle<()>,
    /// Companion process for ACP bridges that need a local gateway
    /// (OpenClaw). Dropping the handle kills it via `kill_on_drop`.
    _gateway: Option<Child>,
}

enum Command {
    Prompt { text: String },
    Cancel,
    Close,
}

fn role_skill_ids(role: &Value) -> Vec<String> {
    role.get("skills")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(id) => Some(id.clone()),
                    Value::Object(object) => {
                        object.get("id").and_then(Value::as_str).map(str::to_string)
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Clone)]
pub struct AcpHost {
    config: Arc<Config>,
    engines: Engines,
    sink: EventSink,
    permissions: PermissionBroker,
    connections: Arc<Mutex<HashMap<String, ConnectionHandle>>>,
    skills: Arc<SkillLayerClient>,
}

impl AcpHost {
    pub fn new(config: Arc<Config>, store: Store, skills: Arc<SkillLayerClient>) -> Self {
        let bus = EventBus::new();
        Self {
            engines: Engines::new(config.clone()),
            config,
            sink: EventSink { bus, store },
            permissions: PermissionBroker::default(),
            connections: Arc::new(Mutex::new(HashMap::new())),
            skills,
        }
    }

    pub fn engines(&self) -> &Engines {
        &self.engines
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.sink.subscribe()
    }

    #[allow(dead_code)]
    pub fn connection(&self, connection_id: &str) -> Option<ConnectionInfo> {
        self.connections
            .lock()
            .unwrap()
            .get(connection_id)
            .map(|handle| handle.info.clone())
    }

    /// Spawn one ACP adapter, initialize, create a session, and keep it alive.
    pub async fn connect(
        &self,
        agent_type: &str,
        cwd: PathBuf,
        role: Option<Value>,
        api_key: Option<String>,
        conversation_id: Option<String>,
        engine_env: BTreeMap<String, String>,
    ) -> Result<Value> {
        let spec = self
            .engines
            .find(agent_type)
            .ok_or_else(|| anyhow!("unsupported engine '{agent_type}'"))?;
        if role.is_some() && spec.overlay == crate::runtime::engine::registry::OverlayKind::None {
            return Err(anyhow!(
                "{} does not expose a per-session API endpoint for model injection",
                spec.name
            ));
        }

        let overlay = match role.as_ref() {
            Some(role) => Some(overlay::prepare(
                spec.overlay,
                role,
                &self.config.model_layer_gateway,
                api_key.as_deref(),
                &self.config.data_dir,
            )?),
            None => None,
        };
        if let (Some(overlay), Some(role)) = (&overlay, role.as_ref()) {
            let skill_ids = role_skill_ids(role);
            if !skill_ids.is_empty() {
                self.skills
                    .materialize(&overlay.home, spec.agent_type, &skill_ids)
                    .await
                    .map_err(|error| {
                        anyhow!("materialize skills for {}: {error}", spec.agent_type)
                    })?;
            }
        }
        let mut env = engine_env;
        if let Some(overlay) = &overlay {
            env.extend(overlay.env.clone());
        }
        let mut extra_args = Vec::new();
        let gateway = match overlay.as_ref().and_then(|overlay| overlay.gateway_port) {
            Some(port) => {
                let child = self
                    .start_gateway(&self.engines.base_command(spec), port, &env)
                    .await?;
                extra_args.push("--url".to_string());
                extra_args.push(format!("ws://127.0.0.1:{port}"));
                Some(child)
            }
            None => None,
        };
        let agent = self.engines.acp_agent(spec, &env, &extra_args)?;

        let connection_id = uuid::Uuid::new_v4().to_string();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = oneshot::channel::<SessionId>();

        let notification_sink = self.sink.clone();
        let notification_conn = connection_id.clone();
        let permission_sink = self.sink.clone();
        let permission_conn = connection_id.clone();
        let permissions = self.permissions.clone();
        let prompt_sink = self.sink.clone();
        let prompt_conn = connection_id.clone();

        let task = tokio::spawn(async move {
            let result = Client
                .builder()
                .on_receive_notification(
                    async move |notification: SessionNotification, _cx| {
                        notification_sink
                            .emit(events::from_notification(&notification_conn, &notification));
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    async move |request: RequestPermissionRequest,
                                responder,
                                connection: ConnectionTo<Agent>| {
                        let permissions = permissions.clone();
                        let sink = permission_sink.clone();
                        let conn = permission_conn.clone();
                        connection.spawn(async move {
                            let (request_id, rx) = permissions.open();
                            let request_value =
                                serde_json::to_value(&request).unwrap_or(Value::Null);
                            sink.emit(EventEnvelope {
                                kind: "permission_request".to_string(),
                                connection_id: conn,
                                seq: 0,
                                payload: json!({
                                    "request_id": request_id,
                                    "request": request_value,
                                }),
                            });
                            let decision = rx.await.unwrap_or(PermissionDecision::Cancelled);
                            let response = match decision {
                                PermissionDecision::Selected(option_id) => {
                                    RequestPermissionResponse::new(
                                        RequestPermissionOutcome::Selected(
                                            SelectedPermissionOutcome::new(option_id),
                                        ),
                                    )
                                }
                                PermissionDecision::Cancelled => RequestPermissionResponse::new(
                                    RequestPermissionOutcome::Cancelled,
                                ),
                            };
                            let _ = responder.respond(response);
                            Ok(())
                        })
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(agent, move |connection: ConnectionTo<Agent>| async move {
                    let _initialize = connection
                        .send_request(
                            InitializeRequest::new(ProtocolVersion::V1)
                                .client_capabilities(ClientCapabilities::new()),
                        )
                        .block_task()
                        .await?;
                    let session = connection
                        .send_request(NewSessionRequest::new(cwd))
                        .block_task()
                        .await?;
                    let session_id = session.session_id;
                    let _ = ready_tx.send(session_id.clone());

                    while let Some(command) = cmd_rx.recv().await {
                        match command {
                            Command::Prompt { text } => {
                                let connection = connection.clone();
                                let session_id = session_id.clone();
                                let sink = prompt_sink.clone();
                                let conn = prompt_conn.clone();
                                tokio::spawn(async move {
                                    let request = PromptRequest::new(
                                        session_id,
                                        vec![ContentBlock::Text(TextContent::new(text))],
                                    );
                                    match connection.send_request(request).block_task().await {
                                        Ok(response) => sink.emit(events::turn_complete(
                                            &conn,
                                            serde_json::to_value(response.stop_reason)
                                                .unwrap_or(Value::Null),
                                        )),
                                        Err(error) => sink.emit(EventEnvelope {
                                            kind: "turn_complete".to_string(),
                                            connection_id: conn,
                                            seq: 0,
                                            payload: json!({"error": error.to_string()}),
                                        }),
                                    }
                                });
                            }
                            Command::Cancel => {
                                let _ = connection
                                    .send_notification(CancelNotification::new(session_id.clone()));
                            }
                            Command::Close => break,
                        }
                    }
                    Ok(())
                })
                .await;
            if let Err(error) = result {
                tracing::warn!("ACP connection ended: {error}");
            }
        });

        let session_id = match tokio::time::timeout(Duration::from_secs(180), ready_rx).await {
            Ok(Ok(session_id)) => session_id,
            Ok(Err(_)) => {
                task.abort();
                return Err(anyhow!("ACP connection closed before session/new"));
            }
            Err(_) => {
                task.abort();
                return Err(anyhow!("ACP initialize timed out"));
            }
        };

        let info = ConnectionInfo {
            id: connection_id.clone(),
            agent_type: spec.agent_type.to_string(),
            session_id: session_id.to_string(),
            conversation_id,
            model_label: overlay
                .map(|overlay| overlay.model_label)
                .unwrap_or_default(),
        };
        self.connections.lock().unwrap().insert(
            connection_id.clone(),
            ConnectionHandle {
                info: info.clone(),
                tx: cmd_tx,
                task,
                _gateway: gateway,
            },
        );

        Ok(json!({
            "connection_id": info.id,
            "session_id": info.session_id,
            "agent_type": info.agent_type,
            "model": info.model_label,
        }))
    }

    /// OpenClaw's ACP command is a bridge to a Gateway, not an agent process.
    /// Start one gateway per role so two roles cannot share a global model.
    async fn start_gateway(
        &self,
        base: &str,
        port: u16,
        env: &BTreeMap<String, String>,
    ) -> Result<Child> {
        let mut command = TokioCommand::new("sh");
        command
            .arg("-c")
            .arg(format!(
                "exec {base} gateway run --port {port} --allow-unconfigured --auth none --bind loopback"
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.spawn().context("spawn the OpenClaw gateway")?;
        for _ in 0..120 {
            if let Some(status) = child.try_wait()? {
                bail!("OpenClaw gateway exited before it was ready ({status})");
            }
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                return Ok(child);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let _ = child.kill().await;
        bail!("OpenClaw gateway did not become ready on 127.0.0.1:{port}")
    }

    pub fn prompt(&self, connection_id: &str, text: String) -> Result<()> {
        let tx = self.sender(connection_id)?;
        tx.send(Command::Prompt { text })
            .map_err(|_| anyhow!("connection is closed"))
    }

    pub fn cancel(&self, connection_id: &str) -> Result<()> {
        let tx = self.sender(connection_id)?;
        tx.send(Command::Cancel)
            .map_err(|_| anyhow!("connection is closed"))
    }

    pub fn close(&self, connection_id: &str) {
        if let Some(handle) = self.connections.lock().unwrap().remove(connection_id) {
            let _ = handle.tx.send(Command::Close);
            handle.task.abort();
        }
    }

    pub fn respond_permission(&self, request_id: &str, option_id: Option<String>) -> bool {
        let decision = match option_id {
            Some(option_id) => PermissionDecision::Selected(option_id),
            None => PermissionDecision::Cancelled,
        };
        self.permissions.resolve(request_id, decision)
    }

    fn sender(&self, connection_id: &str) -> Result<mpsc::UnboundedSender<Command>> {
        self.connections
            .lock()
            .unwrap()
            .get(connection_id)
            .map(|handle| handle.tx.clone())
            .ok_or_else(|| anyhow!("unknown connection '{connection_id}'"))
    }
}
