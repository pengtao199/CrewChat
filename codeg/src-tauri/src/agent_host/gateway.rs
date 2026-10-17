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

/// Ask the gateway for its claude-visible model ids and resolve the one that
/// names `model`. Best-effort: any failure degrades to `None`.
pub async fn claude_model_option_id(
    config: &Value,
    gateway: &str,
    profile_id: Option<&str>,
    model: &str,
) -> Option<String> {
    let api_key = profile_api_key(config, profile_id)?;
    let ids = claude_model_ids(gateway, &api_key).await?;
    let picked = pick_claude_gateway_model_id(&ids, model);
    if picked.is_none() {
        tracing::warn!(
            "[agent-host] model {model} is not in the gateway's claude-visible list; \
             leaving the session model option untouched"
        );
    }
    picked
}

/// The exact client-visible id the gateway exposes for `provider/model`
/// (`anthropic/claude-ccr-h<hex>` or its `[1m]` variant), or `None` when the
/// gateway is unreachable or does not expose the pair.
pub async fn gateway_claude_model_id(
    config: &Value,
    gateway: &str,
    composite: &str,
) -> Option<String> {
    let api_key = profile_api_key(config, None)?;
    let ids = claude_model_ids(gateway, &api_key).await?;
    pick_claude_gateway_model_id(&ids, composite)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn role_model_label_mirrors_the_global_shape() {
        assert_eq!(claude_role_model_label("OpenCode Go", "mimo-v2.6-pro", false), "OpenCode Go/mimo-v2.6-pro");
        assert_eq!(claude_role_model_label("OpenCode Go", "mimo-v2.6-pro", true), "OpenCode Go/mimo-v2.6-pro[1m]");
        assert_eq!(claude_role_model_label("OpenCode Go", "mimo-v2.6-pro[1m]", true), "OpenCode Go/mimo-v2.6-pro[1m]");
        // A role with no provider still names a model.
        assert_eq!(claude_role_model_label("", "opencode/big-pickle", false), "opencode/big-pickle");
    }
}
