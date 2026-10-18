"use client"

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react"

import { useAcpAgents } from "@/hooks/use-acp-agents"
import { useFileTree, type FlatFileEntry } from "@/hooks/use-file-tree"
import { listAgentRolesCached, type AgentRole } from "@/lib/agent-roles"
import { gitLog, listAllConversations } from "@/lib/api"
import type {
  AcpAgentInfo,
  DbConversationSummary,
  GitLogEntry,
} from "@/lib/types"

import {
  agentToSuggestion,
  commitToSuggestion,
  fileToSuggestion,
  sessionToSuggestion,
} from "./suggestion/adapters"
import {
  teamMemberToSuggestion,
  type TeamMentionMember,
} from "./suggestion/team"
import type {
  ReferenceSearch,
  SuggestionGroup,
  SuggestionItem,
} from "./suggestion/types"

// Commit-synchronous on the client (so the guard-critical refs are updated
// during commit, before any later macrotask/microtask can resolve a stale
// in-flight fetch), but a no-op-safe passive effect during the static-export
// prerender where `useLayoutEffect` would warn.
const useIsomorphicLayoutEffect =
  typeof window !== "undefined" ? useLayoutEffect : useEffect

/** Max rows surfaced per group (mirrors the textarea `@` menu's file cap). */
const MAX_PER_GROUP = 50
/** How many commits the git-log group pulls (client-filtered down from here). */
const GIT_LOG_LIMIT = 100
const EMPTY_COMMITS: Promise<GitLogEntry[]> = Promise.resolve([])

/** Display headings for each group; injected so the host can localize them. */
export interface ReferenceGroupLabels {
  file: string
  agent: string
  session: string
  commit: string
  skill: string
  /** [crewchat] Heading of the pinned team-members group (Phase 4.2). */
  team: string
}

/**
 * [crewchat] Row hints the panel appends when the conversation has a team
 * (Phase 4.2). Only used then — a single-agent session's rows are untouched.
 */
export interface ReferenceHintLabels {
  /** Marker on a row that IS a delegation target of this team. */
  member: string
  /** Marker on an agent this team would reject a delegation to. */
  nonMember: string
}

/**
 * English fallbacks, matching the suggestion popup's `emptyLabel`/`loadingLabel`
 * convention (the host passes localized strings at the integration layer).
 */
export const DEFAULT_GROUP_LABELS: ReferenceGroupLabels = {
  file: "Files",
  agent: "Agents",
  session: "Sessions",
  commit: "Commits",
  skill: "Skills",
  team: "Team",
}

/** English fallbacks for {@link ReferenceHintLabels}. */
export const DEFAULT_HINT_LABELS: ReferenceHintLabels = {
  member: "In team",
  nonMember: "Not in this team",
}

/** Raw, already-loaded data the pure group builder turns into suggestions. */
export interface ReferenceSearchSources {
  files: FlatFileEntry[]
  /** Workspace root the `files` were loaded under; null disables the group. */
  workspaceRoot: string | null
  agents: AcpAgentInfo[]
  sessions: DbConversationSummary[]
  commits: GitLogEntry[]
  /** Repo identity for commit URIs; null disables the commit group. */
  repoKey: string | null
  /**
   * [crewchat] Roster members of the conversation's team, in the order the team
   * was configured. Empty (the default) for a single-agent session: the pinned
   * team group and the member hints only exist when there is a team.
   */
  teamMembers?: TeamMentionMember[]
}

/** Case-insensitive substring match against an adapted item's searchable text. */
function suggestionMatches(item: SuggestionItem, lowerQuery: string): boolean {
  if (!lowerQuery) return true
  const ref = item.reference
  return (
    ref.label.toLowerCase().includes(lowerQuery) ||
    ref.id.toLowerCase().includes(lowerQuery) ||
    (item.keywords ?? "").toLowerCase().includes(lowerQuery) ||
    (item.detail ?? "").toLowerCase().includes(lowerQuery)
  )
}

/**
 * Pure: filter + adapt the raw sources into the fixed-order grouped suggestions
 * the `@` panel renders (files → agents → sessions → commits), with the
 * conversation's team members pinned as a leading `team` group when there is a
 * team ([crewchat] Phase 4.2). Each group is independently capped at
 * {@link MAX_PER_GROUP}; empty groups are kept (the popup hides them) so the
 * order is always stable — EXCEPT the team group, which is omitted entirely for
 * a single-agent session so the panel is byte-identical to before. Extracted
 * from the hook so the matching/ordering/dedup logic is testable without React.
 */
