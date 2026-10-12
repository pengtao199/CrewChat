// agent_mentions submodule: routing frame (codeg_internal_agent_routes) (split from acp/agent_mentions.rs —
// mechanical move, see NOTICE.md).
use super::*;

use std::collections::BTreeSet;
use std::sync::LazyLock;
use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::acp::types::PromptInputBlock;

pub(crate) const ROUTE_FRAME_KIND: &str = "codeg_internal_agent_routes";
/// Opening bytes of the frame's descriptor line — the anchor used to find a
/// frame in a transcript that no longer has the separators around it (see
/// [`find_unseparated_frame`]). `kind` is the descriptor's first field, so this
/// is a prefix of every rendered body; a field reorder that broke that is what
/// `the_body_anchor_is_what_the_renderer_starts_with` guards.
pub(crate) const ROUTE_FRAME_BODY_PREFIX: &str = "{\"kind\":\"codeg_internal_agent_routes\"";
pub(crate) const ROUTE_FRAME_VERSION: u8 = 3;
pub(crate) const MAX_ROUTE_FRAME_BYTES: usize = 16 * 1024;
/// Distinct agents one frame may route, applied AFTER deduplication.
pub(crate) const MAX_AGENT_ROUTES: usize = 16;
pub(crate) const MAX_AGENT_REFERENCE_OCCURRENCES: usize = 256;

/// One routed agent inside the frame — nothing but the wire slug.
///
/// The frame is derived from the prompt's own visible `codeg://agent/...`
/// links, so there is no out-of-band claim to carry or verify. Serializing as
/// `{"agentType":"..."}` is load-bearing: the frame's bytes are its identity
/// (see [`render_internal_agent_routes`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRoute {
    pub(crate) agent_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InternalAgentRoutes {
    kind: String,
    version: u8,
    nonce: String,
    #[serde(skip)]
    routes: Vec<AgentRoute>,
}

pub(crate) fn agent_reference_regex() -> &'static Regex {
    // Agent wire slugs are built-ins or `custom:<slug>`, where the slug is
    // limited to ASCII alphanumeric, `-`, `_`, and `.`. Requiring a visible
    // label keeps this in lockstep with the frontend reference tokenizer.
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\[(?:\\.|[^\]\\\r\n])+\]\(codeg://agent/([A-Za-z0-9][A-Za-z0-9._:-]{0,127})\)")
            .expect("agent reference regex is valid")
    });
    &PATTERN
}

pub(crate) fn visible_agent_reference_types(blocks: &[PromptInputBlock]) -> Vec<String> {
    let mut references = Vec::new();
    for block in blocks {
        let PromptInputBlock::Text { text } = block else {
            continue;
        };
        for capture in agent_reference_regex().captures_iter(text) {
            if references.len() >= MAX_AGENT_REFERENCE_OCCURRENCES {
                return references;
            }
            if let Some(agent_type) = capture.get(1) {
                references.push(agent_type.as_str().to_string());
            }
        }
    }
    references
}

/// Append prompt-local routing for every agent the user visibly linked, when
/// this connection actually received the codeg-mcp companion's delegation
/// group.
///
/// The prompt IS the input: a visible `[label](codeg://agent/<type>)` link is
/// the whole criterion. That link is text the agent reads either way, and the
/// frame grants nothing — it only names the channel a delegation should take —
/// so there is deliberately no out-of-band claim to authenticate. Deriving from
/// the text instead means copy-paste, draft restore, queue editing and a
/// remounted composer all behave identically, because they all preserve the
/// link.
///
/// No per-agent special-casing lives here either. `delegation_enabled` is the
/// single launch-time fact this needs: the connection exposed the
/// `delegate_to_agent` tool group, which already implies the agent forwards MCP
/// over the wire (`AcpAgentMeta::supports_mcp` + `agent_delivers_wire_mcp` —
/// that pair is what gates the injection). Whether the named agent is installed
/// is NOT checked here: answering that costs a filesystem sweep per launch, and
/// the delegation spawner is the hard gate for it anyway. A mention that cannot
/// be served comes back as a `delegate_to_agent` error instead.
///
/// The original blocks remain untouched until the connection loop, so
/// broadcasts, previews, and optimistic user messages never contain this
/// internal block.
pub(crate) fn append_agent_routes(blocks: &mut Vec<PromptInputBlock>, delegation_enabled: bool) {
    if !delegation_enabled {
        return;
    }

    let mut seen = BTreeSet::new();
    let routes: Vec<AgentRoute> = visible_agent_reference_types(blocks)
        .into_iter()
        .filter(|agent_type| valid_agent_wire_syntax(agent_type))
        // Dedup first, then bound: a prompt repeating one agent 200 times still
        // routes it, and the cap only bites on distinct agents.
        .filter(|agent_type| seen.insert(agent_type.clone()))
        .take(MAX_AGENT_ROUTES)
        .map(|agent_type| AgentRoute { agent_type })
        .collect();
    if routes.is_empty() {
        return;
    }

    let frame = InternalAgentRoutes {
        kind: ROUTE_FRAME_KIND.to_string(),
        version: ROUTE_FRAME_VERSION,
        nonce: uuid::Uuid::new_v4().to_string(),
        routes,
    };
    blocks.push(PromptInputBlock::Text {
        text: render_internal_agent_routes(&frame),
    });
}

