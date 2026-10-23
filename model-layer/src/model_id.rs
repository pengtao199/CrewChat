//! Model id parsing: CrewChat's Claude virtual ids, human `Provider/model` labels,
//! OpenCode's `crewchat/<model>` namespace, and bare model names.
//!
//! The gateway has no "current model" and no CLI binding: every request names
//! a model, and this module turns that name into exactly one
//! `(provider, model)` pair or an explicit ambiguity error.

use std::fmt;

/// One provider as the resolver sees it (a projection of the store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub kind: String,
    /// Routable models, in catalog order.
    pub models: Vec<String>,
    /// Models advertised with the `[1m]` suffix to Claude clients.
    pub one_million: Vec<String>,
}

/// A fully resolved upstream target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub provider_id: String,
    pub provider_name: String,
    pub base_url: String,
    pub kind: String,
    pub model: String,
    pub one_million: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    Empty,
    NotFound(String),
    Ambiguous {
        model: String,
        providers: Vec<String>,
    },
    BadVirtualId(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "model is required"),
            Self::NotFound(model) => write!(f, "no provider serves model '{model}'"),
            Self::Ambiguous { model, providers } => write!(
                f,
                "model '{model}' is served by multiple providers ({}); use 'Provider/{model}'",
                providers.join(", ")
            ),
            Self::BadVirtualId(id) => write!(f, "invalid claude virtual model id '{id}'"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// The lowercase hex encoded into `anthropic/claude-crewchat-h<hex>`.
pub fn virtual_hex(provider: &str, model: &str) -> String {
    hex::encode(format!("{provider}/{model}").as_bytes())
}

/// Decode a CrewChat virtual id into `(provider, model, one_million)`.
pub fn decode_virtual(id: &str) -> Option<(String, String, bool)> {
    let (_, rest) = id.split_once("claude-crewchat-h")?;
    let (hex_part, one_million) = match rest.strip_suffix("[1m]") {
        Some(value) => (value, true),
        None => (rest, false),
    };
    let bytes = hex::decode(hex_part).ok()?;
    let decoded = String::from_utf8(bytes).ok()?;
    let (provider, model) = decoded.split_once('/')?;
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some((provider.to_string(), model.to_string(), one_million))
}

/// Resolve one incoming `model` against the catalog.
pub fn resolve(raw: &str, providers: &[ProviderView]) -> Result<Target, ResolveError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ResolveError::Empty);
    }

    // 1. Claude's virtual id.
    if raw.contains("claude-crewchat-h") {
        let (provider_name, model, one_million) =
            decode_virtual(raw).ok_or_else(|| ResolveError::BadVirtualId(raw.to_string()))?;
        let provider = providers
            .iter()
            .find(|p| eq(&p.name, &provider_name) || eq(&p.id, &provider_name))
            .ok_or_else(|| ResolveError::NotFound(raw.to_string()))?;
        let canonical = find_model(provider, &model)
            .ok_or_else(|| ResolveError::NotFound(format!("{provider_name}/{model}")))?;
        return Ok(target(provider, canonical, one_million));
    }

    // 2. OpenCode's local provider namespace. `crewchat` is not an
    //    upstream provider; strip it before any provider lookup.
    let stripped = raw.strip_prefix("crewchat/").unwrap_or(raw);
    let (stripped, suffix_one_million) = match stripped.strip_suffix("[1m]") {
        Some(value) => (value, true),
        None => (stripped, false),
    };

    // 3. A prefix that names a real provider (`Provider/model`). Tried before
    //    the bare-model scan so an OpenRouter model like
    //    `cohere/north-mini-code:free` is not mistaken for provider `cohere`.
    for provider in providers {
        for prefix in [&provider.name, &provider.id] {
            let Some(rest) = strip_prefix_ci(stripped, &format!("{prefix}/")) else {
                continue;
            };
            if let Some(canonical) = find_model(provider, rest) {
                return Ok(target(provider, canonical, suffix_one_million));
            }
        }
    }

    // 4. Bare model: exactly one provider must serve it.
    let mut hits = Vec::new();
    for provider in providers {
        if let Some(canonical) = find_model(provider, stripped) {
            hits.push((provider, canonical));
        }
    }
    match hits.len() {
        0 => Err(ResolveError::NotFound(stripped.to_string())),
        1 => {
            let (provider, model) = &hits[0];
            Ok(target(provider, model.clone(), suffix_one_million))
        }
        _ => Err(ResolveError::Ambiguous {
            model: stripped.to_string(),
            providers: hits.iter().map(|(p, _)| p.name.clone()).collect(),
        }),
    }
}

