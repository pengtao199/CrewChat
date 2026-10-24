//! ACP session use cases: role normalization, engine preferences and turns.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{json, Value};

use crate::app::engines::EngineService;
use crate::domain::agent::Agent;
mod turn;

#[cfg(test)]
mod tests;

use turn::{connection_id_of, model_label, test_result};

use crate::domain::ports::{
    AgentRepository, ConnectRequest, ConversationRepository, EventSubscription, LeaderControl,
    ModelCatalog, SessionRuntime, TeamRepository,
};

#[derive(Clone)]
pub struct SessionService {
    agents: Arc<dyn AgentRepository>,
    conversations: Arc<dyn ConversationRepository>,
    runtime: Arc<dyn SessionRuntime>,
    model: Arc<dyn ModelCatalog>,
    engines: EngineService,
    teams: Option<Arc<dyn TeamRepository>>,
    connections: Arc<Mutex<HashMap<String, ConnectionMeta>>>,
    pool: Arc<Mutex<HashMap<(String, String), PooledConnection>>>,
}

#[derive(Clone, Debug)]
struct ConnectionMeta {
    conversation_id: Option<String>,
    role_id: Option<String>,
}

#[derive(Clone)]
struct PooledConnection {
    connection_id: String,
    last_used: Instant,
}

#[derive(Debug)]
pub enum SessionError {
    BadRequest(String),
    BadGateway(String),
    NotFound(String),
}

impl std::error::Error for SessionError {}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadRequest(message) | Self::BadGateway(message) | Self::NotFound(message) => {
                f.write_str(message)
            }
        }
    }
}

