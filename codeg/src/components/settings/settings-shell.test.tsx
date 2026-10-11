import { describe, expect, it } from "vitest"

import { settingsNavItemsFor } from "./settings-shell"

describe("settingsNavItemsFor", () => {
  it("drops the Web service page in a browser session and nothing else", () => {
    const labels = settingsNavItemsFor("web").map((item) => item.labelKey)
    // There is no second server to configure from inside the one you are
    // already talking to.
    expect(labels).not.toContain("web_service")
    // The Browser page is mostly a desktop browser engine, but site rules,
    // what a server starting in a terminal does and the terminal's link menu
    // all still decide something here — and this page is the only place they
    // can be set, so the entry has to lead there.
    expect(labels).toContain("browser")
    expect(labels).toEqual(
      settingsNavItemsFor("tauri")
        .map((item) => item.labelKey)
        .filter((label) => label !== "web_service")
    )
  })

  it("[crewchat] keeps each group's internal order", () => {
    const labels = settingsNavItemsFor("tauri").map((item) => item.labelKey)
    const after = (label: (typeof labels)[number]) =>
      labels[labels.indexOf(label) + 1]
    // Agents → 能力 → 交互 → 系统 (see SETTINGS_NAV_GROUPS).
    expect(after("agent_roles")).toBe("agents")
    expect(after("agents")).toBe("collaboration")
    expect(after("collaboration")).toBe("model_providers")
    expect(after("model_providers")).toBe("skills")
    expect(after("skill_packs")).toBe("mcp")
    expect(after("quick_messages")).toBe("browser")
    expect(after("browser")).toBe("computer_use")
    expect(after("shortcuts")).toBe("general")
  })

  it("keeps every page on the desktop", () => {
    const labels = settingsNavItemsFor("tauri").map((item) => item.labelKey)
    expect(labels).toContain("web_service")
    expect(labels).toContain("browser")
  })
})
