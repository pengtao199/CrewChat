import { act, renderHook } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type { FlatFileEntry } from "@/hooks/use-file-tree"
import type {
  AcpAgentInfo,
  DbConversationSummary,
  GitLogEntry,
} from "@/lib/types"

import type { SuggestionGroup, SuggestionGroupKind } from "./suggestion/types"
import type { TeamMentionMember } from "./suggestion/team"
import {
  buildReferenceGroups,
  DEFAULT_GROUP_LABELS,
  DEFAULT_HINT_LABELS,
  useReferenceSearch,
  type ReferenceSearchSources,
} from "./use-reference-search"

// --- fixtures ---------------------------------------------------------------

function makeFile(
  relativePath: string,
  kind: "file" | "dir" = "file"
): FlatFileEntry {
  const name = relativePath.split("/").pop() ?? relativePath
  return {
    name,
    relativePath,
    kind,
    lowerPath: relativePath.toLowerCase(),
    lowerName: name.toLowerCase(),
  }
}

function makeAgent(
  agentType: string,
  over: { name?: string; description?: string; enabled?: boolean } = {}
): AcpAgentInfo {
  return {
    agent_type: agentType,
    name: over.name ?? agentType,
    description: over.description ?? "",
    available: true,
    enabled: over.enabled ?? true,
    sort_order: 0,
  } as unknown as AcpAgentInfo
}

function makeConversation(id: number, title: string): DbConversationSummary {
  return {
    id,
    title,
    agent_type: "claude_code",
    status: "idle",
    git_branch: null,
  } as unknown as DbConversationSummary
}

function makeCommit(
  hash: string,
  message = "msg",
  author = "Dev"
): GitLogEntry {
  return {
    hash,
    full_hash: `${hash}0000`,
    author,
    date: "2026-01-01",
    message,
    files: [],
    pushed: false,
  }
}

function emptySources(
  over: Partial<ReferenceSearchSources> = {}
): ReferenceSearchSources {
  return {
    files: [],
    workspaceRoot: null,
    agents: [],
    sessions: [],
    commits: [],
    repoKey: null,
    ...over,
  }
}

const itemsOf = (groups: SuggestionGroup[], kind: SuggestionGroupKind) =>
  groups.find((g) => g.kind === kind)?.items ?? []

function makeTeamMember(
  roleId: string,
  name: string,
  agentType: string
): TeamMentionMember {
  return { roleId, name, agentType }
}

// --- pure builder -----------------------------------------------------------

