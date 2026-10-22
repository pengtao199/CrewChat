//! [crewchat] Per-row switches on the Models settings page: enable/disable one
//! model or one credential without rebuilding the whole provider.
//!
//! CCR has no per-model "disabled" flag — a model is routable exactly while it
//! sits inside `provider.models`. Disabling therefore removes it from `models`
//! and records it in a core-owned file (`ccr-disabled-models.json` under
//! `CODEG_DATA_DIR`), so the page can keep showing the row switched off and
//! switch it back on without a fresh probe. The file is core-owned because
//! CCR's loader rebuilds every provider from a fixed field list and would drop
//! an unknown `disabledModels` field on the next load.
//!
//! Every mutation is read → merge into the raw JSON → `saveConfig` under one
//! lock, retried once on a 409 revision conflict (same contract as
//! `providers.rs`). Unknown provider fields survive because the document is
//! round-tripped, never rebuilt from a DTO.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

use super::client;
use crate::app_error::AppCommandError;

const SAVE_TIMEOUT_SECS: u64 = 60;
const DISABLED_MODELS_FILE: &str = "ccr-disabled-models.json";

static TOGGLE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn toggle_lock() -> &'static Mutex<()> {
    TOGGLE_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelToggleRequest {
    /// Provider `id` (or legacy `name`).
    pub provider_id: String,
    pub model: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialToggleRequest {
    /// Provider `id` (or legacy `name`).
    pub provider_id: String,
    /// Credential `id` (or legacy `name` / `label`).
    pub credential_id: String,
    pub enabled: bool,
}

pub async fn set_model_enabled(request: ModelToggleRequest) -> Result<bool, AppCommandError> {
    let model = request.model.trim().to_string();
    if model.is_empty() {
        return Err(AppCommandError::invalid_input("模型 id 不能为空"));
    }

    let _guard = toggle_lock().lock().await;
    let provider_key = mutate(|config| {
        apply_model_toggle(config, &request.provider_id, &model, request.enabled)
    })
    .await?;

    toggle_disabled_model(&provider_key, &model, request.enabled)?;
    Ok(true)
}

pub async fn set_credential_enabled(
    request: CredentialToggleRequest,
) -> Result<bool, AppCommandError> {
    let _guard = toggle_lock().lock().await;
    mutate(|config| {
        apply_credential_toggle(
            config,
            &request.provider_id,
            &request.credential_id,
            request.enabled,
        )
    })
    .await?;
    Ok(true)
}

/// Provider key → models the user switched off. The Swift page merges these
/// back into each provider's `models` so the rows stay visible.
pub fn disabled_models() -> Result<Value, AppCommandError> {
    let map = read_disabled_models();
    serde_json::to_value(map)
        .map_err(|error| AppCommandError::io_error(format!("读取模型停用列表失败: {error}")))
}

async fn mutate<T>(
    mutate_config: impl Fn(&mut Value) -> Result<T, AppCommandError>,
) -> Result<T, AppCommandError> {
    for attempt in 0..2 {
        let mut config = client::call("getConfig", None).await?;
        let outcome = mutate_config(&mut config)?;

        match save_config(config).await {
            Ok(_) => return Ok(outcome),
            Err(error) if attempt == 0 && is_revision_conflict(&error) => continue,
            Err(error) => return Err(error),
        }
    }

    Err(AppCommandError::network("CCR 配置保存冲突，重试后仍未成功"))
}

/// Returns the stable key the disabled-model file is keyed by.
fn apply_model_toggle(
    config: &mut Value,
    provider_id: &str,
    model: &str,
    enabled: bool,
) -> Result<String, AppCommandError> {
    let provider = provider_mut(config, provider_id)?;
    let key = provider_key(provider);
    let mut models = string_list(provider.get("models"));

    if enabled {
        if !models.iter().any(|item| item == model) {
            models.push(model.to_string());
        }
    } else {
        models.retain(|item| item != model);
    }

    provider.insert("models".into(), json!(models));
    Ok(key)
}

fn apply_credential_toggle(
    config: &mut Value,
    provider_id: &str,
    credential_id: &str,
    enabled: bool,
) -> Result<(), AppCommandError> {
    let credential_id = credential_id.trim();
    if credential_id.is_empty() {
        return Err(AppCommandError::invalid_input("凭据 id 不能为空"));
    }

    let provider = provider_mut(config, provider_id)?;
    let credentials = provider
        .get_mut("credentials")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            AppCommandError::invalid_input(
                "这个供应商只有旧版单密钥，没有独立凭据可供单独启停",
            )
        })?;

    let credential = credentials
        .iter_mut()
        .find(|credential| credential_matches(credential, credential_id))
        .ok_or_else(|| AppCommandError::not_found(format!("找不到凭据 {credential_id}")))?;

    let object = credential
        .as_object_mut()
        .ok_or_else(|| AppCommandError::configuration_invalid("凭据配置格式不正确"))?;
    object.insert("enabled".into(), json!(enabled));

    Ok(())
}

fn provider_mut<'a>(
    config: &'a mut Value,
    id: &str,
) -> Result<&'a mut Map<String, Value>, AppCommandError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(AppCommandError::invalid_input("供应商 id 不能为空"));
    }

    config
        .get_mut("Providers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| AppCommandError::configuration_invalid("CCR 配置缺少 Providers 列表"))?
        .iter_mut()
        .find(|provider| provider_matches(provider, id))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AppCommandError::not_found(format!("找不到供应商 {id}")))
}

