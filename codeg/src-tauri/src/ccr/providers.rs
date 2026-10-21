//! [crewchat] CCR provider management — the *write* half of the model layer.
//!
//! The management API only exposes whole-document `saveConfig`, and a provider
//! object carries fields this app does not model (`transformer`, `modelMetadata`,
//! `account`, …). Rebuilding a provider from the Swift DTO would silently drop
//! them, so every mutation here is a **read → merge into the raw JSON → save**
//! under one process-wide lock.
//!
//! Concurrency: `saveConfig` rejects a stale `configRevision` (409). A second
//! writer (CCR's own UI) can win between our read and save, so the merge is
//! retried once against the fresh document.

use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

use super::client;
use crate::app_error::AppCommandError;

const SAVE_TIMEOUT_SECS: u64 = 60;
const PROBE_TIMEOUT_SECS: u64 = 90;

static PROVIDER_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn provider_lock() -> &'static Mutex<()> {
    PROVIDER_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSaveRequest {
    /// Current `id` (or legacy `name`) of the provider being edited.
    pub original_id: Option<String>,
    pub name: String,
    pub base_url: String,
    pub protocol: Option<String>,
    /// New key. Empty/absent keeps the stored one.
    pub api_key: Option<String>,
    /// Explicitly forget the stored key (local providers, key rotation).
    #[serde(default)]
    pub clear_api_key: Option<bool>,
    pub enabled: Option<bool>,
    /// Full desired model list. Absent keeps the stored list.
    pub models: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProbeRequest {
    pub base_url: String,
    pub api_key: Option<String>,
    pub models: Option<Vec<String>>,
    pub force_refresh: Option<bool>,
}

/// Create or update one provider. Returns the saved provider as CCR stored it.
pub async fn save_provider(request: ProviderSaveRequest) -> Result<Value, AppCommandError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(AppCommandError::invalid_input("供应商名字不能为空"));
    }
    let base_url = request.base_url.trim();
    if base_url.is_empty() {
        return Err(AppCommandError::invalid_input("供应商地址不能为空"));
    }

    let _guard = provider_lock().lock().await;

    for attempt in 0..2 {
        let config = client::call("getConfig", None).await?;
        let (next, provider_id) = merge_provider(config, &request)?;

        match save_config(next).await {
            Ok(saved) => {
                return Ok(find_provider(&saved, &provider_id).unwrap_or(Value::Null));
            }
            Err(error) if attempt == 0 && is_revision_conflict(&error) => continue,
            Err(error) => return Err(error),
        }
    }

    Err(AppCommandError::network("CCR 配置保存冲突，重试后仍未成功"))
}

/// Remove one provider. Refuses when a profile still points at it, so the
/// caller never leaves a dangling profile behind.
pub async fn delete_provider(id: String) -> Result<bool, AppCommandError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(AppCommandError::invalid_input("供应商 id 不能为空"));
    }

    let _guard = provider_lock().lock().await;

    for attempt in 0..2 {
        let mut config = client::call("getConfig", None).await?;
        let located = locate_provider(&config, id);
        let Some((index, provider_id, provider_name)) = located else {
            return Ok(false);
        };

        let used_by = profiles_using(&config, &provider_id, &provider_name);
        if !used_by.is_empty() {
            return Err(AppCommandError::invalid_input(format!(
                "供应商正在被配置档案使用：{}。请先在 CCR 里改掉这些档案，再删除。",
                used_by.join("、")
            )));
        }

        if let Some(providers) = config.get_mut("Providers").and_then(Value::as_array_mut) {
            providers.remove(index);
        }

        match save_config(config).await {
            Ok(_) => return Ok(true),
            Err(error) if attempt == 0 && is_revision_conflict(&error) => continue,
            Err(error) => return Err(error),
        }
    }

    Ok(false)
}

