/**
 * [crewchat] The CLI page's readiness card — the default face of the page.
 * These pin the four CCR states, the install/enable rows and the Advanced
 * fold toggle so the card's wording and actions stay put while the page
 * around it evolves.
 */
import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { AcpAgentInfo } from "@/lib/types"

import {
  AgentReadinessCard,
  type AgentReadinessCardProps,
} from "./agent-readiness-card"

function makeAgent(overrides: Partial<AcpAgentInfo>): AcpAgentInfo {
  return {
    agent_type: "claude_code",
    skills_capable: true,
    registry_id: "claude-code",
    registry_version: "2.0.0",
    supports_custom_version: false,
    name: "Claude Code",
    description: "",
    available: true,
    distribution_type: "npx",
    is_acp_adapter: true,
    custom_source: null,
    enabled: true,
    sort_order: 0,
    installed_version: "2.0.0",
    host_tools_agent_mode: false,
    env: {},
    config_json: null,
    config_file_path: null,
    opencode_auth_json: null,
    codex_auth_json: null,
    cline_secrets_json: null,
    codex_config_toml: null,
    codex_model_catalog: null,
    codex_sandbox_settings: null,
    grok_config_toml: null,
    grok_settings: null,
    hermes_config_yaml: null,
    cursor_cli_config_json: null,
    cursor_settings: null,
    model_provider_id: null,
    icon_url: null,
    ...overrides,
  }
}

function makeProps(
  overrides: Partial<AgentReadinessCardProps> = {}
): AgentReadinessCardProps {
  return {
    agent: makeAgent({}),
    enabled: true,
    install: {
      installedVersion: "2.0.0",
      recommendedVersion: "2.0.0",
      hasUpdate: false,
      quickInstallable: false,
      installing: false,
      uvReady: true,
    },
    ccr: {
      kind: "claude-code",
      state: "bound",
      profile: {
        agent: "claude-code",
        id: "default-claude-code",
        model: "OpenCode Go/mimo-v2.6-flash",
        enabled: true,
      },
      busy: false,
      onBind: vi.fn(),
      onReapply: vi.fn(),
      onRetry: vi.fn(),
    },
    installLogs: null,
    onInstall: vi.fn(),
    onUpgrade: vi.fn(),
    advancedOpen: false,
    onToggleAdvanced: vi.fn(),
    ...overrides,
  }
}

function renderCard(overrides: Partial<AgentReadinessCardProps> = {}) {
  const props = makeProps(overrides)
  render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <AgentReadinessCard {...props} />
    </NextIntlClientProvider>
  )
  return props
}

describe("AgentReadinessCard", () => {
  it("shows the bound profile's model and re-applies on request", async () => {
    const user = userEvent.setup()
    const props = renderCard()

    expect(screen.getByText("Bound")).toBeVisible()
    expect(
      screen.getByText(
        "CCR binding: claude-code · Default model OpenCode Go/mimo-v2.6-flash"
      )
    ).toBeVisible()

    await user.click(screen.getByRole("button", { name: "Re-apply" }))
    expect(props.ccr.onReapply).toHaveBeenCalledTimes(1)
  })

  it("names the gateway default when the profile has no model", () => {
    const base = makeProps()
    renderCard({
      ccr: {
        ...base.ccr,
        profile: { agent: "claude-code", id: "p", model: "", enabled: true },
      },
    })
    expect(
      screen.getByText(
        "CCR binding: claude-code · Default model gateway default"
      )
    ).toBeVisible()
  })

  it("flags a disabled profile next to the binding", () => {
    const base = makeProps()
    renderCard({
      ccr: {
        ...base.ccr,
        profile: { agent: "claude-code", id: "p", model: "m", enabled: false },
      },
    })
    expect(screen.getByText(/\(profile disabled\)/)).toBeVisible()
  })

  it("offers to bind a CLI with no profile yet", async () => {
    const user = userEvent.setup()
    const base = makeProps()
    const props = renderCard({
      ccr: { ...base.ccr, state: "bindable", profile: null },
    })

    expect(screen.getByText("Not bound yet")).toBeVisible()
    expect(
      screen.getByText("CCR binding: no profile for claude-code yet")
    ).toBeVisible()
    await user.click(screen.getByRole("button", { name: "Bind to CCR" }))
    expect(props.ccr.onBind).toHaveBeenCalledTimes(1)
  })

  it("surfaces a failed read with a retry", async () => {
    const user = userEvent.setup()
    const base = makeProps()
    const props = renderCard({
      ccr: { ...base.ccr, state: "unreachable", profile: null },
    })

    expect(screen.getByText("CCR unreachable")).toBeVisible()
    await user.click(screen.getByRole("button", { name: "Retry" }))
    expect(props.ccr.onRetry).toHaveBeenCalledTimes(1)
  })

  it("says the CLI is outside CCR's managed set, with no write actions", () => {
    const base = makeProps()
    renderCard({
      ccr: { ...base.ccr, kind: null, state: "unsupported", profile: null },
    })

    expect(screen.getByText("Not managed by CCR")).toBeVisible()
    expect(screen.getByText(/outside CCR's managed set/)).toBeVisible()
    expect(screen.queryByRole("button", { name: "Re-apply" })).toBeNull()
    expect(screen.queryByRole("button", { name: "Bind to CCR" })).toBeNull()
  })

  it("offers Install while missing, with the agent named on the button", async () => {
    const user = userEvent.setup()
    const props = renderCard({
      install: {
        installedVersion: null,
        recommendedVersion: "2.0.0",
        hasUpdate: false,
        quickInstallable: true,
        installing: false,
        uvReady: true,
      },
    })

    expect(screen.getByText("Not installed")).toBeVisible()
    await user.click(
      screen.getByRole("button", { name: "Install Claude Code" })
    )
    expect(props.onInstall).toHaveBeenCalledTimes(1)
  })

  it("offers Upgrade when the installed version trails the recommended one", async () => {
    const user = userEvent.setup()
    const props = renderCard({
      install: {
        installedVersion: "1.9.0",
        recommendedVersion: "2.0.0",
        hasUpdate: true,
        quickInstallable: false,
        installing: false,
        uvReady: true,
      },
    })

    expect(screen.getByText("Upgrade available 1.9.0 → 2.0.0")).toBeVisible()
    await user.click(
      screen.getByRole("button", { name: "Upgrade Claude Code" })
    )
    expect(props.onUpgrade).toHaveBeenCalledTimes(1)
  })

  it("shows the install log while an install is streaming", () => {
    renderCard({
      installLogs: ["Downloading…", "ERROR: boom"],
    })
    expect(screen.getByText("Downloading…")).toBeVisible()
    expect(screen.getByText("ERROR: boom")).toHaveClass("text-destructive")
  })

  it("toggles the Advanced fold through its switch", async () => {
    const user = userEvent.setup()
    const props = renderCard({ advancedOpen: false })
    const toggle = screen.getByRole("button", { name: "Advanced" })
    expect(toggle).toHaveAttribute("aria-expanded", "false")
    await user.click(toggle)
    expect(props.onToggleAdvanced).toHaveBeenCalledTimes(1)
  })
})
