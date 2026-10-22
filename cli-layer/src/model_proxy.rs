//! Thin proxy for the AgentOS model page. The model layer owns providers and
//! keys; `cli-layer` only keeps the existing `ccr_*` command surface alive so
//! the Swift shell does not have to change during the migration.

use std::time::Duration;

use serde_json::{json, Value};

use crate::config::Config;

pub struct ModelLayerClient {
    base_url: String,
    token: Option<String>,
    client: reqwest::Client,
}

impl ModelLayerClient {
    pub fn new(config: &Config) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url: config.model_layer_url.trim_end_matches('/').to_string(),
            token: config.model_layer_token.clone(),
            client,
        }
    }

    pub fn health(&self) -> Value {
        json!({
            "url": self.base_url,
            "tokenConfigured": self.token.is_some(),
            "tokenSource": if self.token.is_some() { "env-or-file" } else { Value::Null },
        })
    }

    pub async fn rpc(&self, method: &str, args: Value) -> Result<Value, String> {
        let mut request = self
            .client
            .post(format!("{}/api/ccr/rpc", self.base_url))
            .json(&json!({"method": method, "args": args}));
        if let Some(token) = &self.token {
            request = request.header("x-ccr-web-auth", token);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("model layer {method}: {error}"))?;
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("model layer {method}: {error}"))?;
        if body.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(body
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("model layer RPC failed")
                .to_string());
        }
        Ok(body.get("value").cloned().unwrap_or(Value::Null))
    }

    pub async fn get_config(&self) -> Result<Value, String> {
        self.rpc("getConfig", json!([])).await
    }

    pub async fn local_gateway_key(&self) -> Option<String> {
        let config = self.get_config().await.ok()?;
        config
            .get("APIKEYS")
            .and_then(Value::as_array)?
            .iter()
            .find(|entry| entry.get("id").and_then(Value::as_str) == Some("local-gateway"))
            .and_then(|entry| entry.get("key"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    pub async fn provider_save(&self, body: &Value) -> Result<Value, String> {
        let mut config = self.get_config().await?;
        let provider = merge_provider(&mut config, body)?;
        let revision = config.get("configRevision").cloned().unwrap_or(Value::Null);
        config["configRevision"] = revision;
        self.rpc("saveConfig", json!([config, {"applyProfile": true}]))
            .await?;
        Ok(json!({"provider": provider, "affectedRunningSessions": 0}))
    }

    pub async fn provider_delete(&self, body: &Value) -> Result<Value, String> {
        let id = body
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "id is required".to_string())?;
        let mut config = self.get_config().await?;
        if let Some(providers) = config.get_mut("Providers").and_then(Value::as_array_mut) {
            providers.retain(|provider| {
                provider.get("id").and_then(Value::as_str) != Some(id)
                    && provider.get("name").and_then(Value::as_str) != Some(id)
            });
        }
        self.rpc("saveConfig", json!([config, {"applyProfile": true}]))
            .await?;
        Ok(json!(true))
    }

    pub async fn provider_probe(&self, body: &Value) -> Result<Value, String> {
        self.rpc("probeProvider", json!([body])).await
    }

    pub async fn disabled_models(&self) -> Result<Value, String> {
        let config = self.get_config().await?;
        let mut out = serde_json::Map::new();
        if let Some(providers) = config.get("Providers").and_then(Value::as_array) {
            for provider in providers {
                let name = provider
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("provider")
                    .to_string();
                let disabled = provider
                    .get("disabledModels")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                out.insert(name, disabled);
            }
        }
        Ok(Value::Object(out))
    }

    pub async fn model_face(&self, _agent_type: &str) -> Result<Value, String> {
        let config = self.get_config().await?;
        let mut models = Vec::new();
        if let Some(providers) = config.get("Providers").and_then(Value::as_array) {
            for provider in providers {
                let name = provider
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Provider");
                let id = provider.get("id").and_then(Value::as_str).unwrap_or(name);
                if let Some(list) = provider.get("models").and_then(Value::as_array) {
                    for model in list.iter().filter_map(Value::as_str) {
                        models.push(json!({
                            "provider": name,
                            "provider_id": id,
                            "model": model,
                        }));
                    }
                }
            }
        }
        Ok(json!({"supported": true, "models": models}))
    }
}

fn merge_provider(config: &mut Value, body: &Value) -> Result<Value, String> {
    let original_id = body
        .get("originalId")
        .or_else(|| body.get("original_id"))
        .and_then(Value::as_str);
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "name is required".to_string())?;
    let providers = config
        .get_mut("Providers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "config has no Providers".to_string())?;

    let index = providers.iter().position(|provider| {
        provider.get("id").and_then(Value::as_str) == original_id
            || provider.get("name").and_then(Value::as_str) == original_id
            || provider.get("name").and_then(Value::as_str) == Some(name)
    });

    let existing = index
        .and_then(|index| providers.get(index).cloned())
        .unwrap_or_else(|| json!({}));
    let id = existing
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| original_id.map(str::to_string))
        .unwrap_or_else(|| slug(name));

    let mut provider = existing.clone();
    provider["id"] = json!(id);
    provider["name"] = json!(name);
    if let Some(base_url) = body
        .get("baseUrl")
        .or_else(|| body.get("base_url"))
        .and_then(Value::as_str)
    {
        provider["api_base_url"] = json!(base_url);
    }
    if let Some(protocol) = body
        .get("protocol")
        .or_else(|| body.get("type"))
        .and_then(Value::as_str)
    {
        provider["type"] = json!(protocol);
    }
    if let Some(enabled) = body.get("enabled").and_then(Value::as_bool) {
        provider["enabled"] = json!(enabled);
    }
    if let Some(models) = body.get("models") {
        provider["models"] = models.clone();
    }

    let existing_credentials = existing
        .get("credentials")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(credentials) = body.get("credentials").and_then(Value::as_array) {
        let mut merged = Vec::new();
        for credential in credentials {
            let credential_name = credential
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let prior = existing_credentials.iter().find(|old| {
                old.get("id").and_then(Value::as_str)
                    == credential.get("id").and_then(Value::as_str)
                    || old.get("name").and_then(Value::as_str) == Some(credential_name)
            });
            let credential_id = credential
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    prior
                        .and_then(|old| old.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let api_key = credential
                .get("apiKey")
                .or_else(|| credential.get("api_key"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && !value.contains('•'))
                .map(str::to_string)
                .or_else(|| {
                    prior
                        .and_then(|old| old.get("api_key"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
            let mut item = json!({
                "id": credential_id,
                "name": credential_name,
                "priority": credential.get("priority").cloned().unwrap_or(json!(0)),
                "weight": credential.get("weight").cloned().unwrap_or(json!(1)),
                "enabled": credential.get("enabled").cloned().unwrap_or(json!(true)),
            });
            if let Some(api_key) = api_key {
                item["api_key"] = json!(api_key);
            }
            if let Some(limits) = credential.get("limits") {
                item["limits"] = limits.clone();
            }
            merged.push(item);
        }
        provider["credentials"] = Value::Array(merged);
    }

    if let Some(index) = index {
        providers[index] = provider.clone();
    } else {
        providers.push(provider.clone());
    }
    Ok(provider)
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        out
    }
}
