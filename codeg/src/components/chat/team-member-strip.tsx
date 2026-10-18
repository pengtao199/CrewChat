"use client"

// [crewchat] Conversation-header team strip (Phase 1.4): one chip per roster
// member of this group conversation, carrying its live state — 排队 / 运行 /
// 等权限 / 完成 (see `lib/team-member-status.ts` for the resolution rules).
//
// Data comes from three places, all refresh-safe:
//   1. the conversation's team (DB row, mirrored in the workspace store);
//   2. the roster (`agent-roles.json`) for each member's name + CLI body;
//   3. the conversation's child rows (DB) joined with the live delegation
//      bindings — bindings for RUNNING tasks are re-seeded from the session
//      snapshot after a reload, so the strip rebuilds itself.
//
// "Awaiting permission" is the one live-only enrichment: the chip subscribes
// to its running child's connection snapshot and upgrades `running` to
// `waiting` while a permission ask is parked there.

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react"
import { AlertTriangle, Users } from "lucide-react"
import { useTranslations } from "next-intl"
import { useShallow } from "zustand/react/shallow"

import { useConnectionStore } from "@/contexts/acp-connections-context"
import { useDelegation } from "@/contexts/delegation-context"
import { useDelegationEnabled } from "@/hooks/use-delegation-enabled"
import { listAgentRoles, type AgentRole } from "@/lib/agent-roles"
import { listChildConversations } from "@/lib/api"
import {
  memberTaskPreview,
  resolveMemberPresence,
  runningChildConnectionIds,
  type MemberBindingSignal,
  type TeamMemberPresence,
} from "@/lib/team-member-status"
import type { ConversationStatus, ConversationTeam } from "@/lib/types"
import { cn } from "@/lib/utils"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"

/** The child row's status write trails `delegation_completed` by a moment —
 *  wait it out before re-reading the children table. */
const CHILDREN_REFRESH_DEBOUNCE_MS = 400

interface TeamMemberStripProps {
  /** Persisted DB id of the conversation whose header hosts the strip; null
   *  for an unsaved draft (nothing to look up — the strip hides). */
  conversationId: number | null
  className?: string
}

/**
 * Renders the member chips for a team conversation, or nothing at all for
 * plain sessions. The outer component resolves only the TEAM (a store lookup
 * that is safe anywhere); everything that needs the ACP providers is inside
 * the body, which mounts only when there is a team — so hosts that render the
 * header without those providers (and every single-agent conversation) never
 * touch them.
 */
export function TeamMemberStrip({
  conversationId,
  className,
}: TeamMemberStripProps) {
  const team = useAppWorkspaceStore(
    useShallow((state) => {
      if (conversationId == null) return null
      const row = state.conversations.find((c) => c.id === conversationId)
      return row?.team ?? null
    })
  )
  if (conversationId == null || !team || team.member_role_ids.length === 0) {
    return null
  }
  return (
    <TeamMemberStripBody
      conversationId={conversationId}
      team={team}
      className={className}
    />
  )
}

