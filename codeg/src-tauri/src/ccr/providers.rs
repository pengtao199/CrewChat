//! [crewchat] CCR provider management — the *write* half of the model layer.
//!
//! The management API only exposes whole-document `saveConfig`, and a provider
//! object carries fields this app does not model (`transformer`, `modelMetadata`,
//! `account`, …). Rebuilding a provider from the Swift DTO would silently drop
//! them, so every mutation here is a **read → merge into the raw JSON → save**
//! under one process-wide lock.
//!
//! Credentials are a pool: the editor sends the full desired list, and this
//! module merges each row into the existing credential object (by id, then by
//! name), preserving fields it does not know. The provider-level `api_key` is
//! removed once the pool is written — the same shape CCR's own provider dialog
//! produces.
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
    /// Legacy single-key form. Ignored when `credentials` is present.
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: Option<bool>,
    pub enabled: Option<bool>,
    /// Full desired model list. Absent keeps the stored list.
    pub models: Option<Vec<String>>,
    /// Full desired credential pool. Absent keeps the stored credentials.
    pub credentials: Option<Vec<ProviderCredentialPatch>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCredentialPatch {
    pub id: Option<String>,
    pub name: Option<String>,
    pub api_key: Option<String>,
    pub priority: Option<i64>,
    pub weight: Option<f64>,
    /// `null` removes the stored limits; absent leaves them alone.
    pub limits: Option<Value>,
    pub enabled: Option<bool>,
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
    if let Some(credentials) = &request.credentials {
        let has_key = credentials.iter().any(|credential| {
            credential
                .api_key
                .as_deref()
                .map(str::trim)
                .is_some_and(|key| !key.is_empty())
        });
        if !has_key {
            return Err(AppCommandError::invalid_input("至少需要一个密钥"));
        }
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

    if original_id.is_none() {
        let provider_id = {
            let providers = config
                .get_mut("Providers")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| AppCommandError::configuration_invalid("CCR 配置缺少 Providers 列表"))?;

            let mut provider = Map::new();
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
            if let Some(credentials) = &request.credentials {
                merge_credentials(&mut provider, credentials);
            }
            let id = ensure_provider_id(&mut provider);
            providers.push(Value::Object(provider));
            id
        };

        return Ok((config, provider_id));
    }

    let original_id = original_id.unwrap();
    let index = config
        .get("Providers")
        .and_then(Value::as_array)
        .and_then(|providers| {
            providers
                .iter()
                .position(|provider| provider_matches(provider, original_id))
        })
        .ok_or_else(|| AppCommandError::not_found(format!("找不到供应商 {original_id}")))?;

    let old_name = config["Providers"][index]
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let provider_id = {
        let provider = config["Providers"][index]
            .as_object_mut()
            .ok_or_else(|| AppCommandError::configuration_invalid("供应商配置格式不正确"))?;

        provider.insert("name".into(), json!(request.name.trim()));
        provider.insert("api_base_url".into(), json!(request.base_url.trim()));
        provider.insert("type".into(), json!(protocol));
        if let Some(enabled) = request.enabled {
            provider.insert("enabled".into(), json!(enabled));
        }
        if let Some(models) = &request.models {
            provider.insert("models".into(), json!(models));
        }

        if let Some(credentials) = &request.credentials {
            merge_credentials(provider, credentials);
        } else if request.clear_api_key == Some(true) {
            for key in ["api_key", "apiKey", "apikey"] {
                provider.remove(key);
            }
        } else if let Some(api_key) = api_key {
            provider.insert("api_key".into(), json!(api_key));
        }

        ensure_provider_id(provider)
    };

    if old_name != request.name.trim() {
        rename_provider_references(&mut config, &old_name, request.name.trim());
    }

    Ok((config, provider_id))
}