describe("buildReferenceGroups", () => {
  it("returns the four groups in a fixed order (no skill group)", () => {
    const groups = buildReferenceGroups("", emptySources())
    expect(groups.map((g) => g.kind)).toEqual([
      "file",
      "agent",
      "session",
      "commit",
    ])
  })

  it("keeps every group present (empty groups are not dropped)", () => {
    const groups = buildReferenceGroups("", emptySources())
    expect(groups).toHaveLength(4)
    expect(groups.every((g) => g.items.length === 0)).toBe(true)
  })

  it("defaults the group headings to the English labels", () => {
    const groups = buildReferenceGroups("", emptySources())
    expect(groups.map((g) => g.label)).toEqual([
      DEFAULT_GROUP_LABELS.file,
      DEFAULT_GROUP_LABELS.agent,
      DEFAULT_GROUP_LABELS.session,
      DEFAULT_GROUP_LABELS.commit,
    ])
  })

  it("accepts injected (localized) group headings", () => {
    const labels = {
      file: "文件",
      agent: "智能体",
      session: "会话",
      commit: "提交",
      skill: "技能",
      team: "团队",
    }
    const groups = buildReferenceGroups("", emptySources(), labels)
    expect(itemsOf(groups, "file")).toBeDefined()
    expect(groups.find((g) => g.kind === "agent")?.label).toBe("智能体")
  })

  it("adapts files into file:// references rooted at the workspace", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        files: [makeFile("a.ts"), makeFile("src/app.ts")],
        workspaceRoot: "/repo",
      })
    )
    const files = itemsOf(groups, "file")
    expect(files).toHaveLength(2)
    expect(files.map((f) => f.reference.uri)).toEqual([
      "file:///repo/a.ts",
      "file:///repo/src/app.ts",
    ])
  })

  it("filters files by name or relative path, case-insensitively", () => {
    const groups = buildReferenceGroups(
      "APP",
      emptySources({
        files: [makeFile("a.ts"), makeFile("src/App.tsx")],
        workspaceRoot: "/repo",
      })
    )
    const files = itemsOf(groups, "file")
    expect(files).toHaveLength(1)
    expect(files[0].reference.id).toBe("src/App.tsx")
  })

  it("omits the file group when there is no workspace root (R8)", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({ files: [makeFile("a.ts")], workspaceRoot: null })
    )
    expect(itemsOf(groups, "file")).toHaveLength(0)
  })

  it("filters agents by name / type / description", () => {
    const groups = buildReferenceGroups(
      "codex",
      emptySources({
        agents: [
          makeAgent("codex", { name: "Codex" }),
          makeAgent("gemini", { name: "Gemini" }),
        ],
      })
    )
    const agents = itemsOf(groups, "agent")
    expect(agents).toHaveLength(1)
    expect(agents[0].reference.id).toBe("codex")
  })

  it("excludes disabled agents (only enabled agents are mentionable)", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        agents: [
          makeAgent("codex", { name: "Codex" }),
          makeAgent("gemini", { name: "Gemini", enabled: false }),
        ],
      })
    )
    const agents = itemsOf(groups, "agent")
    expect(agents).toHaveLength(1)
    expect(agents[0].reference.id).toBe("codex")
  })

  it("adapts sessions into codeg://session references", () => {
    const groups = buildReferenceGroups(
      "login",
      emptySources({
        sessions: [
          makeConversation(7, "Login refactor"),
          makeConversation(8, "Sidebar perf"),
        ],
      })
    )
    const sessions = itemsOf(groups, "session")
    expect(sessions).toHaveLength(1)
    expect(sessions[0].reference.uri).toBe("codeg://session/7")
  })

  it("omits the commit group when there is no repoKey (R8)", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({ commits: [makeCommit("abc1234")], repoKey: null })
    )
    expect(itemsOf(groups, "commit")).toHaveLength(0)
  })

  it("adapts commits and filters by hash / message / author", () => {
    const groups = buildReferenceGroups(
      "bugfix",
      emptySources({
        commits: [
          makeCommit("abc1234", "bugfix: crash"),
          makeCommit("def5678", "feature"),
        ],
        repoKey: "/repo",
      })
    )
    const commits = itemsOf(groups, "commit")
    expect(commits).toHaveLength(1)
    expect(commits[0].reference.uri).toBe("codeg://commit/%2Frepo@abc12340000")
  })

  it("caps each group at 50 items and flags the overflow as truncated", () => {
    const files = Array.from({ length: 60 }, (_, i) => makeFile(`f${i}.ts`))
    const groups = buildReferenceGroups(
      "",
      emptySources({ files, workspaceRoot: "/repo" })
    )
    const fileGroup = groups.find((g) => g.kind === "file")
    expect(fileGroup?.items).toHaveLength(50)
    expect(fileGroup?.truncated).toBe(true)
  })

  it("does not flag truncation when a group exactly fills the cap", () => {
    const files = Array.from({ length: 50 }, (_, i) => makeFile(`f${i}.ts`))
    const groups = buildReferenceGroups(
      "",
      emptySources({ files, workspaceRoot: "/repo" })
    )
    const fileGroup = groups.find((g) => g.kind === "file")
    expect(fileGroup?.items).toHaveLength(50)
    expect(fileGroup?.truncated).toBe(false)
  })

  it("flags truncation for slice-based groups (agents) as well", () => {
    const agents = Array.from({ length: 51 }, (_, i) => makeAgent(`a${i}`))
    const groups = buildReferenceGroups("", emptySources({ agents }))
    const agentGroup = groups.find((g) => g.kind === "agent")
    expect(agentGroup?.items).toHaveLength(50)
    expect(agentGroup?.truncated).toBe(true)
  })

  it("returns everything for an empty query (whitespace-trimmed)", () => {
    const groups = buildReferenceGroups(
      "   ",
      emptySources({
        agents: [makeAgent("codex"), makeAgent("gemini")],
      })
    )
    expect(itemsOf(groups, "agent")).toHaveLength(2)
  })

  // --- team group (Phase 4.2) ----------------------------------------------

  it("omits the team group entirely without a team (panel unchanged)", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({ agents: [makeAgent("codex")] })
    )
    expect(groups.map((g) => g.kind)).toEqual([
      "file",
      "agent",
      "session",
      "commit",
    ])
    // No team → no hints on the agent rows either.
    expect(itemsOf(groups, "agent")[0].detail).toBe(null)
  })

  it("pins the team group first with members by roster name", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        agents: [makeAgent("claude_code", { name: "Claude Code" })],
        teamMembers: [
          makeTeamMember("r1", "后端工程师", "claude_code"),
          makeTeamMember("r2", "前端工程师", "codex"),
        ],
      })
    )
    expect(groups.map((g) => g.kind)).toEqual([
      "team",
      "file",
      "agent",
      "session",
      "commit",
    ])
    const team = groups[0]
    expect(team.label).toBe(DEFAULT_GROUP_LABELS.team)
    expect(team.items.map((i) => i.reference.label)).toEqual([
      "后端工程师",
      "前端工程师",
    ])
    // A member mention is an ordinary agent reference — the backend routes and
    // gates on exactly this link.
    expect(team.items[0].reference.refType).toBe("agent")
    expect(team.items[0].reference.uri).toBe("codeg://agent/claude_code")
  })

  it("filters the team group by member name or body", () => {
    const sources = () =>
      emptySources({
        teamMembers: [
          makeTeamMember("r1", "后端工程师", "claude_code"),
          makeTeamMember("r2", "前端工程师", "codex"),
        ],
      })
    expect(itemsOf(buildReferenceGroups("后端", sources()), "team")).toHaveLength(
      1
    )
    expect(itemsOf(buildReferenceGroups("codex", sources()), "team")).toHaveLength(
      1
    )
    expect(itemsOf(buildReferenceGroups("nope", sources()), "team")).toHaveLength(
      0
    )
  })

  it("marks member and non-member agents in the agent group", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        agents: [
          makeAgent("claude_code", { name: "Claude Code" }),
          makeAgent("gemini", { name: "Gemini" }),
        ],
        teamMembers: [makeTeamMember("r1", "后端工程师", "claude_code")],
      })
    )
    const agents = itemsOf(groups, "agent")
    expect(agents[0].detail).toBe(DEFAULT_HINT_LABELS.member)
    expect(agents[1].detail).toBe(DEFAULT_HINT_LABELS.nonMember)
  })

  it("keeps the agent description after the hint", () => {
    const groups = buildReferenceGroups(
      "",
      emptySources({
        agents: [makeAgent("gemini", { description: "Google CLI" })],
        teamMembers: [makeTeamMember("r1", "后端工程师", "claude_code")],
      })
    )
    expect(itemsOf(groups, "agent")[0].detail).toBe(
      `${DEFAULT_HINT_LABELS.nonMember} · Google CLI`
    )
  })

  it("never lets a hint word match a query", () => {
    // The hint is applied after filtering, so searching for its own text finds
    // nothing rather than every agent row.
    const groups = buildReferenceGroups(
      "Not in this team",
      emptySources({
        agents: [makeAgent("gemini", { name: "Gemini" })],
        teamMembers: [makeTeamMember("r1", "后端工程师", "claude_code")],
      })
    )
    expect(itemsOf(groups, "agent")).toHaveLength(0)
  })

  it("accepts injected team heading and hints", () => {
    const labels = { ...DEFAULT_GROUP_LABELS, team: "团队" }
    const hints = { member: "成员", nonMember: "非成员" }
    const groups = buildReferenceGroups(
      "",
      emptySources({
        agents: [makeAgent("gemini", { name: "Gemini" })],
        teamMembers: [makeTeamMember("r1", "后端工程师", "claude_code")],
      }),
      labels,
      hints
    )
    expect(groups[0].label).toBe("团队")
    expect(itemsOf(groups, "agent")[0].detail).toBe("非成员")
  })

  it("caps the team group and flags the overflow", () => {
    const teamMembers = Array.from({ length: 51 }, (_, i) =>
      makeTeamMember(`r${i}`, `成员${i}`, "claude_code")
    )
    const groups = buildReferenceGroups("", emptySources({ teamMembers }))
    const team = groups.find((g) => g.kind === "team")
    expect(team?.items).toHaveLength(50)
    expect(team?.truncated).toBe(true)
  })
})