export function buildReferenceGroups(
  query: string,
  sources: ReferenceSearchSources,
  labels: ReferenceGroupLabels = DEFAULT_GROUP_LABELS,
  hints: ReferenceHintLabels = DEFAULT_HINT_LABELS
): SuggestionGroup[] {
  const q = query.trim().toLowerCase()

  // [crewchat] Team members first (Phase 4.2). Only enabled agents can be
  // mentioned at all; a member whose roster entry names a disabled CLI would be
  // a delegation the backend gate accepts but the launcher cannot start, so it
  // is left to the agent group's own enabled filter rather than duplicated
  // here. Members are filtered by name (their identity) + body.
  const teamMembers = sources.teamMembers ?? []
  const teamItems: SuggestionItem[] =
    teamMembers.length > 0
      ? teamMembers
          .map(teamMemberToSuggestion)
          .filter((item) => suggestionMatches(item, q))
          .slice(0, MAX_PER_GROUP)
      : []
  const teamBodies = new Set(teamMembers.map((member) => member.agentType))

  // Files: filter the (potentially large) list on its pre-lowered fields before
  // paying to adapt the survivors. `truncated` is a cheap boolean — set when a
  // match is found past the cap — so we never scan the whole list for a count.
  const fileItems: SuggestionItem[] = []
  let fileTruncated = false
  const root = sources.workspaceRoot
  if (root) {
    for (const entry of sources.files) {
      if (q && !entry.lowerName.includes(q) && !entry.lowerPath.includes(q)) {
        continue
      }
      if (fileItems.length >= MAX_PER_GROUP) {
        fileTruncated = true
        break
      }
      fileItems.push(fileToSuggestion(entry, root))
    }
  }

  // Only enabled agents are mentionable — a disabled agent (toggled off in
  // settings) can't be referenced, so it never appears in the `@` panel (its
  // tab count and `truncated` flag follow from this filtered set too).
  const agentMatches = sources.agents
    .filter((agent) => agent.enabled)
    .map(agentToSuggestion)
    .filter((item) => suggestionMatches(item, q))
  // [crewchat] With a team, each agent row says whether delegating to it would
  // be accepted (the backend rejects anything outside the member set). Applied
  // AFTER the query filter so a hint can never itself match a search.
  const agentItems = agentMatches.slice(0, MAX_PER_GROUP).map((item) => {
    if (teamMembers.length === 0) return item
    const hint = teamBodies.has(item.reference.id)
      ? hints.member
      : hints.nonMember
    return { ...item, detail: item.detail ? `${hint} · ${item.detail}` : hint }
  })

  const sessionMatches = sources.sessions
    .map(sessionToSuggestion)
    .filter((item) => suggestionMatches(item, q))
  const sessionItems = sessionMatches.slice(0, MAX_PER_GROUP)

  const commitItems: SuggestionItem[] = []
  let commitTruncated = false
  if (sources.repoKey) {
    const repoKey = sources.repoKey
    for (const entry of sources.commits) {
      const item = commitToSuggestion(entry, repoKey)
      if (!suggestionMatches(item, q)) continue
      if (commitItems.length >= MAX_PER_GROUP) {
        commitTruncated = true
        break
      }
      commitItems.push(item)
    }
  }

  return [
    ...(teamMembers.length > 0
      ? [
          {
            kind: "team" as const,
            label: labels.team,
            items: teamItems,
            truncated: teamMembers.length > MAX_PER_GROUP,
          },
        ]
      : []),
    {
      kind: "file",
      label: labels.file,
      items: fileItems,
      truncated: fileTruncated,
    },
    {
      kind: "agent",
      label: labels.agent,
      items: agentItems,
      truncated: agentMatches.length > MAX_PER_GROUP,
    },
    {
      kind: "session",
      label: labels.session,
      items: sessionItems,
      truncated: sessionMatches.length > MAX_PER_GROUP,
    },
    {
      kind: "commit",
      label: labels.commit,
      items: commitItems,
      truncated: commitTruncated,
    },
  ]
}