/// Probe a base URL and return the detected protocol + models. The UI uses
/// this both as "测试连接" and as the model-list source for a new provider.
pub async fn probe_provider(request: ProviderProbeRequest) -> Result<Value, AppCommandError> {
    let base_url = request.base_url.trim();
    if base_url.is_empty() {
        return Err(AppCommandError::invalid_input("供应商地址不能为空"));
    }

    let mut payload = json!({
        "baseUrl": base_url,
        "mode": "models",
        "forceRefresh": request.force_refresh.unwrap_or(true),
    });

    if let Some(key) = request
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        payload["apiKey"] = json!(key);
    }
    if let Some(models) = request.models {
        payload["models"] = json!(models);
    }

    client::call_with_timeout("probeProvider", Some(vec![payload]), PROBE_TIMEOUT_SECS).await
}

fn merge_provider(
    mut config: Value,
    request: &ProviderSaveRequest,
) -> Result<(Value, String), AppCommandError> {
    let protocol = request
        .protocol
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("openai_chat_completions");
    let api_key = request
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let original_id = request
        .original_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let providers = config
        .get_mut("Providers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| AppCommandError::configuration_invalid("CCR 配置缺少 Providers 列表"))?;

    let provider_id = if let Some(original_id) = original_id {
        let index = providers
            .iter()
            .position(|provider| provider_matches(provider, original_id))
            .ok_or_else(|| AppCommandError::not_found(format!("找不到供应商 {original_id}")))?;

        let provider = providers[index]
            .as_object_mut()
            .ok_or_else(|| AppCommandError::configuration_invalid("供应商配置格式不正确"))?;

        // The name is a stable reference used by profiles; only the fields the
        // editor owns are replaced.
        provider.insert("api_base_url".into(), json!(request.base_url.trim()));
        provider.insert("type".into(), json!(protocol));
        if let Some(enabled) = request.enabled {
            provider.insert("enabled".into(), json!(enabled));
        }
        if let Some(models) = &request.models {
            provider.insert("models".into(), json!(models));
        }
        if request.clear_api_key == Some(true) {
            for key in ["api_key", "apiKey", "apikey"] {
                provider.remove(key);
            }
        } else if let Some(api_key) = api_key {
            provider.insert("api_key".into(), json!(api_key));
        }

        ensure_provider_id(provider)
    } else {
        let mut provider = Map::new();
        let id = uuid::Uuid::new_v4().to_string();
        provider.insert("id".into(), json!(id));
        provider.insert("name".into(), json!(request.name.trim()));
        provider.insert("api_base_url".into(), json!(request.base_url.trim()));
        provider.insert("type".into(), json!(protocol));
        provider.insert("enabled".into(), json!(request.enabled.unwrap_or(true)));
        provider.insert(
            "models".into(),
            json!(request.models.clone().unwrap_or_default()),
        );
        if let Some(api_key) = api_key {
            provider.insert("api_key".into(), json!(api_key));
        }
        let id = ensure_provider_id(&mut provider);
        providers.push(Value::Object(provider));
        id
    };

    Ok((config, provider_id))
}

fn ensure_provider_id(provider: &mut Map<String, Value>) -> String {
    if let Some(id) = provider
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return id.to_string();
    }

    let id = uuid::Uuid::new_v4().to_string();
    provider.insert("id".into(), json!(id));
    id
}

fn provider_matches(provider: &Value, id: &str) -> bool {
    provider.get("id").and_then(Value::as_str) == Some(id)
        || provider.get("name").and_then(Value::as_str) == Some(id)
}

fn locate_provider(config: &Value, id: &str) -> Option<(usize, String, String)> {
    let providers = config.get("Providers").and_then(Value::as_array)?;
    let index = providers
        .iter()
        .position(|provider| provider_matches(provider, id))?;
    let provider = &providers[index];
    let provider_id = provider
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let provider_name = provider
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some((index, provider_id, provider_name))
}

