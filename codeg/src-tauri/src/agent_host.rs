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

use std::collections::BTreeMap;

use serde_json::Value;

use crate::models::AgentType;

/// The per-session overlay: env for routing (claude reads the base URLs at
/// process start), plus ACP config options for the model choice — claude's
/// model is picked via `session/set_config_option`, NOT `ANTHROPIC_MODEL`,
/// because the CLI's own `settings.json` env wins over the process env
/// (measured 2026-10-12) and a bare env name the gateway can't confirm aborts
/// the session.
pub struct AgentHostRecipe {
    pub env: BTreeMap<String, String>,
    pub config_options: BTreeMap<String, String>,
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

/// The lowercase hex CCR encodes into its claude gateway model ids
/// (`anthropic/claude-ccr-h<hex>[1m]`). Pure so the lookup is testable.
pub fn claude_virtual_model_hex(model: &str) -> String {
    model
        .trim()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Pick the gateway's client-visible model id that names `model`. The list
/// carries ids like `anthropic/claude-ccr-h<hex>[1m]`; the `[1m]` variant is
/// preferred (that is the one CCR's own claude wrapper pins). `None` when the
/// model is not exposed to claude — the caller then leaves the model option
/// alone rather than pin something claude would refuse to confirm.
pub fn pick_claude_gateway_model_id(
    model_ids: &[String],
    model: &str,
) -> Option<String> {
    let needle = format!("h{}", claude_virtual_model_hex(model));
    let needle_lower = needle.to_lowercase();
    let matches: Vec<&String> = model_ids
        .iter()
        .filter(|id| id.to_lowercase().contains(&needle_lower))
        .collect();
    matches
        .iter()
        .find(|id| id.to_lowercase().ends_with("[1m]"))
        .or_else(|| matches.first())
        .map(|id| (*id).clone())
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
            Some(AgentHostRecipe {
                env,
                config_options: BTreeMap::new(),
            })
        }
        // See the module docs: codex's overlay rides on `CODEX_HOME`, which
        // would move its session tree out of the parser's reach.
        _ => None,
    }
}

/// Read CCR's config and the gateway's claude-visible model list, and build
/// the launch recipe for one agent. `None` on every "no overlay" path:
/// injection disabled, CLI outside the recipe's scope, CCR unreachable, or a
/// config shape we don't recognize. A model that the gateway does not expose
/// to claude degrades to route-only (no model option) — never an error.
pub async fn recipe_for_agent(agent_type: AgentType) -> Option<AgentHostRecipe> {
    if !injection_enabled() {
        return None;
    }
    let kind = ccr_agent_kind(agent_type)?;
    let config = crate::commands::ccr::ccr_call_core("getConfig", None)
        .await
        .ok()?;
    let gateway = config
        .get("routerEndpoint")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            let port = config.pointer("/gateway/port").and_then(Value::as_u64)?;
            let host = config
                .pointer("/gateway/host")
                .and_then(Value::as_str)
                .unwrap_or("127.0.0.1");
            Some(format!("http://{host}:{port}"))
        })?;
    let mut recipe = recipe_for_ccr_gateway(agent_type, &gateway)?;

    // The enabled profile wins (same rule as the CLI page's binding card);
    // a disabled one still names the CLI's configured model, which beats
    // injecting nothing.
    let profiles = config
        .pointer("/profile/profiles")
        .and_then(Value::as_array);
    let profile = profiles.and_then(|list| {
        list.iter()
            .find(|profile| {
                profile.get("agent").and_then(Value::as_str) == Some(kind)
                    && profile.get("enabled").and_then(Value::as_bool) != Some(false)
            })
            .or_else(|| {
                list.iter()
                    .find(|profile| profile.get("agent").and_then(Value::as_str) == Some(kind))
            })
    });
    if agent_type == AgentType::ClaudeCode {
        let model = profile
            .and_then(|profile| profile.get("model"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty());
        let profile_id = profile
            .and_then(|profile| profile.get("id"))
            .and_then(Value::as_str);
        if let Some(model) = model {
            if let Some(model_id) =
                claude_model_option_id(&config, &gateway, profile_id, model).await
            {
                recipe
                    .config_options
                    .insert("model".into(), model_id);
            }
        }
    }
    Some(recipe)
}

