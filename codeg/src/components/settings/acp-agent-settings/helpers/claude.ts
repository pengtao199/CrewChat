import type { AgentType, ModelProviderInfo } from "@/lib/types"
import { parseClaudeProviderModel } from "@/lib/types"

import { parseConfigJsonText } from "./config-text"
import { patchEnvText } from "./env"
import { CLAUDE_MODEL_ENV_KEYS } from "./types"
import type { ImportantDraftPatch } from "./types"

// Claude Code hardening flags surfaced as toggles below the reasoning settings.
// Each maps to a boolean env var in the native config's `env` (on = "1", off =
// "0"). Because Claude Code's own defaults are the opposite of what we want, the
// toggle values are materialized on save (see the config-save handler) so the
// shown default positions are actually applied — not left implicit/absent.
export const CLAUDE_ATTRIBUTION_HEADER_ENV_KEY =
  "CLAUDE_CODE_ATTRIBUTION_HEADER"

export const CLAUDE_NONESSENTIAL_TRAFFIC_ENV_KEY =
  "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"

export const CLAUDE_ENV_FLAG_ON = "1"

export const CLAUDE_ENV_FLAG_OFF = "0"

// `CLAUDE_CODE_ATTRIBUTION_HEADER` = "send the attribution/billing header" →
// default OFF (don't send). `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` = "disable
// telemetry / redundant pings" → default ON (disabled).
export const CLAUDE_SEND_ATTRIBUTION_HEADER_DEFAULT = false

export const CLAUDE_DISABLE_NONESSENTIAL_TRAFFIC_DEFAULT = true

export function patchImportantConfigText(
  agentType: AgentType,
  configText: string,
  patch: ImportantDraftPatch
): {
  configText: string
  recoveredFromInvalid: boolean
} {
  const parseResult = parseConfigJsonText(configText)
  const config = parseResult.error ? {} : { ...parseResult.config }

  const assignOrRemove = (key: string, value: string | undefined) => {
    const trimmed = value?.trim() ?? ""
    if (!trimmed) {
      delete config[key]
      return
    }
    config[key] = trimmed
  }

  if (agentType === "claude_code") {
    // Claude Code: write apiBaseUrl/apiKey into config.env, not root
    const env =
      typeof config.env === "object" && config.env && !Array.isArray(config.env)
        ? { ...(config.env as Record<string, unknown>) }
        : {}
    const assignEnv = (key: string, value: string | undefined) => {
      const trimmed = value?.trim() ?? ""
      if (!trimmed) {
        delete env[key]
        return
      }
      env[key] = trimmed
    }
    // Remove root-level apiBaseUrl/apiKey if present (legacy cleanup)
    delete config.apiBaseUrl
    delete config.apiKey
    assignEnv("ANTHROPIC_BASE_URL", patch.apiBaseUrl)
    assignEnv("ANTHROPIC_AUTH_TOKEN", patch.apiKey)

    assignEnv(CLAUDE_MODEL_ENV_KEYS.claudeMainModel, patch.claudeMainModel)
    assignEnv(
      CLAUDE_MODEL_ENV_KEYS.claudeReasoningModel,
      patch.claudeReasoningModel
    )
    assignEnv(
      CLAUDE_MODEL_ENV_KEYS.claudeDefaultHaikuModel,
      patch.claudeDefaultHaikuModel
    )
    assignEnv(
      CLAUDE_MODEL_ENV_KEYS.claudeDefaultSonnetModel,
      patch.claudeDefaultSonnetModel
    )
    assignEnv(
      CLAUDE_MODEL_ENV_KEYS.claudeDefaultOpusModel,
      patch.claudeDefaultOpusModel
    )
    assignEnv(
      CLAUDE_MODEL_ENV_KEYS.claudeDefaultFableModel,
      patch.claudeDefaultFableModel
    )
    assignEnv(
      CLAUDE_MODEL_ENV_KEYS.claudeCustomModelOption,
      patch.claudeCustomModelOption
    )
    assignEnv(
      CLAUDE_MODEL_ENV_KEYS.claudeCustomModelOptionName,
      patch.claudeCustomModelOptionName
    )
    assignEnv(
      CLAUDE_MODEL_ENV_KEYS.claudeCustomModelOptionDescription,
      patch.claudeCustomModelOptionDescription
    )

    if (Object.keys(env).length === 0) {
      delete config.env
    } else {
      config.env = env
    }
  } else {
    assignOrRemove("apiBaseUrl", patch.apiBaseUrl)
    assignOrRemove("apiKey", patch.apiKey)
    assignOrRemove("model", patch.model)
  }

  return {
    configText:
      Object.keys(config).length === 0 ? "" : JSON.stringify(config, null, 2),
    recoveredFromInvalid: Boolean(parseResult.error),
  }
}

