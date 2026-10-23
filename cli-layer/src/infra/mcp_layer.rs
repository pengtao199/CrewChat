//! Thin client for the local `mcp-layer` daemon.
//!
//! The daemon owns the MCP registry and starts one proxy instance per Agent.
//! `cli-layer` only saves the Agent allow-list, asks mcp-layer to sync, and
//! injects the returned `mcpServers` into `session/new`.

use std::time::Duration;

use serde_json::{json, Value};

#[derive(Clone)]
pub struct McpLayerClient {
    base_url: Option<String>,
    token: Option<String>,
    client: reqwest::Client,
}

impl McpLayerClient {
    pub fn new() -> Self {
        let runtime = runtime_file();
        let runtime = runtime
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());

        let base_url = env_string("MCP_LAYER_URL").or_else(|| {
            runtime
                .as_ref()
                .and_then(|value| value.get("url"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let token = env_string("MCP_LAYER_TOKEN").or_else(|| {
            runtime
                .as_ref()
                .and_then(|value| value.get("token"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            base_url,
            token,
            client,
        }
    }

    fn endpoint(&self, path: &str) -> Result<String, String> {
        let base = self
            .base_url
            .as_deref()
            .filter(|base| !base.is_empty())
            .ok_or_else(|| "mcp-layer is not running (runtime.json missing)".to_string())?;
        Ok(format!("{}{}", base.trim_end_matches('/'), path))
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let url = self.endpoint(path)?;
        let mut request = self.client.request(method, url);
        if let Some(token) = self.token.as_deref().filter(|token| !token.is_empty()) {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("mcp-layer {path}: {error}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|error| format!("mcp-layer {path}: {error}"))?;
        if status.is_success() {
            Ok(value)
        } else {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .filter(|message| !message.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("mcp-layer {path} returned {status}"));
            Err(message)
        }
    }

    pub async fn list(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/v1/servers", None)
            .await
    }

    pub async fn read(&self, id: &str) -> Result<Value, String> {
        self.request(reqwest::Method::GET, &format!("/v1/servers/{id}"), None)
            .await
    }

    pub async fn upsert(&self, id: &str, spec: &Value) -> Result<Value, String> {
        self.request(
            reqwest::Method::PUT,
            &format!("/v1/servers/{id}"),
            Some(spec),
        )
        .await
    }

    pub async fn remove(&self, id: &str) -> Result<Value, String> {
        self.request(reqwest::Method::DELETE, &format!("/v1/servers/{id}"), None)
            .await
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<Value, String> {
        let action = if enabled { "enable" } else { "disable" };
        self.request(
            reqwest::Method::POST,
            &format!("/v1/servers/{id}/{action}"),
            Some(&json!({})),
        )
        .await
    }

    pub async fn sync_agent(&self, agent_id: &str, server_ids: &[String]) -> Result<Value, String> {
        self.request(
            reqwest::Method::PUT,
            &format!("/v1/agents/{agent_id}"),
            Some(&json!({ "serverIds": server_ids })),
        )
        .await
    }

    pub async fn agent(&self, agent_id: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::GET,
            &format!("/v1/agents/{agent_id}"),
            None,
        )
        .await
    }

    pub async fn gateway(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/v1/gateway", None)
            .await
    }
}

fn env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn runtime_file() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|home| home.join(".agents/mcp/runtime.json"))
}
