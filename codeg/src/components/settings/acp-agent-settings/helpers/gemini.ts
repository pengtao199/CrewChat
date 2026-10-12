import { envFromConfig, findEnvValue, parseConfigJsonText } from "./config-text"
import { acpText } from "./context"
import { patchEnvText } from "./env"
import type { GeminiAuthMode, GeminiImportantValues } from "./types"

export const GEMINI_ENV_KEYS = {
  baseUrl: "GOOGLE_GEMINI_BASE_URL",
  legacyBaseUrl: "GEMINI_BASE_URL",
  geminiApiKey: "GEMINI_API_KEY",
  legacyGeminiApiKey: "GOOGLE_GEMINI_API_KEY",
  googleApiKey: "GOOGLE_API_KEY",
  cloudProject: "GOOGLE_CLOUD_PROJECT",
  cloudProjectLegacy: "GOOGLE_CLOUD_PROJECT_ID",
  cloudLocation: "GOOGLE_CLOUD_LOCATION",
  applicationCredentials: "GOOGLE_APPLICATION_CREDENTIALS",
  model: "GEMINI_MODEL",
} as const

export function inferGeminiAuthMode(values: {
  apiBaseUrl: string
  geminiApiKey: string
  googleApiKey: string
  googleCloudProject: string
  googleCloudLocation: string
  googleApplicationCredentials: string
}): GeminiAuthMode {
  if (values.apiBaseUrl.trim()) return "custom"
  if (values.geminiApiKey.trim()) return "gemini_api_key"
  if (values.googleApiKey.trim()) return "vertex_api_key"
  if (values.googleApplicationCredentials.trim())
    return "vertex_service_account"
  if (values.googleCloudProject.trim() || values.googleCloudLocation.trim()) {
    return "vertex_adc"
  }
  return "login_google"
}

export function extractGeminiImportantValues(
  env: Record<string, string>,
  configText: string
): GeminiImportantValues {
  const parseResult = parseConfigJsonText(configText)
  const config = parseResult.config
  const configEnv = envFromConfig(config)
  const mergedEnv = { ...env, ...configEnv }

  const apiBaseUrl = findEnvValue(mergedEnv, [
    GEMINI_ENV_KEYS.baseUrl,
    GEMINI_ENV_KEYS.legacyBaseUrl,
    "API_BASE_URL",
  ])
  const geminiApiKey = findEnvValue(mergedEnv, [
    GEMINI_ENV_KEYS.geminiApiKey,
    GEMINI_ENV_KEYS.legacyGeminiApiKey,
  ])
  const googleApiKey = findEnvValue(mergedEnv, [GEMINI_ENV_KEYS.googleApiKey])
  const googleCloudProject = findEnvValue(mergedEnv, [
    GEMINI_ENV_KEYS.cloudProject,
    GEMINI_ENV_KEYS.cloudProjectLegacy,
  ])
  const googleCloudLocation = findEnvValue(mergedEnv, [
    GEMINI_ENV_KEYS.cloudLocation,
  ])
  const googleApplicationCredentials = findEnvValue(mergedEnv, [
    GEMINI_ENV_KEYS.applicationCredentials,
  ])
  const model = findEnvValue(mergedEnv, [GEMINI_ENV_KEYS.model, "MODEL"])

  return {
    authMode: inferGeminiAuthMode({
      apiBaseUrl,
      geminiApiKey,
      googleApiKey,
      googleCloudProject,
      googleCloudLocation,
      googleApplicationCredentials,
    }),
    apiBaseUrl,
    geminiApiKey,
    googleApiKey,
    googleCloudProject,
    googleCloudLocation,
    googleApplicationCredentials,
    model: model ?? "",
  }
}

