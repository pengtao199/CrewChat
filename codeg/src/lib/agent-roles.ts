// [crewchat] Agent role roster — the core "Agents" concept of this fork.
//
// A role binds the global capabilities (CLI body, model, local skills, local
// MCP servers) to the two documents that define it: `Soul.md` (personality)
// and `Agents.md` (rules). IDs are server-generated UUIDs; the UI never shows
// them. Storage lives in `commands/agent_roles.rs`.

import { getTransport } from "./transport"

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
  created_at: string | null
  updated_at: string | null
}

export function listAgentRoles(): Promise<AgentRole[]> {
  return getTransport().call("agent_roles_list")
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
  const saved = await getTransport().call<AgentRole>("agent_roles_save", { role })
  cachedRoles = null
  return saved
}

export async function deleteAgentRole(id: string): Promise<boolean> {
  const deleted = await getTransport().call<boolean>("agent_roles_delete", { id })
  cachedRoles = null
  return deleted
}
