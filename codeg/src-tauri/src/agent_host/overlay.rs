//! [crewchat] The launch-time MODEL overlay, per body.
//!
//! A roster role's brain has to land on the CLI that actually runs — the roster
//! row alone changes nothing. Every CLI we support reads its model at STARTUP
//! from a config file in its own home, so the mechanism is one:
//!
//!   give the session its OWN home that contains the file WE write, plus
//!   symlinks back to everything else in the user's real home.
//!
//! Nothing global is touched, the session's own state (transcripts, plugins,
//! pets, history) keeps landing in the shared tree the parsers read, and the
//! whole overlay is disposable.
//!
//! | body | env that points at the overlay | file we own |
//! | --- | --- | --- |
//! | claude | `CLAUDE_CONFIG_DIR` | `settings.json` (`ANTHROPIC_MODEL`) |
//! | codex  | `CODEX_HOME`         | `config.toml` (`model` + our gateway provider) |
//!
//! The gateway ROUTE is the same for both and comes from the single `getConfig`
//! payload the env recipe already reads, so URL, model list and client key can
//! never disagree with each other.
//!
//! Secrets: the gateway key WE resolve never lands in an overlay — claude reads
//! it through the `apiKeyHelper` it already trusts, codex through `env_key`
//! naming an env var we set on the process. A codex overlay DOES inherit the
//! user's own `config.toml` verbatim, so whatever tokens THEIR file carries are
//! copied into it too (under the private `<data dir>`, pruned with the home).
//!
//! Scope today: claude (done in Phase 3.1, reused here) and codex. Every other
//! body reports [`OverlayOutcome::Unsupported`] instead of silently running on
//! the CLI's own model — a roster pick that cannot be honoured must be VISIBLE,
//! not quietly ignored. See `AGENTS/agent-model.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::models::AgentType;

use super::{ccr_gateway_base, injection_enabled};

/// The env var the overlay hands non-claude bodies their gateway key in: codex
/// reads it by name (`env_key` in the generated `config.toml`), opencode
/// references it as `{env:…}` in its JSON. Prefixed so it cannot collide with a
/// user's own variable, and never written to disk.
pub const CODEX_GATEWAY_KEY_ENV: &str = "CODEG_CCR_GATEWAY_KEY";

/// The same variable, named for what opencode does with it. One value, two
/// names on purpose: the variable is shared, the reading side is not.
pub const OPENCODE_GATEWAY_KEY_ENV: &str = CODEX_GATEWAY_KEY_ENV;

/// The provider table name both overlays write (`[model_providers.crewchat]` for
/// codex, `provider.crewchat` for opencode).
const GATEWAY_PROVIDER: &str = "crewchat";

/// A session that must run inside an overlay home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOverlay {
    pub body: AgentType,
    /// Env to merge into the session's runtime env (the only launch-time lever).
    pub env: BTreeMap<String, String>,
    /// The home we created; kept for logging and pruning.
    pub home: PathBuf,
    /// The model the session was actually pinned to (client-visible label).
    pub model_label: String,
}

/// Why an overlay was (or was not) applied. Every non-`Applied` arm means the
/// session runs on the CLI's OWN configuration — the roster's model is not in
/// effect, and the caller must say so rather than pretend otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayOutcome {
    Applied(SessionOverlay),
    /// This body has no overlay yet. The reason is user-facing copy material.
    Unsupported(String),
    /// The body is supported, but this session could not be pinned: injection
    /// disabled, CCR unreachable, no roster model, the brain is not on this
    /// body's gateway face, or the home could not be written.
    Unavailable(String),
}

/// Which bodies have a per-role model overlay — the SINGLE source of truth.
///
/// The launcher ([`prepare_role_overlay`]) and the roster page's honest
/// reporting ([`crate::agent_host::faces`]) both read this, so the UI can never
/// promise a body the launcher cannot actually pin. `Err(reason)` is
/// user-facing copy material.
pub fn overlay_support(body: AgentType) -> Result<(), String> {
    match body {
        AgentType::ClaudeCode | AgentType::Codex | AgentType::OpenCode => Ok(()),
        _ => Err(format!("{body:?} has no per-role model overlay yet")),
    }
}

