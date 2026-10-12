"use client"

// [crewchat] The new-session team picker (Phase 1.2): choose the leader and
// the members a group conversation runs with. The roster is the global Agents
// list (`.data/codeg/agent-roles.json`); leaving it empty keeps the
// conversation a plain single-agent session. The team lands on the
// conversation row as one JSON snapshot when the first message creates it
// (see `createConversation`).

import { useEffect, useMemo, useState } from "react"
import { Loader2, Users } from "lucide-react"
import { useTranslations } from "next-intl"

import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover"
import { listAgentRoles, type AgentRole } from "@/lib/agent-roles"
import type { ConversationTeam } from "@/lib/types"
import { cn } from "@/lib/utils"

interface TeamSelectorProps {
  value: ConversationTeam | null
  onChange: (next: ConversationTeam | null) => void
  className?: string
}

export function TeamSelector({
  value,
  onChange,
  className,
}: TeamSelectorProps) {
  const t = useTranslations("TeamSelector")
  const [open, setOpen] = useState(false)
  const [roles, setRoles] = useState<AgentRole[] | null>(null)

  // Load the roster when the picker is first opened, so the draft's first
  // paint stays free of a roster fetch. A failed load degrades to "empty
  // roster" — the picker explains and the session stays single-agent.
  useEffect(() => {
    if (!open || roles !== null) return
    let cancelled = false
    listAgentRoles()
      .then((list) => {
        if (!cancelled) setRoles(list)
      })
      .catch(() => {
        if (!cancelled) setRoles([])
      })
    return () => {
      cancelled = true
    }
  }, [open, roles])

  const selectedLeaderId = value?.leader_role_id ?? null
  const memberCount = value ? 1 + value.member_role_ids.length : 0

  // The default leader: the roster's manager role when one exists (its name
  // follows the built-in `crewchat-manager` convention), else the head of the
  // list. Only a hint — nothing is picked until the user clicks.
  const defaultLeader = useMemo(() => {
    if (!roles || roles.length === 0) return null
    return roles.find((role) => /manager|管理员/i.test(role.name)) ?? roles[0]
  }, [roles])

  const pickLeader = (roleId: string) => {
    onChange({
      leader_role_id: roleId,
      member_role_ids: (value?.member_role_ids ?? []).filter(
        (id) => id !== roleId
      ),
    })
  }

  const toggleMember = (roleId: string) => {
    if (!value) return
    const current = value.member_role_ids
    const next = current.includes(roleId)
      ? current.filter((id) => id !== roleId)
      : [...current, roleId]
    onChange({ ...value, member_role_ids: next })
  }

  const rowClass =
    "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs transition-colors hover:bg-muted/60"

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className={cn("h-8 gap-1.5 text-xs", className)}
          aria-label={t("aria")}
        >
          <Users className="h-3.5 w-3.5" />
          {value ? t("buttonSet", { count: memberCount }) : t("buttonEmpty")}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-72 p-0">
        {roles === null ? (
          <div className="flex items-center gap-2 p-3 text-xs text-muted-foreground">
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            {t("loading")}
          </div>
        ) : roles.length === 0 ? (
          <p className="p-3 text-xs text-muted-foreground">{t("empty")}</p>
        ) : (
          <div className="max-h-80 space-y-3 overflow-y-auto p-3">
            <div>
              <div className="pb-1 text-2xs font-medium text-muted-foreground">
                {t("leaderLabel")}
              </div>
              {roles.map((role) => {
                const isLeader = selectedLeaderId === role.id
                const isDefault = !isLeader && defaultLeader?.id === role.id
                return (
                  <button
                    key={role.id}
                    type="button"
                    className={cn(rowClass, isLeader && "bg-muted")}
                    onClick={() => pickLeader(role.id)}
                  >
                    <span
                      className={cn(
                        "flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-full border",
                        isLeader
                          ? "border-primary"
                          : "border-muted-foreground/40"
                      )}
                    >
                      {isLeader && (
                        <span className="h-2 w-2 rounded-full bg-primary" />
                      )}
                    </span>
                    <span className="min-w-0 flex-1 truncate">{role.name}</span>
                    {isDefault && (
                      <span className="shrink-0 text-2xs text-muted-foreground">
                        {t("defaultBadge")}
                      </span>
                    )}
                  </button>
                )
              })}
            </div>

            {value && (
              <div>
                <div className="pb-1 text-2xs font-medium text-muted-foreground">
                  {t("membersLabel")}
                </div>
                {roles
                  .filter((role) => role.id !== value.leader_role_id)
                  .map((role) => (
                    <label
                      key={role.id}
                      className={cn(rowClass, "cursor-pointer")}
                    >
                      <Checkbox
                        checked={value.member_role_ids.includes(role.id)}
                        onCheckedChange={() => toggleMember(role.id)}
                        aria-label={t("memberToggleAria", { name: role.name })}
                      />
                      <span className="min-w-0 flex-1 truncate">
                        {role.name}
                      </span>
                    </label>
                  ))}
              </div>
            )}

            {value && (
              <button
                type="button"
                className="w-full rounded-md px-2 py-1 text-left text-2xs text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
                onClick={() => onChange(null)}
              >
                {t("clear")}
              </button>
            )}
          </div>
        )}
      </PopoverContent>
    </Popover>
  )
}