/// The api key that makes the gateway answer as this profile (its discovered
/// model list is filtered per profile). Falls back to the local gateway key.
fn profile_api_key(config: &Value, profile_id: Option<&str>) -> Option<String> {
    let keys = config.get("APIKEYS").and_then(Value::as_array)?;
    let wanted = profile_id.map(|id| format!("profile:{id}"));
    let pick = |matcher: &dyn Fn(&Value) -> bool| {
        keys.iter()
            .find(|entry| matcher(entry))
            .and_then(|entry| entry.get("key"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    if let Some(key) = wanted
        .as_deref()
        .and_then(|wanted| pick(&|entry: &Value| {
            entry.get("id").and_then(Value::as_str) == Some(wanted)
        }))
    {
        return Some(key);
    }
    pick(&|entry: &Value| {
        entry.get("id").and_then(Value::as_str) == Some("local-gateway")
    })
}

/// Ask the gateway for its claude-visible model ids and resolve the one that
/// names `model`. Best-effort: any failure degrades to `None`.
async fn claude_model_option_id(
    config: &Value,
    gateway: &str,
    profile_id: Option<&str>,
    model: &str,
) -> Option<String> {
    let api_key = profile_api_key(config, profile_id)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let response = client
        .get(format!("{}/v1/models", gateway.trim_end_matches('/')))
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("user-agent", "claude-cli/2.0.1 (external, cli)")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    let ids: Vec<String> = body
        .get("data")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let picked = pick_claude_gateway_model_id(&ids, model);
    if picked.is_none() {
        tracing::warn!(
            "[agent-host] model {model} is not in the gateway's claude-visible list; \
             leaving the session model option untouched"
        );
    }
    picked
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
        // the process env, so the recipe routes via env and picks the model
        // via the ACP config option instead.
        assert!(!recipe.env.contains_key("ANTHROPIC_MODEL"));
        assert!(recipe.config_options.is_empty());
    }

    #[test]
    fn an_empty_gateway_yields_no_recipe() {
        assert!(recipe_for_ccr_gateway(AgentType::ClaudeCode, "  ").is_none());
    }

    #[test]
    fn codex_is_out_of_scope_for_v1() {
        assert!(recipe_for_ccr_gateway(AgentType::Codex, "http://127.0.0.1:3456").is_none());
    }

    #[test]
    fn virtual_model_hex_matches_the_gateway_encoding() {
        // Verified against the live gateway list on 2026-10-12.
        assert_eq!(
            claude_virtual_model_hex("OpenCode Go/mimo-v2.6-pro"),
            "4f70656e436f646520476f2f6d696d6f2d76322e362d70726f"
        );
    }

    #[test]
    fn picks_the_one_million_context_variant_when_present() {
        let hex = claude_virtual_model_hex("OpenCode Go/mimo-v2.6-flash");
        let ids = vec![
            format!("anthropic/claude-ccr-h{hex}"),
            format!("anthropic/claude-ccr-h{hex}[1m]"),
            "anthropic/claude-ccr-hdeadbeef[1m]".to_string(),
        ];
        assert_eq!(
            pick_claude_gateway_model_id(&ids, "OpenCode Go/mimo-v2.6-flash").as_deref(),
            Some(format!("anthropic/claude-ccr-h{hex}[1m]").as_str())
        );
    }

    #[test]
    fn falls_back_to_the_plain_id_and_none_when_absent() {
        let hex = claude_virtual_model_hex("OpenCode Go/mimo-v2.5");
        let ids = vec![format!("anthropic/claude-ccr-h{hex}")];
        assert_eq!(
            pick_claude_gateway_model_id(&ids, "OpenCode Go/mimo-v2.5").as_deref(),
            Some(format!("anthropic/claude-ccr-h{hex}").as_str())
        );
        assert!(pick_claude_gateway_model_id(&ids, "OpenCode Go/nope").is_none());
    }
}
