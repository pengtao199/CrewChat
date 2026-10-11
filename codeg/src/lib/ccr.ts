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

/** Apply CCR's agent profiles — writes each bound CLI's own config. */
export function ccrApplyProfile(): Promise<unknown> {
  return ccrCall("applyProfile")
}

/**
 * [crewchat] The CCR profile agent kinds this fork can bind a CLI body to.
 * Everything else (gemini, hermes, cursor, cline, deepseek, …) is outside
 * CCR's profile registry and shows as "CCR 暂不支持" on the Agents settings
 * page. Keys are Codeg `AgentType`s.
 */
export const CCR_AGENT_KIND_BY_BODY: Record<string, string> = {
  claude_code: "claude-code",
  codex: "codex",
  grok: "grok",
  open_code: "opencode",
  pi: "pi",
  kimi_code: "kimi",
}

export function ccrAgentKindForBody(body: string): string | null {
  return CCR_AGENT_KIND_BY_BODY[body] ?? null
}

/** Read CCR's configured agent profiles; [] tolerates configs without any. */
export async function ccrGetProfiles(): Promise<CcrProfileSummary[]> {
  // The default retry count matters here: the management server is known to
  // hand back an empty view while it is persisting, and `[]` would render as
  // "no CLI has a profile" — the retry lets the page see the real list.
  const config = await ccrGetConfig()
  const profiles = config.profile?.profiles
  return Array.isArray(profiles) ? profiles : []
}

/**
 * [crewchat] Display names for the CCR agent kinds a CLI body can bind to,
 * mirroring CCR's own `profileAgentLabel` so a profile created here reads the
 * same as one created in CCR's UI. Kinds outside this table are "CCR 暂不支持"
 * on the CLI page and never get a profile built here.
 */
export const CCR_PROFILE_DISPLAY_NAMES: Record<string, string> = {
  "claude-code": "Claude Code",
  codex: "Codex",
  grok: "Grok CLI",
  kimi: "Kimi CLI",
  kilo: "Kilo CLI",
  opencode: "OpenCode",
  pi: "Pi",
}

/**
 * The profile the CLI page reports for one CCR agent kind. Enabled profiles
 * win over disabled ones; within a group the first (CCR's own order) is the
 * one shown. Returns null when the CLI has no profile at all — the
 * "bindable" state.
 */
export function pickCcrProfileForKind(
  profiles: readonly CcrProfileSummary[],
  kind: string
): CcrProfileSummary | null {
  const matches = profiles.filter((profile) => profile.agent === kind)
  if (matches.length === 0) return null
  return matches.find((profile) => profile.enabled) ?? matches[0]
}

/** CCR's own id slug rule (`uniqueProfileId` in its UI): name → dash slug. */
function ccrProfileSlug(value: string): string {
  return (
    value
      .toLowerCase()
      .replace(/[^a-z0-9_.-]+/g, "-")
      .replace(/^-+|-+$/g, "") || "profile"
  )
}

/** A profile id no existing profile uses, matching CCR's numbering scheme. */
export function uniqueCcrProfileId(
  existing: readonly CcrProfileSummary[],
  value: string
): string {
  const base = ccrProfileSlug(value)
  const taken = new Set(existing.map((profile) => profile.id))
  if (!taken.has(base)) return base
  for (let index = 2; index < 1000; index += 1) {
    const candidate = `${base}-${index}`
    if (!taken.has(candidate)) return candidate
  }
  return `${base}-${Date.now()}`
}

/**
 * [crewchat] Build one missing CCR profile for a CLI body — the CLI page's
 * "bind" write. Model stays empty on purpose: the gateway's default model
 * applies until someone picks a model for the role on the Agents page. The
 * shape mirrors CCR's own `profileConfigFromDraft` output (scope "ccr",
 * surface "cli") so nothing about it is a second kind of profile.
 */
export function buildCcrProfileForKind(
  kind: string,
  existing: readonly CcrProfileSummary[]
): CcrProfileSummary {
  const name = CCR_PROFILE_DISPLAY_NAMES[kind] ?? kind
  const base: CcrProfileSummary = {
    agent: kind,
    enabled: true,
    env: {},
    id: uniqueCcrProfileId(existing, name),
    model: "",
    name,
    scope: "ccr",
    surface: "cli",
  }
  if (kind === "claude-code") {
    return {
      ...base,
      env: { CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY: "1" },
      fableModel: "",
      haikuModel: "",
      managedCompact: false,
      opusModel: "",
      settingsFile: "~/.claude/settings.json",
      sonnetModel: "",
      smallFastModel: "",
    }
  }
  if (kind === "codex" || kind === "opencode" || kind === "kilo") {
    return {
      ...base,
      cliMiddleware: true,
      configFile:
        kind === "opencode"
          ? "~/.config/opencode/opencode.jsonc"
          : kind === "kilo"
            ? "~/.config/kilo/kilo.jsonc"
            : "~/.codex/config.toml",
      configFormat: "separate_profile_files",
      managedCompact: false,
      providerId: "claude-code-router",
      providerName: "Claude Code Router",
      showAllSessions: false,
    }
  }
  // grok / kimi / pi — CCR pins these to the generated (wrapper) scope.
  return base
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
