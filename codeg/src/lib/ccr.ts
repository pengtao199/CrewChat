// [crewchat] Typed access to CCR's management RPC — the single model layer in
// this fork. Every call goes through the backend proxy (`ccr_call`), never to
// CCR's HTTP port directly; see `codeg/NOTICE.md` and
// `docs/plans/codeg-ccr-integration.md`.

import { getTransport } from "./transport"

export interface CcrHealth {
  url: string
  tokenConfigured: boolean
}

export function ccrHealth(): Promise<CcrHealth> {
  return getTransport().call("ccr_health")
}

export function ccrCall<T = unknown>(
  method: string,
  args: unknown[] = []
): Promise<T> {
  return getTransport().call("ccr_call", { method, args })
}

export interface CcrGatewayStatus {
  state: string
  coreEndpoint?: string
  endpoint?: string
  lastError?: string
  lastStartedAt?: string
  pid?: number
}

/** One key inside a provider's credential pool (`provider.credentials`). */
export interface CcrCredential {
  id?: string
  name?: string
  label?: string
  api_key?: string
  apiKey?: string
  apikey?: string
  enabled?: boolean
  priority?: number
  weight?: number
  /** Rate/token limits, e.g. `{ rpm: 60, tpm: 100000 }`. */
  limits?: Record<string, unknown>
  [key: string]: unknown
}

export interface CcrProvider {
  /** Stable id used by routing; preserved when editing. */
  id?: string
  name: string
  type?: string
  api_base_url?: string
  baseUrl?: string
  api_key?: string
  apiKey?: string
  apikey?: string
  enabled?: boolean
  models?: string[]
  /** Credential pool (preferred over the single `api_key`). */
  credentials?: CcrCredential[]
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

export interface CcrProviderPresetEndpoint {
  baseUrl: string
  label?: string
  protocols: string[]
  websiteUrl?: string
}

export interface CcrProviderPreset {
  id: string
  name: string
  aliases?: string[]
  endpoints?: CcrProviderPresetEndpoint[]
  defaultModels?: string[]
  websiteUrl?: string
}

export interface CcrProbeProtocolResult {
  protocol: string
  endpoint: string
  supported: boolean
  status?: number
  message?: string
}

export interface CcrProbeResult {
  detectedProtocol?: string
  models?: string[]
  normalizedBaseUrl?: string
  protocols?: CcrProbeProtocolResult[]
}

export interface CcrAccountMeter {
  id: string
  kind: string
  label: string
  remaining?: number
  limit?: number
  used?: number
  unit: string
  window?: string
  resetAt?: string
}

export interface CcrAccountSnapshot {
  provider: string
  credentialId?: string
  credentialLabel?: string
  status: string
  source?: string
  message?: string
  meters?: CcrAccountMeter[]
  updatedAt?: string
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

export function ccrGetProviderPresets(): Promise<CcrProviderPreset[]> {
  return ccrCall<CcrProviderPreset[]>("getProviderPresets")
}

export function ccrProbeProvider(request: {
  baseUrl: string
  apiKey?: string
  mode?: "connectivity" | "models" | "protocols"
  models?: string[]
  forceRefresh?: boolean
}): Promise<CcrProbeResult> {
  return ccrCall<CcrProbeResult>("probeProvider", [request])
}

export function ccrGetProviderAccountSnapshots(
  provider?: string,
  options?: { forceRefresh?: boolean }
): Promise<CcrAccountSnapshot[]> {
  return ccrCall<CcrAccountSnapshot[]>("getProviderAccountSnapshots", [
    provider,
    options ?? {},
  ])
}

export const CCR_PROTOCOLS = [
  "openai_responses",
  "openai_chat_completions",
  "anthropic_messages",
  "gemini_generate_content",
  "gemini_interactions",
] as const