// --- hook --------------------------------------------------------------------

const mocks = vi.hoisted(() => ({
  agents: [] as AcpAgentInfo[],
  files: { allFiles: [] as FlatFileEntry[], loaded: false },
  listAllConversations: vi.fn(),
  gitLog: vi.fn(),
  listAgentRolesCached: vi.fn(),
}))

vi.mock("@/hooks/use-file-tree", () => ({
  useFileTree: () => ({
    allFiles: mocks.files.allFiles,
    loaded: mocks.files.loaded,
    loading: false,
    reset: () => {},
  }),
}))
vi.mock("@/hooks/use-acp-agents", () => ({
  useAcpAgents: () => ({ agents: mocks.agents, fresh: true, refresh: vi.fn() }),
}))
vi.mock("@/lib/api", () => ({
  listAllConversations: (...args: unknown[]) =>
    mocks.listAllConversations(...args),
  gitLog: (...args: unknown[]) => mocks.gitLog(...args),
}))
vi.mock("@/lib/agent-roles", () => ({
  listAgentRolesCached: (...args: unknown[]) =>
    mocks.listAgentRolesCached(...args),
}))

describe("useReferenceSearch", () => {
  beforeEach(() => {
    mocks.agents = []
    mocks.files = { allFiles: [], loaded: false }
    mocks.listAllConversations.mockReset().mockResolvedValue([])
    mocks.listAgentRolesCached.mockReset().mockResolvedValue([])
    mocks.gitLog
      .mockReset()
      .mockResolvedValue({ entries: [], has_upstream: false })
  })

  it("returns a referentially stable search across data-source updates (R7)", async () => {
    mocks.agents = [makeAgent("codex", { name: "Codex" })]
    const { result, rerender } = renderHook(
      (props: { enabled: boolean }) => useReferenceSearch(props),
      { initialProps: { enabled: true } }
    )
    const first = result.current

    // A background refresh swaps the agents array reference.
    mocks.agents = [
      makeAgent("codex", { name: "Codex" }),
      makeAgent("gemini", { name: "Gemini" }),
    ]
    rerender({ enabled: true })

    // Identity is unchanged — the popup will not re-fetch or reset selection…
    expect(result.current).toBe(first)
    // …yet the stable function reads the freshest data through its refs.
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })
    expect(itemsOf(groups, "agent")).toHaveLength(2)
  })

  it("lazily fetches and awaits sessions + commits on the first search", async () => {
    mocks.listAllConversations.mockResolvedValue([
      makeConversation(7, "Login refactor"),
    ])
    mocks.gitLog.mockResolvedValue({
      entries: [makeCommit("abc1234", "fix")],
      has_upstream: false,
    })
    mocks.files = { allFiles: [makeFile("a.ts")], loaded: true }

    const { result } = renderHook(() =>
      useReferenceSearch({ defaultPath: "/repo", enabled: true })
    )
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })

    expect(mocks.listAllConversations).toHaveBeenCalledTimes(1)
    expect(mocks.gitLog).toHaveBeenCalledWith("/repo", 100)
    expect(itemsOf(groups, "session")).toHaveLength(1)
    expect(itemsOf(groups, "commit")).toHaveLength(1)
    expect(itemsOf(groups, "file")).toHaveLength(1)
  })

  it("reuses the cached network promises across repeated searches", async () => {
    const { result } = renderHook(() =>
      useReferenceSearch({ defaultPath: "/repo", enabled: true })
    )
    await act(async () => {
      await result.current("")
      await result.current("a")
      await result.current("ab")
    })
    expect(mocks.listAllConversations).toHaveBeenCalledTimes(1)
    expect(mocks.gitLog).toHaveBeenCalledTimes(1)
  })

  it("resolves to no groups and touches no network when disabled", async () => {
    const { result } = renderHook(() =>
      useReferenceSearch({ defaultPath: "/repo", enabled: false })
    )
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("x")) as SuggestionGroup[]
    })
    expect(groups).toEqual([])
    expect(mocks.listAllConversations).not.toHaveBeenCalled()
    expect(mocks.gitLog).not.toHaveBeenCalled()
  })

  it("returns no groups when the query is aborted mid-fetch", async () => {
    mocks.listAllConversations.mockImplementation(
      () =>
        new Promise((resolve) =>
          setTimeout(() => resolve([makeConversation(1, "x")]), 10)
        )
    )
    const { result } = renderHook(() =>
      useReferenceSearch({ defaultPath: "/repo", enabled: true })
    )
    let groups!: SuggestionGroup[]
    await act(async () => {
      const controller = new AbortController()
      const pending = result.current("", controller.signal)
      controller.abort()
      groups = (await pending) as SuggestionGroup[]
    })
    expect(groups).toEqual([])
  })

  it("degrades gracefully with no workspace path: agents resolve, files/commits stay empty (R8)", async () => {
    mocks.agents = [makeAgent("codex", { name: "Codex" })]
    mocks.files = { allFiles: [makeFile("a.ts")], loaded: true }

    const { result } = renderHook(() => useReferenceSearch({ enabled: true }))
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })

    expect(itemsOf(groups, "file")).toHaveLength(0)
    expect(itemsOf(groups, "commit")).toHaveLength(0)
    expect(itemsOf(groups, "agent")).toHaveLength(1)
    expect(mocks.gitLog).not.toHaveBeenCalled()
  })

  it("does not leak the previous folder's commits when defaultPath changes mid-fetch", async () => {
    // git-log for repo A hangs until we resolve it by hand; repo B resolves
    // immediately. We switch folders before A resolves.
    let resolveA!: (value: {
      entries: GitLogEntry[]
      has_upstream: boolean
    }) => void
    mocks.gitLog.mockImplementation((repoPath: string) => {
      if (repoPath === "/repoA") {
        return new Promise((resolve) => {
          resolveA = resolve
        })
      }
      return Promise.resolve({
        entries: [makeCommit("bbb", "repo B commit")],
        has_upstream: false,
      })
    })

    const { result, rerender } = renderHook(
      (props: { defaultPath: string }) =>
        useReferenceSearch({ defaultPath: props.defaultPath, enabled: true }),
      { initialProps: { defaultPath: "/repoA" } }
    )

    // Start a search that hangs on gitLog("/repoA").
    let pending!: SuggestionGroup[] | Promise<SuggestionGroup[]>
    await act(async () => {
      pending = result.current("")
    })

    // The composer switches to repo B; commit flushes the ref mirror (pathRef →
    // "/repoB") before the stale repo A fetch resolves.
    //
    // NOTE: jsdom + RTL `act()` flush BOTH layout and passive effects at their
    // boundaries, so a unit test cannot reproduce the real-browser window where
    // a passive effect lags behind a macrotask that resolves the stale promise.
    // This asserts the guard CONTRACT (a folder switch discards the stale
    // result); the production fix that closes the timing window is the
    // commit-synchronous layout-effect mirror of pathRef in the hook.
    await act(async () => {
      rerender({ defaultPath: "/repoB" })
    })

    let groups!: SuggestionGroup[]
    await act(async () => {
      resolveA({
        entries: [makeCommit("aaa", "repo A commit")],
        has_upstream: false,
      })
      groups = (await pending) as SuggestionGroup[]
    })

    // The stale invocation must not render repo A commits into repo B's panel;
    // it bails so the next keystroke re-queries the current folder.
    expect(groups).toEqual([])
  })

  it("retries a lazy fetch after it rejects (a failure is never cached)", async () => {
    mocks.listAllConversations
      .mockRejectedValueOnce(new Error("transient"))
      .mockResolvedValueOnce([makeConversation(1, "Recovered")])

    const { result } = renderHook(() => useReferenceSearch({ enabled: true }))

    let first!: SuggestionGroup[]
    await act(async () => {
      first = (await result.current("")) as SuggestionGroup[]
    })
    // First fetch rejected → the session group is empty, but the others render.
    expect(itemsOf(first, "session")).toHaveLength(0)

    let second!: SuggestionGroup[]
    await act(async () => {
      second = (await result.current("")) as SuggestionGroup[]
    })
    // The failure was not cached, so the second `@` issues a fresh request…
    expect(mocks.listAllConversations).toHaveBeenCalledTimes(2)
    // …which succeeds and populates the group.
    expect(itemsOf(second, "session")).toHaveLength(1)
  })

  // --- team resolution (Phase 4.2) -----------------------------------------

  it("never touches the roster without team member ids", async () => {
    const { result } = renderHook(() => useReferenceSearch({ enabled: true }))
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })
    expect(mocks.listAgentRolesCached).not.toHaveBeenCalled()
    expect(groups.map((g) => g.kind)).not.toContain("team")
  })

  it("resolves team member ids into the pinned group, in team order", async () => {
    mocks.listAgentRolesCached.mockResolvedValue([
      { id: "r2", name: "前端工程师", agent_type: "codex" },
      { id: "r1", name: "后端工程师", agent_type: "claude_code" },
    ])

    const { result } = renderHook(() =>
      useReferenceSearch({ enabled: true, teamMemberRoleIds: ["r1", "r2"] })
    )
    // Flush the roster fetch (and the ref mirror it triggers) before searching,
    // the way a real keystroke lands after the cached roster resolved.
    await act(async () => {})
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })

    expect(mocks.listAgentRolesCached).toHaveBeenCalledTimes(1)
    expect(groups[0].kind).toBe("team")
    expect(groups[0].items.map((i) => i.reference.label)).toEqual([
      "后端工程师",
      "前端工程师",
    ])
  })

  it("skips team ids the roster no longer knows", async () => {
    mocks.listAgentRolesCached.mockResolvedValue([
      { id: "r1", name: "后端工程师", agent_type: "claude_code" },
    ])

    const { result } = renderHook(() =>
      useReferenceSearch({ enabled: true, teamMemberRoleIds: ["r1", "gone"] })
    )
    await act(async () => {})
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })
    expect(groups[0].items.map((i) => i.reference.id)).toEqual(["claude_code"])
  })

  it("degrades to no team group when the roster is unreachable", async () => {
    mocks.listAgentRolesCached.mockRejectedValue(new Error("offline"))

    const { result } = renderHook(() =>
      useReferenceSearch({ enabled: true, teamMemberRoleIds: ["r1"] })
    )
    await act(async () => {})
    let groups!: SuggestionGroup[]
    await act(async () => {
      groups = (await result.current("")) as SuggestionGroup[]
    })
    expect(groups.map((g) => g.kind)).not.toContain("team")
  })
})