fn provider_matches(provider: &Value, id: &str) -> bool {
    provider.get("id").and_then(Value::as_str) == Some(id)
        || provider.get("name").and_then(Value::as_str) == Some(id)
}

fn provider_key(provider: &Map<String, Value>) -> String {
    provider
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| provider.get("name").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

fn credential_matches(credential: &Value, id: &str) -> bool {
    ["id", "name", "label"]
        .iter()
        .any(|key| credential.get(*key).and_then(Value::as_str) == Some(id))
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn disabled_models_path() -> Result<PathBuf, AppCommandError> {
    let dir = std::env::var_os("CODEG_DATA_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|dir| dir.join("codeg")))
        .ok_or_else(|| AppCommandError::io_error("无法解析 CODEG_DATA_DIR"))?;
    Ok(dir.join(DISABLED_MODELS_FILE))
}

fn read_disabled_models() -> BTreeMap<String, Vec<String>> {
    let Ok(path) = disabled_models_path() else {
        return BTreeMap::new();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) else {
        return BTreeMap::new();
    };

    map.into_iter()
        .filter_map(|(key, value)| {
            let models = string_list(Some(&value));
            (!key.trim().is_empty() && !models.is_empty()).then_some((key, models))
        })
        .collect()
}

fn toggle_disabled_model(
    provider_key: &str,
    model: &str,
    enabled: bool,
) -> Result<(), AppCommandError> {
    let mut map = read_disabled_models();
    apply_disabled_toggle(&mut map, provider_key, model, enabled);
    write_disabled_models(&map)
}

fn apply_disabled_toggle(
    map: &mut BTreeMap<String, Vec<String>>,
    provider_key: &str,
    model: &str,
    enabled: bool,
) {
    let row = map.entry(provider_key.to_string()).or_default();

    if enabled {
        row.retain(|item| item != model);
    } else if !row.iter().any(|item| item == model) {
        row.push(model.to_string());
    }

    if row.is_empty() {
        map.remove(provider_key);
    }
}

fn write_disabled_models(map: &BTreeMap<String, Vec<String>>) -> Result<(), AppCommandError> {
    let path = disabled_models_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| AppCommandError::io_error(format!("创建数据目录失败: {error}")))?;
    }

    let body = serde_json::to_vec_pretty(map)
        .map_err(|error| AppCommandError::io_error(format!("序列化模型停用列表失败: {error}")))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)
        .map_err(|error| AppCommandError::io_error(format!("写入模型停用列表失败: {error}")))?;
    std::fs::rename(&tmp, &path)
        .map_err(|error| AppCommandError::io_error(format!("替换模型停用列表失败: {error}")))
}

async fn save_config(config: Value) -> Result<Value, AppCommandError> {
    client::call_with_timeout(
        "saveConfig",
        Some(vec![config, json!({ "applyProfile": true })]),
        SAVE_TIMEOUT_SECS,
    )
    .await
}

fn is_revision_conflict(error: &AppCommandError) -> bool {
    error
        .message
        .contains("Configuration changed since it was loaded")
        || error.message.contains("configRevision")
        || error.message.contains("conflict")
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
                "models": ["deepseek-chat", "deepseek-reasoner"],
                "credentials": [
                    { "id": "c1", "name": "Main", "api_key": "sk-a", "enabled": true, "account": { "enabled": true } },
                    { "id": "c2", "name": "Backup", "api_key": "sk-b", "enabled": true }
                ],
                "transformer": { "use": ["tool"] }
            }]
        })
    }

    #[test]
    fn disabling_a_model_only_removes_it_from_the_routable_list() {
        let mut config = config_with_one_provider();
        let key = apply_model_toggle(&mut config, "p1", "deepseek-chat", false).unwrap();

        assert_eq!(key, "p1");
        let provider = &config["Providers"][0];
        assert_eq!(provider["models"], json!(["deepseek-reasoner"]));
        assert_eq!(provider["transformer"], json!({ "use": ["tool"] }));
    }

    #[test]
    fn enabling_a_model_adds_it_back_once() {
        let mut config = config_with_one_provider();
        apply_model_toggle(&mut config, "p1", "deepseek-chat", false).unwrap();
        apply_model_toggle(&mut config, "p1", "deepseek-chat", true).unwrap();
        apply_model_toggle(&mut config, "p1", "deepseek-chat", true).unwrap();

        assert_eq!(
            config["Providers"][0]["models"],
            json!(["deepseek-reasoner", "deepseek-chat"])
        );
    }

    #[test]
    fn disabled_list_adds_and_removes_without_duplicates() {
        let mut map = BTreeMap::new();
        apply_disabled_toggle(&mut map, "p1", "m1", false);
        apply_disabled_toggle(&mut map, "p1", "m1", false);
        assert_eq!(map.get("p1"), Some(&vec!["m1".to_string()]));

        apply_disabled_toggle(&mut map, "p1", "m1", true);
        assert!(map.is_empty());
    }

    #[test]
    fn credential_toggle_preserves_unknown_fields() {
        let mut config = config_with_one_provider();
        apply_credential_toggle(&mut config, "p1", "c1", false).unwrap();

        let credential = &config["Providers"][0]["credentials"][0];
        assert_eq!(credential["enabled"], false);
        assert_eq!(credential["account"], json!({ "enabled": true }));
        assert_eq!(credential["api_key"], "sk-a");
    }

    #[test]
    fn unknown_provider_is_not_found() {
        let mut config = config_with_one_provider();
        let error = apply_model_toggle(&mut config, "missing", "m", true).unwrap_err();
        assert!(matches!(
            error.code,
            crate::app_error::AppErrorCode::NotFound
        ));
    }

}
