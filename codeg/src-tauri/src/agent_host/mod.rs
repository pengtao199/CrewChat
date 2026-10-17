//! [crewchat] Agent Host recipe v1 — the launch-time overlay that binds a
//! CLI session to CCR (the single model layer) without touching the CLI's own
//! global configuration.
//!
//! Why an overlay instead of CCR's generated wrappers: codeg spawns the real
//! binaries by absolute path, so `~/.claude-code-router/bin/ccr-*-wrapper-*`
//! never runs (measured 2026-10-11, see docs/plans/agents-core-collaboration).
//! Injecting the wrapper's essentials into the session's env gets the same
//! routing per session — which is also what Phase 3.1 needs to give different
//! members different models.
//!
//! Scope (v1): claude only. codex's overlay would need `CODEX_HOME` pointed at
//! CCR's profile home, and codeg's own parser resolves codex sessions from the
//! process-level `CODEX_HOME` — moving the session tree would break history
//! parsing. That stays deferred to 3.1 alongside the member-model work.
//!
//! Wiring: `build_runtime_env_for_agent` merges the recipe into every spawned
//! session (user-initiated and delegation children share that point). Off
//! switch: `CODEG_AGENT_HOST_INJECT=0` ⇒ no overlay at all, exactly the
//! pre-existing behavior. A missing/unreachable CCR ⇒ no overlay (best
//! effort), never an error.
//!
//! Layout: this file is the launch recipe (env routing only); [`gateway`] is the
//! CCR gateway's model catalog and how a roster brain maps onto it; [`role_home`]
//! is Phase 3.1's per-role isolated claude home — the one mechanism that sets a
//! member's model.

mod gateway;
mod role_home;

use std::collections::BTreeMap;

use serde_json::Value;

use crate::models::AgentType;

pub use gateway::{
    claude_role_model_candidates, claude_role_model_label, claude_virtual_model_hex,
    pick_claude_gateway_model_id, pick_claude_gateway_model_id_for, provider_display_name,
};
pub use role_home::{
    build_claude_role_home, claude_role_settings, claude_role_settings_with_plugins,
    prune_stale_role_homes, ClaudeRoleHome,
};

/// The per-session overlay: env for routing. claude reads its base URL at
/// process start, so the env trio is the whole launch-time contract.
///
/// The MODEL is deliberately NOT here. It is not an env var (`settings.json`'s
/// env wins over the process env — measured 2026-10-12), and driving it through
/// the ACP `session/set_config_option` channel cannot work either: claude only
/// accepts the model its current configuration already names, so every attempt
/// to switch a member onto another model fails with `Couldn't confirm` (seen on
/// every member session in the 2026-10-17 logs). Phase 3.1's isolated role home
/// is the working mechanism — it hands the CLI a settings.json that NAMES the
/// member's model at process start — and a session without one simply runs on
/// the CLI's own configured model. One mechanism, no error spam.
pub struct AgentHostRecipe {
    pub env: BTreeMap<String, String>,
}

/// Whether the overlay is on. On by default — the point of the feature is that
/// a codeg session reaches CCR; `CODEG_AGENT_HOST_INJECT=0` reverts to the
/// pre-overlay behavior for A/B testing and rollback.
pub fn injection_enabled() -> bool {
    !matches!(
        std::env::var("CODEG_AGENT_HOST_INJECT").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    )
}

/// The CCR profile agent kind for a codeg agent type, mirroring the frontend's
/// `CCR_AGENT_KIND_BY_BODY`. `None` = outside CCR's registry (no overlay even
/// in theory).
fn ccr_agent_kind(agent_type: AgentType) -> Option<&'static str> {
    match agent_type {
        AgentType::ClaudeCode => Some("claude-code"),
        AgentType::Codex => Some("codex"),
        AgentType::Grok => Some("grok"),
        AgentType::OpenCode => Some("opencode"),
        AgentType::Pi => Some("pi"),
        AgentType::KimiCode => Some("kimi"),
        _ => None,
    }
}

