import type { AgentType } from "@/lib/types"

import {
  CLAUDE_ATTRIBUTION_HEADER_ENV_KEY,
  CLAUDE_DISABLE_NONESSENTIAL_TRAFFIC_DEFAULT,
  CLAUDE_ENV_FLAG_ON,
  CLAUDE_NONESSENTIAL_TRAFFIC_ENV_KEY,
  CLAUDE_SEND_ATTRIBUTION_HEADER_DEFAULT,
} from "./claude"
import {
  envFromConfig,
  findEnvValue,
  parseConfigJsonText,
  pickFirstString,
} from "./config-text"
import { patchEnvText } from "./env"
import { GEMINI_ENV_KEYS } from "./gemini"
import { CLAUDE_MODEL_ENV_KEYS } from "./types"
import type { ImportantConfigKey, ImportantEnvKeys } from "./types"

export function importantEnvKeysByAgent(
  agentType: AgentType
): ImportantEnvKeys {
  if (agentType === "claude_code") {
    return {
      apiBaseUrl: ["ANTHROPIC_BASE_URL", "OPENAI_BASE_URL", "API_BASE_URL"],
      apiKey: ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "OPENAI_API_KEY"],
      model: ["ANTHROPIC_MODEL", "OPENAI_MODEL", "MODEL"],
    }
  }
  if (agentType === "gemini") {
    return {
      apiBaseUrl: ["GOOGLE_GEMINI_BASE_URL", "GEMINI_BASE_URL", "API_BASE_URL"],
      apiKey: [
        GEMINI_ENV_KEYS.geminiApiKey,
        GEMINI_ENV_KEYS.googleApiKey,
        GEMINI_ENV_KEYS.legacyGeminiApiKey,
        "API_KEY",
      ],
      model: ["GEMINI_MODEL", "MODEL"],
    }
  }
  if (agentType === "grok") {
    // Grok's non-interactive credential is XAI_API_KEY (mirrors the backend
    // `agent_env_keys(Grok)`). Model/endpoint have working env overrides too:
    // GROK_DEFAULT_MODEL and GROK_XAI_API_BASE_URL (both read by the Grok binary;
    // XAI_API_BASE_URL is also accepted). XAI_MODEL is NOT read by Grok.
    return {
      apiBaseUrl: ["GROK_XAI_API_BASE_URL", "XAI_API_BASE_URL", "API_BASE_URL"],
      // Only XAI_API_KEY is a real Grok credential; the generic API_KEY alias is
      // NOT read by Grok, so including it would let the auth panel report
      // "configured" for a key the agent never uses.
      apiKey: ["XAI_API_KEY"],
      model: ["GROK_DEFAULT_MODEL", "MODEL"],
    }
  }
  if (agentType === "deepseek") {
    // The endpoint knob is DEEPSEEK_BASE_URL (read per request by the
    // `llm-deepseek` adapter through the launch-environment snapshot, which
    // falls back to `process.env`). DEEPSEEK_ACP_PROVIDER is NOT it — that's
    // the provider ROUTE id, so binding a model provider to it would write a
    // URL into a registry key. Mirrors the backend `agent_env_keys(DeepSeek)`;
    // generic OPENAI_*/API_KEY aliases are NOT read.
    return {
      apiBaseUrl: ["DEEPSEEK_BASE_URL"],
      apiKey: ["DEEPSEEK_API_KEY"],
      model: ["DEEPSEEK_ACP_MODEL"],
    }
  }
  if (agentType === "qoder") {
    // `QODER_PERSONAL_ACCESS_TOKEN` is Qoder's non-interactive credential
    // ("设置后自动使用 PAT 认证" in the CLI package's own README) and the only
    // way to authenticate a headless/server/Docker install, where the
    // `qoder login` browser flow cannot run. `QODER_MODEL` is the env twin of
    // `-m/--model`. Qoder talks only to its own service, so there is no
    // endpoint var at all — an EMPTY list here hides that field rather than
    // offering a box whose value nothing reads (see `importantFieldsFor`).
    // Generic OPENAI_*/API_KEY aliases are deliberately absent: Qoder reads
    // neither, and listing them would let the panel report "configured" off a
    // key that never reaches it. Mirrors the backend `agent_env_keys(Qoder)`.
    return {
      apiBaseUrl: [],
      apiKey: ["QODER_PERSONAL_ACCESS_TOKEN"],
      model: ["QODER_MODEL"],
    }
  }
  return {
    apiBaseUrl: ["OPENAI_BASE_URL", "API_BASE_URL"],
    apiKey: ["OPENAI_API_KEY", "API_KEY"],
    model: ["OPENAI_MODEL", "MODEL"],
  }
}

