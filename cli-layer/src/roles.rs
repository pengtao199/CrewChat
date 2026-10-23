//! Per-role CLI homes. Each session gets its own config dir so two agents on
//! the same CLI can run different models without fighting over a global file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::engines::OverlayKind;

pub struct RoleOverlay {
    pub home: PathBuf,
    pub env: BTreeMap<String, String>,
    pub model_label: String,
    /// Set by engines whose ACP bridge needs a companion process (OpenClaw).
    pub gateway_port: Option<u16>,
}

impl RoleOverlay {
    fn new(home: &Path, env: BTreeMap<String, String>, model_label: String) -> Self {
        Self {
            home: home.to_path_buf(),
            env,
            model_label,
            gateway_port: None,
        }
    }
}

/// Build a disposable home for one role. `api_key` is required by overlays
/// that point a CLI at the local gateway; Claude reads its key through
/// `apiKeyHelper` instead.
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
        OverlayKind::Grok => prepare_grok(&home, &model_label, gateway, api_key),
        OverlayKind::Hermes => prepare_hermes(&home, &model_label, gateway, api_key),
        OverlayKind::Kimi => prepare_kimi(&home, &model_label, gateway, api_key),
        OverlayKind::OpenClaw => prepare_openclaw(&home, &model_label, gateway, api_key),
        OverlayKind::Pi => prepare_pi(&home, &model_label, gateway, api_key),
        OverlayKind::Cline => prepare_cline(&home, &model_label, gateway, api_key),
        OverlayKind::DeepSeek => prepare_deepseek(&home, &model_label, gateway, api_key),
        OverlayKind::None => bail!("this engine has no per-role model overlay"),
    }
}

fn gateway_v1(gateway: &str) -> String {
    format!("{}/v1", gateway.trim_end_matches('/'))
}

fn free_loopback_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("reserve an OpenClaw gateway port")?;
    let port = listener
        .local_addr()
        .context("read the reserved OpenClaw gateway port")?
        .port();
    drop(listener);
    Ok(port)
}

fn require_model<'a>(model: &'a str, engine: &str) -> Result<&'a str> {
    let model = model.trim();
    if model.is_empty() {
        bail!("{engine} overlay needs a role model");
    }
    Ok(model)
}

fn require_key<'a>(api_key: Option<&'a str>, engine: &str) -> Result<&'a str> {
    api_key
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .with_context(|| format!("{engine} overlay needs the local gateway key"))
}

fn write_private(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, contents.as_ref()).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod {}", path.display()))?;
    }
    Ok(())
}

fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn yaml_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
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
    Ok(RoleOverlay::new(home, result, model_label.to_string()))
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
    Ok(RoleOverlay::new(home, env, model_label.to_string()))
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
        // Skills are materialized per role by skill-layer; linking the global
        // tree would put the materialized `skills/` outside the role home.
        if name == "config.toml" || name == "skills" {
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
    Ok(RoleOverlay::new(home, env, model_label.to_string()))
}

fn prepare_grok(
    home: &Path,
    model_label: &str,
    gateway: &str,
    api_key: Option<&str>,
) -> Result<RoleOverlay> {
    let model = require_model(model_label, "Grok")?;
    let key = require_key(api_key, "Grok")?;
    let base = gateway_v1(gateway);
    let config = format!(
        "[models]\ndefault = \"crewchat\"\n\n[model.crewchat]\nmodel = {}\nbase_url = {}\nname = \"CrewChat\"\nenv_key = \"CODEG_CCR_GATEWAY_KEY\"\napi_backend = \"responses\"\ncontext_window = 128000\n",
        toml_string(model),
        toml_string(&base),
    );
    write_private(&home.join("config.toml"), config)?;

    let mut env = BTreeMap::new();
    env.insert("GROK_HOME".to_string(), home.to_string_lossy().to_string());
    env.insert("CODEG_CCR_GATEWAY_KEY".to_string(), key.to_string());
    Ok(RoleOverlay::new(home, env, model.to_string()))
}

