"use client"

// [crewchat] Resolve a member role id — as carried on a delegation card's meta
// (`member_role_id`, Phase 4.1.1) — to the roster's display name.
//
// The delegation card needs the NAME a reader recognizes ("后端工程师"), while
// the backend stamps the stable identity (the role id) onto the card. Resolving
// late keeps a rename or a deletion honest: the roster is read once per process
// (`listAgentRolesCached`) and a role that no longer exists degrades to `null`,
// which the card renders as the agent label.

import { useEffect, useState } from "react"

import { listAgentRolesCached, type AgentRole } from "@/lib/agent-roles"

export function useMemberRoleName(roleId: string | null): string | null {
  const [roles, setRoles] = useState<AgentRole[] | null>(null)

  useEffect(() => {
    if (!roleId) return
    let cancelled = false
    listAgentRolesCached()
      .then((list) => {
        if (!cancelled) setRoles(list)
      })
      .catch(() => {
        // An unreadable roster is not fatal for a card: it falls back to the
        // agent label rather than showing a placeholder.
        if (!cancelled) setRoles([])
      })
    return () => {
      cancelled = true
    }
  }, [roleId])

  if (!roleId) return null
  const name = roles?.find((role) => role.id === roleId)?.name.trim()
  return name ? name : null
}
