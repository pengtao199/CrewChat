// [crewchat] Preferences for the CLI-agents settings list — currently just the
// "only installed" filter, which should survive a reload.

const ONLY_INSTALLED_KEY = "codeg.agents.onlyInstalled"

export function readAgentListOnlyInstalled(): boolean {
  if (typeof window === "undefined") return false
  try {
    return window.localStorage.getItem(ONLY_INSTALLED_KEY) === "1"
  } catch {
    return false
  }
}

export function writeAgentListOnlyInstalled(value: boolean): void {
  if (typeof window === "undefined") return
  try {
    window.localStorage.setItem(ONLY_INSTALLED_KEY, value ? "1" : "0")
  } catch {
    // Storage full or blocked — the filter just does not persist.
  }
}