fn prepare_hermes(
    home: &Path,
    model_label: &str,
    gateway: &str,
    api_key: Option<&str>,
) -> Result<RoleOverlay> {
    let model = require_model(model_label, "Hermes")?;
    let key = require_key(api_key, "Hermes")?;
    let base = gateway_v1(gateway);
    let config = format!(
        "model:\n  default: {model}\n  provider: crewchat\n  base_url: {base}\n\ncustom_providers:\n  - name: crewchat\n    base_url: {base}\n    api_key: {key}\n    api_mode: chat_completions\n    model: {model}\n    models:\n      {model}:\n        context_length: 128000\n",
        model = yaml_string(model),
        base = yaml_string(&base),
        key = yaml_string("${CODEG_CCR_GATEWAY_KEY}"),
    );
    write_private(&home.join("config.yaml"), config)?;

    let mut env = BTreeMap::new();
    env.insert(
        "HERMES_HOME".to_string(),
        home.to_string_lossy().to_string(),
    );
    env.insert("CODEG_CCR_GATEWAY_KEY".to_string(), key.to_string());
    Ok(RoleOverlay::new(home, env, model.to_string()))
}

fn prepare_kimi(
    home: &Path,
    model_label: &str,
    gateway: &str,
    api_key: Option<&str>,
) -> Result<RoleOverlay> {
    let model = require_model(model_label, "Kimi")?;
    let key = require_key(api_key, "Kimi")?;
    let base = gateway_v1(gateway);
    let config = format!(
        "default_model = \"crewchat\"\n\n[providers.crewchat]\ntype = \"openai\"\nbase_url = {}\napi_key_env = \"CODEG_CCR_GATEWAY_KEY\"\n\n[models.crewchat]\nprovider = \"crewchat\"\nmodel = {}\nmax_context_size = 128000\n",
        toml_string(&base),
        toml_string(model),
    );
    write_private(&home.join("config.toml"), config)?;

    let mut env = BTreeMap::new();
    env.insert(
        "KIMI_CODE_HOME".to_string(),
        home.to_string_lossy().to_string(),
    );
    env.insert("CODEG_CCR_GATEWAY_KEY".to_string(), key.to_string());
    Ok(RoleOverlay::new(home, env, model.to_string()))
}

fn prepare_openclaw(
    home: &Path,
    model_label: &str,
    gateway: &str,
    api_key: Option<&str>,
) -> Result<RoleOverlay> {
    let model = require_model(model_label, "OpenClaw")?;
    let key = require_key(api_key, "OpenClaw")?;
    let base = gateway_v1(gateway);
    let config = json!({
        "agents": {
            "defaults": {
                "model": { "primary": format!("crewchat/{model}") }
            }
        },
        "models": {
            "mode": "merge",
            "providers": {
                "crewchat": {
                    "baseUrl": base,
                    "apiKey": "${CODEG_CCR_GATEWAY_KEY}",
                    "api": "openai-completions",
                    "models": [
                        { "id": model, "name": model, "contextWindow": 128000, "maxTokens": 8192 }
                    ]
                }
            }
        }
    });
    let path = home.join("openclaw.json");
    write_private(&path, serde_json::to_vec_pretty(&config)?)?;
    let state = home.join("state");
    std::fs::create_dir_all(&state).with_context(|| format!("create {}", state.display()))?;

    let mut env = BTreeMap::new();
    env.insert(
        "OPENCLAW_CONFIG_PATH".to_string(),
        path.to_string_lossy().to_string(),
    );
    env.insert(
        "OPENCLAW_STATE_DIR".to_string(),
        state.to_string_lossy().to_string(),
    );
    env.insert("CODEG_CCR_GATEWAY_KEY".to_string(), key.to_string());
    Ok(RoleOverlay {
        home: home.to_path_buf(),
        env,
        model_label: model.to_string(),
        gateway_port: Some(free_loopback_port()?),
    })
}

fn prepare_pi(
    home: &Path,
    model_label: &str,
    gateway: &str,
    api_key: Option<&str>,
) -> Result<RoleOverlay> {
    let model = require_model(model_label, "Pi")?;
    let key = require_key(api_key, "Pi")?;
    let base = gateway_v1(gateway);
    let models = json!({
        "providers": {
            "crewchat": {
                "name": "CrewChat",
                "baseUrl": base,
                "api": "openai-completions",
                "apiKey": "$CODEG_CCR_GATEWAY_KEY",
                "models": [
                    {
                        "id": model,
                        "name": model,
                        "input": ["text"],
                        "contextWindow": 128000,
                        "maxTokens": 8192,
                        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
                    }
                ]
            }
        }
    });
    write_private(
        &home.join("models.json"),
        serde_json::to_vec_pretty(&models)?,
    )?;
    write_private(
        &home.join("settings.json"),
        serde_json::to_vec_pretty(&json!({
            "defaultProvider": "crewchat",
            "defaultModel": model,
            "quietStartup": true,
        }))?,
    )?;

    let mut env = BTreeMap::new();
    env.insert(
        "PI_CODING_AGENT_DIR".to_string(),
        home.to_string_lossy().to_string(),
    );
    env.insert("CODEG_CCR_GATEWAY_KEY".to_string(), key.to_string());
    Ok(RoleOverlay::new(home, env, model.to_string()))
}