export function patchGeminiConfigText(
  configText: string,
  patch: {
    apiBaseUrl?: string
    model?: string
    geminiApiKey?: string
    googleApiKey?: string
    googleCloudProject?: string
    googleCloudLocation?: string
    googleApplicationCredentials?: string
  }
): {
  configText: string
  recoveredFromInvalid: boolean
} {
  const parseResult = parseConfigJsonText(configText)
  const config = parseResult.error ? {} : { ...parseResult.config }
  const env =
    typeof config.env === "object" && config.env && !Array.isArray(config.env)
      ? { ...(config.env as Record<string, unknown>) }
      : {}

  const assignOrRemoveEnv = (key: string, value: string | undefined) => {
    if (typeof value !== "string") return
    const trimmed = value.trim()
    if (!trimmed) {
      delete env[key]
      return
    }
    env[key] = trimmed
  }

  if (typeof patch.model === "string") {
    delete config.model
    delete config.model_name
    assignOrRemoveEnv(GEMINI_ENV_KEYS.model, patch.model)
  }
  assignOrRemoveEnv(GEMINI_ENV_KEYS.baseUrl, patch.apiBaseUrl)
  if (typeof patch.apiBaseUrl === "string") {
    assignOrRemoveEnv(GEMINI_ENV_KEYS.legacyBaseUrl, "")
  }
  assignOrRemoveEnv(GEMINI_ENV_KEYS.geminiApiKey, patch.geminiApiKey)
  assignOrRemoveEnv(GEMINI_ENV_KEYS.googleApiKey, patch.googleApiKey)
  if (typeof patch.geminiApiKey === "string") {
    assignOrRemoveEnv(GEMINI_ENV_KEYS.legacyGeminiApiKey, "")
  }
  if (typeof patch.googleCloudProject === "string") {
    const project = patch.googleCloudProject.trim()
    if (!project) {
      delete env[GEMINI_ENV_KEYS.cloudProject]
      delete env[GEMINI_ENV_KEYS.cloudProjectLegacy]
    } else {
      env[GEMINI_ENV_KEYS.cloudProject] = project
      delete env[GEMINI_ENV_KEYS.cloudProjectLegacy]
    }
  }
  assignOrRemoveEnv(GEMINI_ENV_KEYS.cloudLocation, patch.googleCloudLocation)
  assignOrRemoveEnv(
    GEMINI_ENV_KEYS.applicationCredentials,
    patch.googleApplicationCredentials
  )

  if (Object.keys(env).length === 0) {
    delete config.env
  } else {
    config.env = env
  }

  return {
    configText:
      Object.keys(config).length === 0 ? "" : JSON.stringify(config, null, 2),
    recoveredFromInvalid: Boolean(parseResult.error),
  }
}

export function patchGeminiEnvText(
  envText: string,
  patch: {
    apiBaseUrl?: string
    geminiApiKey?: string
    googleApiKey?: string
    googleCloudProject?: string
    googleCloudLocation?: string
    googleApplicationCredentials?: string
    model?: string
  }
): string {
  const envPatch: Record<string, string | undefined> = {}
  if (typeof patch.apiBaseUrl === "string") {
    envPatch[GEMINI_ENV_KEYS.baseUrl] = patch.apiBaseUrl
    envPatch[GEMINI_ENV_KEYS.legacyBaseUrl] = ""
  }
  if (typeof patch.geminiApiKey === "string") {
    envPatch[GEMINI_ENV_KEYS.geminiApiKey] = patch.geminiApiKey
    envPatch[GEMINI_ENV_KEYS.legacyGeminiApiKey] = ""
  }
  if (typeof patch.googleApiKey === "string") {
    envPatch[GEMINI_ENV_KEYS.googleApiKey] = patch.googleApiKey
  }
  if (typeof patch.googleCloudProject === "string") {
    envPatch[GEMINI_ENV_KEYS.cloudProject] = patch.googleCloudProject
    envPatch[GEMINI_ENV_KEYS.cloudProjectLegacy] = ""
  }
  if (typeof patch.googleCloudLocation === "string") {
    envPatch[GEMINI_ENV_KEYS.cloudLocation] = patch.googleCloudLocation
  }
  if (typeof patch.googleApplicationCredentials === "string") {
    envPatch[GEMINI_ENV_KEYS.applicationCredentials] =
      patch.googleApplicationCredentials
  }
  if (typeof patch.model === "string") {
    envPatch[GEMINI_ENV_KEYS.model] = patch.model
  }
  return patchEnvText(envText, envPatch)
}

