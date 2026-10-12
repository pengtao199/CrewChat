import type { GrokStructuredConfig } from "@/lib/types"

import type { GrokAuthMethod } from "./types"

/** Sentinel for the Grok structured selects' "unset / use default" choice
 * (Radix Select forbids an empty-string item value). Maps to `null` on save. */
export const GROK_UNSET = "__grok_unset__"

/** Grok custom-model `api_backend` options (docs.x.ai). Default `responses` —
 * what Grok's own build models use; a BYO OpenAI-compatible proxy would pick
 * `chat_completions`, an Anthropic-format endpoint `messages`. */
export const GROK_DEFAULT_API_BACKEND = "responses"

/** Grok's real credential env var (mirrors the backend `agent_env_keys(Grok)`
 * and `importantEnvKeysByAgent`). */
export const GROK_API_KEY_ENV = "XAI_API_KEY"

/** codeg-side knob recording the chosen authentication method. Read by the
 * launch path (`apply_grok_env_policy`): in `subscription` mode it clears any
 * inherited XAI_API_KEY so the CLI uses the `grok login` browser credential.
 * The Grok binary itself ignores this var. Mirrors Cursor's CURSOR_AUTH_MODE. */
export const GROK_AUTH_MODE_ENV = "GROK_AUTH_MODE"

/** The subscription sign-in command shown (and copied) in the auth card. Grok's
 * `login` is a root subcommand; a bare `grok login` matches the panel's existing
 * hint wording (codeg doesn't resolve the managed binary path here). */
export const GROK_LOGIN_COMMAND = "grok login"

/** Resolve the persisted Grok authentication method, tolerant of legacy rows:
 * an explicit `GROK_AUTH_MODE` wins; otherwise a configured custom model implies
 * `custom`, a saved XAI_API_KEY implies `api_key`, and an empty env means the
 * user relies on `grok login`. `hasCustomModel` reflects whether a codeg-managed
 * `[model.<id>]` is set (it lives in config.toml, not env). Mirrors
 * `inferCursorMode`. */
export function inferGrokMode(
  env: Record<string, string>,
  hasCustomModel = false
): GrokAuthMethod {
  const explicit = (env[GROK_AUTH_MODE_ENV] ?? "").trim()
  if (
    explicit === "subscription" ||
    explicit === "api_key" ||
    explicit === "custom"
  ) {
    return explicit
  }
  if (hasCustomModel) return "custom"
  return (env[GROK_API_KEY_ENV] ?? "").trim() ? "api_key" : "subscription"
}

/**
 * Build the Grok structured-config save payload from the draft's dropdown
 * fields. An empty draft field (the "unset / use default" choice) maps to
 * `null` — which the backend treats as "remove this key" — while a chosen value
 * passes through. This is the single seam that encodes the unset→remove contract
 * for the two managed keys.
 */
export function buildGrokStructuredConfig(draft: {
  grokAuthMode: GrokAuthMethod
  grokPermissionMode: string
  grokReasoningEffort: string
  grokCustomModelId: string
  grokCustomBaseUrl: string
  grokCustomApiKey: string
  grokCustomApiBackend: string
  grokCustomContextWindow: string
  grokAutoCompactThreshold: string
}): GrokStructuredConfig {
  // The custom-model group applies only in the `custom` auth method; the
  // subscription / api_key methods omit the codeg-managed [model.<id>] block
  // (an empty id → the backend removes it). Permission mode, reasoning effort
  // and compaction below stay independent of the auth method.
  const modelId =
    draft.grokAuthMode === "custom" ? draft.grokCustomModelId.trim() : ""
  const positiveInt = (raw: string): number | null => {
    const n = Number.parseInt(raw.trim(), 10)
    return Number.isFinite(n) && n > 0 ? n : null
  }
  const percent = (raw: string): number | null => {
    const trimmed = raw.trim()
    if (!trimmed) return null
    const n = Number.parseInt(trimmed, 10)
    return Number.isFinite(n) ? Math.min(100, Math.max(0, n)) : null
  }
  return {
    permissionMode: draft.grokPermissionMode || null,
    defaultReasoningEffort: draft.grokReasoningEffort || null,
    customModelId: modelId || null,
    // The model-scoped fields only matter when a model id is set; the backend
    // writes them inside the [model.<id>] block. api_backend defaults to
    // `responses` (Grok's build backend) whenever a model is configured.
    customBaseUrl: modelId ? draft.grokCustomBaseUrl.trim() || null : null,
    customApiKey: modelId ? draft.grokCustomApiKey.trim() || null : null,
    customApiBackend: modelId
      ? draft.grokCustomApiBackend || GROK_DEFAULT_API_BACKEND
      : null,
    customContextWindow: modelId
      ? positiveInt(draft.grokCustomContextWindow)
      : null,
    // Compaction is session-global, independent of the custom model.
    autoCompactThresholdPercent: percent(draft.grokAutoCompactThreshold),
  }
}

/**
 * Build the Grok save `persistConfig` options from the draft. The structured
 * controls always merge; the raw config.toml text is only sent when the user
 * actually edited it (dirty) — otherwise the backend merges the structured
 * controls onto the CURRENT on-disk file (never a stale in-memory snapshot). One
 * save persists both surfaces together, so there is no independent save that
 * could discard the other surface's unsaved edits.
 */
export function buildGrokSaveOptions(
  draft: {
    grokAuthMode: GrokAuthMethod
    grokPermissionMode: string
    grokReasoningEffort: string
    grokCustomModelId: string
    grokCustomBaseUrl: string
    grokCustomApiKey: string
    grokCustomApiBackend: string
    grokCustomContextWindow: string
    grokAutoCompactThreshold: string
    grokConfigTomlText: string
  },
  agentGrokConfigToml: string | null
): { grokStructured: GrokStructuredConfig; grokConfigTomlText?: string } {
  const rawDirty = draft.grokConfigTomlText !== (agentGrokConfigToml ?? "")
  return {
    grokStructured: buildGrokStructuredConfig(draft),
    ...(rawDirty ? { grokConfigTomlText: draft.grokConfigTomlText } : {}),
  }
}
