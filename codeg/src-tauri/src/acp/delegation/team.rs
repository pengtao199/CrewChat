//! [crewchat] Conversation team resolution (Phase 1.3).
//!
//! A group conversation carries a [`ConversationTeam`] snapshot of roster
//! role ids (Phase 1.1). Two features read the same resolved view of it:
//!
//! * the broker's `delegate_to_agent` **scope gate** — only the team's
//!   members are delegable, and an out-of-team `agent_type` is refused with
//!   the member list ([`scope_refusal`]);
//! * the leader's **prompt frame** — the agent is told which members are
//!   present and which `agent_type` reaches each one
//!   (`crate::acp::agent_mentions::append_team_frame`).
//!
//! Resolution reads the roster (`db::service::agent_role_service`, the
//! custom-agent registry's `kind = 'role'` rows since Phase 2.1) — the single
//! store the settings UI writes, so role edits show up without a restart.
//! Read failures and unknown role ids degrade to "no members" (unconstrained)
//! with a warning, never to a broken delegation path.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::models::conversation::ConversationTeam;
use crate::models::AgentType;
use crate::models::agent_role::AgentRole;

use super::types::DelegationError;

/// Distinct members one team frame may carry. Kept in lockstep with the
/// frame's own validator (`agent_mentions`): a longer list is truncated at
/// render time.
pub const MAX_TEAM_MEMBERS: usize = 16;

/// Character cap for the duty line rendered into frames and refusals.
pub const MAX_DUTY_CHARS: usize = 60;

/// One delegable member, as the leader sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamMember {
    /// Roster role display name (e.g. 前端工程师).
    pub name: String,
    /// Wire slug of the member's CLI body (e.g. `open_code`) — the value the
    /// leader passes as `delegate_to_agent`'s `agent_type`.
    pub agent_type: String,
    /// Short duty line derived from the roster role (first non-empty line of
    /// its `soul`, capped). Absent when the role has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duty: Option<String>,
    /// [crewchat] The roster role id this member resolves from (Phase 2.2).
    /// NOT part of the wire shape: skipped both ways so the team frame's
    /// byte-exact identity is untouched. The broker stamps it onto the child
    /// conversation so the member's own session can resolve its persona.
    #[serde(skip)]
    pub role_id: String,
    /// [crewchat] The role's MCP selection (Phase 2.3), also off the wire for
    /// the same reason. The broker hands it to the child session's
    /// `session/new` so a member mounts exactly its role's servers.
    #[serde(skip)]
    pub mcp: Vec<String>,
}

/// Resolves the delegable members of a conversation's team.
///
/// `Ok(empty)` covers both "plain single-agent conversation" and "lookup
/// failed, degrade unconstrained" — the two cases behave identically for both
/// consumers (no gate, no frame), so the trait stays infallible by design.
#[async_trait]
pub trait TeamLookup: Send + Sync {
    async fn members_of(&self, conversation_id: i32) -> Vec<TeamMember>;

    /// [crewchat] The role this conversation RUNS AS (Phase 2.2) — the team's
    /// leader role for a group conversation, or a delegation child's member
    /// role (the broker names it in `leader_role_id` with no members; see
    /// `ConversationTeam`). `None` for plain sessions, unresolvable ids, and
    /// every lookup failure (logged, degrade to "no persona").
    async fn conversation_role(&self, conversation_id: i32) -> Option<AgentRole>;
}

/// Default lookup for tests and delegation-disabled paths: every conversation
/// resolves to "no team", i.e. no scope constraint and no team frame.
pub struct NoopTeamLookup;

#[async_trait]
impl TeamLookup for NoopTeamLookup {
    async fn members_of(&self, _conversation_id: i32) -> Vec<TeamMember> {
        Vec::new()
    }

    async fn conversation_role(&self, _conversation_id: i32) -> Option<AgentRole> {
        None
    }
}

/// DB + roster backed lookup (production wiring).
pub struct DbTeamLookup {
    pub db: Arc<crate::db::AppDatabase>,
}

#[async_trait]
impl TeamLookup for DbTeamLookup {
    async fn members_of(&self, conversation_id: i32) -> Vec<TeamMember> {
        match self.resolve(conversation_id).await {
            Ok(members) => members,
            Err(error) => {
                tracing::warn!(
                    "[crewchat][team] resolving conversation {conversation_id}'s team failed \
                     ({error}); delegation stays unrestricted this call"
                );
                Vec::new()
            }
        }
    }