/// Merge the editor's full credential pool into the raw provider object.
/// Existing credentials are matched by id, then by name, so unknown fields on
/// a row (`account`, plugin options, …) survive an edit.
fn merge_credentials(provider: &mut Map<String, Value>, patches: &[ProviderCredentialPatch]) {
    let existing = provider
        .get("credentials")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut next = Vec::with_capacity(patches.len());

    for patch in patches {
        let patch_id = patch
            .id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let patch_name = patch
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let matched = existing.iter().find(|credential| {
            if let Some(id) = patch_id {
                if credential.get("id").and_then(Value::as_str) == Some(id) {
                    return true;
                }
            }
            if let Some(name) = patch_name {
                let stored = credential
                    .get("name")
                    .or_else(|| credential.get("label"))
                    .and_then(Value::as_str);
                if stored == Some(name) {
                    return true;
                }
            }
            false
        });

        let mut credential = matched
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        match patch_name {
            Some(name) => {
                credential.insert("name".into(), json!(name));
                credential.remove("label");
            }
            None => {
                credential.remove("name");
                credential.remove("label");
            }
        }

        match patch.api_key.as_deref().map(str::trim) {
            Some(key) if !key.is_empty() => {
                credential.insert("api_key".into(), json!(key));
                credential.remove("apiKey");
                credential.remove("apikey");
            }
            _ => {
                for key in ["api_key", "apiKey", "apikey"] {
                    credential.remove(key);
                }
            }
        }

        match patch.priority {
            Some(priority) => {
                credential.insert("priority".into(), json!(priority));
            }
            None => {
                credential.remove("priority");
            }
        }

        match patch.weight {
            Some(weight) if weight > 0.0 => {
                credential.insert("weight".into(), json!(weight));
            }
            _ => {
                credential.remove("weight");
            }
        }

        match patch.limits.as_ref() {
            Some(Value::Null) | None => {
                credential.remove("limits");
            }
            Some(limits) => {
                credential.insert("limits".into(), limits.clone());
            }
        }

        if let Some(enabled) = patch.enabled {
            credential.insert("enabled".into(), json!(enabled));
        }

        ensure_credential_id(&mut credential);
        next.push(Value::Object(credential));
    }

    provider.insert("credentials".into(), json!(next));

    // The pool replaces the single-key form, matching CCR's own provider editor.
    for key in ["api_key", "apiKey", "apikey"] {
        provider.remove(key);
    }
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

fn ensure_credential_id(credential: &mut Map<String, Value>) {
    if credential
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        return;
    }

    credential.insert("id".into(), json!(uuid::Uuid::new_v4().to_string()));
}

