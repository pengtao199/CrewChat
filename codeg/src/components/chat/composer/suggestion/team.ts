// [crewchat] Team members in the `@` panel (Phase 4.2).
//
// The conversation's team lives in the `team` column (`leader_role_id` +
// `member_role_ids`) and resolves to roster roles. Members are the only
// delegation targets the backend's team gate accepts, so the panel surfaces
// them as their own pinned group — listed by their roster NAME, which is what
// the delegation card and the team frame call them (Phase 4.1.1).

import type { SuggestionItem } from "./types"

/** One roster member of the conversation's team, resolved for the panel. */
export interface TeamMentionMember {
  /** Roster role id (`AgentRole.id`). */
  roleId: string
  /** Roster display name — the member's identity everywhere else in the UI. */
  name: string
  /** The member's CLI body; also the delegation target the backend gates on. */
  agentType: string
}

/**
 * Team member → agent reference. Deliberately the SAME wire shape as
 * `agentToSuggestion` (`codeg://agent/<agent_type>`): a member mention has to
 * serialize and route like any agent mention, because that link is the routing
 * anchor the backend derives the delegation reminder from. Only the label
 * differs — the member's roster name instead of the CLI's name.
 */
export function teamMemberToSuggestion(
  member: TeamMentionMember
): SuggestionItem {
  return {
    reference: {
      refType: "agent",
      id: member.agentType,
      label: member.name || member.agentType,
      uri: `codeg://agent/${member.agentType}`,
      meta: { agentType: member.agentType },
    },
    detail: null,
    keywords: member.agentType,
  }
}