/// Pure recipe builder for the env side — kept free of IO so 3.1 can reuse it
/// with a member's model, and so the shape is unit-testable.
pub fn recipe_for_ccr_gateway(
    agent_type: AgentType,
    gateway_base_url: &str,
) -> Option<AgentHostRecipe> {
    let base = gateway_base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    match agent_type {
        AgentType::ClaudeCode => {
            let mut env = BTreeMap::new();
            // The same trio CCR's own claude wrapper exports; older claude
            // builds read one or another of them.
            env.insert("ANTHROPIC_BASE_URL".into(), base.to_string());
            env.insert("ANTHROPIC_API_BASE_URL".into(), base.to_string());
            env.insert("CLAUDE_AGENT_API_BASE_URL".into(), base.to_string());
            env.insert(
                "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY".into(),
                "1".into(),
            );
            env.insert(
                "CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT".into(),
                "1".into(),
            );
            // The gateway is loopback; a proxy would blackhole it. Merged
            // with whatever the session already carries.
            for key in ["NO_PROXY", "no_proxy"] {
                let current = env.get(key).cloned().unwrap_or_default();
                let mut hosts: Vec<&str> = current
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .collect();
                for host in ["127.0.0.1", "localhost", "::1"] {
                    if !hosts.contains(&host) {
                        hosts.push(host);
                    }
                }
                env.insert(key.into(), hosts.join(","));
            }
            Some(AgentHostRecipe { env })
        }
        // See the module docs: codex's overlay rides on `CODEX_HOME`, which
        // would move its session tree out of the parser's reach.
        _ => None,
    }
}

/// The gateway base URL from a CCR config payload (`routerEndpoint`, else
/// `gateway.host`/`gateway.port`). Pure so every caller resolves it the same
/// way.
pub fn ccr_gateway_base(config: &Value) -> Option<String> {
    config
        .get("routerEndpoint")
        .and_then(Value::as_str)
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let port = config.pointer("/gateway/port").and_then(Value::as_u64)?;
            let host = config
                .pointer("/gateway/host")
                .and_then(Value::as_str)
                .unwrap_or("127.0.0.1");
            Some(format!("http://{host}:{port}"))
        })
}

/// Read CCR's config and the gateway's base URL, and build the launch recipe for
/// one agent. `None` on every "no overlay" path: injection disabled, CLI outside
/// the recipe's scope, CCR unreachable, or a config shape we don't recognize.
pub async fn recipe_for_agent(agent_type: AgentType) -> Option<AgentHostRecipe> {
    if !injection_enabled() {
        return None;
    }
    // Scope gate: a CLI outside CCR's registry gets no overlay, even if a later
    // recipe builder learns to build one for it (see the module docs on codex).
    if ccr_agent_kind(agent_type).is_none() {
        return None;
    }
    let config = crate::ccr::client::call("getConfig", None)
        .await
        .ok()?;
    let gateway = ccr_gateway_base(&config)?;
    recipe_for_ccr_gateway(agent_type, &gateway)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_recipe_carries_the_route_env() {
        let recipe = recipe_for_ccr_gateway(AgentType::ClaudeCode, "http://127.0.0.1:3456/")
            .unwrap();
        assert_eq!(
            recipe.env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:3456")
        );
        assert_eq!(
            recipe.env.get("ANTHROPIC_API_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:3456")
        );
        assert_eq!(
            recipe
                .env
                .get("CLAUDE_AGENT_API_BASE_URL")
                .map(String::as_str),
            Some("http://127.0.0.1:3456")
        );
        assert_eq!(
            recipe
                .env
                .get("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            recipe.env.get("NO_PROXY").map(String::as_str),
            Some("127.0.0.1,localhost,::1")
        );
        // The model is NOT an env var: claude's settings.json env wins over
        // the process env, so the recipe routes via env and leaves the model to
        // the role home (Phase 3.1) or the CLI's own configuration. Nothing in
        // this recipe tries to switch the model at runtime.
        assert!(!recipe.env.contains_key("ANTHROPIC_MODEL"));
    }

    #[test]
    fn an_empty_gateway_yields_no_recipe() {
        assert!(recipe_for_ccr_gateway(AgentType::ClaudeCode, "  ").is_none());
    }

    #[test]
    fn codex_is_out_of_scope_for_v1() {
        assert!(recipe_for_ccr_gateway(AgentType::Codex, "http://127.0.0.1:3456").is_none());
    }
}
