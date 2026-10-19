import { render, screen, within } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import { TeamMemberStrip } from "@/components/chat/team-member-strip"
import type { AgentRole } from "@/lib/agent-roles"
import type { DbConversationSummary } from "@/lib/types"

const h = vi.hoisted(() => ({
  listAgentRoles: vi.fn(),
  listChildConversations: vi.fn(),
  bindings: [] as unknown[],
  conversations: [] as unknown[],
  childConnection: undefined as unknown,
  /** [crewchat] The delegation switch, as the strip reads it. */
  delegationEnabled: null as boolean | null,
}))

vi.mock("@/lib/agent-roles", () => ({
  listAgentRoles: h.listAgentRoles,
}))
vi.mock("@/hooks/use-delegation-enabled", () => ({
  useDelegationEnabled: () => h.delegationEnabled,
}))
vi.mock("@/lib/api", () => ({
  listChildConversations: h.listChildConversations,
}))
vi.mock("@/stores/app-workspace-store", () => {
  const useStore = (selector: (state: unknown) => unknown) =>
    selector({ conversations: h.conversations })
  useStore.getState = () => ({ conversations: h.conversations })
  useStore.subscribe = () => () => {}
  return { useAppWorkspaceStore: useStore }
})
// Minimal shape-only mocks: the strip reads `listBindings` / the child
// connection snapshot and nothing else from these contexts, so the real
// modules (and their heavy import graphs) stay out of this suite.
vi.mock("@/contexts/delegation-context", () => ({
  useDelegation: () => ({ listBindings: () => h.bindings }),
}))
vi.mock("@/contexts/acp-connections-context", () => ({
  useConnectionStore: () => ({
    subscribeKey: () => () => {},
    getConnection: () => h.childConnection,
  }),
}))

function role(overrides: Partial<AgentRole> = {}): AgentRole {
  return {
    id: "role-front",
    name: "前端工程师",
    soul: "",
    rules: "",
    agent_type: "codex",
    model: "",
    model_provider: "",
    skills: [],
    permission_mode: "",
    auto_approve: false,
    mcp: [],
    created_at: null,
    updated_at: null,
    ...overrides,
  }
}

function childRow(
  overrides: Partial<DbConversationSummary> = {}
): DbConversationSummary {
  return {
    id: 9,
    agent_type: "codex",
    status: "in_progress",
    ...overrides,
  } as DbConversationSummary
}

function binding(overrides: Record<string, unknown> = {}) {
  return {
    parentConnectionId: "parent-conn",
    parentToolUseId: "tool-1",
    childConnectionId: "conn-9",
    childConversationId: 9,
    agentType: "codex",
    status: "running",
    task: null,
    taskId: null,
    ...overrides,
  }
}

function renderStrip(conversationId: number | null = 7) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <TeamMemberStrip conversationId={conversationId} />
    </NextIntlClientProvider>
  )
}

function chipFor(name: string): HTMLElement {
  const label = screen.getByText(name)
  const chip = label.closest("[data-state]")
  if (!(chip instanceof HTMLElement)) throw new Error(`no chip for ${name}`)
  return chip
}

beforeEach(() => {
  h.listAgentRoles.mockReset()
  h.listChildConversations.mockReset()
  h.listAgentRoles.mockResolvedValue([role()])
  h.listChildConversations.mockResolvedValue([])
  h.bindings = []
  h.childConnection = undefined
  h.conversations = [
    {
      id: 7,
      team: { leader_role_id: "role-lead", member_role_ids: ["role-front"] },
    },
  ]
})

describe("TeamMemberStrip", () => {
  it("renders nothing for a plain single-agent conversation", () => {
    h.conversations = [{ id: 7 }]
    renderStrip()
    expect(screen.queryByTestId("team-member-strip")).toBeNull()
  })

  it("renders nothing for an unsaved draft", () => {
    renderStrip(null)
    expect(screen.queryByTestId("team-member-strip")).toBeNull()
  })

  it("shows a member as running while its child row is in flight", async () => {
    h.listChildConversations.mockResolvedValue([childRow()])
    h.bindings = [binding({ task: "build the page" })]
    renderStrip()
    expect(await screen.findByTestId("team-member-strip")).toBeDefined()
    const chip = chipFor("前端工程师")
    expect(chip).toHaveAttribute("data-state", "running")
    expect(chip.getAttribute("title")).toContain("Running")
    expect(chip.getAttribute("title")).toContain("build the page")
  })

  it("keeps a completed member visible after a reload (no live bindings)", async () => {
    h.listChildConversations.mockResolvedValue([
      childRow({ status: "completed" }),
    ])
    renderStrip()
    const chip = await screen.findByTitle(/Done/)
    expect(chip).toHaveAttribute("data-state", "done")
  })

  it("shows a queued member while its child row is not visible yet", async () => {
    h.listChildConversations.mockResolvedValue([])
    h.bindings = [binding()]
    renderStrip()
    const chip = await screen.findByTitle(/Queued/)
    expect(chip).toHaveAttribute("data-state", "queued")
  })

  it("upgrades a running member to waiting when the child asks for a permission", async () => {
    h.listChildConversations.mockResolvedValue([childRow()])
    h.bindings = [binding()]
    h.childConnection = { pendingPermission: { request_id: "req-1" } }
    renderStrip()
    const chip = await screen.findByTitle(/Waiting for permission/)
    expect(chip).toHaveAttribute("data-state", "waiting")
  })

  it("hides the strip when no member role resolves against the roster", async () => {
    h.listAgentRoles.mockResolvedValue([])
    renderStrip()
    // The body mounts (and fetches) before the roster resolves, but with no
    // resolvable member there is no strip to show.
    await screen.findByTestId("team-member-strip").catch(() => null)
    expect(screen.queryByTestId("team-member-strip")).toBeNull()
  })

  // [crewchat] A team is decoration while delegation is off: the lead gets no
  // team frame and no delegation tool. That has to be visible where the user
  // checks on the team — this is the exact confusion the warning exists for.
  it("warns that members are inert while delegation is off", async () => {
    h.listAgentRoles.mockResolvedValue([role({})])
    h.delegationEnabled = false
    renderStrip()
    const warning = await screen.findByTestId("team-delegation-off")
    expect(warning).toHaveTextContent(/Multi-agent delegation is off/)
    expect(
      within(warning).getByRole("button", { name: /Turn it on/ })
    ).toBeInTheDocument()
  })

  it("says nothing about delegation once it is on (or unknown)", async () => {
    h.listAgentRoles.mockResolvedValue([role({})])
    h.delegationEnabled = true
    renderStrip()
    await screen.findByTestId("team-member-strip")
    expect(screen.queryByTestId("team-delegation-off")).toBeNull()
  })
})
