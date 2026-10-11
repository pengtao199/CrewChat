// [crewchat] Shared types and pure helpers for the CCR model settings UI.

import type {
  CcrAccountMeter,
  CcrAccountSnapshot,
  CcrCredential,
  CcrProvider,
} from "@/lib/ccr"

// ---------------------------------------------------------------------------
// Draft shapes (what the provider dialog edits)
// ---------------------------------------------------------------------------

export interface CredentialDraft {
  name: string
  apiKey: string
  priority: string
  weight: string
  limitsText: string
  /** Key visibility toggle in the row (independent of `advancedOpen`). */
  revealKey: boolean
  advancedOpen: boolean
  enabled: boolean
}

export interface ProviderDraft {
  presetId: string
  name: string
  baseUrl: string
  type: string
  modelsText: string
  credentials: CredentialDraft[]
}

export function emptyCredential(): CredentialDraft {
  return {
    name: "",
    apiKey: "",
    priority: "",
    weight: "",
    limitsText: "",
    revealKey: false,
    advancedOpen: false,
    enabled: true,
  }
}

export function emptyDraft(): ProviderDraft {
  return {
    presetId: "custom",
    name: "",
    baseUrl: "",
    type: "openai_chat_completions",
    modelsText: "",
    credentials: [emptyCredential()],
  }
}

// ---------------------------------------------------------------------------
// Reading a provider
// ---------------------------------------------------------------------------

export function providerBaseUrl(provider: CcrProvider): string {
  return provider.api_base_url ?? provider.baseUrl ?? ""
}

export function providerModels(provider: CcrProvider): string[] {
  return Array.isArray(provider.models) ? provider.models : []
}

export function providerCredentials(provider: CcrProvider): CcrCredential[] {
  if (Array.isArray(provider.credentials) && provider.credentials.length > 0) {
    return provider.credentials
  }
  const single = provider.api_key ?? provider.apiKey ?? provider.apikey ?? ""
  return single ? [{ name: "Key 1", api_key: single }] : []
}

export function credentialKey(credential: CcrCredential): string {
  return credential.api_key ?? credential.apiKey ?? credential.apikey ?? ""
}

export function credentialName(
  credential: CcrCredential,
  index: number
): string {
  const name = (credential.name ?? credential.label ?? "").trim()
  return name || `Key ${index + 1}`
}

export function credentialPriority(
  credential: CcrCredential,
  index: number
): number {
  return Number.isFinite(credential.priority)
    ? Number(credential.priority)
    : index + 1
}

export function keyTail(key: string): string {
  const value = key.trim()
  if (!value) return ""
  return value.length <= 8 ? value : `…${value.slice(-6)}`
}

export function limitsSummary(limits?: Record<string, unknown>): string {
  if (!limits) return ""
  const parts: string[] = []
  for (const field of ["rpm", "rph", "rpd", "tpm", "tph", "tpd"]) {
    const value = limits[field]
    if (typeof value === "number" && Number.isFinite(value)) {
      parts.push(`${field} ${value}`)
    }
  }
  return parts.join(" · ")
}

export function meterText(meter: CcrAccountMeter): string {
  const unit = meter.unit || ""
  if (typeof meter.remaining === "number") {
    return typeof meter.limit === "number"
      ? `${meter.remaining}/${meter.limit} ${unit}`.trim()
      : `${meter.remaining} ${unit}`.trim()
  }
  if (typeof meter.used === "number") {
    return `${meter.used} ${unit}`.trim()
  }
  return ""
}

export function headerUsageBadge(snapshots: CcrAccountSnapshot[]): string {
  for (const snapshot of snapshots) {
    for (const meter of snapshot.meters ?? []) {
      const text = meterText(meter)
      if (text) {
        return `${meter.label}: ${text}`
      }
    }
  }
  return ""
}

// ---------------------------------------------------------------------------
// Parsing / writing
// ---------------------------------------------------------------------------

