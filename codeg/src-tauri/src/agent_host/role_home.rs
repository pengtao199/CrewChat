//! [crewchat] Phase 3.1 — the per-ROLE claude home.
//!
//! claude's model is a STARTUP fact: `CLAUDE_CONFIG_DIR/settings.json`'s
//! `ANTHROPIC_MODEL`, and settings env beats the process env (measured
//! 2026-10-12 — see the plan's Agent Host notes). So a member that must run a
//! different model than the CLI's global one gets its own minimal config
//! directory: settings.json carrying the route env + the target model + the
//! same `apiKeyHelper` the global settings uses (the key itself stays in CCR —
//! no secret is written here), with the shared state directories symlinked
//! back so transcripts, plugins and skills keep working.
//!
//! A brain the gateway cannot confirm against its claude-visible list means NO
//! role home at all: claude's ACP adapter aborts a session started on a label
//! it cannot confirm, so the member falls back to the CLI's own (CCR-managed)
//! configuration instead — the session survives, it just does not get the
//! roster's model. [`claude_role_model_candidates`] makes the roster's shapes
//! (provider id, display name, or the model alone) find the same ids.
//!
//! [`claude_role_model_candidates`]: super::gateway::claude_role_model_candidates

use std::collections::BTreeMap;

use serde_json::Value;

use super::gateway::{
    claude_role_model_candidates, claude_role_model_label, gateway_claude_model_id,
};
use super::{ccr_gateway_base, injection_enabled, recipe_for_ccr_gateway};
use crate::models::AgentType;

/// The settings.json a role session runs on, as pure data (testable without
/// touching disk). `route_env` is the gateway trio; `model_label` is the
/// client-visible name claude should start on; `api_key_helper` is copied
/// from the user's global settings when it exists.
pub fn claude_role_settings(
    route_env: &BTreeMap<String, String>,
    model_label: &str,
    api_key_helper: Option<&str>,
) -> Value {
    claude_role_settings_with_plugins(route_env, model_label, api_key_helper, None)
}

/// As [`claude_role_settings`], plus the user's plugin switchboard copied from
/// the global settings — plugins a session expects to have (codegraph,
/// computer-use) are declared by KEY here while their files stay in the shared
/// `plugins/` directory the role home links back.
pub fn claude_role_settings_with_plugins(
    route_env: &BTreeMap<String, String>,
    model_label: &str,
    api_key_helper: Option<&str>,
    enabled_plugins: Option<&Value>,
) -> Value {
    let mut env = serde_json::Map::new();
    for (key, value) in route_env {
        env.insert(key.clone(), Value::String(value.clone()));
    }
    env.insert(
        "ANTHROPIC_MODEL".into(),
        Value::String(model_label.to_string()),
    );
    env.insert("TZ".into(), Value::String("UTC".into()));
    let mut settings = serde_json::Map::new();
    settings.insert("env".into(), Value::Object(env));
    if let Some(helper) = api_key_helper.filter(|helper| !helper.trim().is_empty()) {
        settings.insert("apiKeyHelper".into(), Value::String(helper.to_string()));
    }
    if let Some(plugins) = enabled_plugins.filter(|value| !value.is_null()) {
        settings.insert("enabledPlugins".into(), plugins.clone());
    }
    Value::Object(settings)
}

/// Everything the caller needs to launch a session inside a role home.
pub struct ClaudeRoleHome {
    pub config_dir: std::path::PathBuf,
    pub model_label: String,
}

/// Build (or refresh) the isolated home for one role and return it.
///
/// `None` when the overlay is off, CCR is unreachable, the gateway yields no
/// route, or `home_root` cannot be prepared — every path degrades to "run on
/// the CLI's own configuration", never an error.
pub async fn build_claude_role_home(
    home_root: &std::path::Path,
    role_key: &str,
    provider: &str,
    model: &str,
) -> Option<ClaudeRoleHome> {
    if !injection_enabled() || model.trim().is_empty() {
        return None;
    }
    let config = crate::commands::ccr::ccr_call_core("getConfig", None)
        .await
        .ok()?;
    let gateway = ccr_gateway_base(&config)?;
    let route_env = recipe_for_ccr_gateway(AgentType::ClaudeCode, &gateway)?.env;

    // Resolve the model label BEFORE creating anything. claude's ACP adapter
    // refuses a model it cannot confirm against its discovered list (~5 s, no
    // request reaches the gateway), where `claude -p` only warns — so pinning
    // an unconfirmable label would abort the member's session outright. A
    // brain the gateway does not expose (a roster still naming a retired
    // model, say) therefore degrades to the CLI's own CCR-managed
    // configuration, exactly the rule the launch recipe follows.
    let candidates = claude_role_model_candidates(&config, provider, model);
    let model_label = match gateway_claude_model_id(&config, &gateway, &candidates).await {
        Some(label) => label,
        None => {
            tracing::warn!(
                "[agent-host] role {role_key} brain {} is not in the gateway's \
                 claude-visible list; the session runs on the CLI's configured model",
                claude_role_model_label(provider, model, false)
            );
            return None;
        }
    };

    let (api_key_helper, enabled_plugins) = global_claude_settings_bits();
    let settings = claude_role_settings_with_plugins(
        &route_env,
        &model_label,
        api_key_helper.as_deref(),
        enabled_plugins.as_ref(),
    );

    // A FRESH directory per session. The adapter writes its own `.claude.json`
    // into the config dir, and a second session against the same dir silently
    // fails to start (measured 2026-10-12: Connected → Error ~5.4 s, no
    // `session/create` at the adapter); a fresh dir always starts. Leftovers
    // are pruned at startup by [`prune_stale_role_homes`].
    let config_dir = home_root.join(format!(
        "{}-{}",
        sanitize_role_key(role_key),
        uuid::Uuid::new_v4().simple()
    ));
    if let Err(error) = std::fs::create_dir_all(&config_dir) {
        tracing::warn!(
            "[agent-host] role home {} not created ({error}); the session runs on the global config",
            config_dir.display()
        );
        return None;
    }
    let settings_path = config_dir.join("settings.json");
    let serialized = serde_json::to_vec_pretty(&settings).ok()?;
    if let Err(error) = std::fs::write(&settings_path, serialized) {
        tracing::warn!(
            "[agent-host] role settings {} not written ({error}); the session runs on the global config",
            settings_path.display()
        );
        return None;
    }
    link_shared_claude_dirs(&config_dir);
    tracing::info!(
        "[agent-host] role home ready at {} (model {model_label})",
        config_dir.display()
    );
    Some(ClaudeRoleHome {
        config_dir,
        model_label,
    })
}

