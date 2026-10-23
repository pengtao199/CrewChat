//! Per-role CLI homes. Each session gets its own config dir so two agents on
//! the same CLI can run different models without fighting over a global file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::engines::OverlayKind;

pub struct RoleOverlay {
    pub env: BTreeMap<String, String>,
    pub model_label: String,
}

/// Build a disposable home for one role. `api_key` is only needed by the
/// Codex/OpenCode overlays; Claude reads the key through `apiKeyHelper`.
pub fn prepare(
    kind: OverlayKind,
    role: &Value,
    gateway: &str,
    api_key: Option<&str>,
    data_dir: &Path,
) -> Result<RoleOverlay> {
    let role_id = role.get("id").and_then(Value::as_str).unwrap_or("role");
    let provider = role
        .get("model_provider")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let model = role
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let model_label = if provider.is_empty() {
        model.to_string()
    } else {
        format!("{provider}/{model}")
    };

    let safe_role = role_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let home = data_dir.join("roles").join(format!(
        "{}-{}",
        if safe_role.is_empty() {
            "role"
        } else {
            &safe_role
        },
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home)
        .with_context(|| format!("create role home {}", home.display()))?;

    match kind {
        OverlayKind::Claude => prepare_claude(&home, &model_label, gateway),
        OverlayKind::Codex => prepare_codex(&home, &model_label, gateway, api_key),
        OverlayKind::OpenCode => prepare_opencode(&home, &model_label, gateway, api_key),
        OverlayKind::None => anyhow::bail!("this engine has no per-role model overlay"),
    }
}

fn prepare_claude(home: &Path, model_label: &str, gateway: &str) -> Result<RoleOverlay> {
    let mut env = serde_json::Map::new();
    env.insert("ANTHROPIC_BASE_URL".into(), json!(gateway));
    env.insert("ANTHROPIC_API_BASE_URL".into(), json!(gateway));
    env.insert("CLAUDE_AGENT_API_BASE_URL".into(), json!(gateway));
    env.insert(
        "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY".into(),
        json!("1"),
    );
    if !model_label.is_empty() {
        env.insert("ANTHROPIC_MODEL".into(), json!(model_label));
    }
    env.insert("TZ".into(), json!("UTC"));
    env.insert(
        "CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT".into(),
        json!("1"),
    );
    let mut settings = json!({"env": env});
    // `apiKeyHelper` and `enabledPlugins` are top-level Claude Code settings,
    // not env vars.
    if let Some(helper) = global_claude_helper() {
        settings["apiKeyHelper"] = json!(helper);
    }
    if let Some(plugins) = global_claude_plugins() {
        settings["enabledPlugins"] = plugins;
    }
    std::fs::write(
        home.join("settings.json"),
        serde_json::to_vec_pretty(&settings)?,
    )?;
    link_shared_claude_dirs(home);

    let mut result = BTreeMap::new();
    result.insert(
        "CLAUDE_CONFIG_DIR".to_string(),
        home.to_string_lossy().to_string(),
    );
    Ok(RoleOverlay {
        env: result,
        model_label: model_label.to_string(),
    })
}

/// Keep transcripts, plugins, skills and shell snapshots in the shared tree
/// the rest of the app already reads.
fn link_shared_claude_dirs(home: &Path) {
    const SHARED: &[&str] = &[
        "projects",
        "plugins",
        "todos",
        "statsig",
        "shell-snapshots",
        "downloads",
    ];
    let Some(base) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".claude"))
    else {
        return;
    };
    for name in SHARED {
        let target = base.join(name);
        if !target.is_dir() {
            continue;
        }
        let link = home.join(name);
        if link.exists() {
            continue;
        }
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&target, &link);
    }
}

fn prepare_codex(
    home: &Path,
    model_label: &str,
    gateway: &str,
    api_key: Option<&str>,
) -> Result<RoleOverlay> {
    let api_key = api_key.context("Codex overlay needs the local gateway key")?;
    let base = format!("{}/v1", gateway.trim_end_matches('/'));
    let model = if model_label.is_empty() {
        "gpt-5".to_string()
    } else {
        model_label.to_string()
    };
    let config = format!(
        "model = {model:?}\nmodel_provider = \"crewchat\"\n\n[model_providers.crewchat]\nname = \"CrewChat\"\nbase_url = {base:?}\nwire_api = \"responses\"\nrequires_openai_auth = false\nenv_key = \"CODEG_CCR_GATEWAY_KEY\"\n"
    );
    std::fs::write(home.join("config.toml"), config)?;
    link_shared_codex_home(home);

    let mut env = BTreeMap::new();
    env.insert("CODEX_HOME".to_string(), home.to_string_lossy().to_string());
    env.insert("CODEG_CCR_GATEWAY_KEY".to_string(), api_key.to_string());
    Ok(RoleOverlay {
        env,
        model_label: model_label.to_string(),
    })
}

fn link_shared_codex_home(home: &Path) {
    let Some(base) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".codex"))
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == "config.toml" {
            continue;
        }
        let link = home.join(&name);
        if link.exists() {
            continue;
        }
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(entry.path(), &link);
    }
}

fn prepare_opencode(
    home: &Path,
    model_label: &str,
    gateway: &str,
    api_key: Option<&str>,
) -> Result<RoleOverlay> {
    let api_key = api_key.context("OpenCode overlay needs the local gateway key")?;
    let model = if model_label.is_empty() {
        "mimo-v2.6-flash".to_string()
    } else {
        model_label.to_string()
    };
    let config = json!({
        "$schema": "https://opencode.ai/config.json",
        "model": format!("crewchat/{model}"),
        "provider": {
            "crewchat": {
                "npm": "@ai-sdk/openai-compatible",
                "name": "CrewChat",
                "options": {
                    "baseURL": format!("{}/v1", gateway.trim_end_matches('/')),
                    "apiKey": "{env:CODEG_CCR_GATEWAY_KEY}",
                },
                "models": { model.clone(): { "name": model } }
            }
        }
    });
    std::fs::write(
        home.join("opencode.json"),
        serde_json::to_vec_pretty(&config)?,
    )?;

    let mut env = BTreeMap::new();
    env.insert(
        "OPENCODE_CONFIG".to_string(),
        home.join("opencode.json").to_string_lossy().to_string(),
    );
    env.insert("CODEG_CCR_GATEWAY_KEY".to_string(), api_key.to_string());
    Ok(RoleOverlay {
        env,
        model_label: model_label.to_string(),
    })
}

fn global_claude_plugins() -> Option<Value> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let raw = std::fs::read_to_string(home.join(".claude/settings.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value
        .get("enabledPlugins")
        .cloned()
        .filter(|value| !value.is_null())
}

fn global_claude_helper() -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let raw = std::fs::read_to_string(home.join(".claude/settings.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value
        .get("apiKeyHelper")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_overlay_writes_gateway_and_model() {
        let dir = tempfile::tempdir().unwrap();
        let role = serde_json::json!({
            "id": "r1",
            "model": "mimo-v2.6-flash",
            "model_provider": "OpenCode Go"
        });
        let overlay = prepare(
            OverlayKind::Claude,
            &role,
            "http://127.0.0.1:3456",
            None,
            dir.path(),
        )
        .unwrap();
        let home = overlay.env.get("CLAUDE_CONFIG_DIR").unwrap();
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(std::path::Path::new(home).join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            settings["env"]["ANTHROPIC_BASE_URL"],
            "http://127.0.0.1:3456"
        );
        assert_eq!(
            settings["env"]["ANTHROPIC_MODEL"],
            "OpenCode Go/mimo-v2.6-flash"
        );
    }
}
