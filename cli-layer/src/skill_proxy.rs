//! Thin client for the local `skill-layer` daemon.
//!
//! The daemon owns the global skill library and version registry. `cli-layer`
//! only proxies commands and materializes a role's selected skills into the
//! role's private home before an ACP session starts.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

#[derive(Clone)]
pub struct SkillLayerClient {
    base_url: Option<String>,
    token: Option<String>,
    client: reqwest::Client,
}

impl SkillLayerClient {
    pub fn new() -> Self {
        let runtime = runtime_file();
        let runtime = runtime
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());

        let base_url = env_string("SKILL_LAYER_URL").or_else(|| {
            runtime
                .as_ref()
                .and_then(|value| value.get("url"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let token = env_string("SKILL_LAYER_TOKEN").or_else(|| {
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
            .ok_or_else(|| "skill-layer is not running (runtime.json missing)".to_string())?;
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
            .map_err(|error| format!("skill-layer {path}: {error}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|error| format!("skill-layer {path}: {error}"))?;
        if status.is_success() {
            Ok(value)
        } else {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .filter(|message| !message.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("skill-layer {path} returned {status}"));
            Err(message)
        }
    }

    pub async fn list(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/v1/skills", None).await
    }

    pub async fn detail(&self, id: &str) -> Result<Value, String> {
        self.request(reqwest::Method::GET, &format!("/v1/skills/{id}"), None)
            .await
    }

    pub async fn install(&self, body: &Value) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/v1/skills/install", Some(body))
            .await
    }

    pub async fn check(&self, id: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::POST,
            &format!("/v1/skills/{id}/check"),
            Some(&json!({})),
        )
        .await
    }

    pub async fn update(&self, id: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::POST,
            &format!("/v1/skills/{id}/update"),
            Some(&json!({})),
        )
        .await
    }

    pub async fn remove(&self, id: &str) -> Result<Value, String> {
        self.request(reqwest::Method::DELETE, &format!("/v1/skills/{id}"), None)
            .await
    }

    pub async fn collections(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/v1/collections", None)
            .await
    }

    pub async fn global_links(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/v1/global-links", None)
            .await
    }

    pub async fn materialize(
        &self,
        home: &Path,
        agent_type: &str,
        skill_ids: &[String],
    ) -> Result<Value, String> {
        let skills = skill_ids
            .iter()
            .map(|id| json!({ "id": id }))
            .collect::<Vec<_>>();
        self.request(
            reqwest::Method::POST,
            "/v1/sessions/materialize",
            Some(&json!({
                "home": home.to_string_lossy(),
                "agentType": agent_type,
                "skills": skills,
            })),
        )
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
        .map(|home| home.join(".agents/skill-layer/runtime.json"))
}
