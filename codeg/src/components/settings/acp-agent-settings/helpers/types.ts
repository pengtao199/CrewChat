// [crewchat] Split from acp-agent-settings.tsx (structural split,
// batch 2). Types and their companion enums/values for the CLI settings page.
import type {
  CheckStatus,
  CodexGranularApproval,
  FixAction,
  PreflightResult,
} from "@/lib/types"
import { CodexModelConfig } from "@/lib/types"

export interface AgentCheckState {
  result?: PreflightResult
  error?: string
}

export const CLAUDE_AUTH_MODES = [
  "official_subscription",
  "custom",
  "model_provider",
] as const

export type ClaudeAuthMode = (typeof CLAUDE_AUTH_MODES)[number]

export interface AgentDraft {
  enabled: boolean
  envText: string
  configText: string
  apiBaseUrl: string
  apiKey: string
  model: string
  claudeAuthMode: ClaudeAuthMode
  modelProviderId: number | null
  geminiAuthMode: GeminiAuthMode
  geminiApiKey: string
  googleApiKey: string
  googleCloudProject: string
  googleCloudLocation: string
  googleApplicationCredentials: string
  codexAuthMode: CodexAuthMode
  codexModelProvider: string
  codexProviderOptions: string[]
  codexSupportsWebsockets: boolean
  codexSkills: boolean
  /** `[features].default_mode_request_user_input` — see
   * {@link CODEX_DEFAULT_MODE_REQUEST_USER_INPUT_KEY}. */
  codexDefaultModeRequestUserInput: boolean
  codexServiceTierFast: boolean
  /** Sandbox / approval group — the thread defaults codex applies to turns it
   * starts itself (`/goal`, `/review`, `/compact`). Held as plain draft state
   * (not derived from `codexConfigTomlText`) and merged into config.toml
   * server-side on save. */
  codexApprovalPolicy: CodexApprovalPolicyChoice
  codexGranular: CodexGranularApproval
  codexSandboxMode: CodexSandboxModeChoice
  /** `writable_roots`, one absolute path per line. */
  codexWritableRootsText: string
  codexNetworkAccess: boolean
  codexExcludeTmpdirEnvVar: boolean
  codexExcludeSlashTmp: boolean
  /** The sandbox group as it was read off disk. A save sends only the fields
   * that differ from this, so neither the raw config.toml editor nor an
   * untouched control can revert the other. */
  codexSandboxBaseline: CodexSandboxBaseline
  /** Read-only diagnostics from the backend projection: `default_permissions`
   * makes codex ignore `sandbox_mode` entirely. */
  codexSandboxShadowed: boolean
  codexSandboxHasPermissionsTable: boolean
  claudeMainModel: string
  claudeReasoningModel: string
  claudeDefaultHaikuModel: string
  claudeDefaultSonnetModel: string
  claudeDefaultOpusModel: string
  claudeDefaultFableModel: string
  claudeCustomModelOption: string
  claudeCustomModelOptionName: string
  claudeCustomModelOptionDescription: string
  // Claude Code hardening toggles (native config `env`). `claudeSendAttributionHeader`
  // → CLAUDE_CODE_ATTRIBUTION_HEADER (on="1"/off="0"), default off (don't send).
  // `claudeDisableNonessentialTraffic` → CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC,
  // default on (disabled).
  claudeSendAttributionHeader: boolean
  claudeDisableNonessentialTraffic: boolean
  codexAuthJsonText: string
  codexConfigTomlText: string
  /** Structured codex custom-model list (mirrors the catalog source sidecar).
   *  Drives the model editor + `model_catalog_json` generation on save. */
  codexModelList: CodexModelConfig
  grokConfigTomlText: string
  // Grok authentication method (subscription via `grok login` vs XAI_API_KEY).
  // Recorded in env as GROK_AUTH_MODE; drives which credential body renders and
  // whether XAI_API_KEY is stripped from env on subscription.
  grokAuthMode: GrokAuthMethod
  // Grok structured controls (empty string = "unset / use default"). Backed by
  // ~/.grok/config.toml [ui].permission_mode / [models].default_reasoning_effort;
  // merged onto the current on-disk config server-side on save.
  grokPermissionMode: string
  grokReasoningEffort: string
  // Grok custom model (BYO endpoint) → [model.<id>] + [models].default. Numeric
  // fields are held as strings for their inputs and parsed on save.
  grokCustomModelId: string
  grokCustomBaseUrl: string
  grokCustomApiKey: string
  grokCustomApiBackend: string
  grokCustomContextWindow: string
  grokAutoCompactThreshold: string
  openCodeAuthJsonText: string
  openClawGatewayUrl: string
  openClawGatewayToken: string
  openClawSessionKey: string
  clineProvider: ClineProvider
  clineApiKey: string
  clineModel: string
  clineBaseUrl: string
  // Hermes — `apiKey`/`model`/`apiBaseUrl` are reused for the active provider's
  // key, model.default, and model.base_url. These carry the rest.
  hermesProvider: string
  hermesConfigYaml: string
  hermesHome: string
  hermesSetupCommand: string
  hermesModelCommand: string
}