export function parseModels(text: string): string[] {
  const seen = new Set<string>()
  const out: string[] = []
  for (const line of text.split("\n")) {
    const value = line.trim()
    if (value && !seen.has(value)) {
      seen.add(value)
      out.push(value)
    }
  }
  return out
}

export function parseLimits(text: string): Record<string, unknown> | undefined {
  const trimmed = text.trim()
  if (!trimmed) return undefined
  const parsed: unknown = JSON.parse(trimmed)
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error("limits-not-object")
  }
  return parsed as Record<string, unknown>
}

export function slugId(name: string): string {
  const base = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32)
  return `${base || "provider"}-${Date.now().toString(36)}`
}

export function errorText(error: unknown): string {
  const raw = error as Record<string, unknown>
  if (typeof raw?.message === "string") {
    return raw.message
  }
  return error instanceof Error ? error.message : String(error)
}

// ---------------------------------------------------------------------------
// Draft <-> provider
// ---------------------------------------------------------------------------

export function draftFromProvider(provider: CcrProvider): ProviderDraft {
  const credentials = providerCredentials(provider).map((credential) => ({
    name: (credential.name ?? credential.label ?? "").trim(),
    apiKey: credentialKey(credential),
    priority: Number.isFinite(credential.priority)
      ? String(credential.priority)
      : "",
    weight: Number.isFinite(credential.weight) ? String(credential.weight) : "",
    limitsText: credential.limits
      ? JSON.stringify(credential.limits, null, 2)
      : "",
    revealKey: false,
    enabled: credential.enabled !== false,
    advancedOpen: Boolean(
      credential.limits ||
      Number.isFinite(credential.priority) ||
      Number.isFinite(credential.weight)
    ),
  }))
  return {
    presetId: "custom",
    name: provider.name,
    baseUrl: providerBaseUrl(provider),
    type: provider.type ?? "openai_chat_completions",
    modelsText: providerModels(provider).join("\n"),
    credentials: credentials.length > 0 ? credentials : [emptyCredential()],
  }
}

/** First non-empty key of a draft (for probing / saving). */
export function firstDraftKey(credentials: CredentialDraft[]): string {
  return (
    credentials
      .map((row) => row.apiKey.trim())
      .find((value) => Boolean(value)) ?? ""
  )
}

/** First non-empty key of a provider (for probing an existing provider). */
export function firstProviderKey(provider: CcrProvider): string {
  return (
    providerCredentials(provider)
      .map((credential) => credentialKey(credential).trim())
      .find((value) => Boolean(value)) ?? ""
  )
}

/**
 * Build the config entry for a draft. The pool replaces the single-key form:
 * `api_key` (and its spellings) are dropped and `credentials[]` is written
 * instead, which is the shape CCR prefers.
 */
export function buildProviderEntry(
  existing: CcrProvider | undefined,
  draft: ProviderDraft
): CcrProvider {
  const rows = draft.credentials.filter(
    (row) => row.apiKey.trim() || row.name.trim()
  )
  const credentials: CcrCredential[] = rows.map((row) => {
    const credential: CcrCredential = { api_key: row.apiKey.trim() }
    if (row.name.trim()) {
      credential.name = row.name.trim()
    }
    const priority = Number(row.priority)
    if (row.priority.trim() && Number.isFinite(priority)) {
      credential.priority = priority
    }
    const weight = Number(row.weight)
    if (row.weight.trim() && Number.isFinite(weight) && weight > 0) {
      credential.weight = weight
    }
    const limits = parseLimits(row.limitsText)
    if (limits) {
      credential.limits = limits
    }
    credential.enabled = row.enabled
    return credential
  })

  const name = draft.name.trim()
  const entry: CcrProvider = existing
    ? { ...existing }
    : { id: slugId(name), name, enabled: true }
  entry.name = name
  entry.type = draft.type
  entry.api_base_url = draft.baseUrl.trim()
  delete entry.baseUrl
  delete entry.api_key
  delete entry.apiKey
  delete entry.apikey
  entry.models = parseModels(draft.modelsText)
  entry.credentials = credentials
  return entry
}