fn target(provider: &ProviderView, model: String, one_million: bool) -> Target {
    let flagged = provider.one_million.iter().any(|m| eq(m, &model));
    Target {
        provider_id: provider.id.clone(),
        provider_name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        kind: provider.kind.clone(),
        model,
        one_million: one_million || flagged,
    }
}

fn find_model(provider: &ProviderView, wanted: &str) -> Option<String> {
    provider
        .models
        .iter()
        .find(|model| eq(model, wanted))
        .cloned()
}

fn eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn strip_prefix_ci<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if value.len() < prefix.len() {
        return None;
    }
    let (head, tail) = value.split_at(prefix.len());
    head.eq_ignore_ascii_case(prefix).then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<ProviderView> {
        vec![
            ProviderView {
                id: "opencode-go-chat-completions".into(),
                name: "OpenCode Go".into(),
                base_url: "https://opencode.ai/zen/go/v1".into(),
                kind: "openai_responses".into(),
                models: vec!["mimo-v2.6-flash".into(), "deepseek-v4.1-flash".into()],
                one_million: vec!["mimo-v2.6-flash".into()],
            },
            ProviderView {
                id: "openrouter".into(),
                name: "OpenRouter".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
                kind: "openai_chat_completions".into(),
                models: vec!["cohere/north-mini-code:free".into()],
                one_million: vec![],
            },
        ]
    }

    #[test]
    fn decodes_the_claude_virtual_id() {
        let hex = virtual_hex("OpenCode Go", "mimo-v2.6-flash");
        let id = format!("anthropic/claude-crewchat-h{hex}[1m]");
        assert_eq!(
            decode_virtual(&id),
            Some(("OpenCode Go".into(), "mimo-v2.6-flash".into(), true))
        );
    }

    #[test]
    fn resolves_a_virtual_id_to_one_provider() {
        let target = resolve(
            &format!(
                "anthropic/claude-crewchat-h{}[1m]",
                virtual_hex("OpenCode Go", "mimo-v2.6-flash")
            ),
            &catalog(),
        )
        .unwrap();
        assert_eq!(target.provider_name, "OpenCode Go");
        assert_eq!(target.model, "mimo-v2.6-flash");
        assert!(target.one_million);
    }

    #[test]
    fn strips_opencode_namespace() {
        let target = resolve("crewchat/mimo-v2.6-flash", &catalog()).unwrap();
        assert_eq!(target.provider_name, "OpenCode Go");
    }

    #[test]
    fn a_slash_in_the_model_is_not_a_provider_prefix() {
        let target = resolve("cohere/north-mini-code:free", &catalog()).unwrap();
        assert_eq!(target.provider_name, "OpenRouter");
        assert_eq!(target.model, "cohere/north-mini-code:free");
    }

    #[test]
    fn provider_label_wins_over_bare_match() {
        let target = resolve("OpenCode Go/deepseek-v4.1-flash", &catalog()).unwrap();
        assert_eq!(target.provider_id, "opencode-go-chat-completions");
        assert!(!target.one_million);
    }

    #[test]
    fn bare_model_is_rejected_when_two_providers_serve_it() {
        let mut providers = catalog();
        providers[1].models.push("mimo-v2.6-flash".to_string());
        let error = resolve("mimo-v2.6-flash", &providers).unwrap_err();
        assert!(matches!(error, ResolveError::Ambiguous { .. }));
    }

    #[test]
    fn unknown_model_is_not_found() {
        assert!(matches!(
            resolve("nope", &catalog()),
            Err(ResolveError::NotFound(_))
        ));
    }
}