export type RunningActionKind =
  | "download_binary"
  | "upgrade_binary"
  | "install_npx"
  | "upgrade_npx"
  | "uninstall_binary"
  | "uninstall_npx"
  | "redownload_binary"
  | "custom_install"
  | "upgrade_latest"
  | "install_uv"

export type UiFixAction =
  | FixAction
  | {
      label: string
      kind:
        | "download_binary"
        | "upgrade_binary"
        | "install_npx"
        | "upgrade_npx"
        | "uninstall_binary"
        | "uninstall_npx"
        | "install_opencode_plugins"
        | "custom_install"
        // Payload is the version to install, not the agent type.
        | "upgrade_latest"
      payload: string
      // When true, the fix renders as a greyed-out button (e.g. the uvx
      // agent-install action while the uv runtime isn't ready yet).
      disabled?: boolean
    }

export interface UiCheckItem {
  check_id: string
  label: string
  status: CheckStatus
  message: string
  fixes: UiFixAction[]
  /** Actions on a row of their own along the card's bottom edge, right-aligned
   * and never wrapped apart (see `renderCheck`). */
  footerFixes?: UiFixAction[]
}

export interface ImportantEnvKeys {
  apiBaseUrl: string[]
  apiKey: string[]
  model: string[]
}

export const CLAUDE_MODEL_ENV_KEYS = {
  claudeMainModel: "ANTHROPIC_MODEL",
  claudeReasoningModel: "ANTHROPIC_REASONING_MODEL",
  claudeDefaultHaikuModel: "ANTHROPIC_DEFAULT_HAIKU_MODEL",
  claudeDefaultSonnetModel: "ANTHROPIC_DEFAULT_SONNET_MODEL",
  claudeDefaultOpusModel: "ANTHROPIC_DEFAULT_OPUS_MODEL",
  claudeDefaultFableModel: "ANTHROPIC_DEFAULT_FABLE_MODEL",
  claudeCustomModelOption: "ANTHROPIC_CUSTOM_MODEL_OPTION",
  claudeCustomModelOptionName: "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME",
  claudeCustomModelOptionDescription:
    "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION",
} as const

export const GEMINI_AUTH_MODES = [
  "custom",
  "login_google",
  "gemini_api_key",
  "vertex_adc",
  "vertex_service_account",
  "vertex_api_key",
  "model_provider",
] as const

export type GeminiAuthMode = (typeof GEMINI_AUTH_MODES)[number]

/** Cline provider ids, as the CLI's own registry keys them (`cline auth -p`).
 *
 * `openai-compatible` is NOT interchangeable with `openai`: the `auth`
 * subcommand aliases the latter, but the ACP path does not, so an `openai`
 * selection reaches `session/new` as an unknown provider with an empty model
 * list. The backend's `normalize_cline_provider_id` maps the legacy value on
 * read so an existing config still lands on the right row here. */
/** Cline's own sign-in providers, labelled as its registry labels them. Their
 * credential is an OAuth token cline obtains through a device-code flow and
 * stores itself, so this panel shows the command that starts it rather than a
 * key field — and the backend neither overwrites those entries nor exports
 * `CLINE_PROVIDER`/`CLINE_API_KEY` for them (see `cline_provider_is_agent_managed`). */
export const CLINE_SIGNIN_PROVIDERS = [
  { value: "cline", label: "Cline Usage-Billing" },
  { value: "cline-pass", label: "ClinePass" },
  { value: "openai-codex", label: "OpenAI ChatGPT Subscription" },
] as const

/** Bring-your-own providers this panel can actually configure — every one is
 * authenticated by a single API key, which is all the three fields below can
 * express.
 *
 * Deliberately a subset of cline's ~50-provider registry. AWS Bedrock and GCP
 * Vertex used to be listed and never worked: cline authenticates them with
 * `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_REGION` and
 * `GOOGLE_VERTEX_PROJECT`/`GOOGLE_VERTEX_LOCATION`/`GOOGLE_APPLICATION_CREDENTIALS`
 * respectively, none of which a lone `apiKey` string can carry. They are
 * reachable through `cline auth bedrock`, and an entry made that way is read
 * back and preserved here rather than clobbered.
 *
 * `openai-compatible` is NOT interchangeable with `openai`: the `auth`
 * subcommand aliases the latter, but the ACP path does not, so an `openai`
 * selection reaches `session/new` as an unknown provider with an empty model
 * list. The backend's `normalize_cline_provider_id` maps the legacy value on
 * read so an existing config still lands on the right row here. */
