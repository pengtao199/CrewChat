import { describe, expect, it } from "vitest"

import {
  buildModelGroups,
  ccrStateForBody,
  createEmptyRole,
  profilesFromConfig,
  resolveModelProvider,
  summarizeMcpServer,
  toggleId,
} from "./helpers"
import type { CcrProfileSummary } from "@/lib/ccr"

describe("createEmptyRole", () => {
  it("starts unsaved with an empty id", () => {
    const role = createEmptyRole()
    expect(role.id).toBe("")
    expect(role.model_provider).toBe("")
    expect(role.skills).toEqual([])
    expect(role.mcp).toEqual([])
  })
})

describe("buildModelGroups", () => {
  it("groups models by provider, trims, dedupes and skips empty ones", () => {
    const groups = buildModelGroups({
      Providers: [
        { name: "OpenCode Go", models: ["a", "b", "a"] },
        { name: "NoModels" },
        { id: "p2", name: "", models: [" c "] },
      ],
    })
    expect(groups).toEqual([
      { provider: "OpenCode Go", models: ["a", "b"] },
      { provider: "p2", models: ["c"] },
    ])
  })

  it("tolerates a missing config", () => {
    expect(buildModelGroups(null)).toEqual([])
  })
})

describe("summarizeMcpServer", () => {
  it("prefers the url", () => {
    expect(summarizeMcpServer({ url: "http://x/mcp", command: "npx" })).toBe(
      "http://x/mcp"
    )
  })

  it("falls back to command with args", () => {
    expect(summarizeMcpServer({ command: "npx", args: ["-y", "srv"] })).toBe(
      "npx -y srv"
    )
  })

  it("returns empty for an unknown shape", () => {
    expect(summarizeMcpServer({})).toBe("")
    expect(summarizeMcpServer(null)).toBe("")
  })
})

describe("toggleId", () => {
  it("adds and removes without mutating the input", () => {
    const base = ["a"]
    expect(toggleId(base, "b")).toEqual(["a", "b"])
    expect(toggleId(base, "a")).toEqual([])
    expect(base).toEqual(["a"])
  })
})

describe("resolveModelProvider", () => {
  const groups = [
    { provider: "A", models: ["m1"] },
    { provider: "B", models: ["m2"] },
  ]

  it("keeps a stored provider that still exists", () => {
    expect(resolveModelProvider(groups, "B", "m1")).toBe("B")
  })

  it("derives from the model when the stored provider is gone", () => {
    expect(resolveModelProvider(groups, "Gone", "m2")).toBe("B")
  })

  it("returns empty when nothing matches", () => {
    expect(resolveModelProvider(groups, "", "m9")).toBe("")
  })
})

function profile(overrides: Partial<CcrProfileSummary>): CcrProfileSummary {
  return {
    agent: "claude-code",
    id: "default-claude-code",
    enabled: true,
    ...overrides,
  }
}

describe("profilesFromConfig", () => {
  it("reads the profiles array and tolerates a missing one", () => {
    expect(profilesFromConfig(null)).toEqual([])
    expect(profilesFromConfig({})).toEqual([])
    const list = [profile({})]
    expect(profilesFromConfig({ profile: { profiles: list } })).toEqual(list)
  })
})

describe("ccrStateForBody", () => {
  it("is loading for the empty body of an unsaved role", () => {
    expect(ccrStateForBody("", [], false)).toBe("loading")
  })

  it("is unsupported for a CLI outside CCR's registry, even when unreachable", () => {
    expect(ccrStateForBody("gemini", null, true)).toBe("unsupported")
    expect(ccrStateForBody("hermes", [], false)).toBe("unsupported")
  })

  it("is unreachable before any profile answer when the read failed", () => {
    expect(ccrStateForBody("claude_code", null, true)).toBe("unreachable")
  })

  it("is loading while the read is in flight", () => {
    expect(ccrStateForBody("claude_code", null, false)).toBe("loading")
  })

  it("is bound when a profile carries the body's kind", () => {
    expect(ccrStateForBody("claude_code", [profile({})], false)).toBe("bound")
  })

  it("is bindable when the kind has no profile yet", () => {
    expect(ccrStateForBody("claude_code", [], false)).toBe("bindable")
    expect(
      ccrStateForBody("codex", [profile({ agent: "claude-code" })], false)
    ).toBe("bindable")
  })
})