export function patchGeminiAuthMode(
  current: GeminiImportantValues,
  mode: GeminiAuthMode
) {
  const next = {
    ...current,
    authMode: mode,
  }
  if (mode === "login_google") {
    next.apiBaseUrl = ""
    next.geminiApiKey = ""
    next.googleApiKey = ""
    next.googleCloudProject = ""
    next.googleCloudLocation = ""
    next.googleApplicationCredentials = ""
    return next
  }
  if (mode === "custom") {
    next.googleApiKey = ""
    next.googleCloudProject = ""
    next.googleCloudLocation = ""
    next.googleApplicationCredentials = ""
    return next
  }
  if (mode === "gemini_api_key") {
    next.apiBaseUrl = ""
    next.googleApiKey = ""
    next.googleCloudProject = ""
    next.googleCloudLocation = ""
    next.googleApplicationCredentials = ""
    return next
  }
  if (mode === "vertex_api_key") {
    next.apiBaseUrl = ""
    next.geminiApiKey = ""
    next.googleApplicationCredentials = ""
    return next
  }
  if (mode === "vertex_service_account") {
    next.apiBaseUrl = ""
    next.geminiApiKey = ""
    next.googleApiKey = ""
    return next
  }
  if (mode === "model_provider") {
    next.googleCloudProject = ""
    next.googleCloudLocation = ""
    next.googleApplicationCredentials = ""
    return next
  }
  next.apiBaseUrl = ""
  next.geminiApiKey = ""
  next.googleApiKey = ""
  next.googleApplicationCredentials = ""
  return next
}

export function geminiAuthModeLabel(mode: GeminiAuthMode): string {
  if (mode === "custom")
    return acpText("authModeCustomEndpoint", "Custom Endpoint")
  if (mode === "login_google")
    return acpText("gemini.mode.loginGoogle", "Google Login (OAuth)")
  if (mode === "gemini_api_key") return "Gemini API Key"
  if (mode === "vertex_adc") return "Vertex AI (ADC)"
  if (mode === "vertex_service_account")
    return acpText(
      "gemini.mode.vertexServiceAccount",
      "Vertex AI (Service Account)"
    )
  if (mode === "model_provider")
    return acpText("authModeModelProvider", "Model Provider")
  return "Vertex AI API Key"
}

export function geminiAuthModeHint(mode: GeminiAuthMode): string {
  if (mode === "custom") {
    return acpText(
      "gemini.hint.custom",
      "Fill API URL, API Key and Model, mapped to GOOGLE_GEMINI_BASE_URL / GEMINI_API_KEY / GEMINI_MODEL."
    )
  }
  if (mode === "login_google") {
    return acpText(
      "gemini.hint.loginGoogle",
      "Run gemini in terminal and complete Google login first; API key is not required."
    )
  }
  if (mode === "gemini_api_key") {
    return acpText(
      "gemini.hint.geminiApiKey",
      "Fill GEMINI_API_KEY when using Gemini API."
    )
  }
  if (mode === "vertex_adc") {
    return acpText(
      "gemini.hint.vertexAdc",
      "Use gcloud ADC; GOOGLE_CLOUD_PROJECT and GOOGLE_CLOUD_LOCATION are recommended."
    )
  }
  if (mode === "vertex_service_account") {
    return acpText(
      "gemini.hint.vertexServiceAccount",
      "Set service account JSON path to GOOGLE_APPLICATION_CREDENTIALS."
    )
  }
  if (mode === "model_provider") {
    return acpText(
      "modelProviderHint",
      "Use API URL and API Key from a configured model provider."
    )
  }
  return acpText(
    "gemini.hint.vertexApiKey",
    "Fill GOOGLE_API_KEY when using Vertex AI API key."
  )
}