fn prepare_cline(
    home: &Path,
    model_label: &str,
    gateway: &str,
    api_key: Option<&str>,
) -> Result<RoleOverlay> {
    let model = require_model(model_label, "Cline")?;
    let key = require_key(api_key, "Cline")?;
    let base = gateway_v1(gateway);
    let data = home.join("data");
    let settings = data.join("settings");
    let providers = json!({
        "version": 1,
        "lastUsedProvider": "crewchat",
        "modes": {},
        "providers": {
            "crewchat": {
                "settings": {
                    "provider": "crewchat",
                    "baseUrl": base,
                    "model": model,
                    "protocol": "openai-chat",
                    "client": "openai-compatible"
                },
                "updatedAt": crate::config::now(),
                "tokenSource": "manual"
            }
        }
    });
    write_private(
        &settings.join("providers.json"),
        serde_json::to_vec_pretty(&providers)?,
    )?;
    let models = json!({
        "version": 1,
        "providers": {
            "crewchat": {
                "provider": {
                    "name": "CrewChat",
                    "baseUrl": base,
                    "defaultModelId": model,
                    "protocol": "openai-chat",
                    "client": "openai-compatible"
                },
                "models": {
                    (model): {
                        "id": model,
                        "name": model,
                        "contextWindow": 128000,
                        "maxTokens": 8192
                    }
                }
            }
        }
    });
    write_private(
        &settings.join("models.json"),
        serde_json::to_vec_pretty(&models)?,
    )?;

    let mut env = BTreeMap::new();
    env.insert("CLINE_DIR".to_string(), home.to_string_lossy().to_string());
    env.insert(
        "CLINE_DATA_DIR".to_string(),
        data.to_string_lossy().to_string(),
    );
    env.insert("CLINE_PROVIDER".to_string(), "crewchat".to_string());
    env.insert("CLINE_MODEL".to_string(), model.to_string());
    env.insert("CLINE_API_KEY".to_string(), key.to_string());
    Ok(RoleOverlay::new(home, env, model.to_string()))
}

