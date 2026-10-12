import { toErrorMessage } from "@/lib/app-error"

import { acpText } from "./context"
import type { ConfigParseResult } from "./types"

export function parseConfigJsonText(configText: string): ConfigParseResult {
  const trimmed = configText.trim()
  if (!trimmed) return { config: {}, error: null }

  try {
    const parsed = JSON.parse(trimmed) as unknown
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {
        config: {},
        error: acpText(
          "errors.nativeJsonMustBeObject",
          "Native JSON config must be an object"
        ),
      }
    }
    return { config: parsed as Record<string, unknown>, error: null }
  } catch (err) {
    const message = toErrorMessage(err)
    return {
      config: {},
      error: acpText(
        "errors.nativeJsonInvalid",
        "Native JSON config format error: {message}",
        { message }
      ),
    }
  }
}

export function asObjectRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null
  return value as Record<string, unknown>
}

export function envFromConfig(
  config: Record<string, unknown>
): Record<string, string> {
  const raw = config.env
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    return {}
  }

  const map: Record<string, string> = {}
  for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof value !== "string") continue
    const trimmedKey = key.trim()
    const trimmedValue = value.trim()
    if (!trimmedKey || !trimmedValue) continue
    map[trimmedKey] = trimmedValue
  }
  return map
}

export function pickFirstString(
  source: Record<string, unknown>,
  keys: string[]
): string | null {
  for (const key of keys) {
    const value = source[key]
    if (typeof value !== "string") continue
    const trimmed = value.trim()
    if (trimmed) return trimmed
  }
  return null
}

export function findEnvValue(
  env: Record<string, string>,
  keys: string[]
): string {
  for (const key of keys) {
    const value = env[key]
    if (!value) continue
    const trimmed = value.trim()
    if (trimmed) return trimmed
  }
  return ""
}

/**
 * Compare original and current config objects. For any key present in
 * original but missing in current, set it to `null` in the result so
 * the backend merge can delete it from the file on disk.
 */
export function markRemovedKeysNull(
  original: Record<string, unknown>,
  current: Record<string, unknown>
): Record<string, unknown> {
  const result: Record<string, unknown> = { ...current }
  for (const key of Object.keys(original)) {
    if (!(key in result)) {
      result[key] = null
    } else if (
      original[key] &&
      typeof original[key] === "object" &&
      !Array.isArray(original[key]) &&
      result[key] &&
      typeof result[key] === "object" &&
      !Array.isArray(result[key])
    ) {
      result[key] = markRemovedKeysNull(
        original[key] as Record<string, unknown>,
        result[key] as Record<string, unknown>
      )
    }
  }
  return result
}

/**
 * Build the `config_json` payload for a merge-strategy agent save (Claude Code /
 * Gemini / OpenClaw). Diffs the current config against the original so removed
 * keys become explicit `null`s the backend merge deletes from disk — crucially
 * even when the current config emptied to "" (e.g. the last env flag toggled
 * off), which would otherwise serialize to a null `config_json` no-op and leave
 * the stale key on disk. Returns `null` when both sides are empty (nothing to
 * write, no empty file created). Pure — shared by `persistConfig` and tests.
 */
export function buildMergeConfigPayload(
  currentConfigText: string,
  originalConfigText: string | null | undefined
): string | null {
  const currentConfig = parseConfigJsonText(currentConfigText).config
  const originalConfig = originalConfigText
    ? parseConfigJsonText(originalConfigText).config
    : {}
  if (
    Object.keys(currentConfig).length === 0 &&
    Object.keys(originalConfig).length === 0
  ) {
    return null
  }
  return JSON.stringify(
    markRemovedKeysNull(originalConfig, currentConfig),
    null,
    2
  )
}

export function normalizeConfigText(configText: string): string {
  const parseResult = parseConfigJsonText(configText)
  if (parseResult.error) return configText.trim()
  if (Object.keys(parseResult.config).length === 0) return ""
  return JSON.stringify(parseResult.config, null, 2)
}