export const CLINE_BYO_PROVIDERS = [
  { value: "anthropic", label: "Anthropic" },
  { value: "openai-native", label: "OpenAI" },
  { value: "openai-compatible", label: "OpenAI Compatible" },
  { value: "openrouter", label: "OpenRouter" },
  { value: "gemini", label: "Gemini" },
  { value: "deepseek", label: "DeepSeek" },
  { value: "ollama", label: "Ollama" },
  { value: "lmstudio", label: "LM Studio" },
] as const

export type ClineProvider =
  | (typeof CLINE_SIGNIN_PROVIDERS)[number]["value"]
  | (typeof CLINE_BYO_PROVIDERS)[number]["value"]
  // A provider configured outside codeg (`cline auth bedrock`, a registry id
  // this list does not carry) still has to select a row, or saving from this
  // panel would silently retarget the user's store at whatever the dropdown
  // fell back to.
  | (string & {})

export type ClaudeModelKey = keyof typeof CLAUDE_MODEL_ENV_KEYS

export type ImportantConfigKey =
  | "apiBaseUrl"
  | "apiKey"
  | "model"
  | ClaudeModelKey

export type ImportantDraftPatch = Partial<Pick<AgentDraft, ImportantConfigKey>>

export interface ConfigParseResult {
  config: Record<string, unknown>
  error: string | null
}

/** Grok's three authentication methods:
 *  - `subscription` — sign in with `grok login` (SuperGrok / X Premium+),
 *    whose session lives in `~/.grok/auth.json` (untouched by codeg);
 *  - `api_key` — a non-interactive XAI_API_KEY from the xAI console;
 *  - `custom` — a bring-your-own endpoint: a custom `[model.<id>]` in
 *    ~/.grok/config.toml with its own base_url/api_key (the custom-model card). */
export type GrokAuthMethod = "subscription" | "api_key" | "custom"

export interface GeminiImportantValues {
  authMode: GeminiAuthMode
  apiBaseUrl: string
  geminiApiKey: string
  googleApiKey: string
  googleCloudProject: string
  googleCloudLocation: string
  googleApplicationCredentials: string
  model: string
}

export interface OpenClawImportantValues {
  gatewayUrl: string
  gatewayToken: string
  sessionKey: string
}

export interface ClineImportantValues {
  provider: ClineProvider
  apiKey: string
  model: string
  baseUrl: string
}

export interface CodexTomlImportantValues {
  model: string
  modelProvider: string
  providerNames: string[]
  providerBaseUrls: Record<string, string>
  providerSupportsWebsockets: Record<string, boolean>
  featureResponsesWebsocketsV2: boolean
  featureSkills: boolean
  featureDefaultModeRequestUserInput: boolean
  serviceTierFast: boolean
}

export interface CodexImportantValues {
  apiBaseUrl: string
  apiKey: string | null
  model: string
  modelProvider: string
  providerOptions: string[]
  supportsWebsockets: boolean
  skills: boolean
  defaultModeRequestUserInput: boolean
  serviceTierFast: boolean
}

export const CODEX_AUTH_MODES = [
  "api_key",
  "chatgpt_subscription",
  "model_provider",
] as const

export type CodexAuthMode = (typeof CODEX_AUTH_MODES)[number]

/** The draft value meaning "leave the key out of config.toml", i.e. let codex
 * apply its own default. */
export const CODEX_SANDBOX_UNSET = ""

/** `approval_policy` choices. The three presets are `AskForApproval`'s plain
 * string variants; `granular` is its table variant and reveals five switches.
 * (`on-failure` is only a legacy serde alias of `on-request` upstream, so it is
 * normalized away by the backend rather than offered here.) */
export const CODEX_APPROVAL_POLICY_VALUES = [
  "on-request",
  "untrusted",
  "never",
  "granular",
] as const

export type CodexApprovalPolicyChoice =
  | typeof CODEX_SANDBOX_UNSET
  | (typeof CODEX_APPROVAL_POLICY_VALUES)[number]

/** `SandboxMode`'s complete upstream vocabulary. */
export const CODEX_SANDBOX_MODE_VALUES = [
  "read-only",
  "workspace-write",
  "danger-full-access",
] as const

export type CodexSandboxModeChoice =
  | typeof CODEX_SANDBOX_UNSET
  | (typeof CODEX_SANDBOX_MODE_VALUES)[number]

/** The draft slice the sandbox payload is derived from. */
export type CodexSandboxDraftFields = {
  codexApprovalPolicy: CodexApprovalPolicyChoice
  codexGranular: CodexGranularApproval
  codexSandboxMode: CodexSandboxModeChoice
  codexWritableRootsText: string
  codexNetworkAccess: boolean
  codexExcludeTmpdirEnvVar: boolean
  codexExcludeSlashTmp: boolean
}

/** The sandbox controls as they were read off disk, kept on the draft so a save
 * can send ONLY what the user actually moved. */
export type CodexSandboxBaseline = CodexSandboxDraftFields

export interface HermesDraftValues {
  provider: string
  model: string
  baseUrl: string
  apiKey: string
  hermesHome: string
  setupCommand: string
  modelCommand: string
}