/// Pin one roster role's session to its brain model, for the body that will run
/// it. `home_root` is the disposable root (`<data>/role-sessions`).
pub async fn prepare_role_overlay(
    home_root: &Path,
    body: AgentType,
    role_key: &str,
    provider: &str,
    model: &str,
) -> OverlayOutcome {
    if !injection_enabled() {
        return OverlayOutcome::Unavailable("Agent Host injection is disabled".into());
    }
    if model.trim().is_empty() {
        return OverlayOutcome::Unavailable("the role has no model configured".into());
    }
    if let Err(reason) = overlay_support(body) {
        return OverlayOutcome::Unsupported(reason);
    }
    match body {
        // Claude's overlay landed in Phase 3.1 and stays where it is: it hands
        // the adapter a settings.json that NAMES the model at process start,
        // because claude refuses a model its configuration does not already
        // name (measured 2026-10-17).
        AgentType::ClaudeCode => {
            match super::build_claude_role_home(home_root, role_key, provider, model).await {
                Some(home) => {
                    let mut env = BTreeMap::new();
                    env.insert(
                        "CLAUDE_CONFIG_DIR".into(),
                        home.config_dir.to_string_lossy().to_string(),
                    );
                    OverlayOutcome::Applied(SessionOverlay {
                        body,
                        env,
                        home: home.config_dir,
                        model_label: home.model_label,
                    })
                }
                None => OverlayOutcome::Unavailable(
                    "no claude role home (gateway unreachable or the brain is not \
                     claude-visible)"
                        .into(),
                ),
            }
        }
        AgentType::Codex => codex_overlay(home_root, role_key, model).await,
        AgentType::OpenCode => open_code_overlay(home_root, role_key, model).await,
        // `overlay_support` above already rejected every other body, so this arm
        // is unreachable by construction: adding a body means adding BOTH its
        // arm here and its `overlay_support` entry, never one of the two.
        other => {
            OverlayOutcome::Unsupported(format!("{other:?} has no per-role model overlay yet"))
        }
    }
}

// ---------------------------------------------------------------- codex

/// codex's overlay: an isolated `CODEX_HOME` whose `config.toml` names the
/// roster model and routes it through CCR's gateway, with every other entry of
/// the user's real `~/.codex` symlinked in so sessions, plugins and pets keep
/// working. Measured 2026-10-18: `codex exec` under such a home answers through
/// the gateway (`client=Codex provider=OpenCode Go … 200`).
async fn codex_overlay(home_root: &Path, role_key: &str, model: &str) -> OverlayOutcome {
    let config = match crate::ccr::client::call("getConfig", None).await {
        Ok(config) => config,
        Err(error) => return OverlayOutcome::Unavailable(format!("CCR is unreachable: {error}")),
    };
    let Some(gateway) = ccr_gateway_base(&config) else {
        return OverlayOutcome::Unavailable("CCR exposes no gateway endpoint".into());
    };
    let Some(key) = gateway_client_key(&config, "codex") else {
        return OverlayOutcome::Unavailable("CCR has no client key for the codex profile".into());
    };
    // Confirm the gateway actually serves this model before pinning it: an
    // unconfirmable pick must degrade to the CLI's own configuration, exactly
    // as claude's does, instead of failing every turn.
    let Some(model) = super::gateway::provider_gateway_model(&config, model) else {
        return OverlayOutcome::Unavailable(format!("the gateway does not serve {model}"));
    };

    let base_toml = read_user_codex_config();
    let rendered = match codex_overlay_config(&base_toml, &model, &gateway, CODEX_GATEWAY_KEY_ENV) {
        Ok(rendered) => rendered,
        Err(error) => return OverlayOutcome::Unavailable(error),
    };

    let Some(home) = fresh_overlay_dir(home_root, role_key) else {
        return OverlayOutcome::Unavailable("the overlay directory could not be created".into());
    };
    if let Err(error) = std::fs::write(home.join("config.toml"), rendered) {
        return OverlayOutcome::Unavailable(format!("config.toml not written: {error}"));
    }
    // Mirror the user's real home, minus the file we own. Everything codex
    // keeps beside its config (sessions, history, plugins, pets, auth) stays
    // reachable — and writes land back in the shared tree the parser reads.
    link_shared_home(&user_codex_home(), &home, &["config.toml"]);

    let mut env = BTreeMap::new();
    env.insert("CODEX_HOME".into(), home.to_string_lossy().to_string());
    env.insert(CODEX_GATEWAY_KEY_ENV.into(), key);
    tracing::info!(
        "[agent-host] codex role home ready at {} (model {model})",
        home.display()
    );
    OverlayOutcome::Applied(SessionOverlay {
        body: AgentType::Codex,
        env,
        home,
        model_label: model,
    })
}

