//! Import from a live CCR (preferred) or its SQLite file.
//!
//! The live `getConfig` is authoritative: CCR injects the runtime `APIKEYS`
//! into it, which the static database does not carry. The SQLite path is only
//! a fallback for a stopped CCR, and refuses encrypted caller keys rather than
//! inventing plaintext.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use crate::config::Config;
use crate::store::Store;

pub struct Imported {
    pub config: Value,
    pub claude_models: Vec<String>,
    pub source: String,
}

/// Load, merge sidecar state, and persist one CCR catalog.
pub async fn run(cfg: &Config, store: &Store) -> Result<Imported> {
    let (mut config, source) = load_config(cfg).await?;
    merge_disabled_models(&mut config, cfg);
    validate(&config)?;

    let local_key = local_gateway_key(&config);
    // Discover the Claude-visible ids from the SOURCE gateway (CCR on :3456),
    // not from our own listener: the import runs before we bind, and the
    // `[1m]` flags exist only in that catalog.
    let source_gateway = config
        .get("routerEndpoint")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| cfg.gateway_url());
    let claude_models = match local_key {
        Some(key) => match fetch_claude_models(&source_gateway, &key).await {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!("claude model discovery from {source_gateway} failed: {error}");
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    store.apply_config(&config)?;
    if !claude_models.is_empty() {
        store.mark_one_million(&claude_models)?;
    }
    store.bump_revision()?;

    Ok(Imported {
        config,
        claude_models,
        source,
    })
}

async fn load_config(cfg: &Config) -> Result<(Value, String)> {
    if let Some(url) = &cfg.ccr_web_url {
        match fetch_live(url, cfg.ccr_web_token.as_deref()).await {
            Ok(value) => return Ok((value, format!("live:{url}"))),
            Err(error) => {
                tracing::warn!("live CCR at {url} unavailable ({error}); falling back to sqlite")
            }
        }
    }
    let path = cfg.ccr_home.join("config.sqlite");
    let value = read_sqlite(&path)?;
    Ok((value, format!("sqlite:{}", path.display())))
}

async fn fetch_live(url: &str, token: Option<&str>) -> Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut request = client
        .post(format!("{}/api/ccr/rpc", url.trim_end_matches('/')))
        .json(&json!({"method": "getConfig", "args": []}));
    if let Some(token) = token {
        request = request.header("x-ccr-web-auth", token);
    }
    let response = request.send().await.context("CCR RPC request failed")?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .context("CCR RPC returned invalid JSON")?;
    if !status.is_success() || body.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!(
            "CCR getConfig failed: {}",
            body.pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }
    body.get("value")
        .cloned()
        .context("CCR getConfig returned no value")
}

fn read_sqlite(path: &Path) -> Result<Value> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {}", path.display()))?;
    let raw: String = conn
        .query_row(
            "SELECT value_json FROM app_config WHERE key = 'default'",
            [],
            |row| row.get(0),
        )
        .context("app_config.default is missing; start CCR once or pass CCR_WEB_URL")?;
    let mut config: Value = serde_json::from_str(&raw).context("app_config.default is not JSON")?;

    let has_keys = config
        .get("APIKEYS")
        .and_then(Value::as_array)
        .map(|keys| !keys.is_empty())
        .unwrap_or(false);
    if !has_keys {
        let keys = read_plaintext_api_keys(&conn)?;
        if let Some(object) = config.as_object_mut() {
            object.insert("APIKEYS".into(), Value::Array(keys));
        }
    }
    Ok(config)
}

/// `api_keys.encrypted_key` may be plaintext or encrypted. Only import rows we
/// can prove are plaintext; the alternative is inventing a key that Claude
/// Code's existing `apiKeyHelper` will not match.
fn read_plaintext_api_keys(conn: &Connection) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, encrypted_key, encryption, limits_json, enabled, created_at
         FROM api_keys",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut keys = Vec::new();
    for (id, name, key, encryption, limits, enabled, created_at) in rows {
        let Some(key) = key.filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        let plaintext = encryption
            .as_deref()
            .map(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "" | "none" | "plain" | "plaintext"
                )
            })
            .unwrap_or(true);
        if !plaintext {
            tracing::warn!(
                "skipping encrypted caller key {id}; import through live getConfig instead"
            );
            continue;
        }
        keys.push(json!({
            "id": id,
            "name": name,
            "key": key,
            "limits": limits.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()),
            "enabled": enabled.unwrap_or(1) != 0,
            "createdAt": created_at,
        }));
    }
    Ok(keys)
}

fn merge_disabled_models(config: &mut Value, cfg: &Config) {
    let Some(path) = &cfg.disabled_models_path else {
        return;
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) else {
        tracing::warn!(
            "ignoring malformed disabled-model file at {}",
            path.display()
        );
        return;
    };
    let Some(providers) = config.get_mut("Providers").and_then(Value::as_array_mut) else {
        return;
    };
    for (provider_name, models) in map {
        let Some(list) = models.as_array() else {
            continue;
        };
        let disabled: Vec<Value> = list
            .iter()
            .filter_map(Value::as_str)
            .map(|model| Value::String(model.to_string()))
            .collect();
        if disabled.is_empty() {
            continue;
        }
        for provider in providers.iter_mut() {
            let matches = provider
                .get("name")
                .and_then(Value::as_str)
                .map(|name| name.eq_ignore_ascii_case(&provider_name))
                .unwrap_or(false)
                || provider
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| id.eq_ignore_ascii_case(&provider_name))
                    .unwrap_or(false);
            if !matches {
                continue;
            }
            let object = provider.as_object_mut().unwrap();
            let existing = object
                .entry("disabledModels")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(existing) = existing.as_array_mut() {
                for model in &disabled {
                    if !existing.contains(model) {
                        existing.push(model.clone());
                    }
                }
            }
        }
    }
    tracing::info!("merged disabled models from {}", path.display());
}

fn validate(config: &Value) -> Result<()> {
    let providers = config
        .get("Providers")
        .and_then(Value::as_array)
        .context("imported config has no Providers")?;
    if providers.is_empty() {
        bail!("imported config has an empty Providers list");
    }
    Ok(())
}

fn local_gateway_key(config: &Value) -> Option<String> {
    config
        .get("APIKEYS")
        .and_then(Value::as_array)?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some("local-gateway"))
        .and_then(|entry| entry.get("key"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

async fn fetch_claude_models(gateway: &str, key: &str) -> Result<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let body: Value = client
        .get(format!("{gateway}/v1/models"))
        .header("Authorization", format!("Bearer {key}"))
        .header("anthropic-version", "2023-06-01")
        .header("user-agent", "claude-cli/2.0.0")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(body
        .get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_key_is_found() {
        let config = json!({"APIKEYS": [{"id": "local-gateway", "key": "sk-local"}]});
        assert_eq!(local_gateway_key(&config).as_deref(), Some("sk-local"));
    }

    #[test]
    fn validation_rejects_empty_providers() {
        assert!(validate(&json!({"Providers": []})).is_err());
        assert!(validate(&json!({"Providers": [{"name": "x"}]})).is_ok());
    }
}
