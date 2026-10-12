/**
 * [crewchat] The new-session team picker (Phase 1.2): roster loading, leader
 * picking, member toggling and clearing. Drives the component directly with a
 * mocked roster.
 */
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import { listAgentRoles, type AgentRole } from "@/lib/agent-roles"

import { TeamSelector } from "./team-selector"

vi.mock("@/lib/agent-roles", () => ({
  listAgentRoles: vi.fn(),
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
  vi.mocked(listAgentRoles).mockResolvedValue(ROSTER)
})

describe("TeamSelector", () => {
  it("shows the empty state and loads the roster on open", async () => {
    const user = userEvent.setup()
    renderSelector()

    expect(
      screen.getByRole("button", { name: "Team picker" })
    ).toHaveTextContent("Single agent")
    await user.click(screen.getByRole("button", { name: "Team picker" }))

    await waitFor(() => {
      expect(screen.getByText("管理员")).toBeInTheDocument()
    })
    expect(screen.getByText("后端开发工程师")).toBeInTheDocument()
    // The manager-flavoured name is offered as the default leader.
    expect(screen.getByText("Default")).toBeInTheDocument()
    // No leader picked yet → no members section, no clear action.
    expect(screen.queryByText("Members (delegatable)")).toBeNull()
    expect(screen.queryByText(/Clear team/)).toBeNull()
  })

  it("picks the leader and reports the team", async () => {
    const user = userEvent.setup()
    const onChange = renderSelector()

    await user.click(screen.getByRole("button", { name: "Team picker" }))
    await user.click(await screen.findByText("管理员"))

    expect(onChange).toHaveBeenCalledWith({
      leader_role_id: "manager",
      member_role_ids: [],
    })
  })

  it("toggles members once a leader is set", async () => {
    const user = userEvent.setup()
    const onChange = renderSelector({
      leader_role_id: "manager",
      member_role_ids: [],
    })

    expect(
      screen.getByRole("button", { name: "Team picker" })
    ).toHaveTextContent("Team · 1")
    await user.click(screen.getByRole("button", { name: "Team picker" }))

    expect(await screen.findByText("Members (delegatable)")).toBeInTheDocument()
    await user.click(
      screen.getByRole("checkbox", { name: "Toggle member 后端开发工程师" })
    )
    expect(onChange).toHaveBeenCalledWith({
      leader_role_id: "manager",
      member_role_ids: ["backend"],
    })
  })

  it("clears the team back to a single-agent session", async () => {
    const user = userEvent.setup()
    const onChange = renderSelector({
      leader_role_id: "manager",
      member_role_ids: ["backend", "frontend"],
    })

    await user.click(screen.getByRole("button", { name: "Team picker" }))
    await user.click(await screen.findByText(/Clear team/))
    expect(onChange).toHaveBeenCalledWith(null)
  })

  it("explains an empty roster", async () => {
    const user = userEvent.setup()
    vi.mocked(listAgentRoles).mockResolvedValue([])
    renderSelector()

    await user.click(screen.getByRole("button", { name: "Team picker" }))
    expect(
      await screen.findByText(/No agents in the roster yet/)
    ).toBeInTheDocument()
  })
})
