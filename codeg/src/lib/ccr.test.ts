import { describe, expect, it } from "vitest"

import {
  buildCcrProfileForKind,
  pickCcrProfileForKind,
  uniqueCcrProfileId,
  type CcrProfileSummary,
} from "./ccr"

function profile(overrides: Partial<CcrProfileSummary>): CcrProfileSummary {
  return {
    agent: "claude-code",
    id: "default-claude-code",
    enabled: true,
    ...overrides,
  }
}

describe("pickCcrProfileForKind", () => {
  it("returns null when no profile carries the kind", () => {
    expect(pickCcrProfileForKind([profile({ agent: "codex" })], "grok")).toBe(
      null
    )
  })

  it("prefers the enabled profile over a disabled sibling", () => {
    const disabled = profile({ id: "a", agent: "grok", enabled: false })
    const enabled = profile({ id: "b", agent: "grok", enabled: true })
    expect(pickCcrProfileForKind([disabled, enabled], "grok")).toBe(enabled)
  })

  it("falls back to the first profile when every match is disabled", () => {
    const first = profile({ id: "a", agent: "pi", enabled: false })
    const second = profile({ id: "b", agent: "pi", enabled: false })
    expect(pickCcrProfileForKind([first, second], "pi")).toBe(first)
  })
})

describe("uniqueCcrProfileId", () => {
  it("slugifies a display name the way CCR's UI does", () => {
    expect(uniqueCcrProfileId([], "Claude Code")).toBe("claude-code")
    expect(uniqueCcrProfileId([], "Grok CLI")).toBe("grok-cli")
  })

  it("numbers around existing ids", () => {
    expect(uniqueCcrProfileId([profile({ id: "pi" })], "Pi")).toBe("pi-2")
    expect(
      uniqueCcrProfileId([profile({ id: "pi" }), profile({ id: "pi-2" })], "Pi")
    ).toBe("pi-3")
  })
})

describe("buildCcrProfileForKind", () => {
  it("builds an enabled, model-less ccr-scope profile named like CCR's UI", () => {
    const built = buildCcrProfileForKind("grok", [])
    expect(built).toMatchObject({
      agent: "grok",
      enabled: true,
      id: "grok-cli",
      model: "",
      name: "Grok CLI",
      scope: "ccr",
      surface: "cli",
    })
  })

  it("keeps claude-code's own settings file and gateway discovery env", () => {
    const built = buildCcrProfileForKind("claude-code", [])
    expect(built).toMatchObject({
      agent: "claude-code",
      id: "claude-code",
      settingsFile: "~/.claude/settings.json",
      env: { CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY: "1" },
    })
  })

  it("defaults codex-family profiles onto the CCR provider", () => {
    const codex = buildCcrProfileForKind("codex", [])
    expect(codex).toMatchObject({
      agent: "codex",
      cliMiddleware: true,
      configFile: "~/.codex/config.toml",
      configFormat: "separate_profile_files",
      providerId: "claude-code-router",
      providerName: "Claude Code Router",
      scope: "ccr",
    })
    expect(buildCcrProfileForKind("opencode", []).configFile).toBe(
      "~/.config/opencode/opencode.jsonc"
    )
    expect(buildCcrProfileForKind("kilo", []).configFile).toBe(
      "~/.config/kilo/kilo.jsonc"
    )
  })

  it("never collides with an existing profile id", () => {
    const built = buildCcrProfileForKind("pi", [
      profile({ id: "pi", agent: "pi", enabled: false }),
    ])
    expect(built.id).toBe("pi-2")
  })
})
