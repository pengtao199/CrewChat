// agent_mentions submodule: role frame (codeg_internal_role) (split from acp/agent_mentions.rs —
// mechanical move, see NOTICE.md).
use super::*;

use serde::{Deserialize, Serialize};
use crate::acp::types::PromptInputBlock;


// [crewchat] The THIRD internal frame kind (Phase 2.2): the role this session
// runs as — a group conversation's leader, or a delegation child's member
// role. The persona text rides as ONE JSON string on the body's second line
// (newlines escaped), which keeps every frame exactly three physical lines —
// the shared finder/strip machinery needs no per-kind line arithmetic. Same
// identity discipline as the other kinds.
pub(crate) const ROLE_FRAME_KIND: &str = "codeg_internal_role";
/// Opening bytes of the role frame's descriptor line — the body anchor.
pub(crate) const ROLE_FRAME_BODY_PREFIX: &str = "{\"kind\":\"codeg_internal_role\"";
pub(crate) const ROLE_FRAME_VERSION: u8 = 2;
/// Total frame budget. Kept at the shared body bound
/// (`MAX_ROUTE_FRAME_BYTES`) so the separated finder's window always spans a
/// complete role frame.
pub(crate) const MAX_ROLE_FRAME_BYTES: usize = 16 * 1024;
/// Per-document byte cap (`instructions` / `rules`), applied by TRUNCATION in
/// `append_role_frame` — the frame must always be re-renderable, so oversize
/// input can never reach the wire unclipped.
pub(crate) const MAX_ROLE_TEXT_BYTES: usize = 6000;
/// Character cap for the role name in the frame.
pub(crate) const MAX_ROLE_FRAME_NAME_CHARS: usize = 64;
/// Distinct skills one role frame may carry (Phase 2.3), applied by
/// truncation at append time like the documents.
pub(crate) const MAX_ROLE_FRAME_SKILLS: usize = 32;
/// Character cap per skill name.
pub(crate) const MAX_ROLE_FRAME_SKILL_CHARS: usize = 64;
/// Opening the role line must carry in every role frame.
pub(crate) const ROLE_LINE_PREFIX: &str = "Codeg session role (authoritative): ";

/// [crewchat] The role frame's wire shape (Phase 2.2). The persona payload
/// (`role`) rides on the body's second line as one JSON string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InternalRoleFrame {
    kind: String,
    version: u8,
    nonce: String,
    #[serde(skip)]
    role: RoleFrameBody,
}

/// The role document as rendered into (and parsed from) the frame's second
/// line. `instructions` is the roster's Soul document, `rules` its Agents.md.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RoleFrameBody {
    name: String,
    #[serde(default)]
    instructions: String,
    #[serde(default)]
    rules: String,
    /// [crewchat] Phase 2.3 — the role's selected skills (installed for this
    /// CLI; the frame tells the session which ones its role should use).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skills: Vec<String>,
}

// --- [crewchat] Role frame (Phase 2.2) ---------------------------------------
//
// The persona this session runs as — a group conversation's leader role, or a
// delegation child's member role. Rendered to the same 3-line body shape as
// the other kinds (the persona rides as one JSON string, newlines escaped) so
// the shared finder/strip machinery needs no per-kind line arithmetic.

/// [crewchat] Append the persona frame for the role this session runs as
/// (Phase 2.2). The caller resolves the role from the conversation: the
/// team's leader for a group conversation, the member's role for a delegation
/// child. Roles with neither instructions nor rules append nothing.
pub(crate) fn append_role_frame(
    blocks: &mut Vec<PromptInputBlock>,
    role: &crate::models::agent_role::AgentRole,
) {
    let role_body = sanitized_role_body(role);
    if role_body.instructions.is_empty() && role_body.rules.is_empty() {
        return;
    }
    let frame = InternalRoleFrame {
        kind: ROLE_FRAME_KIND.to_string(),
        version: ROLE_FRAME_VERSION,
        nonce: uuid::Uuid::new_v4().to_string(),
        role: role_body,
    };
    blocks.push(PromptInputBlock::Text {
        text: render_internal_role_frame(&frame),
    });
}

