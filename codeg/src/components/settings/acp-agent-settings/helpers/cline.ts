import { parseConfigJsonText } from "./config-text"
import { CLINE_SIGNIN_PROVIDERS } from "./types"
import type { ClineImportantValues, ClineProvider } from "./types"

/** Whether cline owns this provider's credential (OAuth), rather than codeg.
 *  Mirrors the backend `cline_provider_is_agent_managed`. */
export function isClineSignInProvider(provider: string): boolean {
  return CLINE_SIGNIN_PROVIDERS.some((p) => p.value === provider)
}

/** The terminal command that starts (or repairs) a sign-in. cline drives a
 *  device-code flow: it prints a code and an `authkit.cline.bot/device` URL and
 *  waits for the browser half, which is why this is a command to run rather
 *  than a button to press. */
export function clineAuthCommand(provider: string): string {
  return `cline auth ${provider}`
}

/** Mirrors the backend `normalize_cline_provider_id`, so a legacy `"openai"`
 * typed into the advanced JSON editor still selects a row instead of leaving the
 * provider dropdown blank. */
export function normalizeClineProvider(provider: string): ClineProvider {
  return provider === "openai" ? "openai-compatible" : provider
}

export function extractClineImportantValues(
  configText: string
): ClineImportantValues {
  const parseResult = parseConfigJsonText(configText)
  const config = parseResult.config
  return {
    provider: normalizeClineProvider(
      typeof config.apiProvider === "string" && config.apiProvider
        ? config.apiProvider
        : "anthropic"
    ),
    apiKey: typeof config.apiKey === "string" ? config.apiKey : "",
    model: typeof config.model === "string" ? config.model : "",
    baseUrl: typeof config.apiBaseUrl === "string" ? config.apiBaseUrl : "",
  }
}