fn rename_provider_references(config: &mut Value, old_name: &str, new_name: &str) {
    if old_name.is_empty() || old_name == new_name {
        return;
    }

    if config.get("preferredProvider").and_then(Value::as_str) == Some(old_name) {
        config["preferredProvider"] = json!(new_name);
    }

    if let Some(profiles) = config
        .pointer_mut("/profile/profiles")
        .and_then(Value::as_array_mut)
    {
        for profile in profiles {
            if profile.get("providerName").and_then(Value::as_str) == Some(old_name) {
                profile["providerName"] = json!(new_name);
            }
        }
    }
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
            "preferredProvider": "DeepSeek",
            "Providers": [{
                "id": "p1",
                "name": "DeepSeek",
                "api_base_url": "https://api.deepseek.com/v1",
                "type": "openai_chat_completions",
                "enabled": true,
                "models": ["deepseek-chat"],
                "transformer": { "use": ["tool"] }
            }],
            "profile": {
                "profiles": [{
                    "id": "profile-1",
                    "name": "Claude · DeepSeek",
                    "providerId": "p1",
                    "providerName": "DeepSeek"
                }]
            }
        })
    }

    fn credential(name: &str, key: &str) -> ProviderCredentialPatch {
        ProviderCredentialPatch {
            id: None,
            name: Some(name.into()),
            api_key: Some(key.into()),
            priority: Some(1),
            weight: Some(1.0),
            limits: None,
            enabled: Some(true),
        }
    }

    #[test]
    fn update_preserves_unmodelled_provider_fields() {
        let request = ProviderSaveRequest {
            original_id: Some("p1".into()),
            name: "DeepSeek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            protocol: Some("openai_chat_completions".into()),
            api_key: None,
            clear_api_key: None,
            enabled: Some(false),
            models: Some(vec!["deepseek-chat".into(), "deepseek-reasoner".into()]),
            credentials: Some(vec![credential("Main", "sk-new")]),
        };

        let (config, id) = merge_provider(config_with_one_provider(), &request).unwrap();
        let provider = find_provider(&config, &id).unwrap();

        assert_eq!(provider["name"], "DeepSeek");
        assert_eq!(provider["enabled"], false);
        assert!(provider.get("api_key").is_none());
        assert_eq!(provider["transformer"], json!({ "use": ["tool"] }));
        assert_eq!(
            provider["models"],
            json!(["deepseek-chat", "deepseek-reasoner"])
        );
        assert_eq!(provider["credentials"][0]["name"], "Main");
        assert_eq!(provider["credentials"][0]["api_key"], "sk-new");
        assert!(provider["credentials"][0]["id"].is_string());
    }

    #[test]
    fn credential_pool_preserves_unknown_fields_and_order() {
        let mut config = config_with_one_provider();
        config["Providers"][0]["credentials"] = json!([
            { "id": "c1", "name": "A", "api_key": "sk-a", "account": { "enabled": true } },
            { "id": "c2", "name": "B", "api_key": "sk-b" }
        ]);

        let request = ProviderSaveRequest {
            original_id: Some("p1".into()),
            name: "DeepSeek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            protocol: Some("openai_chat_completions".into()),
            api_key: None,
            clear_api_key: None,
            enabled: None,
            models: None,
            credentials: Some(vec![
                ProviderCredentialPatch {
                    id: Some("c2".into()),
                    name: Some("B2".into()),
                    api_key: Some("sk-b2".into()),
                    priority: Some(2),
                    weight: Some(3.0),
                    limits: Some(json!({ "rpm": 60, "tpm": 100000 })),
                    enabled: Some(false),
                },
                ProviderCredentialPatch {
                    id: Some("c1".into()),
                    name: Some("A".into()),
                    api_key: Some("sk-a".into()),
                    priority: None,
                    weight: None,
                    limits: None,
                    enabled: Some(true),
                },
            ]),
        };

        let (config, id) = merge_provider(config, &request).unwrap();
        let provider = find_provider(&config, &id).unwrap();
        let credentials = provider["credentials"].as_array().unwrap();

        assert_eq!(credentials.len(), 2);
        assert_eq!(credentials[0]["id"], "c2");
        assert_eq!(credentials[0]["name"], "B2");
        assert_eq!(credentials[0]["api_key"], "sk-b2");
        assert_eq!(credentials[0]["priority"], 2);
        assert_eq!(credentials[0]["weight"], 3.0);
        assert_eq!(credentials[0]["limits"], json!({ "rpm": 60, "tpm": 100000 }));
        assert_eq!(credentials[1]["id"], "c1");
        assert_eq!(credentials[1]["account"], json!({ "enabled": true }));
        assert!(credentials[1].get("priority").is_none());
    }

    #[test]
    fn single_key_migrates_into_the_pool() {
        let mut config = config_with_one_provider();
        config["Providers"][0]["api_key"] = json!("sk-legacy");

        let request = ProviderSaveRequest {
            original_id: Some("p1".into()),
            name: "DeepSeek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            protocol: Some("openai_chat_completions".into()),
            api_key: None,
            clear_api_key: None,
            enabled: None,
            models: None,
            credentials: Some(vec![ProviderCredentialPatch {
                id: Some("api_key".into()),
                name: Some("Key 1".into()),
                api_key: Some("sk-legacy".into()),
                priority: None,
                weight: None,
                limits: None,
                enabled: Some(true),
            }]),
        };

        let (config, id) = merge_provider(config, &request).unwrap();
        let provider = find_provider(&config, &id).unwrap();

        assert!(provider.get("api_key").is_none());
        assert_eq!(provider["credentials"][0]["api_key"], "sk-legacy");
    }

    #[test]
    fn renaming_a_provider_updates_profile_references() {
        let request = ProviderSaveRequest {
            original_id: Some("p1".into()),
            name: "DeepSeek New".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            protocol: Some("openai_chat_completions".into()),
            api_key: None,
            clear_api_key: None,
            enabled: None,
            models: None,
            credentials: Some(vec![credential("Main", "sk-new")]),
        };

        let (config, _) = merge_provider(config_with_one_provider(), &request).unwrap();

        assert_eq!(config["preferredProvider"], "DeepSeek New");
        assert_eq!(
            config["profile"]["profiles"][0]["providerName"],
            "DeepSeek New"
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
            credentials: None,
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
        let config = config_with_one_provider();

        assert_eq!(
            profiles_using(&config, "p1", "DeepSeek"),
            vec!["Claude · DeepSeek"]
        );
    }
}