/// opencode's overlay: one layered config file naming the roster model and
/// pointing opencode's provider at CCR, with the gateway key referenced as
/// `{env:…}` so the file reveals nothing.
///
/// Deliberately NOT a home mirror like codex's. opencode merges
/// `$OPENCODE_CONFIG` AFTER the user's global config (the order is documented in
/// `parsers::opencode_context_window::config_files`), so their providers, MCP
/// servers and permissions stay in force and we only override the model route —
/// half the machinery of the codex overlay, for the same result.
///
/// Measured 2026-10-19: `opencode run` under such a layering answered through the
/// gateway (`client=opencode provider=OpenCode Go model=mimo-v2.6-flash 200`),
/// and the `{env:…}` form worked identically.
async fn open_code_overlay(home_root: &Path, role_key: &str, model: &str) -> OverlayOutcome {
    let config = match crate::ccr::client::call("getConfig", None).await {
        Ok(config) => config,
        Err(error) => return OverlayOutcome::Unavailable(format!("CCR is unreachable: {error}")),
    };
    let Some(gateway) = ccr_gateway_base(&config) else {
        return OverlayOutcome::Unavailable("CCR exposes no gateway endpoint".into());
    };
    let Some(key) = gateway_client_key(&config, "opencode") else {
        return OverlayOutcome::Unavailable("CCR has no client key to hand opencode".into());
    };
    // Same rule as codex: a model the gateway does not serve degrades to the
    // CLI's own configuration instead of failing every turn.
    let Some(model) = super::gateway::provider_gateway_model(&config, model) else {
        return OverlayOutcome::Unavailable(format!("the gateway does not serve {model}"));
    };
    let rendered = match open_code_overlay_config(&model, &gateway, OPENCODE_GATEWAY_KEY_ENV) {
        Ok(rendered) => rendered,
        Err(error) => return OverlayOutcome::Unavailable(error),
    };

    let Some(home) = fresh_overlay_dir(home_root, role_key) else {
        return OverlayOutcome::Unavailable("the overlay directory could not be created".into());
    };
    let config_path = home.join("opencode.json");
    if let Err(error) = std::fs::write(&config_path, rendered) {
        return OverlayOutcome::Unavailable(format!("opencode.json not written: {error}"));
    }

    let mut env = BTreeMap::new();
    env.insert(
        "OPENCODE_CONFIG".into(),
        config_path.to_string_lossy().to_string(),
    );
    env.insert(OPENCODE_GATEWAY_KEY_ENV.into(), key);
    tracing::info!(
        "[agent-host] opencode role config layered at {} (model {model})",
        config_path.display()
    );
    OverlayOutcome::Applied(SessionOverlay {
        body: AgentType::OpenCode,
        env,
        home,
        model_label: model,
    })
}