impl SessionService {
    pub fn new(
        agents: Arc<dyn AgentRepository>,
        conversations: Arc<dyn ConversationRepository>,
        runtime: Arc<dyn SessionRuntime>,
        model: Arc<dyn ModelCatalog>,
        engines: EngineService,
        teams: Option<Arc<dyn TeamRepository>>,
    ) -> Self {
        Self {
            agents,
            conversations,
            runtime,
            model,
            engines,
            teams,
            connections: Arc::new(Mutex::new(HashMap::new())),
            pool: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn subscribe(&self) -> Box<dyn EventSubscription> {
        self.runtime.subscribe()
    }

    fn team_state(
        &self,
        conversation_id: Option<&str>,
        role_id: Option<&str>,
        state: &str,
        connection_id: Option<&str>,
    ) {
        let (Some(teams), Some(conversation_id), Some(role_id)) =
            (self.teams.as_ref(), conversation_id, role_id)
        else {
            return;
        };
        let is_team = self
            .conversations
            .get_conversation(conversation_id)
            .ok()
            .flatten()
            .is_some_and(|detail| {
                detail.pointer("/summary/kind").and_then(Value::as_str) == Some("team")
            });
        if is_team {
            let _ = teams.set_conversation_agent(conversation_id, role_id, state, connection_id);
        }
    }

    fn is_team(&self, conversation_id: Option<&str>) -> bool {
        conversation_id.is_some_and(|id| {
            self.conversations
                .get_conversation(id)
                .ok()
                .flatten()
                .is_some_and(|detail| {
                    detail.pointer("/summary/kind").and_then(Value::as_str) == Some("team")
                })
        })
    }

    fn leader_control(
        &self,
        conversation_id: Option<&str>,
        role: Option<&Value>,
    ) -> Option<LeaderControl> {
        let conversation_id = conversation_id?;
        let role_id = role?.get("id").and_then(Value::as_str)?;
        let detail = self
            .conversations
            .get_conversation(conversation_id)
            .ok()??;
        let leader_role_id = detail
            .pointer("/summary/team/leader_role_id")
            .and_then(Value::as_str)?;
        (leader_role_id == role_id).then(|| LeaderControl {
            conversation_id: conversation_id.to_string(),
            leader_role_id: leader_role_id.to_string(),
        })
    }

    pub async fn connect(
        &self,
        agent_type: &str,
        cwd: PathBuf,
        role_id: Option<&str>,
        conversation_id: Option<String>,
        resume_session_id: Option<&str>,
    ) -> std::result::Result<Value, SessionError> {
        let role = self.load_role(role_id)?;
        self.connect_with_role(agent_type, cwd, role, conversation_id, resume_session_id)
            .await
    }

    async fn connect_with_role(
        &self,
        agent_type: &str,
        cwd: PathBuf,
        mut role: Option<Value>,
        conversation_id: Option<String>,
        resume_session_id: Option<&str>,
    ) -> std::result::Result<Value, SessionError> {
        let preferences = self.engines.preferences(agent_type);
        if !preferences.enabled {
            return Err(SessionError::BadRequest(
                "this engine is disabled".to_string(),
            ));
        }

        if let Some(role) = role.as_mut() {
            if let Some(provider) = role
                .get("model_provider")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                let model = role
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if let Some(display) = self.model.normalize_provider(&provider, &model).await {
                    role["model_provider"] = json!(display);
                }
            }
        }

        let api_key = self.model.local_gateway_key().await;
        let leader_control = self.leader_control(conversation_id.as_deref(), role.as_ref());
        let owns_conversation = conversation_id.as_deref().is_some_and(|id| {
            self.conversations
                .get_conversation(id)
                .ok()
                .flatten()
                .is_some_and(|detail| {
                    detail.pointer("/summary/team").is_none() || leader_control.is_some()
                })
        });
        let role_id = role
            .as_ref()
            .and_then(|role| role.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let value = self
            .runtime
            .connect(ConnectRequest {
                agent_type: agent_type.to_string(),
                cwd,
                role,
                api_key,
                conversation_id: conversation_id.clone(),
                resume_session_id: resume_session_id.map(str::to_string),
                engine_env: preferences.env,
                leader_control,
            })
            .await
            .map_err(|error| SessionError::BadGateway(error.to_string()))?;
        if let Some(connection_id) = value
            .get("connectionId")
            .or_else(|| value.get("connection_id"))
            .and_then(Value::as_str)
        {
            self.connections.lock().unwrap().insert(
                connection_id.to_string(),
                ConnectionMeta {
                    conversation_id: conversation_id.clone(),
                    role_id: role_id.clone(),
                },
            );
            if let (Some(conversation_id), Some(session_id)) = (
                conversation_id.as_deref(),
                value.get("session_id").and_then(Value::as_str),
            ) {
                if owns_conversation {
                    let _ = self
                        .conversations
                        .set_external_id(conversation_id, session_id);
                } else if let (Some(teams), Some(role_id)) =
                    (self.teams.as_ref(), role_id.as_deref())
                {
                    let _ = teams.set_conversation_agent_session(
                        conversation_id,
                        role_id,
                        Some(session_id),
                    );
                }
            }
            self.team_state(
                conversation_id.as_deref(),
                role_id.as_deref(),
                "idle",
                Some(connection_id),
            );
        }
        Ok(value)
    }

    fn load_role(&self, role_id: Option<&str>) -> std::result::Result<Option<Value>, SessionError> {
        role_id
            .and_then(|id| self.agents.get(id).ok().flatten())
            .map(|agent| agent.to_value())
            .transpose()
            .map_err(|error| SessionError::BadRequest(error.to_string()))
    }

    /// Run one prompt against an already-resolved Agent snapshot.
    ///
    /// This is shared by Test Run and team delegation. `conversation_id` is
    /// optional: Test Run passes `None` so no history is written; delegated
    /// member turns pass the team conversation and a speaker id.
    pub async fn run_agent(
        &self,
        agent: &Agent,
        prompt: &str,
        cwd: Option<PathBuf>,
        conversation_id: Option<&str>,
        speaker_role_id: Option<&str>,
        timeout_seconds: u64,
    ) -> std::result::Result<Value, SessionError> {
        let started = Instant::now();
        let model = model_label(agent);
        let cwd = cwd
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let value = match self
            .connect_with_role(
                &agent.agent_type,
                cwd,
                Some(
                    agent
                        .to_value()
                        .map_err(|error| SessionError::BadRequest(error.to_string()))?,
                ),
                conversation_id.map(str::to_string),
                None,
            )
            .await
        {
            Ok(value) => value,
            Err(error) => {
                return Ok(test_result(
                    false,
                    "connect",
                    error.to_string(),
                    model,
                    started.elapsed(),
                    None,
                    None,
                ))
            }
        };
        let connection_id = connection_id_of(&value);
        let events = self.runtime.subscribe();
        if let Err(error) = self.runtime.prompt(&connection_id, prompt.to_string()) {
            self.runtime.close(&connection_id);
            return Ok(test_result(
                false,
                "prompt",
                error.to_string(),
                model,
                started.elapsed(),
                None,
                Some(connection_id),
            ));
        }
        let outcome = turn::wait_for_turn(&connection_id, events, timeout_seconds).await;
        self.runtime.close(&connection_id);
        self.connections.lock().unwrap().remove(&connection_id);
        let (ok, phase, error, text, usage, artifacts, events_seen) = outcome;
        Ok(turn::turn_result(
            self.conversations.as_ref(),
            ok,
            phase,
            error,
            text,
            usage,
            artifacts,
            events_seen,
            model,
            started.elapsed(),
            Some(connection_id),
            conversation_id,
            speaker_role_id,
        ))
    }

    /// Run a member turn on a pooled ACP connection. The connection is created
    /// lazily on the first delegation and reused for later tasks in the same
    /// team conversation. Failed/cancelled turns close the connection so the
    /// next delegation starts from a clean process.
    pub async fn run_member(
        &self,
        agent: &Agent,
        prompt: &str,
        cwd: PathBuf,
        conversation_id: &str,
        timeout_seconds: u64,
    ) -> std::result::Result<Value, SessionError> {
        let started = Instant::now();
        let model = model_label(agent);
        let connection_id = self.member_connection(agent, cwd, conversation_id).await?;
        let events = self.runtime.subscribe();
        if let Err(error) = self.runtime.prompt(&connection_id, prompt.to_string()) {
            self.close_member(conversation_id, &agent.id);
            return Ok(test_result(
                false,
                "prompt",
                error.to_string(),
                model,
                started.elapsed(),
                None,
                Some(connection_id),
            ));
        }
        let (ok, phase, error, text, usage, artifacts, events_seen) =
            turn::wait_for_turn(&connection_id, events, timeout_seconds).await;
        if ok {
            if let Some(pooled) = self
                .pool
                .lock()
                .unwrap()
                .get_mut(&(conversation_id.to_string(), agent.id.clone()))
            {
                pooled.last_used = Instant::now();
            }
        } else {
            self.close_member(conversation_id, &agent.id);
        }
        Ok(turn::turn_result(
            self.conversations.as_ref(),
            ok,
            phase,
            error,
            text,
            usage,
            artifacts,
            events_seen,
            model,
            started.elapsed(),
            Some(connection_id),
            Some(conversation_id),
            Some(&agent.id),
        ))
    }

    async fn member_connection(
        &self,
        agent: &Agent,
        cwd: PathBuf,
        conversation_id: &str,
    ) -> std::result::Result<String, SessionError> {
        let key = (conversation_id.to_string(), agent.id.clone());
        let existing = self
            .pool
            .lock()
            .unwrap()
            .get(&key)
            .map(|pooled| pooled.connection_id.clone());
        if let Some(existing) = existing {
            if self.runtime.is_connected(&existing) {
                return Ok(existing);
            }
            self.close_member(conversation_id, &agent.id);
        }

        let resume_session_id = self
            .teams
            .as_ref()
            .and_then(|teams| {
                teams
                    .conversation_agent(conversation_id, &agent.id)
                    .ok()
                    .flatten()
            })
            .and_then(|agent| {
                agent
                    .get("external_session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let value = self
            .connect_with_role(
                &agent.agent_type,
                cwd,
                Some(
                    agent
                        .to_value()
                        .map_err(|error| SessionError::BadRequest(error.to_string()))?,
                ),
                Some(conversation_id.to_string()),
                resume_session_id.as_deref(),
            )
            .await?;
        let connection_id = connection_id_of(&value);
        self.pool.lock().unwrap().insert(
            key,
            PooledConnection {
                connection_id: connection_id.clone(),
                last_used: Instant::now(),
            },
        );
        Ok(connection_id)
    }

    fn close_member(&self, conversation_id: &str, role_id: &str) {
        let key = (conversation_id.to_string(), role_id.to_string());
        if let Some(pooled) = self.pool.lock().unwrap().remove(&key) {
            self.connections
                .lock()
                .unwrap()
                .remove(&pooled.connection_id);
            self.runtime.close(&pooled.connection_id);
        }
    }

    /// Cancel the member turn currently using this conversation's pooled
    /// connection. Returns false when the member is not connected.
    pub fn cancel_role(&self, conversation_id: &str, role_id: &str) -> bool {
        let key = (conversation_id.to_string(), role_id.to_string());
        let connection_id = self
            .pool
            .lock()
            .unwrap()
            .get(&key)
            .map(|pooled| pooled.connection_id.clone());
        connection_id.is_some_and(|id| self.runtime.cancel(&id).is_ok())
    }

    /// Close member connections that have been idle longer than `ttl`.
    pub fn reap_idle(&self, ttl: Duration) -> usize {
        let now = Instant::now();
        let stale = self
            .pool
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, pooled)| now.duration_since(pooled.last_used) >= ttl)
            .map(|(key, pooled)| (key.clone(), pooled.connection_id.clone()))
            .collect::<Vec<_>>();
        for ((conversation_id, role_id), connection_id) in &stale {
            self.pool
                .lock()
                .unwrap()
                .remove(&(conversation_id.clone(), role_id.clone()));
            self.connections.lock().unwrap().remove(connection_id);
            self.runtime.close(connection_id);
        }
        stale.len()
    }

    pub async fn test_run(&self, role_id: &str) -> std::result::Result<Value, SessionError> {
        let agent = self
            .agents
            .get(role_id)
            .map_err(|error| SessionError::BadRequest(error.to_string()))?
            .ok_or_else(|| SessionError::NotFound("role not found".to_string()))?;
        self.run_agent(&agent, "Reply with OK.", None, None, None, 90)
            .await
    }

    pub fn prompt(
        &self,
        connection_id: &str,
        text: String,
        conversation_id: Option<&str>,
    ) -> std::result::Result<(), SessionError> {
        if text.trim().is_empty() {
            return Err(SessionError::BadRequest("prompt has no text".to_string()));
        }
        let meta = self.connections.lock().unwrap().get(connection_id).cloned();
        let conversation_id = meta
            .as_ref()
            .and_then(|meta| meta.conversation_id.clone())
            .or_else(|| conversation_id.map(str::to_string));
        let events = self.runtime.subscribe();
        let speaker_role_id = meta.and_then(|meta| meta.role_id);
        if let Some(conversation_id) = conversation_id.as_deref() {
            let _ = self.conversations.append_turn(
                conversation_id,
                "user",
                &json!([{"type": "text", "text": text}]),
            );
            self.team_state(
                Some(conversation_id),
                speaker_role_id.as_deref(),
                "running",
                Some(connection_id),
            );
        }
        if let Err(error) = self.runtime.prompt(connection_id, text) {
            if let Some(conversation_id) = conversation_id.as_deref() {
                self.team_state(
                    Some(conversation_id),
                    speaker_role_id.as_deref(),
                    "idle",
                    Some(connection_id),
                );
            }
            return Err(SessionError::BadRequest(error.to_string()));
        }
        if let Some(conversation_id) = conversation_id {
            let teams = if self.is_team(Some(&conversation_id)) {
                self.teams.clone()
            } else {
                None
            };
            self.spawn_turn_collector(
                connection_id.to_string(),
                conversation_id,
                speaker_role_id,
                teams,
                events,
            );
        }
        Ok(())
    }

    fn spawn_turn_collector(
        &self,
        connection_id: String,
        conversation_id: String,
        speaker_role_id: Option<String>,
        teams: Option<Arc<dyn TeamRepository>>,
        mut events: Box<dyn EventSubscription>,
    ) {
        let conversations = self.conversations.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(1800), async {
                let mut text = String::new();
                loop {
                    let Some(event) = events.recv().await else {
                        break;
                    };
                    if event.connection_id != connection_id {
                        continue;
                    }
                    match event.kind.as_str() {
                        "content_delta" => {
                            if let Some(delta) = event.payload.get("text").and_then(Value::as_str) {
                                text.push_str(delta);
                            }
                        }
                        "turn_complete" => {
                            if !text.is_empty() {
                                let _ = conversations.append_turn_with_speaker(
                                    &conversation_id,
                                    "assistant",
                                    speaker_role_id.as_deref(),
                                    &json!([{"type": "text", "text": text}]),
                                );
                            }
                            if let (Some(teams), Some(role_id)) =
                                (teams.as_ref(), speaker_role_id.as_deref())
                            {
                                let _ = teams.set_conversation_agent(
                                    &conversation_id,
                                    role_id,
                                    "idle",
                                    None,
                                );
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await;
        });
    }

    pub fn events_since(&self, connection_id: &str, since_seq: u64) -> Result<Vec<Value>> {
        self.conversations.events_since(connection_id, since_seq)
    }

    pub fn conversation_events_since(
        &self,
        conversation_id: &str,
        since_seq: u64,
    ) -> Result<Vec<Value>> {
        self.conversations
            .events_for_conversation(conversation_id, since_seq)
    }

    pub fn cancel(&self, connection_id: &str) -> std::result::Result<(), SessionError> {
        self.runtime
            .cancel(connection_id)
            .map_err(|error| SessionError::BadRequest(error.to_string()))
    }

    pub fn disconnect(&self, connection_id: &str) {
        if let Some(meta) = self.connections.lock().unwrap().get(connection_id).cloned() {
            if let Some(conversation_id) = meta.conversation_id.as_deref() {
                self.team_state(
                    Some(conversation_id),
                    meta.role_id.as_deref(),
                    "idle",
                    Some(connection_id),
                );
            }
        }
        let keys = self
            .pool
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, pooled)| pooled.connection_id == connection_id)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            self.pool.lock().unwrap().remove(&key);
        }
        self.connections.lock().unwrap().remove(connection_id);
        self.runtime.close(connection_id);
    }

    pub fn respond_permission(
        &self,
        request_id: &str,
        option_id: Option<String>,
    ) -> std::result::Result<bool, SessionError> {
        if self.runtime.respond_permission(request_id, option_id) {
            Ok(true)
        } else {
            Err(SessionError::NotFound(
                "unknown permission request".to_string(),
            ))
        }
    }

    pub fn update_preferences(
        &self,
        agent_type: &str,
        enabled: bool,
        env: &BTreeMap<String, String>,
    ) -> Result<()> {
        self.engines.save_preferences(agent_type, enabled, env)
    }
}