/**
 * Make a Claude agent's native config provider-authoritative. When a provider
 * was bound in an earlier session, the on-disk config loaded into the draft can
 * still carry stale model keys (e.g. a leftover ANTHROPIC_CUSTOM_MODEL_OPTION)
 * that no longer match the provider — `handleModelProviderSelect` only rewrites
 * configText when the dropdown changes, not on reload. A config-management save
 * would otherwise persist that stale text back over the backend bind cascade, so
 * re-derive the provider-controlled keys here (empty => cleared by `assignEnv`)
 * before saving. Unrelated config/env keys are preserved.
 */
export function applyClaudeProviderToConfigText(
  configText: string,
  provider: Pick<ModelProviderInfo, "api_url" | "api_key" | "model">
): string {
  const model = parseClaudeProviderModel(provider.model ?? null)
  return patchImportantConfigText("claude_code", configText, {
    apiBaseUrl: provider.api_url,
    apiKey: provider.api_key,
    claudeMainModel: model.main ?? "",
    claudeReasoningModel: model.reasoning ?? "",
    claudeDefaultHaikuModel: model.haiku ?? "",
    claudeDefaultSonnetModel: model.sonnet ?? "",
    claudeDefaultOpusModel: model.opus ?? "",
    claudeDefaultFableModel: model.fable ?? "",
    claudeCustomModelOption: model.customOption ?? "",
    claudeCustomModelOptionName: model.customOptionName ?? "",
    claudeCustomModelOptionDescription: model.customOptionDescription ?? "",
  }).configText
}

/**
 * Decide the config text to persist for a config-management save. For a bound
 * Claude agent with VALID config JSON, rewrite the provider-controlled keys to be
 * provider-authoritative (see {@link applyClaudeProviderToConfigText}). Anything
 * else — non-Claude, unbound, or INVALID JSON — passes through unchanged. The
 * invalid-JSON passthrough is important: persistConfig must still surface the
 * parse error, otherwise patchImportantConfigText would silently recover the bad
 * text as `{}` and persist provider-derived config over the user's broken edits.
 */
export function configTextForClaudeSave(
  configText: string,
  agentType: AgentType,
  modelProviderId: number | null,
  provider: Pick<ModelProviderInfo, "api_url" | "api_key" | "model"> | undefined
): string {
  if (
    agentType === "claude_code" &&
    modelProviderId != null &&
    provider &&
    !parseConfigJsonText(configText).error
  ) {
    return applyClaudeProviderToConfigText(configText, provider)
  }
  return configText
}

/**
 * Set a Claude Code env flag to an explicit value inside the native config's
 * `env` (creating `env` if needed), preserving all other keys. Used by the
 * hardening toggles and their save-time materialization so the flag is always
 * written explicitly ("1"/"0") rather than left implicit/absent. Pure — shared
 * by the toggle handler, the save path, and tests.
 */
export function setClaudeEnvFlagInConfigText(
  configText: string,
  envKey: string,
  value: string
): { configText: string; recoveredFromInvalid: boolean } {
  const parseResult = parseConfigJsonText(configText)
  const config: Record<string, unknown> = parseResult.error
    ? {}
    : { ...parseResult.config }
  const env =
    typeof config.env === "object" && config.env && !Array.isArray(config.env)
      ? { ...(config.env as Record<string, unknown>) }
      : {}
  env[envKey] = value
  config.env = env
  return {
    configText: JSON.stringify(config, null, 2),
    recoveredFromInvalid: Boolean(parseResult.error),
  }
}

/**
 * Materialize both Claude hardening toggles into the native config `env` AND the
 * DB env overlay (envText), writing the explicit "1"/"0" per toggle so the shown
 * default positions are actually applied on save. Returns the inputs UNCHANGED
 * when `configText` is invalid JSON — never recover it here, or the caller's
 * merge diff would treat the recovered minimal config as authoritative and
 * delete every other on-disk key. Pure — shared by the save handler and tests.
 */
export function materializeClaudeHardeningFlags(
  configText: string,
  envText: string,
  flags: { sendAttributionHeader: boolean; disableNonessentialTraffic: boolean }
): { configText: string; envText: string } {
  if (parseConfigJsonText(configText).error) {
    return { configText, envText }
  }
  const entries: Array<[string, boolean]> = [
    [CLAUDE_ATTRIBUTION_HEADER_ENV_KEY, flags.sendAttributionHeader],
    [CLAUDE_NONESSENTIAL_TRAFFIC_ENV_KEY, flags.disableNonessentialTraffic],
  ]
  let nextConfig = configText
  let nextEnv = envText
  for (const [key, on] of entries) {
    const value = on ? CLAUDE_ENV_FLAG_ON : CLAUDE_ENV_FLAG_OFF
    nextConfig = setClaudeEnvFlagInConfigText(nextConfig, key, value).configText
    nextEnv = patchEnvText(nextEnv, { [key]: value })
  }
  return { configText: nextConfig, envText: nextEnv }
}