    async fn conversation_role(&self, conversation_id: i32) -> Option<AgentRole> {
        match self.conversation_role_inner(conversation_id).await {
            Ok(role) => role,
            Err(error) => {
                tracing::warn!(
                    "[crewchat][role] resolving conversation {conversation_id}'s role failed \
                     ({error}); the session runs without a persona frame"
                );
                None
            }
        }
    }
}

impl DbTeamLookup {
    async fn conversation_role_inner(&self, conversation_id: i32) -> Result<Option<AgentRole>, String> {
        let Some(team) = self.team_of(conversation_id).await? else {
            return Ok(None);
        };
        crate::db::service::agent_role_service::get_role(&self.db.conn, &team.leader_role_id)
            .await
            .map_err(|error| format!("read role: {error}"))
    }
}

impl DbTeamLookup {
    /// The conversation row's `team` snapshot, parsed. `Ok(None)` = no team.
    async fn team_of(&self, conversation_id: i32) -> Result<Option<ConversationTeam>, String> {
        use sea_orm::EntityTrait;
        let row = crate::db::entities::conversation::Entity::find_by_id(conversation_id)
            .one(&self.db.conn)
            .await
            .map_err(|error| error.to_string())?;
        let Some(raw) = row.and_then(|row| row.team) else {
            return Ok(None);
        };
        let team: ConversationTeam =
            serde_json::from_str(&raw).map_err(|error| format!("parse team JSON: {error}"))?;
        Ok(Some(team))
    }

    async fn resolve(&self, conversation_id: i32) -> Result<Vec<TeamMember>, String> {
        let Some(team) = self.team_of(conversation_id).await? else {
            return Ok(Vec::new());
        };
        if team.member_role_ids.is_empty() {
            return Ok(Vec::new());
        }
        let roles = crate::db::service::agent_role_service::list_roles(&self.db.conn)
            .await
            .map_err(|error| format!("read roster: {error}"))?;
        let members = members_from_roles(&team, &roles);
        if members.is_empty() {
            tracing::warn!(
                "[crewchat][team] conversation {conversation_id} names {} member role(s) but \
                 none resolves against the roster; delegation stays unrestricted",
                team.member_role_ids.len()
            );
        }
        Ok(members)
    }
}

/// Pure team → member resolution, split out so it is testable without the DB
/// or the roster file. Unknown role ids are skipped; the roster order inside
/// `member_role_ids` is preserved (the frame's member order is therefore the
/// order the team was picked in).
pub fn members_from_roles(
    team: &ConversationTeam,
    roles: &[crate::models::agent_role::AgentRole],
) -> Vec<TeamMember> {
    team.member_role_ids
        .iter()
        .filter_map(|id| roles.iter().find(|role| &role.id == id))
        .filter(|role| !role.agent_type.trim().is_empty())
        .map(|role| TeamMember {
            name: role.name.trim().to_string(),
            agent_type: role.agent_type.trim().to_string(),
            duty: first_line_capped(&role.soul, MAX_DUTY_CHARS),
            role_id: role.id.clone(),
            mcp: role.mcp.clone(),
        })
        .collect()
}

/// The duty line for a roster role: its `soul`'s first non-empty line,
/// trimmed and capped. `None` when the soul has no content.
fn first_line_capped(text: &str, cap: usize) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut capped: String = line.chars().take(cap).collect();
    if line.chars().count() > cap {
        capped.push('…');
    }
    Some(capped)
}

