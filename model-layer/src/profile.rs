//! Write the global Claude Code settings that point at this gateway.
//!
//! This is the only global CLI file we touch. Agent sessions never depend on
//! it: `agent_host/overlay` gives each session its own home. It exists so a
//! `claude` started in a terminal still reaches the same model catalog after
//! CCR is gone.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::app::App;

pub fn apply_claude(app: &App) -> Result<Value, String> {
    let key = app
        .store
        .local_gateway_key()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no local-gateway caller key to hand Claude Code".to_string())?;

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())?;
    // No spaces in this path: Claude Code runs `apiKeyHelper` through an
    // unquoted shell command, so `Application Support/...` becomes
    // `/Users/.../Application` and fails. Keep the helper under `~/.crewchat`.
    let bin = home.join(".crewchat/bin");
    std::fs::create_dir_all(&bin).map_err(|error| format!("create {}: {error}", bin.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700));
    }

    let helper_path = bin.join("claude-api-key.sh");
    let key_path = bin.join("claude-api-key");

    // Clean up the old helper location; it is no longer referenced.
    let old_bin = app.config.data_dir.join("bin");
    let _ = std::fs::remove_file(old_bin.join("claude-api-key.sh"));
    let _ = std::fs::remove_file(old_bin.join("claude-api-key"));
    let script = if app.store.uses_keychain() {
        // No plaintext key file: the helper asks the same Keychain item the
        // model layer uses. `security` created the item, so it has access.
        let _ = std::fs::remove_file(&key_path);
        "#!/bin/sh\n/usr/bin/security find-generic-password \
         -a 'caller:local-gateway' -s 'com.crewchat.model-layer' -w\n"
            .to_string()
    } else {
        write_private(&key_path, key.as_bytes())?;
        format!(
            "#!/bin/sh\ncat {}\n",
            shell_single_quote(&key_path.to_string_lossy())
        )
    };
    write_private(&helper_path, script.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&helper_path, std::fs::Permissions::from_mode(0o755));
    }

    let settings_path = home.join(".claude/settings.json");
    let model = claude_model(app);
    let mut settings = read_json(&settings_path).unwrap_or_else(|| json!({}));
    if !settings.is_object() {
        settings = json!({});
    }

    if settings_path.exists() {
        let backup = settings_path.with_extension("json.crewchat-backup");
        if !backup.exists() {
            let _ = std::fs::copy(&settings_path, &backup);
        }
    }

    let object = settings.as_object_mut().unwrap();
    object.insert(
        "apiKeyHelper".into(),
        Value::String(helper_path.to_string_lossy().to_string()),
    );
    let env = object
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    if !env.is_object() {
        *env = Value::Object(Map::new());
    }
    let env = env.as_object_mut().unwrap();
    let gateway = app.config.gateway_url();
    for (name, value) in [
        ("ANTHROPIC_BASE_URL", gateway.as_str()),
        ("ANTHROPIC_API_BASE_URL", gateway.as_str()),
        ("CLAUDE_AGENT_API_BASE_URL", gateway.as_str()),
        ("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY", "1"),
        ("NO_PROXY", "127.0.0.1,localhost,::1"),
        ("no_proxy", "127.0.0.1,localhost,::1"),
        ("CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT", "1"),
        ("TZ", "UTC"),
    ] {
        env.insert(name.into(), Value::String(value.into()));
    }
    if let Some(model) = &model {
        for name in [
            "ANTHROPIC_MODEL",
            "CCR_CLAUDE_CODE_MODEL",
            "CODEXL_CLAUDE_CODE_MODEL",
        ] {
            env.insert(name.into(), Value::String(model.clone()));
        }
    }

    write_settings(&settings_path, &settings)?;

    Ok(json!({
        "applied": true,
        "settingsFile": settings_path.to_string_lossy(),
        "helper": helper_path.to_string_lossy(),
        "gateway": gateway,
        "model": model,
    }))
}

fn claude_model(app: &App) -> Option<String> {
    let config = app.store.build_config().ok()?;
    let label = profile_model(&config).or_else(|| first_model(&config))?;
    Some(with_one_million(&config, label))
}

fn profile_model(config: &Value) -> Option<String> {
    if let Some(profiles) = config
        .pointer("/profile/profiles")
        .and_then(Value::as_array)
    {
        if let Some(model) = profiles
            .iter()
            .find(|profile| {
                profile.get("agent").and_then(Value::as_str) == Some("claude-code")
                    && profile
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(true)
            })
            .and_then(|profile| profile.get("model"))
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
        {
            return Some(model.to_string());
        }
    }
    config
        .pointer("/profile/claudeCode/model")
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .map(str::to_string)
}

fn first_model(config: &Value) -> Option<String> {
    let provider = config.get("Providers")?.as_array()?.first()?;
    let name = provider.get("name")?.as_str()?;
    let model = provider.get("models")?.as_array()?.first()?.as_str()?;
    Some(format!("{name}/{model}"))
}

fn with_one_million(config: &Value, label: String) -> String {
    if label.ends_with("[1m]") {
        return label;
    }
    let Some((provider, model)) = label.split_once('/') else {
        return label;
    };
    let Some(providers) = config.get("Providers").and_then(Value::as_array) else {
        return label;
    };
    for entry in providers {
        let matches = entry
            .get("name")
            .and_then(Value::as_str)
            .map(|name| name.eq_ignore_ascii_case(provider))
            .unwrap_or(false);
        if !matches {
            continue;
        }
        let flagged = entry
            .get("models")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .any(|value| value.as_str() == Some(&format!("{model}[1m]")))
            })
            .unwrap_or(false);
        if flagged {
            return format!("{label}[1m]");
        }
    }
    label
}

fn read_json(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_settings(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let body = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, &body)?;
    std::fs::rename(&tmp, path).map_err(|error| format!("replace {}: {error}", path.display()))
}

fn write_private(path: &Path, body: &[u8]) -> Result<(), String> {
    std::fs::write(path, body).map_err(|error| format!("write {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_a_path_with_spaces_and_quotes() {
        assert_eq!(shell_single_quote("/a b/c"), "'/a b/c'");
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }
}