/// Pure: the `opencode.json` an opencode overlay home carries.
///
/// Layered over the user's own config by opencode itself, so this file only says
/// "this provider exists" and "run this model"; the key is an `{env:…}`
/// reference (opencode resolves it at load), which keeps the file secret-free.
pub fn open_code_overlay_config(
    model: &str,
    gateway_base: &str,
    key_env: &str,
) -> Result<String, String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("empty model".into());
    }
    let base = gateway_base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("empty gateway base url".into());
    }

    let mut options = serde_json::Map::new();
    options.insert(
        "baseURL".to_string(),
        serde_json::Value::String(format!("{base}/v1")),
    );
    options.insert(
        "apiKey".to_string(),
        serde_json::Value::String(format!("{{env:{key_env}}}")),
    );
    let mut models = serde_json::Map::new();
    models.insert(model.to_string(), serde_json::json!({ "name": model }));
    let mut provider = serde_json::Map::new();
    provider.insert(
        "npm".to_string(),
        serde_json::Value::String("@ai-sdk/openai-compatible".to_string()),
    );
    provider.insert(
        "name".to_string(),
        serde_json::Value::String("CCR (crewchat)".to_string()),
    );
    provider.insert("options".to_string(), serde_json::Value::Object(options));
    provider.insert("models".to_string(), serde_json::Value::Object(models));
    let mut providers = serde_json::Map::new();
    providers.insert(
        GATEWAY_PROVIDER.to_string(),
        serde_json::Value::Object(provider),
    );

    let document = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "model": format!("{GATEWAY_PROVIDER}/{model}"),
        "provider": serde_json::Value::Object(providers),
    });
    serde_json::to_string_pretty(&document).map_err(|error| error.to_string())
}

/// Pure: the `config.toml` an overlay home runs on. Inherits the user's own
/// configuration (so sandboxing, approvals, MCP servers and marketplaces are
/// unchanged), then pins the model and the gateway route on top.
///
/// Built with `toml_edit` so the user's comments and layout survive: the file
/// stays recognisable when someone opens it.
pub fn codex_overlay_config(
    base_toml: &str,
    model: &str,
    gateway_base: &str,
    key_env: &str,
) -> Result<String, String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("empty model".into());
    }
    let base = gateway_base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("empty gateway base url".into());
    }
    let mut doc = base_toml
        .parse::<toml_edit::Document>()
        .map_err(|e| format!("invalid codex config.toml: {e}"))?;

    doc["model"] = toml_edit::value(model);
    doc["model_provider"] = toml_edit::value(GATEWAY_PROVIDER);

    // The provider table is ours end to end, so it is replaced wholesale rather
    // than merged: a stale `env_key`/`wire_api` from an earlier build must not
    // survive an upgrade. CCR's gateway speaks the OpenAI **responses** API
    // (verified against `/v1/responses`).
    let mut provider = toml_edit::Table::new();
    provider.insert("name", toml_edit::value("CCR (crewchat)"));
    provider.insert("base_url", toml_edit::value(format!("{base}/v1")));
    provider.insert("wire_api", toml_edit::value("responses"));
    provider.insert("requires_openai_auth", toml_edit::value(false));
    provider.insert("env_key", toml_edit::value(key_env));

    let providers =
        doc["model_providers"].or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    if let Some(table) = providers.as_table_mut() {
        table.insert(GATEWAY_PROVIDER, toml_edit::Item::Table(provider));
    } else {
        return Err("config.toml has a non-table model_providers".into());
    }
    Ok(doc.to_string())
}

/// The gateway key for one CLI. CCR publishes one key per CLI profile
/// (`profile:default-codex`, …) plus a shared `local-gateway` key; the CLI's own
/// profile key is the honest choice because the profile is what selects the
/// upstream. Falls back to the shared key, then to the configured default.
pub fn gateway_client_key(config: &Value, kind: &str) -> Option<String> {
    let entries = config.get("APIKEYS").and_then(Value::as_array);
    let key_of = |entry: &Value| {
        entry
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
    };
    if let Some(entries) = entries {
        let profile_key = entries.iter().find(|entry| {
            entry
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("profile:") && id.ends_with(kind))
        });
        if let Some(key) = profile_key.and_then(&key_of) {
            return Some(key);
        }
        let shared = entries
            .iter()
            .find(|entry| entry.get("id").and_then(Value::as_str) == Some("local-gateway"));
        if let Some(key) = shared.and_then(&key_of) {
            return Some(key);
        }
    }
    config
        .get("APIKEY")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

// ------------------------------------------------------------- shared bits