fn profiles_using(config: &Value, provider_id: &str, provider_name: &str) -> Vec<String> {
    let profiles = config
        .pointer("/profile/profiles")
        .and_then(Value::as_array);

    let Some(profiles) = profiles else {
        return Vec::new();
    };

    profiles
        .iter()
        .filter(|profile| {
            let by_id = !provider_id.is_empty()
                && profile.get("providerId").and_then(Value::as_str) == Some(provider_id);
            let by_name = !provider_name.is_empty()
                && profile.get("providerName").and_then(Value::as_str) == Some(provider_name);
            by_id || by_name
        })
        .filter_map(|profile| {
            profile
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| profile.get("id").and_then(Value::as_str))
                .map(str::to_string)
        })
        .collect()
}

fn find_provider(config: &Value, id: &str) -> Option<Value> {
    config
        .get("Providers")
        .and_then(Value::as_array)?
        .iter()
        .find(|provider| provider_matches(provider, id))
        .cloned()
}

fn is_revision_conflict(error: &AppCommandError) -> bool {
    error
        .message
        .contains("Configuration changed since it was loaded")
        || error.message.contains("configRevision")
        || error.message.contains("conflict")
}

async fn save_config(config: Value) -> Result<Value, AppCommandError> {
    client::call_with_timeout(
        "saveConfig",
        Some(vec![config, json!({ "applyProfile": true })]),
        SAVE_TIMEOUT_SECS,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_one_provider() -> Value {
        json!({
            "configRevision": "r1",
            "Providers": [{
                "id": "p1",
                "name": "DeepSeek",
                "api_base_url": "https://api.deepseek.com/v1",
                "type": "openai_chat_completions",
                "enabled": true,
                "models": ["deepseek-chat"],
                "transformer": { "use": ["tool"] }
            }],
            "profile": { "profiles": [] }
        })
    }

    #[test]
    fn update_preserves_unmodelled_provider_fields() {
        let request = ProviderSaveRequest {
            original_id: Some("p1".into()),
            name: "Ignored".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            protocol: Some("openai_chat_completions".into()),
            api_key: Some("sk-new".into()),
            clear_api_key: None,
            enabled: Some(false),
            models: Some(vec!["deepseek-chat".into(), "deepseek-reasoner".into()]),
        };

        let (config, id) = merge_provider(config_with_one_provider(), &request).unwrap();
        let provider = find_provider(&config, &id).unwrap();

        assert_eq!(provider["name"], "DeepSeek");
        assert_eq!(provider["enabled"], false);
        assert_eq!(provider["api_key"], "sk-new");
        assert_eq!(provider["transformer"], json!({ "use": ["tool"] }));
        assert_eq!(
            provider["models"],
            json!(["deepseek-chat", "deepseek-reasoner"])
        );
    }

    #[test]
    fn create_assigns_id_and_keeps_empty_key() {
        let mut config = config_with_one_provider();
        config["Providers"] = json!([]);

        let request = ProviderSaveRequest {
            original_id: None,
            name: "Local".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            protocol: Some("openai_chat_completions".into()),
            api_key: None,
            clear_api_key: None,
            enabled: None,
            models: Some(vec!["llama3".into()]),
        };

        let (config, id) = merge_provider(config, &request).unwrap();
        let provider = find_provider(&config, &id).unwrap();

        assert!(!id.is_empty());
        assert_eq!(provider["name"], "Local");
        assert_eq!(provider["enabled"], true);
        assert!(provider.get("api_key").is_none());
    }

    #[test]
    fn delete_is_refused_while_a_profile_uses_the_provider() {
        let mut config = config_with_one_provider();
        config["profile"]["profiles"] = json!([{
            "id": "profile-1",
            "name": "Claude · DeepSeek",
            "providerId": "p1",
            "providerName": "DeepSeek"
        }]);

        assert_eq!(
            profiles_using(&config, "p1", "DeepSeek"),
            vec!["Claude · DeepSeek"]
        );
    }
}