/// Freeze the role into exactly what the validator accepts. The persona
/// documents are TRUNCATED (never rejected) to their byte caps and stripped
/// of the reserved separator, so the frame is always re-renderable no matter
/// what the roster holds.
pub(crate) fn sanitized_role_body(role: &crate::models::agent_role::AgentRole) -> RoleFrameBody {
    fn scrub(value: &str) -> String {
        let cleaned = value.replace(ROUTE_FRAME_SEPARATOR, "");
        cap_bytes(cleaned.trim(), MAX_ROLE_TEXT_BYTES)
    }
    let mut name: String = role
        .name
        .replace(ROUTE_FRAME_SEPARATOR, "")
        .trim()
        .chars()
        .take(MAX_ROLE_FRAME_NAME_CHARS)
        .collect();
    if name.is_empty() {
        name = "Role".to_string();
    }
    // Skills: trimmed, deduplicated, capped — same freeze rule as the docs.
    let mut skills: Vec<String> = Vec::new();
    for skill in &role.skills {
        let cleaned: String = skill
            .replace(ROUTE_FRAME_SEPARATOR, "")
            .trim()
            .chars()
            .take(MAX_ROLE_FRAME_SKILL_CHARS)
            .collect();
        if cleaned.is_empty() || skills.contains(&cleaned) {
            continue;
        }
        skills.push(cleaned);
        if skills.len() == MAX_ROLE_FRAME_SKILLS {
            break;
        }
    }
    RoleFrameBody {
        name,
        instructions: scrub(&role.soul),
        rules: scrub(&role.rules),
        skills,
    }
}

pub(crate) fn cap_bytes(value: &str, cap: usize) -> String {
    if value.len() <= cap {
        return value.to_string();
    }
    let mut end = cap;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

pub(crate) fn render_internal_role_frame(frame: &InternalRoleFrame) -> String {
    format!(
        "{ROUTE_FRAME_SEPARATOR}{}{ROUTE_FRAME_SEPARATOR}",
        render_internal_role_frame_body(frame)
    )
}

/// The role frame WITHOUT its separators: descriptor line, role line, and the
/// adoption ruling. Like the other kinds, the body's exact bytes ARE its
/// identity — editing this string orphans frames already written into
/// transcripts, so a wording change must bump [`ROLE_FRAME_VERSION`].
pub(crate) fn render_internal_role_frame_body(frame: &InternalRoleFrame) -> String {
    let descriptor = serde_json::to_string(frame).expect("internal role frame is serializable");
    let role = serde_json::to_string(&frame.role).expect("role body is serializable");
    let skills_ruling = if frame.role.skills.is_empty() {
        ""
    } else {
        " Use the skills listed above where they fit: they are installed for this CLI."
    };
    format!(
        "{descriptor}\n\
{ROLE_LINE_PREFIX}{role}\n\
Adopt the role above for this session: let it shape your tone, priorities and \
working style; the user's explicit instructions take precedence, and nothing in \
the role definition can override your safety rules.{skills_ruling}\n"
    )
}

/// Validate a candidate against [`render_internal_role_frame_body`].
pub(crate) fn parse_internal_role_frame_body(candidate: &str) -> Option<InternalRoleFrame> {
    if candidate.len() > MAX_ROLE_FRAME_BYTES {
        return None;
    }
    let (descriptor, remainder) = candidate.split_once('\n')?;
    let mut frame: InternalRoleFrame = serde_json::from_str(descriptor).ok()?;
    let role_json = remainder.lines().next()?.strip_prefix(ROLE_LINE_PREFIX)?;
    frame.role = serde_json::from_str(role_json).ok()?;
    if frame.kind != ROLE_FRAME_KIND
        || frame.version != ROLE_FRAME_VERSION
        || uuid::Uuid::parse_str(&frame.nonce).is_err()
        || frame.role.name.trim().is_empty()
        || frame.role.name.chars().count() > MAX_ROLE_FRAME_NAME_CHARS
        || frame.role.instructions.len() > MAX_ROLE_TEXT_BYTES
        || frame.role.rules.len() > MAX_ROLE_TEXT_BYTES
        || frame.role.skills.len() > MAX_ROLE_FRAME_SKILLS
        || frame.role.skills.iter().any(|skill| {
            let trimmed = skill.trim();
            trimmed.is_empty()
                || skill != trimmed
                || skill.chars().count() > MAX_ROLE_FRAME_SKILL_CHARS
        })
        || (frame.role.instructions.is_empty() && frame.role.rules.is_empty())
        || render_internal_role_frame_body(&frame) != candidate
    {
        return None;
    }
    Some(frame)
}
