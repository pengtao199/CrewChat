/**
 * [crewchat] The new-session Agent picker (Phase 1.2): roster loading, Agent
 * (= leader) picking, member toggling and clearing. Drives the component
 * directly with a mocked roster.
 */
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import { listAgentRolesCached, type AgentRole } from "@/lib/agent-roles"

import { TeamSelector } from "./team-selector"

vi.mock("@/lib/agent-roles", () => ({
  listAgentRolesCached: vi.fn(),
}))

function role(overrides: Partial<AgentRole>): AgentRole {
  return {
    id: "role-1",
    name: "管理员",
    soul: "",
    rules: "",
    agent_type: "claude_code",
    model: "",
    model_provider: "",
    skills: [],
    mcp: [],
    permission_mode: "",
    auto_approve: false,
    created_at: null,
    updated_at: null,
    ...overrides,
  }
}

const ROSTER = [
  role({ id: "manager", name: "管理员" }),
  role({ id: "backend", name: "后端开发工程师" }),
  role({ id: "frontend", name: "前端工程师" }),
]

function renderSelector(
  value: Parameters<typeof TeamSelector>[0]["value"] = null
) {
  const onChange = vi.fn()
  render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <TeamSelector value={value} onChange={onChange} />
    </NextIntlClientProvider>
  )
  return onChange
}

beforeEach(() => {
  vi.clearAllMocks()
  vi.mocked(listAgentRolesCached).mockResolvedValue(ROSTER)
})

describe("TeamSelector", () => {
  it("shows the empty state and lists the roster", async () => {
    const user = userEvent.setup()
    renderSelector()

    expect(
      screen.getByRole("button", { name: "Agent picker" })
    ).toHaveTextContent("Choose Agent")
    await user.click(screen.getByRole("button", { name: "Agent picker" }))

    await waitFor(() => {
      expect(screen.getByText("管理员")).toBeInTheDocument()
    })
    expect(screen.getByText("后端开发工程师")).toBeInTheDocument()
    // The manager-flavoured name is offered as the default Agent.
    expect(screen.getByText("Default")).toBeInTheDocument()
    // No Agent picked yet → no members section, no clear action.
    expect(screen.queryByText("Members (delegatable)")).toBeNull()
    expect(screen.queryByText(/Clear Agent/)).toBeNull()
  })

  it("picks the Agent and reports the team", async () => {
    const user = userEvent.setup()
    const onChange = renderSelector()

    await user.click(screen.getByRole("button", { name: "Agent picker" }))
    await user.click(await screen.findByText("管理员"))

    expect(onChange).toHaveBeenCalledWith({
      leader_role_id: "manager",
      member_role_ids: [],
    })
  })

  it("names the Agent in the closed button and toggles members", async () => {
    const user = userEvent.setup()
    const onChange = renderSelector({
      leader_role_id: "manager",
      member_role_ids: [],
    })

    // The button names the Agent instead of counting heads: an Agent-only pick
    // used to read "Team · 1", which said nothing about who is running. The
    // roster resolves asynchronously, so this waits for the name.
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Agent picker" })
      ).toHaveTextContent("Agent · 管理员")
    )
    await user.click(screen.getByRole("button", { name: "Agent picker" }))

    expect(await screen.findByText("Members (delegatable)")).toBeInTheDocument()
    await user.click(
      screen.getByRole("checkbox", { name: "Toggle member 后端开发工程师" })
    )
    expect(onChange).toHaveBeenCalledWith({
      leader_role_id: "manager",
      member_role_ids: ["backend"],
    })
  })

  it("appends the member count once the Agent has members", async () => {
    renderSelector({
      leader_role_id: "manager",
      member_role_ids: ["backend", "frontend"],
    })

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Agent picker" })
      ).toHaveTextContent("Agent · 管理员 +2")
    )
  })

  it("clears the Agent back to a plain single-agent session", async () => {
    const user = userEvent.setup()
    const onChange = renderSelector({
      leader_role_id: "manager",
      member_role_ids: ["backend", "frontend"],
    })

    await user.click(screen.getByRole("button", { name: "Agent picker" }))
    await user.click(await screen.findByText(/Clear Agent/))
    expect(onChange).toHaveBeenCalledWith(null)
  })

  it("explains an empty roster", async () => {
    const user = userEvent.setup()
    vi.mocked(listAgentRolesCached).mockResolvedValue([])
    renderSelector()

    await user.click(screen.getByRole("button", { name: "Agent picker" }))
    expect(
      await screen.findByText(/No agents in the roster yet/)
    ).toBeInTheDocument()
  })
})