/// Directories under the user's `~/.claude` that hold state a role session
/// still needs (transcripts for the parser, plugins, skills, todos). Linked
/// into the role home; a name that is not a directory there is skipped.
const SHARED_CLAUDE_DIRS: &[&str] = &[
    "projects",
    "plugins",
    "todos",
    "statsig",
    "shell-snapshots",
    "downloads",
];

fn link_shared_claude_dirs(config_dir: &std::path::Path) {
    let Some(shared) = dirs::home_dir().map(|home| home.join(".claude")) else {
        return;
    };
    for name in SHARED_CLAUDE_DIRS {
        let target = shared.join(name);
        if !target.is_dir() {
            continue;
        }
        let link = config_dir.join(name);
        if link.exists() {
            continue;
        }
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(&target, &link);
        #[cfg(not(unix))]
        let result: std::io::Result<()> = Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symlink unsupported",
        ));
        if let Err(error) = result {
            tracing::debug!(
                "[agent-host] could not link {} into the role home: {error}",
                target.display()
            );
        }
    }
}

/// The pieces of the user's global claude settings a role home inherits: the
/// `apiKeyHelper` it already trusts (copied, never the key it prints) and the
/// `enabledPlugins` switchboard (the plugin FILES stay in the shared directory
/// the home links back, so only the switches need copying).
fn global_claude_settings_bits() -> (Option<String>, Option<Value>) {
    let Some(raw) = dirs::home_dir()
        .map(|home| home.join(".claude").join("settings.json"))
        .and_then(|path| std::fs::read_to_string(path).ok())
    else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return (None, None);
    };
    let helper = value
        .get("apiKeyHelper")
        .and_then(Value::as_str)
        .map(str::to_string);
    let plugins = value.get("enabledPlugins").cloned();
    (helper, plugins)
}

/// Remove role-session homes older than `max_age`. Called at startup: a
/// per-session home is created for every delegated child and cannot be
/// reused, so this bounds the directory to recent sessions without needing a
/// teardown hook on every disconnect path.
pub fn prune_stale_role_homes(home_root: &std::path::Path, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(home_root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age > max_age {
            if let Err(error) = std::fs::remove_dir_all(&path) {
                tracing::debug!(
                    "[agent-host] could not prune stale role home {}: {error}",
                    path.display()
                );
            }
        }
    }
}

/// A filesystem-safe directory name for a role id.
fn sanitize_role_key(role_key: &str) -> String {
    let cleaned: String = role_key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    if cleaned.is_empty() {
        "role".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_settings_carry_route_env_model_and_the_helper() {
        let mut route = BTreeMap::new();
        route.insert("ANTHROPIC_BASE_URL".to_string(), "http://127.0.0.1:3456".to_string());
        let settings = claude_role_settings(&route, "OpenCode Go/mimo-v2.6-pro[1m]", Some("/ccr/key"));
        assert_eq!(settings["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:3456");
        assert_eq!(settings["env"]["ANTHROPIC_MODEL"], "OpenCode Go/mimo-v2.6-pro[1m]");
        assert_eq!(settings["env"]["TZ"], "UTC");
        assert_eq!(settings["apiKeyHelper"], "/ccr/key");
        // No key material of our own: the helper PRINTS the key at runtime.
        assert!(!settings.to_string().contains("sk-"));
    }

    #[test]
    fn role_settings_omit_a_missing_helper() {
        let settings = claude_role_settings(&BTreeMap::new(), "Provider/model", None);
        assert!(settings.get("apiKeyHelper").is_none());
        assert!(claude_role_settings(&BTreeMap::new(), "m", Some("  ")).get("apiKeyHelper").is_none());
    }

    #[test]
    fn role_keys_become_safe_directory_names() {
        assert_eq!(sanitize_role_key("71119f47-2863"), "71119f47-2863");
        assert_eq!(sanitize_role_key("a/b\\c d"), "a-b-c-d");
        assert_eq!(sanitize_role_key(""), "role");
    }

    #[test]
    fn prune_removes_only_old_role_homes() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh");
        let stale = dir.path().join("stale");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::create_dir_all(&stale).unwrap();
        // Backdate the stale one far enough to trip any sane max-age.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 48);
        let times = std::fs::FileTimes::new().set_modified(old);
        std::fs::File::open(&stale)
            .unwrap()
            .set_times(times)
            .unwrap();
        prune_stale_role_homes(dir.path(), std::time::Duration::from_secs(60 * 60 * 24));
        assert!(fresh.exists(), "recent homes stay");
        assert!(!stale.exists(), "homes past the max age are pruned");
    }
}