/// The refusal for a delegation whose `agent_type` is not a team member.
///
/// `None` means "allowed": either the conversation has no resolvable team
/// (plain session, degraded lookup — see [`TeamLookup`]) or the requested
/// type is one of the members. The refusal message is what the parent LLM
/// reads as the tool result, so it names every delegable member as
/// `name (agent_type)` and tells the caller to retry with one of them.
pub fn scope_refusal(agent_type: AgentType, members: &[TeamMember]) -> Option<DelegationError> {
    if members.is_empty() {
        return None;
    }
    let requested = agent_type.as_wire();
    if members
        .iter()
        .any(|member| member.agent_type == requested)
    {
        return None;
    }
    let available = members
        .iter()
        .map(|member| format!("{} ({})", member.name, member.agent_type))
        .collect::<Vec<_>>()
        .join(", ");
    Some(DelegationError::NotInTeam {
        requested: requested.into_owned(),
        available,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::agent_role::AgentRole;

    fn role(id: &str, name: &str, agent_type: &str, soul: &str) -> AgentRole {
        AgentRole {
            id: id.to_string(),
            name: name.to_string(),
            soul: soul.to_string(),
            rules: String::new(),
            agent_type: agent_type.to_string(),
            model: String::new(),
            model_provider: String::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            created_at: None,
            updated_at: None,
        }
    }

    fn team(leader: &str, members: &[&str]) -> ConversationTeam {
        ConversationTeam {
            leader_role_id: leader.to_string(),
            member_role_ids: members.iter().map(|id| id.to_string()).collect(),
        }
    }

    #[test]
    fn members_resolve_in_team_order_and_skip_unknown_ids() {
        let roles = vec![
            role("front", "前端工程师", "codex", "你是前端工程师，专注于界面与交互实现。\n第二行不进入职责"),
            role("ops", "运维工程师", "claude_code", ""),
        ];
        let members = members_from_roles(&team("lead", &["ops", "ghost", "front"]), &roles);
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].name, "运维工程师");
        assert_eq!(members[0].agent_type, "claude_code");
        assert_eq!(members[0].duty, None, "empty soul must omit the duty");
        assert_eq!(members[1].name, "前端工程师");
        assert_eq!(
            members[1].duty.as_deref(),
            Some("你是前端工程师，专注于界面与交互实现。"),
            "duty is the soul's first non-empty line, not a later one"
        );
    }

    #[test]
    fn duty_is_capped_with_an_ellipsis() {
        let long = "长".repeat(MAX_DUTY_CHARS + 10);
        let roles = vec![role("a", "A", "codex", &long)];
        let members = members_from_roles(&team("lead", &["a"]), &roles);
        let duty = members[0].duty.as_deref().unwrap();
        assert_eq!(duty.chars().count(), MAX_DUTY_CHARS + 1);
        assert!(duty.ends_with('…'));
    }

    #[test]
    fn empty_agent_type_roles_are_not_members() {
        let roles = vec![role("a", "A", "  ", "soul")];
        assert!(members_from_roles(&team("lead", &["a"]), &roles).is_empty());
    }

    #[test]
    fn scope_refusal_allows_members_and_refuses_outsiders_with_the_member_list() {
        let members = vec![
            TeamMember {
                name: "后端开发工程师".into(),
                agent_type: "open_code".into(),
                duty: None,
                role_id: "r-backend".into(),
                mcp: Vec::new(),
            },
            TeamMember {
                name: "前端工程师".into(),
                agent_type: "codex".into(),
                duty: None,
                role_id: "r-frontend".into(),
                mcp: Vec::new(),
            },
            TeamMember {
                name: "运维工程师".into(),
                agent_type: "claude_code".into(),
                duty: None,
                role_id: "r-ops".into(),
                mcp: Vec::new(),
            },
        ];
        assert!(scope_refusal(AgentType::Codex, &members).is_none());
        assert!(scope_refusal(AgentType::OpenCode, &members).is_none());
        // A member whose body is the leader's own CLI is still delegable.
        assert!(scope_refusal(AgentType::ClaudeCode, &members).is_none());

        let error = scope_refusal(AgentType::Gemini, &members).expect("gemini is out of team");
        let DelegationError::NotInTeam { requested, available } = error else {
            panic!("expected NotInTeam, got {error:?}");
        };
        assert_eq!(requested, "gemini");
        assert_eq!(
            available,
            "后端开发工程师 (open_code), 前端工程师 (codex), 运维工程师 (claude_code)"
        );
    }

    #[test]
    fn scope_refusal_is_unconstrained_without_members() {
        assert!(scope_refusal(AgentType::Gemini, &[]).is_none());
    }

    #[test]
    fn not_in_team_maps_to_a_stable_wire_code() {
        let outcome = crate::acp::delegation::types::DelegationOutcome::from_err(
            DelegationError::NotInTeam {
                requested: "gemini".into(),
                available: "前端工程师 (codex)".into(),
            },
            None,
        );
        let crate::acp::delegation::types::DelegationOutcome::Err { code, message, .. } = outcome
        else {
            panic!("expected Err outcome");
        };
        assert_eq!(code, "not_in_team");
        assert!(message.contains("gemini"));
        assert!(message.contains("前端工程师 (codex)"));
    }
}