fn prepare_deepseek(
    home: &Path,
    model_label: &str,
    gateway: &str,
    api_key: Option<&str>,
) -> Result<RoleOverlay> {
    let model = require_model(model_label, "DeepSeek")?;
    let key = require_key(api_key, "DeepSeek")?;
    let base = gateway_v1(gateway);
    let config = format!(
        "llm-pi-ai:\n  providers:\n    crewchat:\n      displayName: CrewChat\n      api: openai-completions\n      baseURL: {base}\n      apiKeyEnv: CODEG_CCR_GATEWAY_KEY\n      models:\n        - id: {model}\n          name: {model}\n          contextWindow: 128000\n",
        base = yaml_string(&base),
        model = yaml_string(model),
    );
    write_private(&home.join("settings.yaml"), config)?;

    let mut env = BTreeMap::new();
    env.insert("DSH_HOME".to_string(), home.to_string_lossy().to_string());
    env.insert(
        "DEEPSEEK_ACP_SESSIONS_ROOT".to_string(),
        home.join("sessions").to_string_lossy().to_string(),
    );
    env.insert("DEEPSEEK_ACP_PROVIDER".to_string(), "crewchat".to_string());
    env.insert("DEEPSEEK_ACP_MODEL".to_string(), model.to_string());
    env.insert("CODEG_CCR_GATEWAY_KEY".to_string(), key.to_string());
    Ok(RoleOverlay::new(home, env, model.to_string()))
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

    fn role() -> Value {
        serde_json::json!({
            "id": "r1",
            "model": "mimo-v2.6-flash",
            "model_provider": "OpenCode Go"
        })
    }

    fn prepare_for(kind: OverlayKind) -> (tempfile::TempDir, RoleOverlay) {
        let dir = tempfile::tempdir().unwrap();
        let overlay = prepare(
            kind,
            &role(),
            "http://127.0.0.1:3456",
            Some("sk-local"),
            dir.path(),
        )
        .unwrap();
        (dir, overlay)
    }

    #[test]
    fn grok_overlay_writes_gateway_model() {
        let (_dir, overlay) = prepare_for(OverlayKind::Grok);
        let home = overlay.env.get("GROK_HOME").unwrap();
        let config = std::fs::read_to_string(Path::new(home).join("config.toml")).unwrap();
        assert!(config.contains("default = \"crewchat\""));
        assert!(config.contains("model = \"OpenCode Go/mimo-v2.6-flash\""));
        assert!(config.contains("base_url = \"http://127.0.0.1:3456/v1\""));
        assert!(config.contains("api_backend = \"responses\""));
    }

    #[test]
    fn hermes_overlay_writes_gateway_model() {
        let (_dir, overlay) = prepare_for(OverlayKind::Hermes);
        let home = overlay.env.get("HERMES_HOME").unwrap();
        let config = std::fs::read_to_string(Path::new(home).join("config.yaml")).unwrap();
        assert!(config.contains("provider: crewchat"));
        assert!(config.contains("'OpenCode Go/mimo-v2.6-flash'"));
        assert!(config.contains("api_mode: chat_completions"));
    }

    #[test]
    fn kimi_overlay_writes_gateway_model() {
        let (_dir, overlay) = prepare_for(OverlayKind::Kimi);
        let home = overlay.env.get("KIMI_CODE_HOME").unwrap();
        let config = std::fs::read_to_string(Path::new(home).join("config.toml")).unwrap();
        assert!(config.contains("type = \"openai\""));
        assert!(config.contains("api_key_env = \"CODEG_CCR_GATEWAY_KEY\""));
        assert!(config.contains("model = \"OpenCode Go/mimo-v2.6-flash\""));
    }

    #[test]
    fn openclaw_overlay_writes_gateway_model() {
        let (_dir, overlay) = prepare_for(OverlayKind::OpenClaw);
        let path = overlay.env.get("OPENCLAW_CONFIG_PATH").unwrap();
        let config: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            config["agents"]["defaults"]["model"]["primary"],
            "crewchat/OpenCode Go/mimo-v2.6-flash"
        );
        assert_eq!(
            config["models"]["providers"]["crewchat"]["baseUrl"],
            "http://127.0.0.1:3456/v1"
        );
        assert_eq!(
            config["models"]["providers"]["crewchat"]["apiKey"],
            "${CODEG_CCR_GATEWAY_KEY}"
        );
        assert!(overlay.env.contains_key("OPENCLAW_STATE_DIR"));
        assert!(overlay.gateway_port.is_some());
    }

    #[test]
    fn pi_overlay_writes_gateway_model() {
        let (_dir, overlay) = prepare_for(OverlayKind::Pi);
        let home = PathBuf::from(overlay.env.get("PI_CODING_AGENT_DIR").unwrap());
        let models: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("models.json")).unwrap())
                .unwrap();
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["defaultProvider"], "crewchat");
        assert_eq!(settings["defaultModel"], "OpenCode Go/mimo-v2.6-flash");
        assert_eq!(
            models["providers"]["crewchat"]["baseUrl"],
            "http://127.0.0.1:3456/v1"
        );
        assert_eq!(
            models["providers"]["crewchat"]["apiKey"],
            "$CODEG_CCR_GATEWAY_KEY"
        );
    }

    #[test]
    fn cline_overlay_writes_gateway_model() {
        let (_dir, overlay) = prepare_for(OverlayKind::Cline);
        let home = PathBuf::from(overlay.env.get("CLINE_DIR").unwrap());
        let providers: Value = serde_json::from_str(
            &std::fs::read_to_string(home.join("data/settings/providers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(providers["lastUsedProvider"], "crewchat");
        assert_eq!(
            providers["providers"]["crewchat"]["settings"]["baseUrl"],
            "http://127.0.0.1:3456/v1"
        );
        assert_eq!(
            providers["providers"]["crewchat"]["settings"]["model"],
            "OpenCode Go/mimo-v2.6-flash"
        );
        assert_eq!(overlay.env.get("CLINE_PROVIDER").unwrap(), "crewchat");
    }

    #[test]
    fn deepseek_overlay_writes_gateway_model() {
        let (_dir, overlay) = prepare_for(OverlayKind::DeepSeek);
        let home = overlay.env.get("DSH_HOME").unwrap();
        let config = std::fs::read_to_string(Path::new(home).join("settings.yaml")).unwrap();
        assert!(config.contains("llm-pi-ai:"));
        assert!(config.contains("api: openai-completions"));
        assert!(config.contains("baseURL: 'http://127.0.0.1:3456/v1'"));
        assert!(config.contains("apiKeyEnv: CODEG_CCR_GATEWAY_KEY"));
        assert_eq!(
            overlay.env.get("DEEPSEEK_ACP_MODEL").unwrap(),
            "OpenCode Go/mimo-v2.6-flash"
        );
    }
}
