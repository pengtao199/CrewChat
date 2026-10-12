import type { HermesLocalConfig } from "@/lib/types"

import type { HermesDraftValues } from "./types"

/**
 * Parse the normalized Hermes projection carried in `AcpAgentInfo.config_json`
 * (produced by the backend from ~/.hermes/.env + config.yaml). Falls back to a
 * sensible default provider when nothing is configured yet.
 */
export function parseHermesConfig(configText: string): HermesDraftValues {
  let parsed: HermesLocalConfig = {}
  if (configText.trim()) {
    try {
      parsed = JSON.parse(configText) as HermesLocalConfig
    } catch {
      parsed = {}
    }
  }
  return {
    provider: parsed.provider ?? "openrouter",
    model: parsed.model ?? "",
    baseUrl: parsed.baseUrl ?? "",
    apiKey: parsed.apiKey ?? "",
    hermesHome: parsed.hermesHome ?? "",
    setupCommand: parsed.setupCommand ?? "",
    modelCommand: parsed.modelCommand ?? "",
  }
}
