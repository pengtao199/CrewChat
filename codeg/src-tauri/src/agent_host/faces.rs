//! [crewchat] What each BODY's gateway face can actually be pinned to.
//!
//! The roster page must only offer models the chosen body can be pinned to, or
//! it is promising something the launcher will quietly ignore. Two faces exist,
//! and they are NOT the same list:
//!
//! - **claude** — the gateway answers a claude client with ids of the shape
//!   `anthropic/claude-ccr-h<hex>[1m]`; the hex DECODES back to
//!   `Provider display name/model`, which is exactly what the roster stores.
//! - **codex** — routed through the OpenAI **responses** face, which takes the
//!   provider's own model names (`Providers[].models` / `autoFetchKnownModels`).
//!
//! A body with no overlay ([`overlay_support`]) has an EMPTY face and a reason:
//! no choice can be honoured, so the page says so instead of listing models.

use serde_json::Value;

use crate::models::AgentType;

use super::gateway::{claude_face_ids, decode_claude_virtual_model};
use super::{ccr_gateway_base, overlay_support};

/// One selectable `(provider, model)` pair, as the roster writes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RoleModelOption {
    /// Provider as the roster stores it — the display name (`OpenCode Go`),
    /// which is the form the gateway's claude face encodes.
    pub provider: String,
    /// CCR's provider id (`opencode-go-chat-completions`), for callers that
    /// would rather keep the id.
    pub provider_id: String,
    pub model: String,
}

/// A body's face: whether it can be pinned at all, why not when it cannot, and
/// which pairs it offers. `models` empty WITH `supported = true` means "could
/// not determine" (CCR unreachable) — never "nothing is available".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BodyModelFace {
    pub supported: bool,
    pub reason: Option<String>,
    pub models: Vec<RoleModelOption>,
}

impl BodyModelFace {
    fn unsupported(reason: String) -> Self {
        Self {
            supported: false,
            reason: Some(reason),
            models: Vec::new(),
        }
    }

    fn undetermined(reason: String) -> Self {
        Self {
            supported: true,
            reason: Some(reason),
            models: Vec::new(),
        }
    }
}

/// Every `(provider, model)` pair CCR serves, in provider order. Pure: the
/// caller passes the `getConfig` payload.
pub fn all_provider_models(config: &Value) -> Vec<RoleModelOption> {
    let mut out = Vec::new();
    let Some(providers) = config.get("Providers").and_then(Value::as_array) else {
        return out;
    };
    for provider in providers {
        let name = provider
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let id = provider
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        for field in ["models", "autoFetchKnownModels"] {
            let Some(models) = provider.get(field).and_then(Value::as_array) else {
                continue;
            };
            for model in models {
                let Some(model) = model.as_str().map(str::trim).filter(|m| !m.is_empty()) else {
                    continue;
                };
                let option = RoleModelOption {
                    provider: name.to_string(),
                    provider_id: id.to_string(),
                    model: model.to_string(),
                };
                if !out.contains(&option) {
                    out.push(option);
                }
            }
        }
    }
    out
}

/// The claude face, decoded from the gateway's live id list. Pure, so the
/// decode is testable without a gateway.
pub fn claude_face_options(config: &Value, model_ids: &[String]) -> Vec<RoleModelOption> {
    let known = all_provider_models(config);
    let mut out = Vec::new();
    for id in model_ids {
        let Some((provider, model)) = decode_claude_virtual_model(id) else {
            continue;
        };
        // The id carries the provider's display name; recover the id when CCR
        // still has that provider, so callers get both forms.
        let provider_id = known
            .iter()
            .find(|option| option.provider == provider)
            .map(|option| option.provider_id.clone())
            .unwrap_or_default();
        let option = RoleModelOption {
            provider,
            provider_id,
            model,
        };
        if !out.contains(&option) {
            out.push(option);
        }
    }
    out
}