/**
 * Which of the three generic env fields this agent actually has a variable for.
 *
 * An empty list in {@link importantEnvKeysByAgent} means "this agent reads no
 * env var for that slot" — Qoder, for instance, talks only to its own service
 * and has no endpoint override. Rendering the input anyway offers a box whose
 * value nothing will ever read, and (before `patchEnvByImportantKey` guarded
 * it) wrote the typed value to an env var literally named `undefined`.
 */
export function importantFieldsFor(agentType: AgentType): {
  apiBaseUrl: boolean
  apiKey: boolean
  model: boolean
} {
  const keys = importantEnvKeysByAgent(agentType)
  return {
    apiBaseUrl: keys.apiBaseUrl.length > 0,
    apiKey: keys.apiKey.length > 0,
    model: keys.model.length > 0,
  }
}

export function extractImportantConfigValues(
  agentType: AgentType,
  env: Record<string, string>,
  configText: string
): {
  apiBaseUrl: string
  apiKey: string
  model: string
  claudeMainModel: string
  claudeReasoningModel: string
  claudeDefaultHaikuModel: string
  claudeDefaultSonnetModel: string
  claudeDefaultOpusModel: string
  claudeDefaultFableModel: string
  claudeCustomModelOption: string
  claudeCustomModelOptionName: string
  claudeCustomModelOptionDescription: string
  claudeSendAttributionHeader: boolean
  claudeDisableNonessentialTraffic: boolean
  configError: string | null
} {
  const parseResult = parseConfigJsonText(configText)
  const config = parseResult.config
  const keys = importantEnvKeysByAgent(agentType)

  const configEnv = envFromConfig(config)
  const mergedEnv = { ...env, ...configEnv }

  const apiBaseUrl =
    pickFirstString(config, ["apiBaseUrl", "api_base_url"]) ??
    findEnvValue(mergedEnv, keys.apiBaseUrl)
  const apiKey =
    pickFirstString(config, ["apiKey", "api_key"]) ??
    findEnvValue(mergedEnv, keys.apiKey)
  const model =
    pickFirstString(config, ["model", "model_name"]) ??
    findEnvValue(mergedEnv, keys.model)
  const claudeMainModel = findEnvValue(mergedEnv, [
    CLAUDE_MODEL_ENV_KEYS.claudeMainModel,
  ])
  const claudeReasoningModel = findEnvValue(mergedEnv, [
    CLAUDE_MODEL_ENV_KEYS.claudeReasoningModel,
  ])
  const claudeDefaultHaikuModel = findEnvValue(mergedEnv, [
    CLAUDE_MODEL_ENV_KEYS.claudeDefaultHaikuModel,
  ])
  const claudeDefaultSonnetModel = findEnvValue(mergedEnv, [
    CLAUDE_MODEL_ENV_KEYS.claudeDefaultSonnetModel,
  ])
  const claudeDefaultOpusModel = findEnvValue(mergedEnv, [
    CLAUDE_MODEL_ENV_KEYS.claudeDefaultOpusModel,
  ])
  const claudeDefaultFableModel = findEnvValue(mergedEnv, [
    CLAUDE_MODEL_ENV_KEYS.claudeDefaultFableModel,
  ])
  const claudeCustomModelOption = findEnvValue(mergedEnv, [
    CLAUDE_MODEL_ENV_KEYS.claudeCustomModelOption,
  ])
  const claudeCustomModelOptionName = findEnvValue(mergedEnv, [
    CLAUDE_MODEL_ENV_KEYS.claudeCustomModelOptionName,
  ])
  const claudeCustomModelOptionDescription = findEnvValue(mergedEnv, [
    CLAUDE_MODEL_ENV_KEYS.claudeCustomModelOptionDescription,
  ])

  // Present in env → on iff value is "1"; absent → the toggle's default.
  const attributionRaw = findEnvValue(mergedEnv, [
    CLAUDE_ATTRIBUTION_HEADER_ENV_KEY,
  ])
  const claudeSendAttributionHeader =
    agentType === "claude_code"
      ? attributionRaw
        ? attributionRaw === CLAUDE_ENV_FLAG_ON
        : CLAUDE_SEND_ATTRIBUTION_HEADER_DEFAULT
      : false
  const nonessentialRaw = findEnvValue(mergedEnv, [
    CLAUDE_NONESSENTIAL_TRAFFIC_ENV_KEY,
  ])
  const claudeDisableNonessentialTraffic =
    agentType === "claude_code"
      ? nonessentialRaw
        ? nonessentialRaw === CLAUDE_ENV_FLAG_ON
        : CLAUDE_DISABLE_NONESSENTIAL_TRAFFIC_DEFAULT
      : false

  return {
    apiBaseUrl: apiBaseUrl ?? "",
    apiKey: apiKey ?? "",
    model: model ?? "",
    claudeMainModel: agentType === "claude_code" ? (claudeMainModel ?? "") : "",
    claudeReasoningModel:
      agentType === "claude_code" ? claudeReasoningModel : "",
    claudeDefaultHaikuModel:
      agentType === "claude_code" ? claudeDefaultHaikuModel : "",
    claudeDefaultSonnetModel:
      agentType === "claude_code" ? claudeDefaultSonnetModel : "",
    claudeDefaultOpusModel:
      agentType === "claude_code" ? claudeDefaultOpusModel : "",
    claudeDefaultFableModel:
      agentType === "claude_code" ? claudeDefaultFableModel : "",
    claudeCustomModelOption:
      agentType === "claude_code" ? claudeCustomModelOption : "",
    claudeCustomModelOptionName:
      agentType === "claude_code" ? claudeCustomModelOptionName : "",
    claudeCustomModelOptionDescription:
      agentType === "claude_code" ? claudeCustomModelOptionDescription : "",
    claudeSendAttributionHeader,
    claudeDisableNonessentialTraffic,
    configError: parseResult.error,
  }
}

export function patchEnvByImportantKey(
  agentType: AgentType,
  envText: string,
  key: ImportantConfigKey,
  value: string
): string {
  const keys = importantEnvKeysByAgent(agentType)
  // The FIRST key of each list is the one codeg writes; the rest are aliases it
  // only reads. An agent that has no env var for a slot leaves that list empty,
  // and `[0]` is then `undefined` — which `patchEnvText` would happily write as
  // an env var literally named `undefined`, silently swallowing what the user
  // typed. `writeKey` turns that into a no-op instead; the field is also hidden
  // (see `importantFieldsFor`), so this is the belt to that suspenders.
  const writeKey = (candidates: string[]): string | undefined => candidates[0]
  const patch = (candidates: string[]): string => {
    const target = writeKey(candidates)
    return target ? patchEnvText(envText, { [target]: value }) : envText
  }
  if (key === "apiBaseUrl") {
    return patch(keys.apiBaseUrl)
  }
  if (key === "apiKey") {
    return patch(keys.apiKey)
  }
  if (key === "model") {
    return patch(keys.model)
  }
  return patchEnvText(envText, { [CLAUDE_MODEL_ENV_KEYS[key]]: value })
}
