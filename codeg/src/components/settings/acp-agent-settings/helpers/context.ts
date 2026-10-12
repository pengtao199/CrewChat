import { setCustomAgentDisplay } from "@/lib/custom-agents"
import type { AcpAgentInfo, CheckStatus } from "@/lib/types"

export type AcpTranslator = (
  key: string,
  values?: Record<string, string | number>
) => string

let acpTranslator: AcpTranslator | null = null

export function acpText(
  key: string,
  fallback: string,
  values?: Record<string, string | number>
): string {
  if (!acpTranslator) return fallback
  return acpTranslator(key, values)
}

/**
 * Publish a freshly fetched agent list into the custom-agent display map
 * (names + icons behind `getAgentLabel` / `getAgentIconUrl`). The map is
 * normally hydrated by `useAcpAgents`, but that hook lives in the workspace
 * surfaces — the settings window fetches its own list, so without this every
 * custom agent here falls back to the initial-letter glyph.
 */
export function publishAgentDisplay(list: AcpAgentInfo[]): void {
  setCustomAgentDisplay(
    list.map((agent) => ({
      agentType: agent.agent_type,
      name: agent.name,
      iconUrl: agent.icon_url,
    }))
  )
}

export function statusTone(status: CheckStatus): string {
  if (status === "pass") return "text-green-500"
  if (status === "warn") return "text-yellow-500"
  return "text-red-500"
}

export function setAcpTranslator(next: AcpTranslator | null) {
  acpTranslator = next
}
