use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use tower_mcp::client::{ClientTransport, HttpClientTransport, McpClient, StdioClientTransport};

use crate::http::AppState;
use crate::registry::{ServerRecord, ServerSpec};

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

pub type HealthCache = Arc<Mutex<HashMap<String, ServerHealth>>>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerHealth {
    pub checking: bool,
    pub available: bool,
    pub checked_at: Option<String>,
    pub error: Option<String>,
    pub tool_count: Option<usize>,
}

impl ServerHealth {
    pub fn unknown() -> Self {
        Self {
            checking: false,
            available: false,
            checked_at: None,
            error: None,
            tool_count: None,
        }
    }

    pub fn checking() -> Self {
        Self {
            checking: true,
            ..Self::unknown()
        }
    }
}

pub fn new_cache() -> HealthCache {
    Arc::new(Mutex::new(HashMap::new()))
}

pub fn startup(state: &AppState) {
    let servers = match state.registry.lock() {
        Ok(registry) => registry.list_servers().unwrap_or_default(),
        Err(_) => return,
    };
    for server in servers.into_iter().filter(|server| server.enabled) {
        spawn_check(state, server);
    }
}

pub fn spawn_check(state: &AppState, server: ServerRecord) -> ServerHealth {
    let mut cache = match state.health.lock() {
        Ok(cache) => cache,
        Err(_) => return ServerHealth::unknown(),
    };
    if let Some(existing) = cache.get(&server.id) {
        if existing.checking {
            return existing.clone();
        }
    }
    let checking = ServerHealth::checking();
    cache.insert(server.id.clone(), checking.clone());
    drop(cache);

    let state = state.clone();
    tokio::spawn(async move {
        let health = probe(&server).await;
        if let Ok(mut cache) = state.health.lock() {
            cache.insert(server.id, health);
        }
    });

    checking
}

pub fn invalidate_and_check(state: &AppState, server: ServerRecord) {
    remove(state, &server.id);
    spawn_check(state, server);
}

pub fn remove(state: &AppState, id: &str) {
    if let Ok(mut cache) = state.health.lock() {
        cache.remove(id);
    }
}

async fn probe(server: &ServerRecord) -> ServerHealth {
    match tokio::time::timeout(CHECK_TIMEOUT, probe_spec(&server.spec)).await {
        Ok(Ok(tool_count)) => ServerHealth {
            checking: false,
            available: true,
            checked_at: Some(now()),
            error: None,
            tool_count: Some(tool_count),
        },
        Ok(Err(error)) => ServerHealth {
            checking: false,
            available: false,
            checked_at: Some(now()),
            error: Some(format!("{error:#}")),
            tool_count: None,
        },
        Err(_) => ServerHealth {
            checking: false,
            available: false,
            checked_at: Some(now()),
            error: Some(format!(
                "MCP server did not respond within {} seconds",
                CHECK_TIMEOUT.as_secs()
            )),
            tool_count: None,
        },
    }
}

async fn probe_spec(spec: &ServerSpec) -> Result<usize> {
    match spec.transport.as_str() {
        "stdio" => {
            let command = spec
                .command
                .as_deref()
                .filter(|command| !command.trim().is_empty())
                .ok_or_else(|| anyhow!("stdio server has no command"))?;
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(&spec.args);
            cmd.envs(&spec.env);
            cmd.kill_on_drop(true);
            let transport = StdioClientTransport::spawn_command(&mut cmd)
                .await
                .context("start MCP process")?;
            probe_client(transport).await
        }
        "http" | "streamable_http" | "sse" => {
            let url = spec
                .url
                .as_deref()
                .filter(|url| !url.trim().is_empty())
                .ok_or_else(|| anyhow!("remote server has no url"))?;
            let mut transport = HttpClientTransport::new(url);
            if let Some(token) = spec
                .bearer_token
                .as_deref()
                .filter(|token| !token.is_empty())
            {
                transport = transport.bearer_token(token);
            }
            for (name, value) in &spec.headers {
                transport = transport.header(name.as_str(), value.as_str());
            }
            probe_client(transport).await
        }
        other => Err(anyhow!("unsupported transport: {other}")),
    }
}

async fn probe_client<T: ClientTransport>(transport: T) -> Result<usize> {
    let client = McpClient::connect(transport)
        .await
        .context("connect to MCP server")?;
    let result = async {
        client
            .initialize("crewchat-mcp-probe", env!("CARGO_PKG_VERSION"))
            .await
            .context("MCP initialize failed")?;
        let tools = client.list_tools().await.context("MCP tools/list failed")?;
        Ok::<usize, anyhow::Error>(tools.tools.len())
    }
    .await;
    let _ = client.shutdown().await;
    result
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