/// The frame's exact bytes ARE its identity: `parse_internal_agent_routes`
/// only accepts a candidate it can re-render byte-for-byte, which is what lets
/// the parsers strip a Codeg-authored frame without ever deleting look-alike
/// user prose.
///
/// CONSEQUENCE: editing this string orphans every frame already written into an
/// agent's on-disk transcript — they stop round-tripping and become visible
/// history. Any wording change must therefore bump [`ROUTE_FRAME_VERSION`];
/// `route_frame_wording_is_pinned_to_its_version` guards that.
///
/// No compatibility renderer exists yet: `parse_internal_agent_routes` accepts
/// the current version only, so a bump deliberately abandons the frames already
/// on disk. That is a pre-release call — there is no shipped history worth
/// preserving. Once there is, a wording change also has to keep the previous
/// version's renderer alive for PARSING, and dispatch on the candidate's own
/// `version` when round-tripping, so old transcripts keep cleaning up.
///
/// The instruction is narrow on purpose. Agents already discover
/// `delegate_to_agent` from the companion's tool schema; what they do wrong is
/// reach for their own sub-agent mechanism instead. This frame only binds the
/// channel — it does not tell the agent whether to delegate at all.
pub(crate) fn render_internal_agent_routes(frame: &InternalAgentRoutes) -> String {
    format!(
        "{ROUTE_FRAME_SEPARATOR}{}{ROUTE_FRAME_SEPARATOR}",
        render_internal_agent_routes_body(frame)
    )
}

/// The frame WITHOUT its separators: exactly three newline-terminated lines.
///
/// Split out because the separators are the one part of the frame an agent may
/// not give back. They are reserved and scrubbed at ingress, but that only
/// guarantees codeg never *sends* a stray one — it says nothing about what the
/// agent writes into its own store. Antigravity's ACP server, for one, joins
/// the prompt's text blocks with a space and replaces each separator with one,
/// so its trajectory holds this body and no separators at all.
///
/// Everything the identity argument needs is in here: the nonce, the routes,
/// and the full instruction wording. See [`find_unseparated_frame`].
pub(crate) fn render_internal_agent_routes_body(frame: &InternalAgentRoutes) -> String {
    let descriptor = serde_json::to_string(frame).expect("internal route frame is serializable");
    let routes = serde_json::to_string(&frame.routes).expect("agent routes are serializable");
    format!(
        "{descriptor}\n\
Codeg composer routing metadata (authoritative): {routes}\n\
When you delegate any of this work, route it through the `delegate_to_agent` \
tool from the `codeg-mcp` server with `agent_type` taken from the matching \
route: do not substitute your own native sub-agent, task, or spawn mechanism, \
and do not route it through any other delegation tool.\n"
    )
}

/// Validate a candidate against [`render_internal_agent_routes_body`].
///
/// Shared by both finders so the separated and unseparated forms can never
/// drift into accepting different things.
pub(crate) fn parse_internal_agent_routes_body(candidate: &str) -> Option<InternalAgentRoutes> {
    if candidate.len() > MAX_ROUTE_FRAME_BYTES {
        return None;
    }
    let (descriptor, remainder) = candidate.split_once('\n')?;
    let mut frame: InternalAgentRoutes = serde_json::from_str(descriptor).ok()?;
    let routes_json = remainder
        .lines()
        .next()?
        .strip_prefix("Codeg composer routing metadata (authoritative): ")?;
    frame.routes = serde_json::from_str(routes_json).ok()?;
    if frame.kind != ROUTE_FRAME_KIND
        || frame.version != ROUTE_FRAME_VERSION
        || uuid::Uuid::parse_str(&frame.nonce).is_err()
        || frame.routes.is_empty()
        || frame.routes.len() > MAX_AGENT_ROUTES
        || frame
            .routes
            .iter()
            .any(|route| !valid_agent_wire_syntax(&route.agent_type))
        || render_internal_agent_routes_body(&frame) != candidate
    {
        return None;
    }
    Some(frame)
}