export interface UseReferenceSearchOptions {
  /**
   * Workspace root for the file + commit groups (and the commit `repoKey`).
   * When empty/null those two groups stay empty while agents/sessions still
   * resolve, so a brand-new draft tab degrades gracefully (R8).
   */
  defaultPath?: string | null
  /**
   * Gates loading. When false the search resolves to empty groups and the file
   * tree is never fetched — let the host pre-warm only the active composer.
   */
  enabled?: boolean
  /** Localized group headings; English fallbacks when omitted. */
  labels?: ReferenceGroupLabels
  /** [crewchat] Localized team-row hints; English fallbacks when omitted. */
  hints?: ReferenceHintLabels
  /**
   * [crewchat] Roster role ids of the conversation's team members (Phase 4.2),
   * in team order. Empty/omitted for a single-agent session (or a host with no
   * team concept), in which case the roster is never fetched and the panel is
   * unchanged. Resolved through the shared cached roster promise, so several
   * composers and the delegation cards cost one request between them.
   */
  teamMemberRoleIds?: readonly string[]
}

/**
 * Compose the live data sources (file tree, ACP agents, conversations, git log)
 * into a single {@link ReferenceSearch} for the composer's `@` panel. (Skills,
 * commands and experts are inserted via the `/` / `$` triggers and the expert
 * menu, not this panel.)
 *
 * Referential stability is the contract: the suggestion popup re-runs its fetch
 * whenever the `search` identity changes (`suggestion-popup.tsx`), so the
 * returned function is an empty-dependency `useCallback` that reads every source
 * from a ref. A background refresh of any source (e.g. the agent list reloading
 * on window focus) updates the refs but leaves `search` identity untouched — the
 * open panel keeps its results and the user's selection (R7).
 *
 * Files and agents are hook-loaded (and pre-warmed via `enabled`). Sessions and
 * the git log are fetched lazily on the first `@`, key-cached in a ref, and
 * awaited by `search` so the first open is populated without an extra keystroke;
 * window focus busts those caches so they stay fresh.
 */
