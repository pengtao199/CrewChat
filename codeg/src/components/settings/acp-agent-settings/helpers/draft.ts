import type { AcpAgentInfo } from "@/lib/types"
import { parseCodexModelConfig } from "@/lib/types"

import { extractClineImportantValues } from "./cline"
import {
  CODEX_GRANULAR_DEFAULT,
  codexSandboxBaselineOf,
  extractCodexImportantValues,
  inferCodexAuthMode,
} from "./codex"
import { updateTomlRootBooleanKey } from "./codex-toml"
import { envMapToText, patchEnvText } from "./env"
import { extractGeminiImportantValues } from "./gemini"
import { GROK_DEFAULT_API_BACKEND, inferGrokMode } from "./grok"
import { parseHermesConfig } from "./hermes"
import { extractImportantConfigValues } from "./important-config"
import { extractOpenClawImportantValues } from "./openclaw"
import { extractOpenCodeConfigValues } from "./opencode"
import { CODEX_SANDBOX_UNSET } from "./types"
import type {
  AgentDraft,
  CodexApprovalPolicyChoice,
  CodexAuthMode,
  CodexSandboxDraftFields,
  CodexSandboxModeChoice,
  GrokAuthMethod,
  ImportantConfigKey,
  ImportantDraftPatch,
} from "./types"

export function applyImportantFieldToDraft(
  draft: AgentDraft,
  key: ImportantConfigKey,
  value: string
): AgentDraft {
  if (key === "apiBaseUrl") return { ...draft, apiBaseUrl: value }
  if (key === "apiKey") return { ...draft, apiKey: value }
  if (key === "model") return { ...draft, model: value }
  if (key === "claudeMainModel") return { ...draft, claudeMainModel: value }
  if (key === "claudeReasoningModel") {
    return { ...draft, claudeReasoningModel: value }
  }
  if (key === "claudeDefaultHaikuModel") {
    return { ...draft, claudeDefaultHaikuModel: value }
  }
  if (key === "claudeDefaultSonnetModel") {
    return { ...draft, claudeDefaultSonnetModel: value }
  }
  if (key === "claudeDefaultOpusModel") {
    return { ...draft, claudeDefaultOpusModel: value }
  }
  if (key === "claudeDefaultFableModel") {
    return { ...draft, claudeDefaultFableModel: value }
  }
  if (key === "claudeCustomModelOption") {
    return { ...draft, claudeCustomModelOption: value }
  }
  if (key === "claudeCustomModelOptionName") {
    return { ...draft, claudeCustomModelOptionName: value }
  }
  return { ...draft, claudeCustomModelOptionDescription: value }
}

export function buildImportantPatchFromDraft(
  draft: AgentDraft
): ImportantDraftPatch {
  return {
    apiBaseUrl: draft.apiBaseUrl,
    apiKey: draft.apiKey,
    model: draft.model,
    claudeMainModel: draft.claudeMainModel,
    claudeReasoningModel: draft.claudeReasoningModel,
    claudeDefaultHaikuModel: draft.claudeDefaultHaikuModel,
    claudeDefaultSonnetModel: draft.claudeDefaultSonnetModel,
    claudeDefaultOpusModel: draft.claudeDefaultOpusModel,
    claudeDefaultFableModel: draft.claudeDefaultFableModel,
    claudeCustomModelOption: draft.claudeCustomModelOption,
    claudeCustomModelOptionName: draft.claudeCustomModelOptionName,
    claudeCustomModelOptionDescription:
      draft.claudeCustomModelOptionDescription,
  }
}

