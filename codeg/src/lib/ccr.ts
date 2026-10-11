// [crewchat] Typed access to CCR's management RPC — the single model layer in
// this fork. Every call goes through the backend proxy (`ccr_call`), never to
// CCR's HTTP port directly; see `codeg/NOTICE.md` and
// `docs/plans/codeg-ccr-integration.md`.

import { ccrCall, ccrHealth } from "./api"
import type { CcrHealth } from "./api"

export { ccrHealth }
export type { CcrHealth }

export interface CcrGatewayStatus {
  state: string
  coreEndpoint?: string
  endpoint?: string
  lastError?: string
  lastStartedAt?: string
  pid?: number
}

export interface CcrProvider {
  /** Stable id used by routing; preserved when editing. */
  id?: string
  name: string
  type?: string
  api_base_url?: string
  baseUrl?: string
  apiKey?: string
  enabled?: boolean
  models?: string[]
  [key: string]: unknown
}

export interface CcrProfileSummary {
  agent: string
  id: string
  name?: string
  model?: string
  enabled?: boolean
  [key: string]: unknown
}

export interface CcrConfig {
  Providers?: CcrProvider[]
  profile?: { profiles?: CcrProfileSummary[] } & Record<string, unknown>
  [key: string]: unknown
}

export function ccrGatewayStatus(): Promise<CcrGatewayStatus> {
  return ccrCall<CcrGatewayStatus>("getGatewayStatus")
}

export function ccrStartGateway(): Promise<unknown> {
  return ccrCall("startGateway")
}

export function ccrStopGateway(): Promise<unknown> {
  return ccrCall("stopGateway")
}

export function ccrRestartGateway(): Promise<unknown> {
  return ccrCall("restartGateway")
}

/**
 * Read CCR's config. The management server is known to hand back an empty view
 * while it is persisting (observed on v3.1.3), so retry briefly before giving
 * the page an empty config it would render as "no providers".
 */
export async function ccrGetConfig(retries = 3): Promise<CcrConfig> {
  let last: CcrConfig = {}
  for (let attempt = 0; attempt < retries; attempt += 1) {
    last = await ccrCall<CcrConfig>("getConfig")
    if (Array.isArray(last?.Providers)) {
      return last
    }
    await new Promise((resolve) => {
      setTimeout(resolve, 400)
    })
  }
  return last
}

export function ccrSaveConfig(config: CcrConfig): Promise<unknown> {
  return ccrCall("saveConfig", [config])
}

export const CCR_PROTOCOLS = [
  "openai_responses",
  "openai_chat_completions",
  "anthropic_messages",
  "gemini_generate_content",
  "gemini_interactions",
] as const