export function useReferenceSearch({
  defaultPath,
  enabled = true,
  labels,
  hints,
  teamMemberRoleIds,
}: UseReferenceSearchOptions): ReferenceSearch {
  const path = defaultPath || null

  const { allFiles, loaded } = useFileTree({
    folderPath: path ?? undefined,
    enabled,
  })
  const { agents } = useAcpAgents()

  // [crewchat] Resolve the team's role ids against the roster (Phase 4.2). The
  // effect is keyed on the joined ids so a store refresh that hands us a fresh
  // but equal array does not refetch or re-render the panel. The state carries
  // the key it was loaded for, so a stale roster is never read for a newer id
  // set (and no synchronous reset is needed when the ids go empty).
  const teamRoleIds = teamMemberRoleIds ?? []
  const teamRoleIdsKey = teamRoleIds.join("|")
  const [rosterState, setRosterState] = useState<{
    key: string
    roles: AgentRole[]
  } | null>(null)
  useEffect(() => {
    if (!teamRoleIdsKey) return
    let cancelled = false
    listAgentRolesCached()
      .then((roles) => {
        if (!cancelled) setRosterState({ key: teamRoleIdsKey, roles })
      })
      .catch(() => {
        // An unreachable roster degrades to "no team group": the panel keeps
        // its normal groups rather than claiming a team it cannot name.
        if (!cancelled) setRosterState({ key: teamRoleIdsKey, roles: [] })
      })
    return () => {
      cancelled = true
    }
  }, [teamRoleIdsKey])

  const roster =
    rosterState && rosterState.key === teamRoleIdsKey ? rosterState.roles : null

  const teamMembers = useMemo<TeamMentionMember[]>(() => {
    if (!teamRoleIdsKey || !roster) return []
    const byId = new Map(roster.map((role) => [role.id, role]))
    return teamRoleIdsKey
      .split("|")
      .map((roleId) => byId.get(roleId))
      .filter((role): role is AgentRole => role != null)
      .map((role) => ({
        roleId: role.id,
        name: role.name,
        agentType: role.agent_type,
      }))
  }, [teamRoleIdsKey, roster])

  // Mirror every changing source into a ref so `search` can stay identity-stable
  // (see the doc comment). Initialized from the first render so the refs are
  // sane even before the sync effect below runs.
  const filesRef = useRef<{ root: string | null; files: FlatFileEntry[] }>({
    root: null,
    files: [],
  })
  const agentsRef = useRef(agents)
  const pathRef = useRef(path)
  const enabledRef = useRef(enabled)
  const labelsRef = useRef(labels)
  const hintsRef = useRef(hints)
  const teamMembersRef = useRef(teamMembers)

  // `pathRef` and `enabledRef` gate the post-await freshness check in `search`,
  // so they must reflect the *committed* folder/enabled state synchronously at
  // commit — a passive effect can lag behind a stale in-flight fetch that
  // resolves in the post-commit / pre-effect window, leaking the old folder's
  // commits into the new panel. A layout effect (not a render-phase write) keeps
  // them commit-accurate without updating from an uncommitted transition render.
  useIsomorphicLayoutEffect(() => {
    pathRef.current = path
    enabledRef.current = enabled
  }, [path, enabled])

  useEffect(() => {
    // Only expose files once the tree has loaded for the *current* path, so the
    // search never joins the current workspace root onto a previous folder's
    // relative paths during a folder switch.
    filesRef.current =
      loaded && path
        ? { root: path, files: allFiles }
        : { root: null, files: [] }
    agentsRef.current = agents
    labelsRef.current = labels
    hintsRef.current = hints
    teamMembersRef.current = teamMembers
  }, [allFiles, loaded, path, agents, labels, hints, teamMembers])

  // Lazily-fetched network sources, key-cached so repeat searches reuse the
  // in-flight/resolved promise while a folder switch refetches.
  const sessionsRef = useRef<{
    key: string
    promise: Promise<DbConversationSummary[]>
  } | null>(null)
  const commitsRef = useRef<{
    key: string
    promise: Promise<GitLogEntry[]>
  } | null>(null)

  // Bust the lazy caches when the window regains focus so a session created in
  // another window (or new commits) show up on the next `@` — matching the
  // focus-refresh idiom of the other data hooks, without per-keystroke fetches.
  useEffect(() => {
    const onFocus = () => {
      sessionsRef.current = null
      commitsRef.current = null
    }
    window.addEventListener("focus", onFocus)
    return () => window.removeEventListener("focus", onFocus)
  }, [])

  return useCallback<ReferenceSearch>(async (query, signal) => {
    if (!enabledRef.current) return []

    const path = pathRef.current

    // Lazy session fetch. On rejection the cache entry is cleared (not cached as
    // an empty result) so the next `@` retries instead of wedging on `[]`.
    const sessionsKey = "all"
    let sessionsEntry = sessionsRef.current
    if (sessionsEntry?.key !== sessionsKey) {
      const created: NonNullable<typeof sessionsRef.current> = {
        key: sessionsKey,
        promise: listAllConversations().catch(() => {
          if (sessionsRef.current === created) sessionsRef.current = null
          return [] as DbConversationSummary[]
        }),
      }
      sessionsRef.current = created
      sessionsEntry = created
    }

    // Lazy git-log fetch, keyed by path with the same retry-on-rejection policy.
    let commitsPromise = EMPTY_COMMITS
    if (path) {
      let commitsEntry = commitsRef.current
      if (commitsEntry?.key !== path) {
        const created: NonNullable<typeof commitsRef.current> = {
          key: path,
          promise: gitLog(path, GIT_LOG_LIMIT)
            .then((result) => result.entries)
            .catch(() => {
              if (commitsRef.current === created) commitsRef.current = null
              return [] as GitLogEntry[]
            }),
        }
        commitsRef.current = created
        commitsEntry = created
      }
      commitsPromise = commitsEntry.promise
    } else {
      commitsRef.current = null
    }

    const [sessions, commits] = await Promise.all([
      sessionsEntry.promise,
      commitsPromise,
    ])
    // Discard this result if it can no longer be trusted for the live panel: a
    // newer query aborted us, the composer was disabled, or the workspace folder
    // changed while the network fetch was in flight (the popup only aborts on a
    // query change, so a folder switch would otherwise leak the old repo's
    // commits — built against `path` — into the new folder's panel). The next
    // keystroke re-runs the search against the current folder.
    if (signal?.aborted || !enabledRef.current || pathRef.current !== path) {
      return []
    }

    const fileState = filesRef.current
    return buildReferenceGroups(
      query,
      {
        files: fileState.files,
        workspaceRoot: fileState.root,
        agents: agentsRef.current,
        sessions,
        commits,
        repoKey: path,
        teamMembers: teamMembersRef.current,
      },
      labelsRef.current ?? DEFAULT_GROUP_LABELS,
      hintsRef.current ?? DEFAULT_HINT_LABELS
    )
  }, [])
}