/// Apply the role-model overlay on the USER-INITIATED connect path.
///
/// The delegation path gets its role from the broker; a session the user opens
/// gets it from the frontend, which knows the conversation's team. Both paths
/// resolve to the same overlay, so this helper does the one thing the command
/// layer must not duplicate: read the role row and merge the overlay env.
///
/// Never fails the connect: a role that cannot be pinned leaves the session on
/// the CLI's own model and says so in the log.
pub async fn apply_connect_role_overlay(
    db: &crate::db::AppDatabase,
    data_dir: &Path,
    agent_type: AgentType,
    role_id: Option<&str>,
    runtime_env: &mut BTreeMap<String, String>,
) -> Option<RoleSessionPolicy> {
    let role_id = role_id.map(str::trim).filter(|id| !id.is_empty())?;
    let role = match crate::db::service::agent_role_service::get_role(&db.conn, role_id).await {
        Ok(Some(role)) => role,
        Ok(None) => {
            tracing::warn!(
                "[agent-host] connect asked for unknown role {role_id}; \
                 the session runs on the CLI's own model"
            );
            return None;
        }
        Err(error) => {
            tracing::warn!(
                "[agent-host] role {role_id} not read ({error}); \
                 the session runs on the CLI's own model"
            );
            return None;
        }
    };
    // [crewchat] Phase 4.6 — the permission decision is INDEPENDENT of the model
    // overlay: a body whose model cannot be pinned still honours the Agent's
    // permission choice, so this is returned whatever the overlay outcome is.
    let policy = RoleSessionPolicy {
        permission_mode: role.permission_mode.trim().to_string(),
        auto_approve: role.auto_approve,
    };
    match prepare_role_overlay(
        &data_dir.join("role-sessions"),
        agent_type,
        &role.id,
        &role.model_provider,
        &role.model,
    )
    .await
    {
        OverlayOutcome::Applied(overlay) => {
            runtime_env.extend(overlay.env);
            tracing::info!(
                "[agent-host] session runs on the role model {} (role {}, home {})",
                overlay.model_label,
                role.id,
                overlay.home.display()
            );
        }
        OverlayOutcome::Unsupported(reason) => tracing::warn!(
            "[agent-host] role {} cannot be pinned on this body ({reason}); \
             the session runs on the CLI's own model",
            role.id
        ),
        OverlayOutcome::Unavailable(reason) => tracing::warn!(
            "[agent-host] role {} model not applied ({reason}); \
             the session runs on the CLI's own model",
            role.id
        ),
    }
    Some(policy)
}

/// [crewchat] The permission half of a role's session policy (Phase 4.6): the
/// mode the session starts in, and whether its permission requests may be
/// answered without asking the user. Both come from the Agent's roster row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSessionPolicy {
    /// The session-mode id the role's sessions start in; empty = leave the
    /// CLI / CCR default alone.
    pub permission_mode: String,
    pub auto_approve: bool,
}

/// A FRESH directory per session, under `home_root`. A home cannot be reused:
/// the adapter writes per-session state into it and a second session against
/// the same directory silently fails to start (measured for claude 2026-10-12).
/// [`super::prune_stale_role_homes`] bounds the directory at startup.
pub fn fresh_overlay_dir(home_root: &Path, role_key: &str) -> Option<PathBuf> {
    let dir = home_root.join(format!(
        "{}-{}",
        sanitize_role_key(role_key),
        uuid::Uuid::new_v4().simple()
    ));
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(error) => {
            tracing::warn!(
                "[agent-host] overlay home {} not created ({error})",
                dir.display()
            );
            None
        }
    }
}

/// Symlink every entry of the user's real CLI home into the overlay, except the
/// names the overlay owns. Non-Unix gets nothing: the overlay still works, the
/// session just does not see the user's other state.
///
/// claude's home keeps its OWN, deliberately narrow link list
/// ([`super::role_home`]) — that set was chosen against a working session, and
/// widening it to "everything" is a separate change someone has to verify with
/// the real CLI. codex mirrors wholesale because its home holds state the
/// parser reads (`sessions/`, `session_index.jsonl`) plus the user's own
/// plugins, pets and marketplaces.
pub fn link_shared_home(shared_home: &Path, overlay: &Path, owned: &[&str]) {
    let Ok(entries) = std::fs::read_dir(shared_home) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if owned.contains(&name) {
            continue;
        }
        let target = overlay.join(name);
        if target.exists() {
            continue;
        }
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(entry.path(), &target);
        #[cfg(not(unix))]
        let result: std::io::Result<()> = Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symlink unsupported",
        ));
        if let Err(error) = result {
            tracing::debug!(
                "[agent-host] could not link {} into the overlay: {error}",
                entry.path().display()
            );
        }
    }
}