/// What `body` can be pinned to, resolved live. Never fails: an unreachable CCR
/// reports "undetermined" so the page can say that instead of showing an empty
/// picker.
pub async fn body_model_face(body: AgentType) -> BodyModelFace {
    if let Err(reason) = overlay_support(body) {
        return BodyModelFace::unsupported(reason);
    }
    let config = match crate::ccr::client::call("getConfig", None).await {
        Ok(config) => config,
        Err(error) => return BodyModelFace::undetermined(format!("CCR is unreachable: {error}")),
    };
    let Some(gateway) = ccr_gateway_base(&config) else {
        return BodyModelFace::undetermined("CCR exposes no gateway endpoint".into());
    };
    match body {
        AgentType::Codex => BodyModelFace {
            supported: true,
            reason: None,
            models: all_provider_models(&config),
        },
        // claude can only be pinned to what the gateway ADVERTISES to a claude
        // client; a model CCR serves to codex but not to claude is a choice
        // that would abort the session, so it must not be offered.
        AgentType::ClaudeCode => match claude_face_ids(&config, &gateway).await {
            Some(ids) => BodyModelFace {
                supported: true,
                reason: None,
                models: claude_face_options(&config, &ids),
            },
            None => BodyModelFace::undetermined(
                "the gateway's claude model list could not be read".into(),
            ),
        },
        // `overlay_support` already answered for everything else.
        other => BodyModelFace::unsupported(format!("{other:?} has no per-role model overlay yet")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> Value {
        json!({
            "Providers": [
                {
                    "id": "opencode-go-chat-completions",
                    "name": "OpenCode Go",
                    "models": ["mimo-v2.6-flash", "deepseek-v4.1-flash"],
                },
                {
                    "id": "agnes",
                    "name": "Agnes",
                    "autoFetchKnownModels": ["agnes-2.5-flash"],
                }
            ]
        })
    }

    #[test]
    fn decodes_the_provider_and_model_back_out_of_a_gateway_id() {
        // Round-trips with the encoder the lookup already uses.
        let hex = super::super::gateway::claude_virtual_model_hex("OpenCode Go/mimo-v2.6-flash");
        assert_eq!(
            decode_claude_virtual_model(&format!("anthropic/claude-ccr-h{hex}[1m]")),
            Some(("OpenCode Go".to_string(), "mimo-v2.6-flash".to_string()))
        );
        assert_eq!(
            decode_claude_virtual_model(&format!("anthropic/claude-ccr-h{hex}")),
            Some(("OpenCode Go".to_string(), "mimo-v2.6-flash".to_string()))
        );
        // Foreign ids and malformed hex are skipped, never guessed at.
        assert!(decode_claude_virtual_model("gpt-5").is_none());
        assert!(decode_claude_virtual_model("anthropic/claude-ccr-hzz").is_none());
        assert!(decode_claude_virtual_model("anthropic/claude-ccr-h6f").is_none());
    }

    #[test]
    fn claude_face_is_what_the_gateway_advertises_and_keeps_both_provider_forms() {
        let good = super::super::gateway::claude_virtual_model_hex("OpenCode Go/mimo-v2.6-flash");
        let stale = super::super::gateway::claude_virtual_model_hex("Retired Provider/old-model");
        let options = claude_face_options(
            &config(),
            &[
                format!("anthropic/claude-ccr-h{good}[1m]"),
                // A model CCR serves but no longer under a known provider still
                // appears — with an empty id, so the UI keeps showing the truth.
                format!("anthropic/claude-ccr-h{stale}"),
                "some-other-model".to_string(),
            ],
        );
        assert_eq!(options.len(), 2);
        assert_eq!(
            options[0],
            RoleModelOption {
                provider: "OpenCode Go".into(),
                provider_id: "opencode-go-chat-completions".into(),
                model: "mimo-v2.6-flash".into(),
            }
        );
        assert_eq!(options[1].provider, "Retired Provider");
        assert_eq!(options[1].provider_id, "");
    }

    #[test]
    fn provider_models_cover_both_lists_for_codex() {
        let options = all_provider_models(&config());
        assert_eq!(
            options
                .iter()
                .map(|option| option.model.as_str())
                .collect::<Vec<_>>(),
            vec!["mimo-v2.6-flash", "deepseek-v4.1-flash", "agnes-2.5-flash"]
        );
        assert_eq!(options[2].provider, "Agnes");
    }

    #[tokio::test]
    async fn unsupported_bodies_report_a_reason_instead_of_an_empty_picker() {
        let face = body_model_face(AgentType::Gemini).await;
        assert!(!face.supported);
        assert!(face.reason.is_some());
        assert!(face.models.is_empty());
    }
}
