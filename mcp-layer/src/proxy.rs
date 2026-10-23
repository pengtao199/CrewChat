use crate::config::{ensure_private_file, Config};
use crate::registry::{AgentRecord, ServerRecord, ServerSpec};
use anyhow::{bail, Context, Result};
use mcp_proxy::{Proxy, ProxyConfig};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStatus {
    pub agent_id: String,
    pub port: u16,
    pub endpoint: String,
    pub state: String,
}

struct Instance {
    port: u16,
    server_ids: Vec<String>,
    token: String,
    config_path: PathBuf,
    task: JoinHandle<()>,
}

#[derive(Clone)]
pub struct ProxyManager {
    config: Config,
    instances: Arc<Mutex<HashMap<String, Instance>>>,
}

impl ProxyManager {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            instances: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn start_agent(&self, agent: &AgentRecord, servers: &[ServerRecord]) -> Result<u16> {
        if servers.is_empty() {
            self.stop_agent(&agent.agent_id).await;
            bail!("cannot start an MCP proxy without servers");
        }

        let mut instances = self.instances.lock().await;
        if let Some(existing) = instances.get(&agent.agent_id) {
            if existing.server_ids == agent.server_ids
                && existing.token == agent.token
                && !existing.task.is_finished()
            {
                return Ok(existing.port);
            }
            if let Some(instance) = instances.remove(&agent.agent_id) {
                instance.task.abort();
                let _ = tokio::fs::remove_dir_all(
                    instance.config_path.parent().unwrap_or(Path::new(".")),
                )
                .await;
            }
        }

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("reserve a proxy loopback port")?;
        let port = listener.local_addr()?.port();
        let config_path = proxy_config_path(&self.config, &agent.agent_id);
        let toml = render_proxy_config(agent, servers, port)?;
        ensure_private_file(&config_path)?;
        tokio::fs::write(&config_path, toml).await?;

        let config = ProxyConfig::load(&config_path)
            .with_context(|| format!("load proxy config {}", config_path.display()))?;
        let proxy = Proxy::from_config(config)
            .await
            .with_context(|| format!("start MCP proxy for {}", agent.agent_id))?;
        let (router, _session) = proxy.into_router();
        let agent_id = agent.agent_id.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::warn!("MCP proxy for agent {agent_id} stopped: {error}");
            }
        });

        instances.insert(
            agent.agent_id.clone(),
            Instance {
                port,
                server_ids: agent.server_ids.clone(),
                token: agent.token.clone(),
                config_path,
                task,
            },
        );
        Ok(port)
    }

    pub async fn stop_agent(&self, agent_id: &str) {
        let mut instances = self.instances.lock().await;
        if let Some(instance) = instances.remove(agent_id) {
            instance.task.abort();
            if let Some(dir) = instance.config_path.parent() {
                let _ = tokio::fs::remove_dir_all(dir).await;
            }
        }
    }

    pub async fn stop_all(&self) {
        let mut instances = self.instances.lock().await;
        for (_, instance) in instances.drain() {
            instance.task.abort();
        }
    }

    pub async fn status(&self) -> Vec<ProxyStatus> {
        let instances = self.instances.lock().await;
        let mut out = instances
            .iter()
            .map(|(agent_id, instance)| ProxyStatus {
                agent_id: agent_id.clone(),
                port: instance.port,
                endpoint: endpoint(instance.port),
                state: if instance.task.is_finished() {
                    "stopped".to_string()
                } else {
                    "running".to_string()
                },
            })
            .collect::<Vec<_>>();
        out.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        out
    }

    pub async fn running(&self, agent_id: &str) -> Option<u16> {
        let instances = self.instances.lock().await;
        instances
            .get(agent_id)
            .filter(|instance| !instance.task.is_finished())
            .map(|instance| instance.port)
    }
}

pub fn endpoint(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

fn proxy_config_path(config: &Config, agent_id: &str) -> PathBuf {
    let safe: String = agent_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    config
        .proxy_dir
        .join(if safe.is_empty() { "agent" } else { &safe })
        .join("proxy.toml")
}

#[derive(Serialize)]
struct ProxyFile<'a> {
    proxy: ProxySection<'a>,
    backends: Vec<BackendSection<'a>>,
    auth: AuthSection<'a>,
}

#[derive(Serialize)]
struct ProxySection<'a> {
    name: &'a str,
    hot_reload: bool,
    tool_discovery: bool,
    listen: ListenSection,
}

#[derive(Serialize)]
struct ListenSection {
    host: &'static str,
    port: u16,
}

#[derive(Serialize)]
struct BackendSection<'a> {
    name: &'a str,
    transport: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'a str>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bearer_token: Option<&'a str>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct AuthSection<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    tokens: Vec<&'a str>,
}

pub fn render_proxy_config(
    agent: &AgentRecord,
    servers: &[ServerRecord],
    port: u16,
) -> Result<String> {
    let backends = servers
        .iter()
        .map(backend_section)
        .collect::<Result<Vec<_>>>()?;
    let file = ProxyFile {
        proxy: ProxySection {
            name: "mcp-layer",
            hot_reload: false,
            tool_discovery: false,
            listen: ListenSection {
                host: "127.0.0.1",
                port,
            },
        },
        backends,
        auth: AuthSection {
            kind: "bearer",
            tokens: vec![agent.token.as_str()],
        },
    };
    Ok(toml::to_string_pretty(&file)?)
}

fn backend_section(spec: &ServerRecord) -> Result<BackendSection<'_>> {
    let ServerSpec {
        id,
        transport,
        command,
        args,
        env,
        url,
        headers,
        bearer_token,
        ..
    } = &spec.spec;
    match transport.as_str() {
        "stdio" => Ok(BackendSection {
            name: id,
            transport: "stdio",
            command: command.as_deref(),
            args: args.clone(),
            env: env.clone(),
            url: None,
            bearer_token: None,
            headers: BTreeMap::new(),
        }),
        "http" | "streamable_http" => Ok(BackendSection {
            name: id,
            transport: "http",
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: url.as_deref(),
            bearer_token: bearer_token.as_deref(),
            headers: headers.clone(),
        }),
        "sse" => Ok(BackendSection {
            name: id,
            transport: "http",
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: url.as_deref(),
            bearer_token: bearer_token.as_deref(),
            headers: headers.clone(),
        }),
        other => bail!("unsupported transport for proxy: {other}"),
    }
}
