//! [crewchat] The CCR gateway's claude-visible model catalog, and how a
//! roster brain (`provider` + `model`) maps onto one of its ids.
//!
//! The gateway does not list model names as the roster writes them: a
//! claude-client request is answered with ids of the shape
//! `anthropic/claude-ccr-h<hex>[1m]`, where `<hex>` is the lowercase hex of
//! the `Provider/model` string CCR resolved. Everything here is either pure
//! (the hex encoding, the pick) or a best-effort read of `/v1/models` with the
//! claude user agent; a miss is `None` so the caller degrades rather than
//! pinning something claude would refuse to confirm.

use serde_json::Value;

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
pub fn pick_claude_gateway_model_id(model_ids: &[String], model: &str) -> Option<String> {
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

/// The first candidate the gateway exposes to claude, as the exact
/// client-visible id (`anthropic/claude-ccr-h<hex>[1m]`).
pub fn pick_claude_gateway_model_id_for(
    model_ids: &[String],
    candidates: &[String],
) -> Option<String> {
    if let Some(id) = candidates
        .iter()
        .find_map(|candidate| pick_claude_gateway_model_id(model_ids, candidate))
    {
        return Some(id);
    }
    // No candidate matched a whole `provider/model` encoding. A roster's
    // provider can be stale (it named a gateway CCR no longer has) while the
    // MODEL is still served, so accept a bare model name when exactly ONE
    // provider serves it — guessing between providers would be worse than
    // degrading to the CLI's own configuration.
    candidates
        .iter()
        .filter(|candidate| !candidate.contains('/'))
        .find_map(|model| pick_claude_gateway_model_id_by_model(model_ids, model))
}

/// The id for a model named WITHOUT a usable provider, matched on the hex
/// suffix CCR encodes for it. Only a unique provider qualifies.
fn pick_claude_gateway_model_id_by_model(model_ids: &[String], model: &str) -> Option<String> {
    let needle = claude_virtual_model_hex(model);
    let hits: Vec<&String> = model_ids
        .iter()
        .filter(|id| {
            gateway_id_hex(id).is_some_and(|hex| hex.len() > needle.len() && hex.ends_with(&needle))
        })
        .collect();
    let mut providers: Vec<&str> = hits
        .iter()
        .map(|id| id.strip_suffix("[1m]").unwrap_or(id))
        .collect();
    providers.sort_unstable();
    providers.dedup();
    if providers.len() != 1 {
        return None;
    }
    hits.iter()
        .find(|id| id.to_lowercase().ends_with("[1m]"))
        .or_else(|| hits.first())
        .map(|id| (*id).clone())
}

/// The hex CCR encodes into a client-visible id, with the `[1m]` suffix
/// stripped. `None` on an id that is not one of ours.
fn gateway_id_hex(model_id: &str) -> Option<&str> {
    let (_, hex) = model_id.split_once("claude-ccr-h")?;
    Some(hex.strip_suffix("[1m]").unwrap_or(hex))
}

/// Decode a client-visible id back into the `(provider display name, model)` CCR
/// encoded — the inverse of [`claude_virtual_model_hex`], and what lets the
/// roster page list exactly the models a claude session may be pinned to.
/// `None` on an id that is not one of ours, or whose hex is not UTF-8.
pub fn decode_claude_virtual_model(model_id: &str) -> Option<(String, String)> {
    let hex = gateway_id_hex(model_id)?;
    if hex.len() % 2 != 0 {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect();
    let decoded = String::from_utf8(bytes?).ok()?;
    let (provider, model) = decoded.split_once('/')?;
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some((provider.to_string(), model.to_string()))
}

/// The claude face's model ids, read live from the gateway (`/v1/models` with the
/// claude user agent). `None` when CCR is unreachable or has no usable key —
/// the caller reports "cannot determine" rather than an empty choice set.
pub async fn claude_face_ids(config: &Value, gateway: &str) -> Option<Vec<String>> {
    let api_key = local_gateway_api_key(config)?;
    claude_model_ids(gateway, &api_key).await
}

/// The display name CCR encodes into its claude-visible model ids for a
/// provider the roster named. The roster may hold either the provider's id
/// (`opencode-go-chat-completions`) or its display name (`OpenCode Go`), but
/// the gateway only ever encodes the NAME (`h<hex of "OpenCode Go/model">`),
/// so an id must be mapped before the lookup. Anything unrecognized passes
/// through unchanged — the lookup then simply misses and the caller degrades.
pub fn provider_display_name(config: &Value, provider: &str) -> String {
    let provider = provider.trim();
    if provider.is_empty() {
        return String::new();
    }
    let named = config
        .get("Providers")
        .and_then(Value::as_array)
        .and_then(|providers| {
            providers.iter().find_map(|entry| {
                let name = entry.get("name").and_then(Value::as_str)?;
                let id = entry.get("id").and_then(Value::as_str);
                let hit = id == Some(provider) || name == provider;
                hit.then(|| name.to_string())
            })
        });
    named.unwrap_or_else(|| provider.to_string())
}

/// Candidate `provider/model` labels for a role brain, most specific first.
/// A roster may name the provider by id, by display name, or omit it
/// altogether (the model alone) — the caller resolves them against the
/// gateway's discovered list and takes the first that matches.
pub fn claude_role_model_candidates(config: &Value, provider: &str, model: &str) -> Vec<String> {
    let model = model.trim();
    if model.is_empty() {
        return Vec::new();
    }
    let mut candidates: Vec<String> = Vec::new();
    let mut push = |label: String| {
        if !label.is_empty() && !candidates.contains(&label) {
            candidates.push(label);
        }
    };
    let provider = provider.trim();
    if provider.is_empty() {
        // No provider: the model may live under any of CCR's providers, so try
        // each one's display name before falling back to the bare model.
        if let Some(providers) = config.get("Providers").and_then(Value::as_array) {
            for entry in providers {
                if let Some(name) = entry.get("name").and_then(Value::as_str) {
                    push(claude_role_model_label(name, model, false));
                }
            }
        }
    } else {
        push(claude_role_model_label(
            &provider_display_name(config, provider),
            model,
            false,
        ));
        push(claude_role_model_label(provider, model, false));
    }
    push(model.to_string());
    candidates
}

/// The client-visible model label for a role: `provider/model`, mirroring the
/// shape CCR's own claude wrapper writes into the global settings. A gateway
/// listing that exposes only the `[1m]` variant of the pair earns the suffix.
pub fn claude_role_model_label(provider: &str, model: &str, one_million: bool) -> String {
    let provider = provider.trim();
    let model = model.trim();
    let base = if provider.is_empty() {
        model.to_string()
    } else {
        format!("{provider}/{model}")
    };
    if one_million && !base.ends_with("[1m]") {
        format!("{base}[1m]")
    } else {
        base
    }
}

/// The api key that makes the gateway answer (its discovered model list is
/// filtered per key). The local gateway key, which sees every provider.
fn local_gateway_api_key(config: &Value) -> Option<String> {
    config
        .get("APIKEYS")
        .and_then(Value::as_array)?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some("local-gateway"))
        .and_then(|entry| entry.get("key"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The claude-visible model ids the gateway answers with for `api_key`.
/// Best-effort: any failure (unreachable gateway, unexpected body) is `None`.
async fn claude_model_ids(gateway: &str, api_key: &str) -> Option<Vec<String>> {
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
    Some(
        body.get("data")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|entry| entry.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect(),
    )
}

/// The model NAME a non-claude session must send to the gateway, or `None` when
/// the gateway does not serve it.
///
/// The bodies that are not claude reach the gateway through an OpenAI-shaped
/// face — codex via **responses**, opencode via **chat-completions** — and both
/// take the model exactly as the roster writes it; no client-author-prefixed
/// virtual id is involved, unlike claude (verified 2026-10-18/19: a bare
/// `mimo-v2.6-flash` answered 200 through `/v1/responses` AND
/// `/v1/chat/completions`). The check exists so a roster naming a model the
/// gateway does not have degrades to the CLI's own configuration instead of
/// failing mid-turn.
pub fn provider_gateway_model(config: &Value, model: &str) -> Option<String> {
    let wanted = model.trim();
    if wanted.is_empty() {
        return None;
    }
    let providers = config.get("Providers").and_then(Value::as_array)?;
    for provider in providers {
        // `models` is what the provider serves; `autoFetchKnownModels` is what
        // its catalog probe found. Either is a legitimate source — a freshly
        // added provider may only have the fetched list.
        for field in ["models", "autoFetchKnownModels"] {
            let Some(entries) = provider.get(field).and_then(Value::as_array) else {
                continue;
            };
            for entry in entries {
                let Some(name) = entry.as_str() else { continue };
                if name.trim().eq_ignore_ascii_case(wanted) {
                    return Some(name.trim().to_string());
                }
            }
        }
    }
    None
}

/// The exact client-visible id the gateway exposes for the first candidate it
/// knows (`anthropic/claude-ccr-h<hex>` or its `[1m]` variant), or `None` when
/// the gateway is unreachable or exposes none of them.
pub async fn gateway_claude_model_id(
    config: &Value,
    gateway: &str,
    candidates: &[String],
) -> Option<String> {
    let api_key = local_gateway_api_key(config)?;
    let ids = claude_model_ids(gateway, &api_key).await?;
    pick_claude_gateway_model_id_for(&ids, candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn cd_mxdel_must_be_served_by_some_provider() {
        let config = json!({
            "Providers": [
                { "name": "OpenCode Go", "models": ["mimo-v2.6-flash", "deepseek-v4.1-flash"] },
                { "name": "Other", "autoFetchKnownModels": ["step-5-preview-free"] }
            ]
        });
        // Served by name, from either list, and matched case-insensitively.
        assert_eq!(
            provider_gateway_model(&config, "mimo-v2.6-flash").as_deref(),
            Some("mimo-v2.6-flash")
        );
        assert_eq!(
            provider_gateway_model(&config, " MiMo-V2.6-Flash ").as_deref(),
            Some("mimo-v2.6-flash")
        );
        assert_eq!(
            provider_gateway_model(&config, "step-5-preview-free").as_deref(),
            Some("step-5-preview-free")
        );
        // A retired model (or a provider that is not configured at all) is not
        // pinnable: the caller degrades instead of 404-ing every turn.
        assert!(provider_gateway_model(&config, "deepseek-flash").is_none());
        assert!(provider_gateway_model(&config, "  ").is_none());
        assert!(provider_gateway_model(&json!({}), "m").is_none());
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

    #[test]
    fn role_model_label_mirrors_the_global_shape() {
        assert_eq!(
            claude_role_model_label("OpenCode Go", "mimo-v2.6-pro", false),
            "OpenCode Go/mimo-v2.6-pro"
        );
        assert_eq!(
            claude_role_model_label("OpenCode Go", "mimo-v2.6-pro", true),
            "OpenCode Go/mimo-v2.6-pro[1m]"
        );
        assert_eq!(
            claude_role_model_label("OpenCode Go", "mimo-v2.6-pro[1m]", true),
            "OpenCode Go/mimo-v2.6-pro[1m]"
        );
        // A role with no provider still names a model.
        assert_eq!(
            claude_role_model_label("", "opencode/big-pickle", false),
            "opencode/big-pickle"
        );
    }

    fn one_provider_config() -> Value {
        serde_json::json!({
            "Providers": [
                { "id": "opencode-go-chat-completions", "name": "OpenCode Go" }
            ]
        })
    }

    #[test]
    fn a_provider_id_resolves_to_its_display_name() {
        let config = one_provider_config();
        // The gateway only encodes display NAMES, so an id from the roster must
        // be mapped; an unknown value passes through (and simply won't match).
        assert_eq!(
            provider_display_name(&config, "opencode-go-chat-completions"),
            "OpenCode Go"
        );
        assert_eq!(provider_display_name(&config, "OpenCode Go"), "OpenCode Go");
        assert_eq!(provider_display_name(&config, "75f800be"), "75f800be");
        assert_eq!(provider_display_name(&config, "  "), "");
    }

    #[test]
    fn role_candidates_cover_the_id_the_name_and_the_bare_model() {
        let config = one_provider_config();
        assert_eq!(
            claude_role_model_candidates(
                &config,
                "opencode-go-chat-completions",
                "mimo-v2.6-flash"
            ),
            vec![
                "OpenCode Go/mimo-v2.6-flash".to_string(),
                "opencode-go-chat-completions/mimo-v2.6-flash".to_string(),
                "mimo-v2.6-flash".to_string(),
            ]
        );
        // No provider: every known provider is tried before the bare model.
        assert_eq!(
            claude_role_model_candidates(&config, "", "deepseek-v4.1-flash"),
            vec![
                "OpenCode Go/deepseek-v4.1-flash".to_string(),
                "deepseek-v4.1-flash".to_string(),
            ]
        );
        assert!(claude_role_model_candidates(&config, "OpenCode Go", "  ").is_empty());
    }

    #[test]
    fn picks_the_first_candidate_the_gateway_exposes() {
        let hex = claude_virtual_model_hex("OpenCode Go/mimo-v2.6-flash");
        let ids = vec![format!("anthropic/claude-ccr-h{hex}[1m]")];
        let candidates = vec![
            // A roster still holding a retired brain — the gateway knows none
            // of its forms.
            "75f800be/deepseek-flash".to_string(),
            "deepseek-flash".to_string(),
            "OpenCode Go/mimo-v2.6-flash".to_string(),
        ];
        assert_eq!(
            pick_claude_gateway_model_id_for(&ids, &candidates).as_deref(),
            Some(format!("anthropic/claude-ccr-h{hex}[1m]").as_str())
        );
        // None of them exposed → the caller degrades to the CLI's own config.
        assert!(
            pick_claude_gateway_model_id_for(&ids, &["75f800be/deepseek-flash".to_string()])
                .is_none()
        );
    }

    #[test]
    fn a_stale_provider_still_resolves_a_model_only_one_provider_serves() {
        let hex = claude_virtual_model_hex("OpenCode Go/mimo-v2.6-flash");
        let ids = vec![
            format!("anthropic/claude-ccr-h{hex}"),
            format!("anthropic/claude-ccr-h{hex}[1m]"),
        ];
        // The roster's provider (`75f800be`) is one CCR no longer has, but the
        // model is served by exactly one provider → pin its `[1m]` id.
        let candidates =
            claude_role_model_candidates(&one_provider_config(), "75f800be", "mimo-v2.6-flash");
        assert_eq!(
            pick_claude_gateway_model_id_for(&ids, &candidates).as_deref(),
            Some(format!("anthropic/claude-ccr-h{hex}[1m]").as_str())
        );

        // The same model name under TWO providers is ambiguous: never guess.
        let other = claude_virtual_model_hex("Other/mimo-v2.6-flash");
        let ambiguous = vec![ids[1].clone(), format!("anthropic/claude-ccr-h{other}[1m]")];
        assert!(pick_claude_gateway_model_id_for(&ambiguous, &candidates).is_none());

        // A model under one provider but without the `[1m]` variant still works.
        let plain = vec![format!("anthropic/claude-ccr-h{hex}")];
        assert_eq!(
            pick_claude_gateway_model_id_for(&plain, &candidates).as_deref(),
            Some(format!("anthropic/claude-ccr-h{hex}").as_str())
        );
    }
}
