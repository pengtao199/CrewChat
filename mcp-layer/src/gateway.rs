use crate::config::{ensure_private_file, Config};
use crate::registry::{AgentRecord, ServerRecord};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub state: String,
    pub pid: Option<u32>,
    pub url: String,
    pub last_error: Option<String>,
}

struct Inner {
    child: Option<Child>,
    admin_token: Option<String>,
    state: String,
    last_error: Option<String>,
}

#[derive(Clone)]
pub struct Gateway {
    config: Config,
    client: reqwest::Client,
    inner: Arc<Mutex<Inner>>,
}

impl Gateway {
    pub fn new(config: Config) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            inner: Arc::new(Mutex::new(Inner {
                child: None,
                admin_token: None,
                state: "stopped".to_string(),
                last_error: None,
            })),
        }
    }

    /// Attach to an already-running gateway (tests / embedding).
    pub fn attach(config: Config, admin_token: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            inner: Arc::new(Mutex::new(Inner {
                child: None,
                admin_token: Some(admin_token),
                state: "running".to_string(),
                last_error: None,
            })),
        }
    }

    pub async fn ensure_started(&self) -> Result<()> {
        {
            let mut inner = self.inner.lock().await;
            if inner.state == "running" {
                if inner.child.is_none() {
                    return Ok(());
                }
                if let Some(child) = inner.child.as_mut() {
                    if child.try_wait()?.is_none() {
                        return Ok(());
                    }
                }
                inner.state = "stopped".to_string();
                inner.child = None;
            }
            if let Some(child) = inner.child.as_mut() {
                if child.try_wait()?.is_none() {
                    inner.state = "running".to_string();
                    return Ok(());
                }
                inner.child = None;
            }
            if !self.config.gateway_bin.is_file() {
                let message = format!(
                    "gateway binary not found at {}; run mcp-layer/scripts/build-gateway.sh",
                    self.config.gateway_bin.display()
                );
                inner.state = "unavailable".to_string();
                inner.last_error = Some(message.clone());
                bail!(message);
            }
            inner.state = "starting".to_string();
            inner.last_error = None;
        }

        if let Err(error) = self.start_process().await {
            let mut inner = self.inner.lock().await;
            inner.state = "failed".to_string();
            inner.last_error = Some(error.to_string());
            if let Some(mut child) = inner.child.take() {
                let _ = child.kill().await;
            }
            return Err(error);
        }

        if let Err(error) = self.wait_health().await {
            let mut inner = self.inner.lock().await;
            inner.state = "failed".to_string();
            inner.last_error = Some(error.to_string());
            if let Some(mut child) = inner.child.take() {
                let _ = child.kill().await;
            }
            return Err(error);
        }

        let token = match self.load_or_init_admin().await {
            Ok(token) => token,
            Err(error) => {
                let mut inner = self.inner.lock().await;
                inner.state = "failed".to_string();
                inner.last_error = Some(error.to_string());
                if let Some(mut child) = inner.child.take() {
                    let _ = child.kill().await;
                }
                return Err(error);
            }
        };
        let mut inner = self.inner.lock().await;
        inner.admin_token = Some(token);
        inner.state = "running".to_string();
        inner.last_error = None;
        Ok(())
    }

    async fn start_process(&self) -> Result<()> {
        if let Some(parent) = self.config.gateway_db.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.config.gateway_log.parent() {
            fs::create_dir_all(parent)?;
        }
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.gateway_log)
            .with_context(|| format!("open gateway log {}", self.config.gateway_log.display()))?;
        let log_err = log.try_clone()?;

        let mut command = Command::new(&self.config.gateway_bin);
        command
            .arg("start")
            .arg("--enterprise")
            .arg("--port")
            .arg(self.config.gateway_port.to_string())
            .arg("--sqlite-db-path")
            .arg(&self.config.gateway_db)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .kill_on_drop(true);
        let child = command
            .spawn()
            .with_context(|| format!("spawn gateway {}", self.config.gateway_bin.display()))?;
        let mut inner = self.inner.lock().await;
        inner.child = Some(child);
        Ok(())
    }

    async fn wait_health(&self) -> Result<()> {
        let url = format!("{}/health", self.config.gateway_url());
        for _ in 0..100 {
            match self.client.get(&url).send().await {
                Ok(response) if response.status().is_success() => return Ok(()),
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
        bail!("gateway did not become healthy at {url}")
    }

    async fn load_or_init_admin(&self) -> Result<String> {
        if let Ok(token) = fs::read_to_string(&self.config.gateway_admin_token_path) {
            let token = token.trim().to_string();
            if !token.is_empty() {
                if self.admin_get("/api/v0/servers", &token).await.is_ok() {
                    return Ok(token);
                }
                bail!(
                    "stored gateway admin token is invalid; remove {} and the gateway database to reinitialize",
                    self.config.gateway_admin_token_path.display()
                );
            }
        }

        let response = self
            .client
            .post(format!("{}/init", self.config.gateway_url()))
            .json(&json!({"mode": "enterprise"}))
            .send()
            .await
            .context("initialize gateway")?;
        let status = response.status();
        let value: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("gateway initialization failed");
            bail!(
                "{message}; remove {} and the gateway database to reinitialize",
                self.config.gateway_admin_token_path.display()
            );
        }
        let token = value
            .get("admin_access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| anyhow::anyhow!("gateway init did not return admin_access_token"))?
            .to_string();
        ensure_private_file(&self.config.gateway_admin_token_path)?;
        fs::write(&self.config.gateway_admin_token_path, format!("{token}\n"))?;
        Ok(token)
    }

    async fn admin_token(&self) -> Result<String> {
        self.ensure_started().await?;
        let inner = self.inner.lock().await;
        inner
            .admin_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("gateway admin token is unavailable"))
    }

    async fn admin_get(&self, path: &str, token: &str) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}{}", self.config.gateway_url(), path))
            .bearer_auth(token)
            .send()
            .await?;
        let status = response.status();
        let value: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!(
                "gateway GET {path} returned {status}: {}",
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
            );
        }
        Ok(value)
    }

    async fn admin_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let token = self.admin_token().await?;
        let mut request = self
            .client
            .request(
                method.clone(),
                format!("{}{}", self.config.gateway_url(), path),
            )
            .bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|value| {
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| text.trim().to_string());
            bail!("gateway {} {path} returned {status}: {message}", method);
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub async fn register_server(&self, record: &ServerRecord) -> Result<()> {
        let body = record.spec.gateway_payload();
        self.admin_request(
            reqwest::Method::POST,
            "/api/v0/servers?force=true",
            Some(body),
        )
        .await?;
        if !record.spec.enabled {
            self.set_server_enabled(&record.id, false).await?;
        }
        Ok(())
    }

    pub async fn set_server_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let action = if enabled { "enable" } else { "disable" };
        self.admin_request(
            reqwest::Method::POST,
            &format!("/api/v0/servers/{id}/{action}"),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn delete_server(&self, id: &str) -> Result<()> {
        let response = self
            .client
            .delete(format!("{}/api/v0/servers/{id}", self.config.gateway_url()))
            .bearer_auth(self.admin_token().await?)
            .send()
            .await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("gateway delete server {id} returned {status}: {text}");
    }

    pub async fn list_gateway_servers(&self) -> Result<Vec<String>> {
        let value = self
            .admin_request(reqwest::Method::GET, "/api/v0/servers", None)
            .await?;
        let mut names = Vec::new();
        if let Some(items) = value.as_array() {
            for item in items {
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    names.push(name.to_string());
                }
            }
        }
        Ok(names)
    }

    pub async fn list_clients(&self) -> Result<Vec<Value>> {
        let value = self
            .admin_request(reqwest::Method::GET, "/api/v0/clients", None)
            .await?;
        Ok(value.as_array().cloned().unwrap_or_default())
    }

    pub async fn sync_agent(&self, agent: &AgentRecord) -> Result<()> {
        let clients = self.list_clients().await?;
        let existing = clients.iter().find(|client| {
            client.get("name").and_then(Value::as_str) == Some(agent.client_name.as_str())
        });
        if let Some(client) = existing {
            let current_ids = string_array(client.get("allow_list"));
            let current_token = client.get("access_token").and_then(Value::as_str);
            if current_ids == agent.server_ids && current_token == Some(agent.token.as_str()) {
                return Ok(());
            }
            self.delete_client(&agent.client_name).await?;
        }

        let body = json!({
            "name": agent.client_name,
            "description": format!("CrewChat Agent {}", agent.agent_id),
            "access_token": agent.token,
            "allow_list": agent.server_ids,
        });
        self.admin_request(reqwest::Method::POST, "/api/v0/clients", Some(body))
            .await?;
        Ok(())
    }

    pub async fn delete_client(&self, name: &str) -> Result<()> {
        let response = self
            .client
            .delete(format!(
                "{}/api/v0/clients/{name}",
                self.config.gateway_url()
            ))
            .bearer_auth(self.admin_token().await?)
            .send()
            .await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("gateway delete client {name} returned {status}: {text}");
    }

    pub async fn reconcile_servers(&self, records: &[ServerRecord]) -> Result<Vec<String>> {
        let wanted: Vec<&str> = records.iter().map(|record| record.id.as_str()).collect();
        let existing = self.list_gateway_servers().await?;
        let mut warnings = Vec::new();
        for name in existing {
            if !wanted.contains(&name.as_str()) {
                if let Err(error) = self.delete_server(&name).await {
                    warnings.push(format!("remove stale server {name}: {error}"));
                }
            }
        }
        for record in records {
            if let Err(error) = self.register_server(record).await {
                warnings.push(format!("register server {}: {error}", record.id));
            }
        }
        Ok(warnings)
    }

    pub async fn reconcile_agents(&self, agents: &[AgentRecord]) -> Result<Vec<String>> {
        let mut warnings = Vec::new();
        for agent in agents {
            if let Err(error) = self.sync_agent(agent).await {
                warnings.push(format!("sync agent {}: {error}", agent.agent_id));
            }
        }
        Ok(warnings)
    }

    pub async fn status(&self) -> GatewayStatus {
        let inner = self.inner.lock().await;
        GatewayStatus {
            state: inner.state.clone(),
            pid: inner.child.as_ref().and_then(Child::id),
            url: self.config.gateway_mcp_url(),
            last_error: inner.last_error.clone(),
        }
    }

    pub async fn stop(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(mut child) = inner.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        inner.state = "stopped".to_string();
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