export function buildAgentDraft(agent: AcpAgentInfo): AgentDraft {
  const configText =
    typeof agent.config_json === "string" && agent.config_json.trim()
      ? agent.config_json
      : ""
  const hermesValues =
    agent.agent_type === "hermes" ? parseHermesConfig(configText) : null
  const openCodeAuthJsonText = agent.opencode_auth_json ?? ""
  const codexAuthJsonText = agent.codex_auth_json ?? ""
  const codexConfigTomlText =
    agent.agent_type === "codex"
      ? updateTomlRootBooleanKey(
          agent.codex_config_toml ?? "",
          "disable_response_storage",
          true
        )
      : (agent.codex_config_toml ?? "")
  const codexSandbox = agent.codex_sandbox_settings ?? null
  // Seeded once, then fingerprinted, so a save can tell a real control change
  // from "untouched, still whatever config.toml says".
  const codexSandboxFields: CodexSandboxDraftFields = {
    // The granular table and the string presets are mutually exclusive upstream,
    // so a present table always wins the selector.
    codexApprovalPolicy: codexSandbox?.granular
      ? "granular"
      : ((codexSandbox?.approval_policy ??
          CODEX_SANDBOX_UNSET) as CodexApprovalPolicyChoice),
    codexGranular: codexSandbox?.granular ?? CODEX_GRANULAR_DEFAULT,
    codexSandboxMode: (codexSandbox?.sandbox_mode ??
      CODEX_SANDBOX_UNSET) as CodexSandboxModeChoice,
    codexWritableRootsText: (
      codexSandbox?.workspace_write.writable_roots ?? []
    ).join("\n"),
    codexNetworkAccess: codexSandbox?.workspace_write.network_access ?? false,
    codexExcludeTmpdirEnvVar:
      codexSandbox?.workspace_write.exclude_tmpdir_env_var ?? false,
    codexExcludeSlashTmp:
      codexSandbox?.workspace_write.exclude_slash_tmp ?? false,
  }
  const grokConfigTomlText = agent.grok_config_toml ?? ""
  const grokPermissionMode = agent.grok_settings?.permission_mode ?? ""
  const grokReasoningEffort =
    agent.grok_settings?.default_reasoning_effort ?? ""
  const important = extractImportantConfigValues(
    agent.agent_type,
    agent.env,
    configText
  )
  const geminiImportant = extractGeminiImportantValues(agent.env, configText)
  const openClawImportant = extractOpenClawImportantValues(
    agent.env,
    configText
  )
  const codexImportant = extractCodexImportantValues(
    codexAuthJsonText,
    codexConfigTomlText
  )
  const openCodeImportant = extractOpenCodeConfigValues(
    configText,
    openCodeAuthJsonText
  )
  const clineImportant = extractClineImportantValues(configText)
  const codexAuthMode: CodexAuthMode =
    agent.agent_type === "codex" && agent.model_provider_id != null
      ? "model_provider"
      : agent.agent_type === "codex"
        ? inferCodexAuthMode(codexAuthJsonText)
        : "api_key"
  const grokAuthMode: GrokAuthMethod =
    agent.agent_type === "grok"
      ? inferGrokMode(
          agent.env,
          Boolean(agent.grok_settings?.custom_model_id?.trim())
        )
      : "api_key"
  const rawEnvText = envMapToText(agent.env)
  // When codex is in official subscription mode, clean up API keys/URLs from env.
  // Grok mirrors this: record the auth-method knob, and in subscription mode
  // strip XAI_API_KEY so the editable env can't override the `grok login`
  // credential (the launch path enforces the same — see apply_grok_env_policy).
  const envText =
    agent.agent_type === "codex" && codexAuthMode === "chatgpt_subscription"
      ? patchEnvText(rawEnvText, {
          OPENAI_API_KEY: "",
          OPENAI_BASE_URL: "",
        })
      : agent.agent_type === "grok"
        ? patchEnvText(rawEnvText, {
            GROK_AUTH_MODE: grokAuthMode,
            ...(grokAuthMode === "subscription" ? { XAI_API_KEY: "" } : {}),
          })
        : rawEnvText
  return {
    enabled: agent.enabled,
    envText,
    configText,
    apiBaseUrl:
      agent.agent_type === "hermes"
        ? (hermesValues?.baseUrl ?? "")
        : agent.agent_type === "codex"
          ? codexImportant.apiBaseUrl
          : agent.agent_type === "gemini"
            ? geminiImportant.apiBaseUrl
            : important.apiBaseUrl,
    apiKey:
      agent.agent_type === "hermes"
        ? (hermesValues?.apiKey ?? "")
        : agent.agent_type === "codex"
          ? (codexImportant.apiKey ?? "")
          : agent.agent_type === "gemini"
            ? geminiImportant.geminiApiKey || geminiImportant.googleApiKey
            : important.apiKey,
    model:
      agent.agent_type === "hermes"
        ? (hermesValues?.model ?? "")
        : agent.agent_type === "codex"
          ? codexImportant.model
          : agent.agent_type === "gemini"
            ? geminiImportant.model
            : agent.agent_type === "open_code"
              ? openCodeImportant.model
              : important.model,
    claudeAuthMode:
      agent.agent_type === "claude_code" && agent.model_provider_id != null
        ? "model_provider"
        : agent.agent_type === "claude_code" &&
            (important.apiBaseUrl || important.apiKey)
          ? "custom"
          : "official_subscription",
    modelProviderId: agent.model_provider_id ?? null,
    geminiAuthMode:
      agent.agent_type === "gemini" && agent.model_provider_id != null
        ? "model_provider"
        : geminiImportant.authMode,
    geminiApiKey: geminiImportant.geminiApiKey,
    googleApiKey: geminiImportant.googleApiKey,
    googleCloudProject: geminiImportant.googleCloudProject,
    googleCloudLocation: geminiImportant.googleCloudLocation,
    googleApplicationCredentials: geminiImportant.googleApplicationCredentials,
    codexAuthMode,
    codexModelProvider: codexImportant.modelProvider,
    codexProviderOptions: codexImportant.providerOptions,
    codexSupportsWebsockets: codexImportant.supportsWebsockets,
    codexSkills: codexImportant.skills,
    codexDefaultModeRequestUserInput:
      codexImportant.defaultModeRequestUserInput,
    codexServiceTierFast: codexImportant.serviceTierFast,
    ...codexSandboxFields,
    codexSandboxBaseline: codexSandboxBaselineOf(codexSandboxFields),
    codexSandboxShadowed:
      codexSandbox?.shadowed_by_default_permissions ?? false,
    codexSandboxHasPermissionsTable:
      codexSandbox?.has_permissions_table ?? false,
    claudeMainModel: important.claudeMainModel,
    claudeReasoningModel: important.claudeReasoningModel,
    claudeDefaultHaikuModel: important.claudeDefaultHaikuModel,
    claudeDefaultSonnetModel: important.claudeDefaultSonnetModel,
    claudeDefaultOpusModel: important.claudeDefaultOpusModel,
    claudeDefaultFableModel: important.claudeDefaultFableModel,
    claudeCustomModelOption: important.claudeCustomModelOption,
    claudeCustomModelOptionName: important.claudeCustomModelOptionName,
    claudeCustomModelOptionDescription:
      important.claudeCustomModelOptionDescription,
    claudeSendAttributionHeader: important.claudeSendAttributionHeader,
    claudeDisableNonessentialTraffic:
      important.claudeDisableNonessentialTraffic,
    codexAuthJsonText,
    codexConfigTomlText,
    codexModelList: parseCodexModelConfig(agent.codex_model_catalog ?? null),
    grokConfigTomlText,
    grokAuthMode,
    grokPermissionMode,
    grokReasoningEffort,
    grokCustomModelId: agent.grok_settings?.custom_model_id ?? "",
    grokCustomBaseUrl: agent.grok_settings?.custom_base_url ?? "",
    grokCustomApiKey: agent.grok_settings?.custom_api_key ?? "",
    grokCustomApiBackend:
      agent.grok_settings?.custom_api_backend ?? GROK_DEFAULT_API_BACKEND,
    grokCustomContextWindow:
      agent.grok_settings?.custom_context_window != null
        ? String(agent.grok_settings.custom_context_window)
        : "",
    grokAutoCompactThreshold:
      agent.grok_settings?.auto_compact_threshold_percent != null
        ? String(agent.grok_settings.auto_compact_threshold_percent)
        : "",
    openCodeAuthJsonText,
    openClawGatewayUrl: openClawImportant.gatewayUrl,
    openClawGatewayToken: openClawImportant.gatewayToken,
    openClawSessionKey: openClawImportant.sessionKey,
    clineProvider: clineImportant.provider,
    clineApiKey: clineImportant.apiKey,
    clineModel: clineImportant.model,
    clineBaseUrl: clineImportant.baseUrl,
    hermesProvider: hermesValues?.provider ?? "openrouter",
    hermesConfigYaml: agent.hermes_config_yaml ?? "",
    hermesHome: hermesValues?.hermesHome ?? "",
    hermesSetupCommand: hermesValues?.setupCommand ?? "",
    hermesModelCommand: hermesValues?.modelCommand ?? "",
  }
}