function TeamMemberStripBody({
  conversationId,
  team,
  className,
}: {
  conversationId: number
  team: ConversationTeam
  className?: string
}) {
  const t = useTranslations("TeamMemberStrip")
  const { listBindings } = useDelegation()
  // [crewchat] Members are only meaningful with delegation on (the team frame is
  // injected only for delegation-enabled connections); with it off the chips sit
  // on "待命" forever and the leader cannot see anyone. Say so here — this strip
  // is where the user looks to check on the team.
  const delegationEnabled = useDelegationEnabled()
  const [roles, setRoles] = useState<AgentRole[] | null>(null)
  const [children, setChildren] = useState<
    import("@/lib/types").DbConversationSummary[] | null
  >(null)

  useEffect(() => {
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
  }, [])

  const refreshChildren = useCallback(() => {
    let cancelled = false
    listChildConversations(conversationId)
      .then((rows) => {
        if (!cancelled) setChildren(rows)
      })
      .catch(() => {
        if (!cancelled) setChildren([])
      })
    return () => {
      cancelled = true
    }
  }, [conversationId])

  // Initial load, and a conversation change starts over.
  useEffect(() => refreshChildren(), [refreshChildren])

  const bindings = listBindings()
  // Stable across renders that didn't change the binding map: the strip
  // refetches its child rows when a delegation starts or resolves, but a
  // plain re-render must not loop through the debounce.
  const bindingsKey = bindings
    .map((binding) => `${binding.parentToolUseId}:${binding.status}`)
    .join("|")
  const refreshRef = useRef(refreshChildren)
  useEffect(() => {
    refreshRef.current = refreshChildren
  }, [refreshChildren])
  const firstBindingsRef = useRef(true)
  useEffect(() => {
    if (firstBindingsRef.current) {
      firstBindingsRef.current = false
      return
    }
    const timer = window.setTimeout(
      () => refreshRef.current(),
      CHILDREN_REFRESH_DEBOUNCE_MS
    )
    return () => window.clearTimeout(timer)
  }, [bindingsKey])

  const rolesById = useMemo(() => {
    const map = new Map<string, AgentRole>()
    for (const role of roles ?? []) map.set(role.id, role)
    return map
  }, [roles])

  const memberViews = useMemo(() => {
    const rows = children ?? []
    return team.member_role_ids
      .map((roleId) => rolesById.get(roleId))
      .filter((role): role is AgentRole => Boolean(role))
      .map((role) => {
        const memberChildren = rows
          .filter((row) => row.agent_type === role.agent_type)
          .sort((a, b) => b.id - a.id)
        const memberBindings: MemberBindingSignal[] = bindings
          .filter((binding) => binding.agentType === role.agent_type)
          .map((binding) => ({
            childConversationId: binding.childConversationId,
            childConnectionId: binding.childConnectionId,
            status: binding.status,
            errorCode: binding.errorCode,
            task: binding.task,
          }))
        return {
          role,
          signals: {
            // The backend only stores the four `ConversationStatus` values.
            children: memberChildren.map((row) => ({
              id: row.id,
              status: row.status as ConversationStatus,
            })),
            bindings: memberBindings,
          },
        }
      })
  }, [team, rolesById, children, bindings])

  if (memberViews.length === 0) return null

  return (
    <div
      className={cn(
        "flex h-8 shrink-0 items-center gap-2 border-b border-border/50 px-3",
        className
      )}
      role="status"
      aria-label={t("aria")}
      data-testid="team-member-strip"
    >
      <Users
        className="h-3.5 w-3.5 shrink-0 text-muted-foreground/60"
        aria-hidden
      />
      <div className="flex min-w-0 items-center gap-1.5 overflow-x-auto">
        {memberViews.map(({ role, signals }) => (
          <TeamMemberChip
            key={role.id}
            name={role.name}
            presence={resolveMemberPresence(signals)}
            task={memberTaskPreview(signals)}
            connectionIds={runningChildConnectionIds(signals)}
          />
        ))}
      </div>
      {/* [crewchat] Members without delegation are decoration: the leader gets no
          team frame and no delegation tool, so nothing here will ever run. That
          must be visible where the user checks on the team, with a way to fix it. */}
      {delegationEnabled === false && (
        <span
          className="flex shrink-0 items-center gap-1 pl-1 text-2xs text-amber-600 dark:text-amber-400"
          data-testid="team-delegation-off"
        >
          <AlertTriangle className="h-3 w-3 shrink-0" aria-hidden />
          <span className="truncate">{t("delegationOff")}</span>
          <button
            type="button"
            className="shrink-0 text-primary hover:underline"
            onClick={() => {
              if (typeof window === "undefined") return
              window.location.assign("/settings/collaboration")
            }}
          >
            {t("delegationOffAction")} →
          </button>
        </span>
      )}
    </div>
  )
}

const PRESENCE_DOT: Record<TeamMemberPresence, string> = {
  idle: "bg-muted-foreground/30",
  queued: "bg-sky-500",
  running: "bg-blue-500",
  waiting: "bg-amber-500",
  done: "bg-emerald-500",
  failed: "bg-red-500",
  cancelled: "bg-muted-foreground/40",
}

const PRESENCE_KEYS = {
  idle: "statusIdle",
  queued: "statusQueued",
  running: "statusRunning",
  waiting: "statusWaiting",
  done: "statusDone",
  failed: "statusFailed",
  cancelled: "statusCancelled",
} as const satisfies Record<TeamMemberPresence, string>

function TeamMemberChip({
  name,
  presence,
  task,
  connectionIds,
}: {
  name: string
  presence: TeamMemberPresence
  task: string | null
  connectionIds: string[]
}) {
  const t = useTranslations("TeamMemberStrip")
  const awaitingPermission = useAnyAwaitingPermission(connectionIds)
  const resolved: TeamMemberPresence =
    presence === "running" && awaitingPermission ? "waiting" : presence
  const title = [t(PRESENCE_KEYS[resolved]), task ?? undefined]
    .filter(Boolean)
    .join(" · ")

  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border/60 bg-muted/30 px-2 py-[1px] text-2xs",
        resolved === "waiting" && "border-amber-500/40 bg-amber-500/10"
      )}
      title={title}
      data-state={resolved}
    >
      <span
        className={cn(
          "h-1.5 w-1.5 shrink-0 rounded-full",
          PRESENCE_DOT[resolved],
          (resolved === "running" || resolved === "waiting") && "animate-pulse"
        )}
        aria-hidden
      />
      <span className="max-w-[9rem] truncate text-foreground/80">{name}</span>
      <span className="text-muted-foreground/70">
        {t(PRESENCE_KEYS[resolved])}
      </span>
    </span>
  )
}

/**
 * True while ANY of the given child connections has a permission ask parked.
 * One subscription for the whole set (a member may run several children), and
 * a boolean snapshot so `useSyncExternalStore` never sees a fresh object.
 */
function useAnyAwaitingPermission(connectionIds: string[]): boolean {
  const store = useConnectionStore()
  const key = connectionIds.join("|")
  const ids = useMemo(() => (key === "" ? [] : key.split("|")), [key])
  const subscribe = useCallback(
    (onStoreChange: () => void) => {
      const unsubscribes = ids.map((id) =>
        store.subscribeKey(id, onStoreChange)
      )
      return () => {
        for (const unsubscribe of unsubscribes) unsubscribe()
      }
    },
    [store, ids]
  )
  const getSnapshot = useCallback(
    () => ids.some((id) => store.getConnection(id)?.pendingPermission != null),
    [store, ids]
  )
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}
