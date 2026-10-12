// agent_mentions submodule: team frame (codeg_internal_team) (split from acp/agent_mentions.rs —
// mechanical move, see NOTICE.md).
use super::*;

use serde::{Deserialize, Serialize};
use crate::acp::types::PromptInputBlock;


// [crewchat] The SECOND internal frame kind (Phase 1.3): the delegable
// members of a group conversation. Same identity discipline as the routing
// frame — a byte-exact body between reserved separators — so the one
// strip/parse pass covers both; the two kinds are told apart by the body's
// descriptor prefix. Appended when the conversation has a team, whether or
// not the prompt routed any agent links.
pub(crate) const TEAM_FRAME_KIND: &str = "codeg_internal_team";
/// Opening bytes of the team frame's descriptor line — the body anchor.
pub(crate) const TEAM_FRAME_BODY_PREFIX: &str = "{\"kind\":\"codeg_internal_team\"";
pub(crate) const TEAM_FRAME_VERSION: u8 = 1;
pub(crate) const MAX_TEAM_FRAME_BYTES: usize = 8 * 1024;
/// Character cap for a member name rendered into the frame (roster names are
/// short by construction; the cap keeps a pathological roster from crowding
/// the frame budget).
pub(crate) const MAX_TEAM_FRAME_NAME_CHARS: usize = 64;
/// Character cap for the duty line. One over the resolver's cap
/// (`team::MAX_DUTY_CHARS`) so the resolver's truncation ellipsis fits.
pub(crate) const MAX_TEAM_FRAME_DUTY_CHARS: usize = crate::acp::delegation::team::MAX_DUTY_CHARS + 1;
/// Opening the members line must carry in every team frame.
pub(crate) const TEAM_MEMBERS_LINE_PREFIX: &str = "Codeg session team (authoritative): ";

/// [crewchat] The team frame's wire shape (Phase 1.3). `members` is skipped
/// in the descriptor exactly like `routes` is — it rides on its own line so
/// the descriptor stays a two-field identity blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InternalTeamFrame {
    kind: String,
    version: u8,
    nonce: String,
    #[serde(skip)]
    members: Vec<crate::acp::delegation::team::TeamMember>,
}

// --- [crewchat] Team frame (Phase 1.3) ---------------------------------------
//
// Rendered to the same 3-line body shape as the routing frame, so the
// unseparated-finder's newline count and the separator machinery cover both
// kinds without branching. Line 2 names the delegable members; line 3 is the
// delegation ruling (the routing frame binds the CHANNEL when agent links are
// present, this one binds the TARGETS when the conversation runs with a
// team).

/// [crewchat] Append the session-team frame for a group conversation's leader
/// (Phase 1.3): who is present, and which `agent_type` reaches each member.
///
/// The caller gates this on the connection's delegation verdict (the routing
/// frame's own gate), because a member list is only actionable when
/// `delegate_to_agent` was exposed. A plain session resolves to no members,
/// and an empty list appends nothing at all — the frame is derived server-side
/// from the conversation's team snapshot, never from user text.
pub(crate) fn append_team_frame(
    blocks: &mut Vec<PromptInputBlock>,
    members: &[crate::acp::delegation::team::TeamMember],
) {
    let members = sanitized_team_members(members);
    if members.is_empty() {
        return;
    }
    let frame = InternalTeamFrame {
        kind: TEAM_FRAME_KIND.to_string(),
        version: TEAM_FRAME_VERSION,
        nonce: uuid::Uuid::new_v4().to_string(),
        members,
    };
    blocks.push(PromptInputBlock::Text {
        text: render_internal_team_frame(&frame),
    });
}