fn user_codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".codex"),
    }
}

/// The user's own `config.toml`, or an empty base when there is none. An
/// unreadable file is treated as absent: the overlay then carries only our
/// routing, which still works.
fn read_user_codex_config() -> String {
    std::fs::read_to_string(user_codex_home().join("config.toml")).unwrap_or_default()
}

/// A filesystem-safe directory name for a role id.
pub fn sanitize_role_key(role_key: &str) -> String {
    let cleaned: String = role_key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
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
    use serde_json::json;

    const BASE: &str = r#"
model = "agnes-2.5-pro-alpha"
model_provider = "custom"
approval_policy = "never"

[model_providers.custom]
name = "custom"
base_url = "https://apihub.agnes-ai.com/v1"
wire_api = "responses"

[mcp_servers.codegraph]
command = "codegraph"
"#;

    #[test]
    fn codex_config_pins_model_and_gateway_and_keeps_the_rest() {
        let rendered =
            codex_overlay_config(BASE, "mimo-v2.6-flash", "http://127.0.0.1:3456/", "K").unwrap();
        let doc = rendered.parse::<toml_edit::Document>().unwrap();
        assert_eq!(doc["model"].as_str(), Some("mimo-v2.6-flash"));
        assert_eq!(doc["model_provider"].as_str(), Some("crewchat"));
        assert_eq!(doc["approval_policy"].as_str(), Some("never"));
        // The user's own provider and MCP servers survive: the overlay routes
        // ONE provider, it does not rewrite the session's capabilities.
        assert_eq!(
            doc["model_providers"]["custom"]["wire_api"].as_str(),
            Some("responses")
        );
        assert_eq!(
            doc["mcp_servers"]["codegraph"]["command"].as_str(),
            Some("codegraph")
        );
        let ours = &doc["model_providers"]["crewchat"];
        assert_eq!(ours["base_url"].as_str(), Some("http://127.0.0.1:3456/v1"));
        assert_eq!(ours["wire_api"].as_str(), Some("responses"));
        assert_eq!(ours["requires_openai_auth"].as_bool(), Some(false));
        assert_eq!(ours["env_key"].as_str(), Some("K"));
    }

    #[test]
    fn codex_config_is_idempotent_and_replaces_a_stale_provider_table() {
        let once = codex_overlay_config(BASE, "m", "http://127.0.0.1:3456", "K").unwrap();
        let twice = codex_overlay_config(&once, "m2", "http://127.0.0.1:9999", "K2").unwrap();
        let doc = twice.parse::<toml_edit::Document>().unwrap();
        assert_eq!(doc["model"].as_str(), Some("m2"));
        let ours = &doc["model_providers"]["crewchat"];
        assert_eq!(ours["base_url"].as_str(), Some("http://127.0.0.1:9999/v1"));
        assert_eq!(ours["env_key"].as_str(), Some("K2"));
    }

    #[test]
    fn codex_config_starts_from_an_empty_base() {
        let rendered = codex_overlay_config("", "m", "http://gw:1", "K").unwrap();
        let doc = rendered.parse::<toml_edit::Document>().unwrap();
        assert_eq!(doc["model"].as_str(), Some("m"));
        assert_eq!(
            doc["model_providers"]["crewchat"]["base_url"].as_str(),
            Some("http://gw:1/v1")
        );
    }

    #[test]
    fn opencode_config_declares_the_provider_and_references_the_key_by_env() {
        let rendered =
            open_code_overlay_config("mimo-v2.6-flash", "http://127.0.0.1:3456/", "K").unwrap();
        let doc: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(doc["model"].as_str(), Some("crewchat/mimo-v2.6-flash"));
        let provider = &doc["provider"]["crewchat"];
        assert_eq!(provider["npm"].as_str(), Some("@ai-sdk/openai-compatible"));
        assert_eq!(
            provider["options"]["baseURL"].as_str(),
            Some("http://127.0.0.1:3456/v1")
        );
        // The key is a REFERENCE, never the value — opening the file must not
        // reveal a secret.
        assert_eq!(provider["options"]["apiKey"].as_str(), Some("{env:K}"));
        assert!(provider["models"]["mimo-v2.6-flash"].is_object());
        assert!(!rendered.contains("sk-"));
    }

    #[test]
    fn opencode_config_rejects_empty_inputs() {
        assert!(open_code_overlay_config("  ", "http://gw:1", "K").is_err());
        assert!(open_code_overlay_config("m", "  ", "K").is_err());
    }

    #[test]
    fn codex_config_rejects_empty_inputs() {
        assert!(codex_overlay_config(BASE, "  ", "http://gw:1", "K").is_err());
        assert!(codex_overlay_config(BASE, "m", "  ", "K").is_err());
        assert!(codex_overlay_config("model = ", "m", "http://gw:1", "K").is_err());
    }

    #[test]
    fn gateway_key_prefers_the_body_profile_then_the_shared_key() {
        let config = json!({
            "APIKEY": "sk-ccr-default",
            "APIKEYS": [
                { "id": "local-gateway", "key": "sk-ccr-shared" },
                { "id": "profile:default-codex", "key": "ccr-profile-codex" }
            ]
        });
        assert_eq!(
            gateway_client_key(&config, "codex").as_deref(),
            Some("ccr-profile-codex")
        );
        // No codex profile → the shared gateway key.
        assert_eq!(
            gateway_client_key(&config, "grok").as_deref(),
            Some("sk-ccr-shared")
        );
        // Nothing usable in APIKEYS → the default key.
        let bare = json!({ "APIKEY": "sk-ccr-default" });
        assert_eq!(
            gateway_client_key(&bare, "codex").as_deref(),
            Some("sk-ccr-default")
        );
        assert_eq!(gateway_client_key(&json!({}), "codex"), None);
    }

    #[test]
    fn overlay_dirs_are_fresh_and_named_after_the_role() {
        let root = tempfile::tempdir().unwrap();
        let a = fresh_overlay_dir(root.path(), "backend-architect").unwrap();
        let b = fresh_overlay_dir(root.path(), "backend-architect").unwrap();
        assert_ne!(a, b, "a home is never reused");
        assert!(a
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("backend-architect-"));
    }

    #[test]
    fn mirror_links_everything_except_the_files_we_own() {
        let shared = tempfile::tempdir().unwrap();
        let overlay = tempfile::tempdir().unwrap();
        std::fs::write(shared.path().join("config.toml"), "model = \"x\"").unwrap();
        std::fs::create_dir_all(shared.path().join("sessions")).unwrap();
        std::fs::write(overlay.path().join("config.toml"), "model = \"ours\"").unwrap();
        link_shared_home(shared.path(), overlay.path(), &["config.toml"]);
        // Ours stays ours…
        let ours = std::fs::read_to_string(overlay.path().join("config.toml")).unwrap();
        assert_eq!(ours, "model = \"ours\"");
        // …and the session tree is reachable through the mirror.
        #[cfg(unix)]
        assert!(overlay.path().join("sessions").is_dir());
    }

    #[tokio::test]
    async fn unsupported_bodies_say_so_instead_of_silently_running_the_default() {
        let root = tempfile::tempdir().unwrap();
        let outcome = prepare_role_overlay(
            root.path(),
            AgentType::Gemini,
            "role",
            "OpenCode Go",
            "mimo-v2.6-flash",
        )
        .await;
        assert!(matches!(outcome, OverlayOutcome::Unsupported(_)));
    }

    #[tokio::test]
    async fn a_role_without_a_model_is_unavailable_not_unsupported() {
        let root = tempfile::tempdir().unwrap();
        let outcome = prepare_role_overlay(root.path(), AgentType::Codex, "role", "p", "  ").await;
        assert!(matches!(outcome, OverlayOutcome::Unavailable(_)));
    }
}
