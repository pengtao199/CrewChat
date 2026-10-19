// [crewchat] Agent role roster — the core "Agents" concept of this fork.
//
// A role binds the global capabilities (CLI body, model, local skills, local
// MCP servers) to the two documents that define it: `Soul.md` (personality)
// and `Agents.md` (rules). IDs are server-generated UUIDs; the UI never shows
// them. Storage lives in `commands/agent_roles.rs`.

import { getTransport } from "./transport"
import type { AgentType } from "./types"

export interface AgentRole {
  /** Server-generated UUID. Empty until the role is first saved. */
  id: string
  name: string
  /** `Soul.md` — personality document. */
  soul: string
  /** `Agents.md` — rules document. */
  rules: string
  /** Body: the CLI agent type, e.g. `claude_code`. */
  agent_type: string
  model: string
  /** CCR provider the model belongs to (the picker's left-hand column). */
  model_provider: string
  skills: string[]
  mcp: string[]
  /**
   * [crewchat] The session-mode id this role's sessions START in — a CLI
   * approval/sandbox preset (e.g. codex's `workspace-write`). Empty means
   * "leave the CLI / CCR default alone", which is what every role did before
   * Phase 4.6.
   */
  permission_mode: string
  /**
   * [crewchat] Answer this role's permission requests automatically instead of
   * interrupting the chat. Configured here (Agents page) so the conversation
   * never has to ask; OFF unless the user turns it on.
   */
  auto_approve: boolean
  created_at: string | null
  updated_at: string | null
}

export function listAgentRoles(): Promise<AgentRole[]> {
  return getTransport().call("agent_roles_list")
}

/**
 * [crewchat] One `(provider, model)` pair an engine can be pinned to, as the
 * roster writes it.
 */
export interface RoleModelOption {
  provider: string
  provider_id: string
  model: string
}

/**
 * [crewchat] What an engine can actually be pinned to.
 *
 * `supported: false` means no model choice can be honoured for that engine yet
 * (the launcher has no overlay for it), so the page must say so rather than
 * offer a picker. `models: []` with `supported: true` means "could not
 * determine" (CCR unreachable) — never "nothing is available".
 */
export interface BodyModelFace {
  supported: boolean
  reason: string | null
  models: RoleModelOption[]
}

/**
 * [crewchat] Ask the backend which models this engine's gateway face offers.
 * The backend is the only place that knows (it reads CCR and decodes the
 * claude-visible ids), so the page never has to guess.
 */
export function agentRoleModelFace(
  agentType: AgentType
): Promise<BodyModelFace> {
  return getTransport().call("agent_role_model_face", { agentType })
}

/**
 * Roster shared by the transcript-side label lookups (delegation cards resolve a
 * member role id to its name). The roster is small and slow-changing, and a card
 * must not refetch it once per delegation, so one promise is memoized for the
 * process; a failed load is NOT cached (a transient error would otherwise pin
 * every card to the fallback label for the rest of the session).
 *
 * `saveAgentRole` / `deleteAgentRole` drop it so an edit is visible immediately.
 */
let cachedRoles: Promise<AgentRole[]> | null = null

export function listAgentRolesCached(): Promise<AgentRole[]> {
  if (!cachedRoles) {
    cachedRoles = listAgentRoles().catch((err: unknown) => {
      cachedRoles = null
      throw err
    })
  }
  return cachedRoles
}

/** Test seam: drop the module cache between cases. */
export function resetAgentRolesCache(): void {
  cachedRoles = null
}

export async function saveAgentRole(role: AgentRole): Promise<AgentRole> {
  const saved = await getTransport().call<AgentRole>("agent_roles_save", {
    role,
  })
  cachedRoles = null
  return saved
}

export async function deleteAgentRole(id: string): Promise<boolean> {
  const deleted = await getTransport().call<boolean>("agent_roles_delete", {
    id,
  })
  cachedRoles = null
  return deleted
}