/// Freeze a resolved member list into exactly what the validator accepts:
/// valid wire slugs, non-empty capped names, capped duties, member cap. The
/// frame is byte-exact re-rendered at parse time, so anything the renderer
/// would emit must survive this pass unchanged; sanitizing HERE (not at the
/// resolver) keeps `team.rs` free of frame-shape rules.
pub(crate) fn sanitized_team_members(
    members: &[crate::acp::delegation::team::TeamMember],
) -> Vec<crate::acp::delegation::team::TeamMember> {
    fn cap_chars(value: &str, cap: usize) -> String {
        value.chars().take(cap).collect()
    }
    members
        .iter()
        .filter(|member| valid_agent_wire_syntax(&member.agent_type))
        .filter_map(|member| {
            let name = cap_chars(member.name.trim(), MAX_TEAM_FRAME_NAME_CHARS);
            if name.is_empty() {
                return None;
            }
            let duty = member
                .duty
                .as_deref()
                .map(str::trim)
                .filter(|duty| !duty.is_empty())
                .map(|duty| cap_chars(duty, MAX_TEAM_FRAME_DUTY_CHARS));
            Some(crate::acp::delegation::team::TeamMember {
                name,
                agent_type: member.agent_type.clone(),
                duty,
                role_id: member.role_id.clone(),
                mcp: Vec::new(),
            })
        })
        .take(crate::acp::delegation::team::MAX_TEAM_MEMBERS)
        .collect()
}

pub(crate) fn render_internal_team_frame(frame: &InternalTeamFrame) -> String {
    format!(
        "{ROUTE_FRAME_SEPARATOR}{}{ROUTE_FRAME_SEPARATOR}",
        render_internal_team_frame_body(frame)
    )
}

/// The team frame WITHOUT its separators: descriptor line, members line, and
/// the delegation ruling — three newline-terminated lines, same as the
/// routing frame (the unseparated finder counts newlines, not kinds).
///
/// Like the routing frame, the body's exact bytes ARE its identity: editing
/// this string orphans frames already written into transcripts, so a wording
/// change must bump [`TEAM_FRAME_VERSION`].
pub(crate) fn render_internal_team_frame_body(frame: &InternalTeamFrame) -> String {
    let descriptor = serde_json::to_string(frame).expect("internal team frame is serializable");
    let members = serde_json::to_string(&frame.members).expect("team members are serializable");
    format!(
        "{descriptor}\n\
{TEAM_MEMBERS_LINE_PREFIX}{members}\n\
Only the members above are delegable in this session: route their work through \
the `delegate_to_agent` tool from the `codeg-mcp` server with the member's \
`agentType` as `agent_type`, and do not substitute your own native sub-agent, \
task, or spawn mechanism.\n"
    )
}

/// Validate a candidate against [`render_internal_team_frame_body`].
pub(crate) fn parse_internal_team_frame_body(candidate: &str) -> Option<InternalTeamFrame> {
    if candidate.len() > MAX_TEAM_FRAME_BYTES {
        return None;
    }
    let (descriptor, remainder) = candidate.split_once('\n')?;
    let mut frame: InternalTeamFrame = serde_json::from_str(descriptor).ok()?;
    let members_json = remainder
        .lines()
        .next()?
        .strip_prefix(TEAM_MEMBERS_LINE_PREFIX)?;
    frame.members = serde_json::from_str(members_json).ok()?;
    if frame.kind != TEAM_FRAME_KIND
        || frame.version != TEAM_FRAME_VERSION
        || uuid::Uuid::parse_str(&frame.nonce).is_err()
        || frame.members.is_empty()
        || frame.members.len() > crate::acp::delegation::team::MAX_TEAM_MEMBERS
        || frame.members.iter().any(|member| {
            member.name.trim().is_empty()
                || member.name.chars().count() > MAX_TEAM_FRAME_NAME_CHARS
                || member
                    .duty
                    .as_deref()
                    .is_some_and(|duty| duty.chars().count() > MAX_TEAM_FRAME_DUTY_CHARS)
                || !valid_agent_wire_syntax(&member.agent_type)
        })
        || render_internal_team_frame_body(&frame) != candidate
    {
        return None;
    }
    Some(frame)
}
