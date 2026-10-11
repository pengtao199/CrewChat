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
  skills: string[]
  mcp: string[]
  created_at: string | null
  updated_at: string | null
}

export function listAgentRoles(): Promise<AgentRole[]> {
  return getTransport().call("agent_roles_list")
}

export function saveAgentRole(role: AgentRole): Promise<AgentRole> {
  return getTransport().call("agent_roles_save", { role })
}

export function deleteAgentRole(id: string): Promise<boolean> {
  return getTransport().call("agent_roles_delete", { id })
}
