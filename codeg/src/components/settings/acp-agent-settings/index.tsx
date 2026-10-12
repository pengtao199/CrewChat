"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { Reorder } from "motion/react"
import { useLocale, useTranslations } from "next-intl"
import { useImeGuard } from "@/hooks/use-ime-guard"
import { useSearchParams } from "next/navigation"
import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Copy,
  Download,
  Eye,
  EyeOff,
  GripVertical,
  Loader2,
  Minus,
  PackagePlus,
  Pencil,
  Plug,
  Plus,
  RefreshCw,
  Save,
  Stethoscope,
  Trash2,
  Wrench,
} from "lucide-react"
import { isDesktop, openUrl } from "@/lib/platform"
import { getActiveRemoteConnectionId } from "@/lib/transport"
import { toast } from "sonner"
import { customAgentId, isCustomAgentType } from "@/lib/custom-agents"
import { AgentIcon } from "@/components/agent-icon"
import { AddCustomAgentDialog } from "@/components/settings/add-custom-agent-dialog"
import { SettingCard, SettingRow } from "@/components/shared/setting-card"
import { CustomAgentMcpToggle } from "@/components/settings/custom-agent-mcp-toggle"
import { CustomAgentSkillsToggle } from "@/components/settings/custom-agent-skills-toggle"
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible"
import { Input } from "@/components/ui/input"
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import { cn, copyTextToClipboard, randomUUID } from "@/lib/utils"
import {
  acpClearBinaryCache,
  acpDetectAgentLocalVersion,
  acpDownloadAgentBinary,
  acpFetchAgentLatestRelease,
  acpInstallUvTool,
  acpGetAgentStatus,
  acpListAgents,
  acpPreflight,
  acpPrepareNpxAgent,
  acpReorderAgents,
  acpDeleteCustomAgent,
  acpUninstallAgent,
  acpUpdateAgentConfig,
  acpUpdateAgentEnv,
  acpUpdateHermesConfig,
  acpRevealHermesHome,
  acpOpenHermesSetupTerminal,
  codexPollDeviceCode,
  codexRequestDeviceCode,
  listModelProviders,
  opencodeProviderCatalog,
} from "@/lib/api"
import type {
  AcpAgentInfo,
  AgentLatestRelease,
  AgentType,
  CheckStatus,
  CodexSandboxStructuredConfig,
  GrokStructuredConfig,
  ModelProviderInfo,
  OpenCodeCatalogProvider,
} from "@/lib/types"
import {
  HERMES_PROVIDERS,
  parseClaudeProviderModel,
  parseCodexModelConfig,
  serializeCodexModelConfig,
  type CodexModelConfig,
} from "@/lib/types"
import { CodexModelListEditor } from "@/components/settings/codex-model-list-editor"
import {
  OpenCodeConnectDialog,
  OpenCodeCustomProviderDialog,
} from "@/components/settings/opencode-connect-dialog"
import { OpenCodeBehaviorSection } from "@/components/settings/opencode-behavior-section"
import { OpenCodePermissionsSection } from "@/components/settings/opencode-permissions-section"
import { AgentDiagnosticsDialog } from "@/components/settings/agent-diagnostics-dialog"
import { AgentReadinessCard, type AgentCcrState } from "../agent-readiness-card"
import { useAgentCcrBinding } from "../use-agent-ccr-binding"
import {
  buildConnectedModelOptions,
  buildConnectedProviders,
  disconnectProvider,
  modelReferencesProvider,
  setProviderApiKey,
  setProviderEnabled,
} from "@/lib/opencode-connect"
import { toErrorMessage } from "@/lib/app-error"
import { getInstallErrorHintKey } from "@/lib/agent-install-error"
import { ccrAgentKindForBody, pickCcrProfileForKind } from "@/lib/ccr"
import {
  readAgentListOnlyInstalled,
  writeAgentListOnlyInstalled,
} from "@/lib/agent-list-prefs"
import { useAgentInstallStream } from "@/hooks/use-agent-install-stream"
import { OpencodePluginsModal } from "../opencode-plugins-modal"
import {
  ANTIGRAVITY_ENV_KEYS,
  AntigravityConfigPanel,
} from "../antigravity-config-panel"
import { CodeBuddyConfigPanel } from "../codebuddy-config-panel"
import { CursorConfigPanel } from "../cursor-config-panel"
import { DeepSeekConfigPanel } from "../deepseek-config-panel"
import { DeepSeekModelListEditor } from "../deepseek-model-list-editor"
import { KimiCodeConfigPanel } from "../kimi-code-config-panel"
import { PiConfigPanel } from "../pi-config-panel"
import { QoderConfigPanel } from "../qoder-config-panel"

import {
  CLAUDE_ATTRIBUTION_HEADER_ENV_KEY,
  CLAUDE_AUTH_MODES,
  CLAUDE_ENV_FLAG_OFF,
  CLAUDE_ENV_FLAG_ON,
  CLAUDE_NONESSENTIAL_TRAFFIC_ENV_KEY,
  CLINE_BYO_PROVIDERS,
  CLINE_SIGNIN_PROVIDERS,
  CODEX_APPROVAL_POLICY_VALUES,
  CODEX_AUTH_MODES,
  CODEX_DEFAULT_MODEL_PROVIDER,
  CODEX_GRANULAR_KEYS,
  CODEX_SANDBOX_MODE_VALUES,
  CODEX_SANDBOX_UNSET,
  CODEX_SANDBOX_UNSET_OPTION,
  GEMINI_AUTH_MODES,
  GROK_DEFAULT_API_BACKEND,
  GROK_LOGIN_COMMAND,
  GROK_UNSET,
  OPENCLAW_ENV_KEYS,
  OPENCODE_PROVIDER_NPM_OPTIONS,
  OpenCodeModelCombobox,
  PACKAGE_ACTION_FIX_KINDS,
  applyImportantFieldToDraft,
  asObjectRecord,
  buildAgentDraft,
  buildGrokSaveOptions,
  buildImportantPatchFromDraft,
  buildMergeConfigPayload,
  buildOpenCodeModelOptions,
  buildOpenCodeNpmOptions,
  canCheckLatestRelease,
  clineAuthCommand,
  codexSandboxSaveConfig,
  codexSandboxSeedsAcpPreset,
  codexWorkspaceWriteApplies,
  compareVersion,
  configTextForClaudeSave,
  ensureOpenCodeProviderNpm,
  envMapToText,
  extractClineImportantValues,
  extractCodexImportantValues,
  extractGeminiImportantValues,
  extractImportantConfigValues,
  extractOpenCodeConfigValues,
  findEnvValue,
  firstRelativeWritableRoot,
  geminiAuthModeHint,
  geminiAuthModeLabel,
  getAgentChecks,
  hasCodexChatgptTokens,
  hasComparableVersion,
  hostToolsAgentModeEnabled,
  importantEnvKeysByAgent,
  importantFieldsFor,
  isClineSignInProvider,
  isValidCustomVersion,
  materializeClaudeHardeningFlags,
  normalizeConfigText,
  parseCodexAuthJsonText,
  parseConfigJsonText,
  parseEnvText,
  parseHermesConfig,
  patchCodexAuthJsonText,
  patchCodexConfigTomlText,
  patchEnvByImportantKey,
  patchEnvText,
  patchGeminiAuthMode,
  patchGeminiConfigText,
  patchGeminiEnvText,
  patchImportantConfigText,
  patchOpenCodeAuthJsonText,
  patchOpenCodeConfigText,
  providerToRebindTo,
  publishAgentDisplay,
  rebaseDeepSeekDraft,
  removeTomlSection,
  setAcpTranslator,
  setClaudeEnvFlagInConfigText,
  setHostToolsAgentMode,
  showsCodexReadOnlyAcpWarning,
  statusTone,
  summarizeChecks,
  updateTomlRootStringKey,
} from "./helpers"
import type {
  AcpTranslator,
  AgentCheckState,
  AgentDraft,
  ClaudeAuthMode,
  CodexApprovalPolicyChoice,
  CodexAuthMode,
  CodexSandboxModeChoice,
  GeminiAuthMode,
  GrokAuthMethod,
  ImportantConfigKey,
  RunningActionKind,
  UiCheckItem,
  UiFixAction,
} from "./helpers"
import { AgentReorderItem } from "./agent-reorder-item"

// [crewchat] Split from acp-agent-settings.tsx (structural split, batch 2):
// helpers live in ./helpers/*; this file keeps the page component only.
export * from "./helpers"

export function AcpAgentSettings() {
  const ime = useImeGuard()
  const locale = useLocale()
  const t = useTranslations("AcpAgentSettings")
  const rawTranslator = t as unknown as AcpTranslator
  setAcpTranslator((key, values) => rawTranslator(key, values))
  const searchParams = useSearchParams()
  const [agents, setAgents] = useState<AcpAgentInfo[]>([])
  const [loadingAgents, setLoadingAgents] = useState(true)
  const [addCustomOpen, setAddCustomOpen] = useState(false)
  // Registry id of the custom agent being edited; non-null renders the edit
  // instance of the add dialog (its own instance so the two flows never share
  // form state).
  const [editCustomAgentId, setEditCustomAgentId] = useState<string | null>(
    null
  )
  const [removingCustomAgent, setRemovingCustomAgent] = useState(false)
  const [loadingError, setLoadingError] = useState<string | null>(null)
  const [checkState, setCheckState] = useState<
    Partial<Record<AgentType, AgentCheckState>>
  >({})
  const [checking, setChecking] = useState<Partial<Record<AgentType, boolean>>>(
    {}
  )
  const [busyBinaryAction, setBusyBinaryAction] = useState<
    Partial<Record<AgentType, boolean>>
  >({})
  const [runningActionKind, setRunningActionKind] = useState<
    Partial<Record<AgentType, RunningActionKind>>
  >({})
  const [savingEnv, setSavingEnv] = useState<
    Partial<Record<AgentType, boolean>>
  >({})
  const [savingConfig, setSavingConfig] = useState<
    Partial<Record<AgentType, boolean>>
  >({})
  const [modelProviders, setModelProviders] = useState<ModelProviderInfo[]>([])
  const [uninstallConfirmAgent, setUninstallConfirmAgent] =
    useState<AcpAgentInfo | null>(null)
  const [removeConfirmAgent, setRemoveConfirmAgent] =
    useState<AcpAgentInfo | null>(null)
  const [customInstallAgent, setCustomInstallAgent] =
    useState<AcpAgentInfo | null>(null)
  const [customVersionInput, setCustomVersionInput] = useState("")
  // The "Upgrade to unreviewed latest" awaiting confirmation.
  const [upgradeLatestTarget, setUpgradeLatestTarget] = useState<{
    agent: AcpAgentInfo
    version: string
  } | null>(null)
  // The unreviewed latest release found for the agent the detail pane shows,
  // looked up once per visit (see the effect below). Tagged with its agent so
  // it can never surface on another one.
  const [visitLatestRelease, setVisitLatestRelease] = useState<{
    agentType: AgentType
    release: AgentLatestRelease
  } | null>(null)
  const [pluginModalOpen, setPluginModalOpen] = useState(false)
  const [pluginModalAgent, setPluginModalAgent] = useState<AgentType | null>(
    null
  )
  const [expandedChecks, setExpandedChecks] = useState<Record<string, boolean>>(
    {}
  )
  // Check cards the user folded or unfolded by hand since this page mounted.
  const userToggledChecksRef = useRef<Set<string>>(new Set())
  const [selectedAgentType, setSelectedAgentType] = useState<AgentType | null>(
    null
  )
  const [drafts, setDrafts] = useState<Partial<Record<AgentType, AgentDraft>>>(
    {}
  )
  const [configErrors, setConfigErrors] = useState<
    Partial<Record<AgentType, string | null>>
  >({})
  const [showApiKeys, setShowApiKeys] = useState<
    Partial<Record<AgentType, boolean>>
  >({})
  // Whether the Grok panel's "advanced (raw config.toml)" escape hatch is open.
  const [grokAdvancedOpen, setGrokAdvancedOpen] = useState(false)
  // Show/hide toggle for the Grok custom-model API key (kept separate from the
  // per-agent `showApiKeys` map, which is keyed by AgentType only).
  const [showGrokCustomKey, setShowGrokCustomKey] = useState(false)
  // True for the WHOLE duration of a Grok save (write + post-save reseed), so
  // both Grok save buttons stay disabled and can't interleave while the draft
  // is being rebuilt from disk.
  const [grokSaving, setGrokSaving] = useState(false)
  const [openCodeProviderId, setOpenCodeProviderId] = useState("")
  const [openCodeNewModelIds, setOpenCodeNewModelIds] = useState<
    Record<string, string>
  >({})
  const [openCodeModelIdDrafts, setOpenCodeModelIdDrafts] = useState<
    Record<string, string>
  >({})
  const [openCodeModelConfigExpanded, setOpenCodeModelConfigExpanded] =
    useState<Record<string, boolean>>({})
  const [openCodeDeleteProviderId, setOpenCodeDeleteProviderId] = useState<
    string | null
  >(null)
  const [openCodeCatalog, setOpenCodeCatalog] = useState<
    OpenCodeCatalogProvider[]
  >([])
  const [openCodeCatalogLoading, setOpenCodeCatalogLoading] = useState(false)
  // True once the catalog fetch has settled at least once (success OR failure).
  // Gates "Add custom provider" so the catalog-id collision check runs against a
  // known set — an empty catalog while still loading must not let a catalog id
  // (e.g. "openai") slip in as a custom provider.
  const [openCodeCatalogReady, setOpenCodeCatalogReady] = useState(false)
  // Dedupe the one-shot catalog fetch without putting volatile state in the
  // effect deps (which would re-run the effect and self-cancel the request).
  const openCodeCatalogRequestedRef = useRef(false)
  const [openCodeConnectOpen, setOpenCodeConnectOpen] = useState(false)
  const [diagnosticsOpen, setDiagnosticsOpen] = useState(false)
  // Add-a-custom-provider dialog (separate from the catalog connect dialog).
  const [openCodeCustomOpen, setOpenCodeCustomOpen] = useState(false)
  // When set, the connect dialog opens in edit mode for this connected provider.
  const [openCodeEditProviderId, setOpenCodeEditProviderId] = useState<
    string | null
  >(null)
  const [dragging, setDragging] = useState<AgentType | null>(null)
  const [reordering, setReordering] = useState(false)
  // [crewchat] The list is split installed / not-installed; the only-installed
  // filter remembers itself, and the not-installed group starts folded so the
  // page opens on the working set.
  const [onlyInstalled, setOnlyInstalled] = useState(() =>
    readAgentListOnlyInstalled()
  )
  const [uninstalledOpen, setUninstalledOpen] = useState(false)
  const pendingOrderRef = useRef<AgentType[] | null>(null)
  const busyActionRef = useRef<Set<AgentType>>(new Set())
  const handledSearchAgentRef = useRef<string | null>(null)
  const agentListRef = useRef<HTMLDivElement | null>(null)
  const installStream = useAgentInstallStream()
  const [streamAgentType, setStreamAgentType] = useState<AgentType | null>(null)
  const installLogEndRef = useRef<HTMLDivElement | null>(null)
  const [codexDeviceCode, setCodexDeviceCode] = useState<{
    userCode: string
    verificationUrl: string
    deviceAuthId: string
    interval: number
  } | null>(null)
  const [codexLoginStatus, setCodexLoginStatus] = useState<
    "idle" | "requesting" | "polling" | "success" | "error"
  >("idle")
  const [codexLoginError, setCodexLoginError] = useState<string | null>(null)
  const codexPollCancelledRef = useRef(false)
  // [crewchat] The readiness card's state: whether its Advanced fold is open
  // (it ships folded — the card is this page's default face), plus the CCR
  // profile list and its write actions, which live in their own hook.
  const [advancedOpen, setAdvancedOpen] = useState(false)
  const ccrBinding = useAgentCcrBinding()

  const sortedAgents = useMemo(
    () =>
      [...agents].sort(
        (a, b) => a.sort_order - b.sort_order || a.name.localeCompare(b.name)
      ),
    [agents]
  )
  // [crewchat] Group by "installed" — the same fact the connect gate uses
  // (installed_version non-null), so the list never disagrees with what can
  // actually be launched.
  const installedAgents = useMemo(
    () => sortedAgents.filter((agent) => Boolean(agent.installed_version)),
    [sortedAgents]
  )
  const uninstalledAgents = useMemo(
    () => sortedAgents.filter((agent) => !agent.installed_version),
    [sortedAgents]
  )
  const selectedAgent = useMemo(
    () =>
      sortedAgents.find((agent) => agent.agent_type === selectedAgentType) ??
      null,
    [selectedAgentType, sortedAgents]
  )
  const agentTypesKey = useMemo(
    () =>
      [...new Set(agents.map((agent) => agent.agent_type))].sort().join(","),
    [agents]
  )
  const requestedAgentType = useMemo(
    () => searchParams.get("agent"),
    [searchParams]
  )

  const refreshAgents = useCallback(async () => {
    setLoadingAgents(true)
    setLoadingError(null)
    try {
      const [next, providers] = await Promise.all([
        acpListAgents(),
        listModelProviders().catch(() => [] as ModelProviderInfo[]),
      ])
      setAgents(next)
      publishAgentDisplay(next)
      setModelProviders(providers)
      setDrafts((prev) => {
        const updated = { ...prev }
        for (const agent of next) {
          if (!updated[agent.agent_type]) {
            updated[agent.agent_type] = buildAgentDraft(agent)
            continue
          }
          // An EXISTING draft is deliberately kept (it may hold in-progress
          // edits) — but for the keys a structured panel owns, keeping it is
          // what loses data: the enable switch persists `draft.envText`
          // wholesale, so a draft still holding this window's pre-refresh
          // values would restore them over whatever another window (or
          // another surface here) just saved. Rebase only those keys; every
          // other key, and every unsaved edit to them, is untouched.
          const existing = updated[agent.agent_type]
          if (agent.agent_type === "deepseek" && existing) {
            updated[agent.agent_type] = rebaseDeepSeekDraft(existing, agent)
          }
        }
        return updated
      })
      setConfigErrors((prev) => {
        const updated = { ...prev }
        for (const agent of next) {
          if (typeof updated[agent.agent_type] !== "undefined") continue
          const configText =
            typeof agent.config_json === "string" ? agent.config_json : ""
          updated[agent.agent_type] = parseConfigJsonText(configText).error
        }
        return updated
      })
    } catch (err) {
      const message = toErrorMessage(err)
      setLoadingError(message)
    } finally {
      setLoadingAgents(false)
    }
  }, [])

  const runPreflight = useCallback(
    async (agentType: AgentType, forceRefresh?: boolean) => {
      setChecking((prev) => ({ ...prev, [agentType]: true }))
      try {
        const [resultState, versionState, statusState] =
          await Promise.allSettled([
            acpPreflight(agentType, forceRefresh),
            acpDetectAgentLocalVersion(agentType),
            acpGetAgentStatus(agentType),
          ])

        if (versionState.status === "fulfilled") {
          setAgents((prev) => {
            if (versionState.value === null) return prev
            let changed = false
            const next = prev.map((agent) => {
              if (agent.agent_type !== agentType) return agent
              if (agent.installed_version === versionState.value) return agent
              changed = true
              return { ...agent, installed_version: versionState.value }
            })
            return changed ? next : prev
          })
        }

        // Re-sync `available` from the authoritative backend status. It is
        // recomputed live (e.g. `uvx_agent_launchable` for custom uvx
        // agents), so an install that provisions the runtime flips it true
        // here — otherwise the version-status panel would stay stuck on the
        // unavailable / "runtime not ready" branch with the freshly installed
        // version shown.
        if (statusState.status === "fulfilled") {
          setAgents((prev) => {
            let changed = false
            const next = prev.map((agent) => {
              if (agent.agent_type !== agentType) return agent
              if (agent.available === statusState.value.available) return agent
              changed = true
              return { ...agent, available: statusState.value.available }
            })
            return changed ? next : prev
          })
        }

        if (resultState.status === "fulfilled") {
          setCheckState((prev) => ({
            ...prev,
            [agentType]: { result: resultState.value },
          }))
        } else {
          const message =
            resultState.reason instanceof Error
              ? resultState.reason.message
              : String(resultState.reason)
          setCheckState((prev) => ({
            ...prev,
            [agentType]: { error: message },
          }))
        }
      } catch (err) {
        const message = toErrorMessage(err)
        setCheckState((prev) => ({ ...prev, [agentType]: { error: message } }))
      } finally {
        setChecking((prev) => ({ ...prev, [agentType]: false }))
      }
    },
    []
  )

  const runAllPreflight = useCallback(
    async (agentTypes: AgentType[]) => {
      if (agentTypes.length === 0) return
      setChecking((prev) => {
        const next = { ...prev }
        for (const agentType of agentTypes) {
          next[agentType] = true
        }
        return next
      })
      await Promise.all(agentTypes.map((agentType) => runPreflight(agentType)))
    },
    [runPreflight]
  )

  useEffect(() => {
    return () => installStream.reset()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => {
    const container = installLogEndRef.current?.parentElement
    if (container) {
      container.scrollTop = container.scrollHeight
    }
  }, [installStream.logs])

  useEffect(() => {
    if (
      installStream.status === "success" ||
      installStream.status === "failed"
    ) {
      if (streamAgentType) {
        runPreflight(streamAgentType).catch(() => {})
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [installStream.status])

  useEffect(() => {
    refreshAgents().catch((err) => {
      console.error("[Settings] refresh agents failed:", err)
    })
  }, [refreshAgents])

  useEffect(() => {
    if (loadingAgents || !agentTypesKey) return
    const agentTypes = agentTypesKey.split(",") as AgentType[]
    runAllPreflight(agentTypes).catch((err) => {
      console.error("[Settings] run all preflight failed:", err)
    })
  }, [agentTypesKey, loadingAgents, runAllPreflight])

  useEffect(() => {
    if (!requestedAgentType) {
      handledSearchAgentRef.current = null
      return
    }
    if (sortedAgents.length === 0) {
      return
    }
    if (handledSearchAgentRef.current === requestedAgentType) {
      return
    }
    const matched = sortedAgents.find(
      (agent) => agent.agent_type === requestedAgentType
    )
    if (matched) {
      setSelectedAgentType(matched.agent_type)
    }
    handledSearchAgentRef.current = requestedAgentType
  }, [requestedAgentType, sortedAgents])

  useEffect(() => {
    if (!selectedAgentType) return
    const container = agentListRef.current
    if (!container) return
    const selected = container.querySelector<HTMLElement>(
      `[data-agent-type="${selectedAgentType}"]`
    )
    if (!selected) return
    selected.scrollIntoView({ block: "nearest", behavior: "smooth" })
  }, [selectedAgentType, sortedAgents])

  useEffect(() => {
    if (sortedAgents.length === 0) {
      setSelectedAgentType(null)
      return
    }
    setSelectedAgentType((prev) => {
      if (prev && sortedAgents.some((agent) => agent.agent_type === prev)) {
        return prev
      }
      return sortedAgents[0].agent_type
    })
  }, [sortedAgents])

  // Look up the selected agent's unreviewed latest release once per visit:
  // selecting it (by click, the first auto-select, or `?agent=`) is a visit,
  // and leaving it ends one. Re-renders, agent list refreshes and preflight
  // runs do not ask again, so the lookup keys on the agent's identity only.
  // Every visit starts empty and shows nothing but its own answer: while the
  // lookup runs, or after it fails, there is no offer.
  const latestLookupTarget =
    selectedAgent && canCheckLatestRelease(selectedAgent)
      ? selectedAgent.agent_type
      : null
  useEffect(() => {
    setVisitLatestRelease(null)
    if (!latestLookupTarget) return
    let current = true
    acpFetchAgentLatestRelease(latestLookupTarget)
      .then((release) => {
        if (current && release) {
          setVisitLatestRelease({ agentType: latestLookupTarget, release })
        }
      })
      .catch((err) => {
        console.warn("[Settings] latest release lookup failed:", err)
      })
    return () => {
      current = false
    }
  }, [latestLookupTarget])

  // A settings save (env or native config) only takes effect on the NEXT agent
  // start, so any running session of that agent stays on its launch-time config
  // until restarted. The backend returns how many running sessions were left
  // stale; surface that as one info toast. Debounced + max-coalesced so a button
  // that saves env AND config together (e.g. Codex, Gemini) shows a single toast
  // rather than one per call.
  const affectedReportRef = useRef<{
    max: number
    timer: ReturnType<typeof setTimeout> | null
  }>({ max: 0, timer: null })
  const reportAffectedSessions = useCallback(
    (affected: number) => {
      const r = affectedReportRef.current
      r.max = Math.max(r.max, affected)
      if (r.timer) clearTimeout(r.timer)
      r.timer = setTimeout(() => {
        const count = affectedReportRef.current.max
        affectedReportRef.current = { max: 0, timer: null }
        if (count > 0) {
          toast.info(t("toasts.affectedRunningSessions", { count }))
        }
      }, 150)
    },
    [t]
  )

  const persistEnv = useCallback(
    async (
      agentType: AgentType,
      enabled: boolean,
      envText: string,
      modelProviderId?: number | null,
      /** The keys a STRUCTURED panel owns, when the env came from one rather
       * than from `draft.envText`. `undefined` for a key deletes it. See the
       * draft sync below. */
      draftEnvPatch?: Record<string, string | undefined>
    ) => {
      const parsedEnv = parseEnvText(envText)
      setSavingEnv((prev) => ({ ...prev, [agentType]: true }))
      try {
        const affected = await acpUpdateAgentEnv(agentType, {
          enabled,
          env: parsedEnv,
          modelProviderId: modelProviderId ?? null,
        })
        setAgents((prev) =>
          prev.map((agent) =>
            agent.agent_type === agentType
              ? {
                  ...agent,
                  enabled,
                  env: parsedEnv,
                  model_provider_id: modelProviderId ?? null,
                }
              : agent
          )
        )
        // A structured panel writes env the raw editor never sees, and the
        // agents refetch deliberately preserves existing drafts (to protect
        // in-progress edits). The enable switch persists `draft.envText`
        // WHOLESALE, so a draft left holding pre-save text silently undoes the
        // save the moment the user flips it. Fold the panel's keys into the
        // draft — in the same commit as `setAgents`, so no await window exists
        // in which the switch could fire with the old text, and no refetch
        // failure can leave it stale.
        //
        // A PATCH, not a wholesale replace: the textarea stays editable while
        // the panel's request is in flight, so overwriting the draft with the
        // panel's own map would erase whatever was typed in the meantime.
        if (draftEnvPatch) {
          const keys = importantEnvKeysByAgent(agentType)
          setDrafts((prev) => {
            const current = prev[agentType]
            if (!current) return prev
            const envText = patchEnvText(current.envText, draftEnvPatch)
            const mergedEnv = parseEnvText(envText)
            return {
              ...prev,
              [agentType]: {
                ...current,
                enabled,
                envText,
                modelProviderId: modelProviderId ?? null,
                // The structured mirrors read the same keys, so they have to
                // move with the text or the two views disagree.
                apiBaseUrl: findEnvValue(mergedEnv, keys.apiBaseUrl),
                apiKey: findEnvValue(mergedEnv, keys.apiKey),
                model: findEnvValue(mergedEnv, keys.model),
              },
            }
          })
        }
        reportAffectedSessions(affected)
      } finally {
        setSavingEnv((prev) => ({ ...prev, [agentType]: false }))
      }
    },
    [reportAffectedSessions]
  )

  const persistConfig = useCallback(
    async (
      agentType: AgentType,
      configText: string,
      options?: {
        openCodeAuthJsonText?: string
        codexAuthJsonText?: string
        codexConfigTomlText?: string
        codexModelCatalog?: string
        codexSandbox?: CodexSandboxStructuredConfig
        grokConfigTomlText?: string
        grokStructured?: GrokStructuredConfig
      }
    ) => {
      const parsedConfig = parseConfigJsonText(configText)
      if (parsedConfig.error) {
        throw new Error(parsedConfig.error)
      }
      const codexAuthJsonText = options?.codexAuthJsonText
      if (agentType === "codex" && typeof codexAuthJsonText === "string") {
        const authError = parseCodexAuthJsonText(codexAuthJsonText)
        if (authError) {
          throw new Error(authError)
        }
      }
      let normalizedConfig = normalizeConfigText(configText)
      if (agentType === "open_code" && normalizedConfig) {
        normalizedConfig = ensureOpenCodeProviderNpm(normalizedConfig)
      }
      // For agents using merge strategy, mark removed keys as null
      // so the backend merge_json_values can delete them from disk.
      let configForPersist =
        agentType === "open_code" && !normalizedConfig ? "{}" : normalizedConfig
      const usesMerge =
        agentType === "claude_code" ||
        agentType === "gemini" ||
        agentType === "open_claw"
      if (usesMerge) {
        const originalAgent = agents.find((a) => a.agent_type === agentType)
        // Diff even when the current config emptied to "" so removed keys still
        // produce null-deletion patches (`configForPersist` would otherwise be
        // "" → a null config_json no-op that leaves the stale key on disk).
        configForPersist =
          buildMergeConfigPayload(configText, originalAgent?.config_json) ?? ""
      }
      setSavingConfig((prev) => ({ ...prev, [agentType]: true }))
      try {
        const affected = await acpUpdateAgentConfig(agentType, {
          config_json: configForPersist || null,
          opencode_auth_json:
            typeof options?.openCodeAuthJsonText === "string"
              ? options.openCodeAuthJsonText
              : null,
          codex_auth_json:
            typeof codexAuthJsonText === "string" ? codexAuthJsonText : null,
          codex_config_toml:
            typeof options?.codexConfigTomlText === "string"
              ? options.codexConfigTomlText
              : null,
          codex_model_catalog:
            typeof options?.codexModelCatalog === "string"
              ? options.codexModelCatalog
              : null,
          codex_sandbox: options?.codexSandbox ?? null,
          grok_config_toml:
            typeof options?.grokConfigTomlText === "string"
              ? options.grokConfigTomlText
              : null,
          grok_structured: options?.grokStructured ?? null,
        })
        reportAffectedSessions(affected)
        setAgents((prev) =>
          prev.map((agent) =>
            agent.agent_type === agentType
              ? {
                  ...agent,
                  config_json: normalizedConfig || null,
                  opencode_auth_json:
                    typeof options?.openCodeAuthJsonText === "string"
                      ? options.openCodeAuthJsonText
                      : agent.opencode_auth_json,
                  codex_auth_json:
                    typeof codexAuthJsonText === "string"
                      ? codexAuthJsonText
                      : agent.codex_auth_json,
                  codex_config_toml:
                    typeof options?.codexConfigTomlText === "string"
                      ? options.codexConfigTomlText
                      : agent.codex_config_toml,
                  grok_config_toml:
                    typeof options?.grokConfigTomlText === "string"
                      ? options.grokConfigTomlText
                      : agent.grok_config_toml,
                  grok_settings: options?.grokStructured
                    ? {
                        default_reasoning_effort:
                          options.grokStructured.defaultReasoningEffort,
                        permission_mode: options.grokStructured.permissionMode,
                        // buildGrokStructuredConfig already trims/gates/clamps
                        // these to what the backend writes, so mirror them
                        // directly (reseedGrokDraft re-reads disk right after).
                        custom_model_id: options.grokStructured.customModelId,
                        custom_base_url: options.grokStructured.customBaseUrl,
                        custom_api_key: options.grokStructured.customApiKey,
                        custom_api_backend:
                          options.grokStructured.customApiBackend,
                        custom_context_window:
                          options.grokStructured.customContextWindow,
                        auto_compact_threshold_percent:
                          options.grokStructured.autoCompactThresholdPercent,
                      }
                    : agent.grok_settings,
                }
              : agent
          )
        )
      } finally {
        setSavingConfig((prev) => ({ ...prev, [agentType]: false }))
      }
    },
    [agents, reportAffectedSessions]
  )

  // After a Grok save, re-read the merged on-disk config and rebuild the Grok
  // draft. The agents-updated refetch deliberately does NOT overwrite existing
  // drafts (to preserve in-progress edits), so without this the collapsed raw
  // editor and the structured dropdowns could drift out of sync with disk (the
  // structured merge and the raw editor each write keys the other doesn't echo).
  const reseedAgentDraft = useCallback(async (agentType: AgentType) => {
    try {
      const fresh = await acpListAgents()
      setAgents(fresh)
      publishAgentDisplay(fresh)
      const agent = fresh.find((a) => a.agent_type === agentType)
      if (agent) {
        setDrafts((prev) => ({ ...prev, [agentType]: buildAgentDraft(agent) }))
      }
    } catch (err) {
      // Non-fatal: the save already committed, and the agents-updated
      // subscription will resync shortly — never surface this as a save failure.
      console.error(`[Settings] reseed ${agentType} draft failed:`, err)
    }
  }, [])

  const reseedGrokDraft = useCallback(
    () => reseedAgentDraft("grok"),
    [reseedAgentDraft]
  )

  const runBinaryAction = useCallback(
    async (
      agent: AcpAgentInfo,
      mode: "download" | "upgrade",
      kind?: RunningActionKind,
      versionOverride?: string
    ) => {
      if (busyActionRef.current.has(agent.agent_type)) return
      busyActionRef.current.add(agent.agent_type)
      setBusyBinaryAction((prev) => ({ ...prev, [agent.agent_type]: true }))
      setRunningActionKind((prev) => ({
        ...prev,
        [agent.agent_type]:
          kind ?? (mode === "download" ? "download_binary" : "upgrade_binary"),
      }))
      // A custom-version install must replace whatever is cached, otherwise a
      // higher cached version would still win on connect.
      const clearCache = mode === "upgrade" || Boolean(versionOverride)
      const actionLabel =
        kind === "upgrade_latest"
          ? t("actions.upgrade")
          : versionOverride
            ? t("actions.customInstall")
            : mode === "upgrade"
              ? t("actions.upgrade")
              : t("actions.install")
      const taskId = randomUUID()
      setStreamAgentType(agent.agent_type)
      await installStream.start(taskId)
      try {
        if (clearCache) {
          await acpClearBinaryCache(agent.agent_type)
        }
        await acpDownloadAgentBinary(
          agent.agent_type,
          taskId,
          versionOverride ?? null
        )
        await runPreflight(agent.agent_type)
        const detectedVersion = await acpDetectAgentLocalVersion(
          agent.agent_type
        )
        setAgents((prev) =>
          prev.map((item) =>
            item.agent_type === agent.agent_type
              ? { ...item, installed_version: detectedVersion }
              : item
          )
        )
        toast.success(
          t("toasts.agentActionCompleted", {
            name: agent.name,
            action: actionLabel,
          }),
          {
            description: detectedVersion
              ? t("toasts.localVersion", { version: detectedVersion })
              : t("toasts.installCompletedVersionLater"),
          }
        )
      } catch (err) {
        const message = toErrorMessage(err)
        toast.error(
          t("toasts.agentActionFailed", {
            name: agent.name,
            action: actionLabel,
          }),
          {
            description: message,
          }
        )
        if (clearCache) {
          // The cache was cleared before downloading, so a failure here may
          // have removed the previously working binary — resync local state so
          // the UI doesn't keep showing a phantom version.
          try {
            const detected = await acpDetectAgentLocalVersion(agent.agent_type)
            setAgents((prev) =>
              prev.map((item) =>
                item.agent_type === agent.agent_type
                  ? { ...item, installed_version: detected ?? null }
                  : item
              )
            )
          } catch (detectErr) {
            console.error(
              "[Settings] failed to resync installed version after binary install failure:",
              detectErr
            )
          }
        }
        throw err
      } finally {
        busyActionRef.current.delete(agent.agent_type)
        setBusyBinaryAction((prev) => ({ ...prev, [agent.agent_type]: false }))
        setRunningActionKind((prev) => ({
          ...prev,
          [agent.agent_type]: undefined,
        }))
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [runPreflight, t, installStream.start]
  )

  const runNpxAction = useCallback(
    async (
      agent: AcpAgentInfo,
      mode: "install" | "upgrade",
      versionOverride?: string,
      kind?: RunningActionKind
    ) => {
      if (busyActionRef.current.has(agent.agent_type)) return
      busyActionRef.current.add(agent.agent_type)
      setBusyBinaryAction((prev) => ({ ...prev, [agent.agent_type]: true }))
      setRunningActionKind((prev) => ({
        ...prev,
        [agent.agent_type]:
          kind ??
          (versionOverride
            ? "custom_install"
            : mode === "install"
              ? "install_npx"
              : "upgrade_npx"),
      }))
      // A custom-version install forces a clean reinstall so the requested
      // version replaces whatever is currently installed.
      const cleanFirst = mode === "upgrade" || Boolean(versionOverride)
      const actionLabel =
        kind === "upgrade_latest"
          ? t("actions.upgrade")
          : versionOverride
            ? t("actions.customInstall")
            : mode === "upgrade"
              ? t("actions.upgrade")
              : t("actions.install")
      const taskId = randomUUID()
      setStreamAgentType(agent.agent_type)
      await installStream.start(taskId)
      try {
        const installedVersion = await acpPrepareNpxAgent(
          agent.agent_type,
          agent.registry_version,
          taskId,
          cleanFirst,
          versionOverride ?? null
        )
        setAgents((prev) =>
          prev.map((item) =>
            item.agent_type === agent.agent_type
              ? { ...item, installed_version: installedVersion }
              : item
          )
        )
        await runPreflight(agent.agent_type)
        const detectedVersion = await acpDetectAgentLocalVersion(
          agent.agent_type
        )
        if (detectedVersion && detectedVersion !== installedVersion) {
          setAgents((prev) =>
            prev.map((item) =>
              item.agent_type === agent.agent_type
                ? { ...item, installed_version: detectedVersion }
                : item
            )
          )
        }
        const finalVersion = detectedVersion ?? installedVersion
        toast.success(
          t("toasts.agentActionCompleted", {
            name: agent.name,
            action: actionLabel,
          }),
          {
            description: finalVersion
              ? t("toasts.localVersion", { version: finalVersion })
              : t("toasts.installCompletedVersionLater"),
          }
        )
      } catch (err) {
        const message = toErrorMessage(err)
        const hintKey = getInstallErrorHintKey(message)
        toast.error(
          t("toasts.agentActionFailed", {
            name: agent.name,
            action: actionLabel,
          }),
          {
            description: hintKey ? t(hintKey, { name: agent.name }) : message,
          }
        )
        if (cleanFirst) {
          // Clean reinstall may have removed the old install before failing —
          // resync local state so the UI doesn't keep showing a phantom version.
          try {
            const detected = await acpDetectAgentLocalVersion(agent.agent_type)
            setAgents((prev) =>
              prev.map((item) =>
                item.agent_type === agent.agent_type
                  ? { ...item, installed_version: detected ?? null }
                  : item
              )
            )
          } catch (detectErr) {
            console.error(
              "[Settings] failed to resync installed version after upgrade failure:",
              detectErr
            )
          }
        }
        throw err
      } finally {
        busyActionRef.current.delete(agent.agent_type)
        setBusyBinaryAction((prev) => ({ ...prev, [agent.agent_type]: false }))
        setRunningActionKind((prev) => ({
          ...prev,
          [agent.agent_type]: undefined,
        }))
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [runPreflight, t, installStream.start]
  )

  /**
   * Remove a custom agent's definition. Recorded transcripts are kept — the
   * conversations that reference this agent are still readable afterwards,
   * they just cannot be resumed. Deleting them is a separate, explicit action.
   *
   * Confirmation happens in the `removeConfirmAgent` AlertDialog, never via
   * `window.confirm`: the Tauri webview does not reliably block on the native
   * prompt, so the deletion used to run before the user answered.
   */
  const handleRemoveCustomAgent = useCallback(
    async (agent: AcpAgentInfo) => {
      const id = customAgentId(agent.agent_type)
      if (!id) return
      setRemovingCustomAgent(true)
      try {
        await acpDeleteCustomAgent(id, false)
        toast.success(t("customAgentRemoved", { name: agent.name }))
        setSelectedAgentType(null)
        await refreshAgents()
      } catch (err) {
        toast.error(toErrorMessage(err))
      } finally {
        setRemovingCustomAgent(false)
      }
    },
    [refreshAgents, t]
  )

  const runUninstallAction = useCallback(
    async (agent: AcpAgentInfo) => {
      if (busyActionRef.current.has(agent.agent_type)) return
      busyActionRef.current.add(agent.agent_type)
      setBusyBinaryAction((prev) => ({ ...prev, [agent.agent_type]: true }))
      setRunningActionKind((prev) => ({
        ...prev,
        [agent.agent_type]:
          agent.distribution_type === "binary"
            ? "uninstall_binary"
            : "uninstall_npx",
      }))
      const taskId = randomUUID()
      setStreamAgentType(agent.agent_type)
      await installStream.start(taskId)
      try {
        await acpUninstallAgent(agent.agent_type, taskId)
        setAgents((prev) =>
          prev.map((item) =>
            item.agent_type === agent.agent_type
              ? { ...item, installed_version: null }
              : item
          )
        )
        await runPreflight(agent.agent_type)
        toast.success(t("toasts.uninstallCompleted", { name: agent.name }), {
          description: t("toasts.localVersionRemoved"),
        })
      } catch (err) {
        const message = toErrorMessage(err)
        const hintKey = getInstallErrorHintKey(message)
        toast.error(t("toasts.uninstallFailed", { name: agent.name }), {
          description: hintKey ? t(hintKey, { name: agent.name }) : message,
        })
        throw err
      } finally {
        busyActionRef.current.delete(agent.agent_type)
        setBusyBinaryAction((prev) => ({ ...prev, [agent.agent_type]: false }))
        setRunningActionKind((prev) => ({
          ...prev,
          [agent.agent_type]: undefined,
        }))
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [runPreflight, t, installStream.start]
  )

  // Install ONLY the uv runtime (uvx) — separate from preparing a uvx agent's
  // package. Triggered by the uv preflight check's "Install uv" fix. On success
  // `runPreflight` re-syncs the uv check + `available`, unblocking the agent's
  // version-status install action.
  const runUvInstall = useCallback(
    async (agent: AcpAgentInfo) => {
      if (busyActionRef.current.has(agent.agent_type)) return
      busyActionRef.current.add(agent.agent_type)
      setBusyBinaryAction((prev) => ({ ...prev, [agent.agent_type]: true }))
      setRunningActionKind((prev) => ({
        ...prev,
        [agent.agent_type]: "install_uv",
      }))
      const actionLabel = t("actions.install")
      const taskId = randomUUID()
      setStreamAgentType(agent.agent_type)
      await installStream.start(taskId)
      try {
        await acpInstallUvTool(taskId)
        await runPreflight(agent.agent_type)
        toast.success(
          t("toasts.agentActionCompleted", { name: "uv", action: actionLabel })
        )
      } catch (err) {
        const message = toErrorMessage(err)
        toast.error(
          t("toasts.agentActionFailed", { name: "uv", action: actionLabel }),
          { description: message }
        )
        throw err
      } finally {
        busyActionRef.current.delete(agent.agent_type)
        setBusyBinaryAction((prev) => ({ ...prev, [agent.agent_type]: false }))
        setRunningActionKind((prev) => ({
          ...prev,
          [agent.agent_type]: undefined,
        }))
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [runPreflight, t, installStream.start]
  )

  const handleFixAction = async (agent: AcpAgentInfo, action: UiFixAction) => {
    if (
      busyBinaryAction[agent.agent_type] ||
      busyActionRef.current.has(agent.agent_type)
    ) {
      return
    }
    if (action.kind === "open_url") {
      await openUrl(action.payload)
      return
    }
    if (action.kind === "download_binary") {
      await runBinaryAction(agent, "download")
      return
    }
    if (action.kind === "upgrade_binary") {
      await runBinaryAction(agent, "upgrade")
      return
    }
    if (action.kind === "install_npx") {
      await runNpxAction(agent, "install")
      return
    }
    if (action.kind === "upgrade_npx") {
      await runNpxAction(agent, "upgrade")
      return
    }
    if (action.kind === "uninstall_binary" || action.kind === "uninstall_npx") {
      setUninstallConfirmAgent(agent)
      return
    }
    if (action.kind === "redownload_binary") {
      await runBinaryAction(agent, "upgrade", "redownload_binary")
      return
    }
    if (action.kind === "install_opencode_plugins") {
      setPluginModalAgent(agent.agent_type)
      setPluginModalOpen(true)
      return
    }
    if (action.kind === "install_uv") {
      await runUvInstall(agent)
      return
    }
    if (action.kind === "custom_install") {
      setCustomVersionInput("")
      setCustomInstallAgent(agent)
      return
    }
    if (action.kind === "upgrade_latest") {
      setUpgradeLatestTarget({ agent, version: action.payload })
      return
    }
    await runPreflight(agent.agent_type)
  }

  const confirmRemoveCustomAgent = useCallback(() => {
    if (!removeConfirmAgent) return
    const target = removeConfirmAgent
    handleRemoveCustomAgent(target)
      .catch((err) => {
        console.error("[Settings] remove custom agent failed:", err)
      })
      .finally(() => {
        setRemoveConfirmAgent(null)
      })
  }, [handleRemoveCustomAgent, removeConfirmAgent])

  const confirmUninstall = useCallback(() => {
    if (!uninstallConfirmAgent) return
    const target = uninstallConfirmAgent
    runUninstallAction(target)
      .catch((err) => {
        console.error("[Settings] uninstall action failed:", err)
      })
      .finally(() => {
        setUninstallConfirmAgent(null)
      })
  }, [runUninstallAction, uninstallConfirmAgent])

  const confirmCustomInstall = useCallback(() => {
    if (!customInstallAgent) return
    const agent = customInstallAgent
    const version = customVersionInput.trim()
    if (!isValidCustomVersion(version)) return
    // Close immediately; progress streams into the detail panel log, and any
    // failure is surfaced via toast inside the run* actions.
    const run =
      agent.distribution_type === "binary"
        ? runBinaryAction(agent, "upgrade", "custom_install", version)
        : runNpxAction(agent, "upgrade", version)
    run.catch((err) => {
      console.error("[Settings] custom install failed:", err)
    })
    setCustomInstallAgent(null)
  }, [customInstallAgent, customVersionInput, runBinaryAction, runNpxAction])

  // Custom install's path with the offered version filled in — the same clean
  // reinstall, never a fallback to the recommended version on failure.
  const confirmUpgradeLatest = useCallback(() => {
    if (!upgradeLatestTarget) return
    const { agent, version } = upgradeLatestTarget
    const run =
      agent.distribution_type === "binary"
        ? runBinaryAction(agent, "upgrade", "upgrade_latest", version)
        : runNpxAction(agent, "upgrade", version, "upgrade_latest")
    run.catch((err) => {
      console.error("[Settings] upgrade to latest failed:", err)
    })
    setUpgradeLatestTarget(null)
  }, [runBinaryAction, runNpxAction, upgradeLatestTarget])

  const persistReorder = useCallback(
    async (order: AgentType[]) => {
      if (order.length === 0) return
      setReordering(true)
      try {
        await acpReorderAgents(order)
      } catch (err) {
        console.error("[Settings] reorder agents failed:", err)
        const message = toErrorMessage(err)
        toast.error(t("toasts.saveAgentOrderFailed"), {
          description: message,
        })
        await refreshAgents()
      } finally {
        setReordering(false)
      }
    },
    [refreshAgents, t]
  )

  // [crewchat] The drag group covers only the installed section; the rest are
  // appended after it so the persisted order stays a permutation of every
  // agent (no sort_order collisions).
  const handleReorder = useCallback(
    (next: AcpAgentInfo[]) => {
      const moved = new Set(next.map((agent) => agent.agent_type))
      const rest = sortedAgents.filter((agent) => !moved.has(agent.agent_type))
      const reordered = [...next, ...rest].map((agent, index) => ({
        ...agent,
        sort_order: index,
      }))
      setAgents(reordered)
      pendingOrderRef.current = reordered.map((agent) => agent.agent_type)
    },
    [sortedAgents]
  )

  // One package operation at a time across ALL agents: while any
  // install/upgrade/uninstall runs, every agent's package-action buttons are
  // disabled — the busy flag is keyed per agent, so without this, selecting
  // another agent in the list offers a second, concurrent install. The
  // spinner stays precise via the per-agent `runningActionKind`.
  const anyBinaryActionBusy = Object.values(busyBinaryAction).some(Boolean)

  const renderFixButton = (
    agent: AcpAgentInfo,
    fix: UiFixAction,
    index: number,
    className?: string
  ) => {
    const busyGated =
      anyBinaryActionBusy && PACKAGE_ACTION_FIX_KINDS.includes(fix.kind)
    const running = runningActionKind[agent.agent_type] === fix.kind
    return (
      <Button
        key={`${fix.label}-${index}`}
        size="xs"
        variant="outline"
        className={cn(
          "h-6 bg-muted/30 hover:bg-muted/50",
          // Two disabled looks: while the global one-package-op-
          // at-a-time gate is busy, every parked package action
          // dims (backend-disabled or not) so the lockout shows
          // on agents other than the busy one; only the button
          // showing the spinner, and — when the gate is idle — a
          // backend-declared inapplicable fix, keep the full-
          // opacity chip look.
          busyGated && !running
            ? "disabled:opacity-50"
            : "disabled:bg-muted/30 disabled:opacity-100",
          // The unreviewed-latest offer installs a release nobody has vetted,
          // so it wears the warning tone, disabled or not.
          fix.kind === "upgrade_latest" &&
            "border-amber-500/40 bg-amber-500/10 text-amber-700 hover:bg-amber-500/20 hover:text-amber-800 disabled:bg-amber-500/10 dark:text-amber-300 dark:hover:text-amber-200",
          className
        )}
        disabled={("disabled" in fix && fix.disabled === true) || busyGated}
        onClick={() => {
          handleFixAction(agent, fix).catch((err) => {
            console.error("[Settings] fix action failed:", err)
          })
        }}
      >
        {running ? (
          <Loader2 className="h-3 w-3 animate-spin" />
        ) : fix.kind === "download_binary" ||
          fix.kind === "install_npx" ||
          fix.kind === "install_uv" ? (
          <Download className="h-3 w-3" />
        ) : fix.kind === "upgrade_binary" ||
          fix.kind === "upgrade_npx" ||
          fix.kind === "redownload_binary" ||
          fix.kind === "upgrade_latest" ? (
          <Wrench className="h-3 w-3" />
        ) : fix.kind === "uninstall_binary" || fix.kind === "uninstall_npx" ? (
          <Trash2 className="h-3 w-3" />
        ) : fix.kind === "install_opencode_plugins" ? (
          <Download className="h-3 w-3" />
        ) : fix.kind === "custom_install" ? (
          <PackagePlus className="h-3 w-3" />
        ) : null}
        {fix.label}
      </Button>
    )
  }

  const renderCheck = (agent: AcpAgentInfo, check: UiCheckItem) => {
    const checkKey = `${agent.agent_type}:${check.check_id}`
    const expanded = expandedChecks[checkKey] ?? check.status !== "pass"
    const footerFixes = check.footerFixes ?? []

    return (
      <div
        key={check.check_id}
        className="rounded-md border bg-muted/20 px-3 py-2 space-y-2"
      >
        <button
          type="button"
          className="w-full flex items-center justify-between gap-2 text-left"
          onClick={() => {
            userToggledChecksRef.current.add(checkKey)
            setExpandedChecks((prev) => ({
              ...prev,
              [checkKey]: !expanded,
            }))
          }}
        >
          <div className="min-w-0 flex items-center gap-1.5">
            {expanded ? (
              <ChevronDown className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
            ) : (
              <ChevronRight className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
            )}
            <span className="text-xs font-medium truncate">{check.label}</span>
          </div>
          <span
            className={`text-2xs font-semibold shrink-0 ${statusTone(check.status)}`}
          >
            {check.status.toUpperCase()}
          </span>
        </button>

        {expanded && (
          <div className="flex items-start justify-between gap-2">
            <div className="min-w-0 text-2xs text-muted-foreground break-words">
              {check.message}
            </div>
            {check.fixes.length > 0 && (
              <div className="flex flex-wrap gap-1.5 justify-end max-w-[13.75rem] shrink-0">
                {check.fixes.map((fix, index) =>
                  renderFixButton(agent, fix, index)
                )}
              </div>
            )}
          </div>
        )}
        {/*
          Never wrapped apart: the unreviewed-latest offer holds the row's
          start and Custom install its end, the offer's auto end margin taking
          up the slack. When the row runs out of width the labels wrap instead
          (Button's base classes forbid both shrinking and wrapping, hence the
          overrides). Custom install gives way down to its longest word, the
          flex default minimum; the offer takes what is left and only below
          its own longest word breaks inside one, which
          `overflow-wrap: anywhere` allows for a long version such as
          `2026.10.03-abcdef1`.
        */}
        {expanded && footerFixes.length > 0 && (
          <div className="flex flex-nowrap items-start justify-end gap-1.5">
            {footerFixes.map((fix, index) =>
              renderFixButton(
                agent,
                fix,
                index,
                cn(
                  "h-auto min-h-6 shrink rounded-2xl py-1 text-left whitespace-normal",
                  fix.kind === "upgrade_latest" &&
                    "me-auto min-w-0 [overflow-wrap:anywhere]"
                )
              )
            )}
          </div>
        )}
      </div>
    )
  }

  const selectedCurrent = selectedAgent
    ? checkState[selectedAgent.agent_type]
    : undefined
  const selectedDraft = selectedAgent
    ? (drafts[selectedAgent.agent_type] ?? buildAgentDraft(selectedAgent))
    : null
  const selectedConfigError = selectedAgent
    ? (configErrors[selectedAgent.agent_type] ?? null)
    : null
  // [crewchat] Readiness derivations for the card above the Advanced fold —
  // install / enable / CCR binding, all from data the page already holds.
  const selectedCcrKind = selectedAgent
    ? ccrAgentKindForBody(selectedAgent.agent_type)
    : null
  const selectedCcrProfile = useMemo(
    () =>
      selectedCcrKind && ccrBinding.profiles
        ? pickCcrProfileForKind(ccrBinding.profiles, selectedCcrKind)
        : null,
    [selectedCcrKind, ccrBinding.profiles]
  )
  // The card's four CCR states — bound / bindable / unsupported / unreachable
  // — plus "loading" while the first read is still in flight.
  const selectedCcrState: AgentCcrState = !selectedCcrKind
    ? "unsupported"
    : ccrBinding.profiles === null
      ? ccrBinding.error
        ? "unreachable"
        : "loading"
      : selectedCcrProfile
        ? "bound"
        : "bindable"
  const selectedHasUpdate = Boolean(
    selectedAgent?.installed_version &&
    selectedAgent.registry_version &&
    hasComparableVersion(selectedAgent.registry_version) &&
    hasComparableVersion(selectedAgent.installed_version) &&
    compareVersion(
      selectedAgent.installed_version,
      selectedAgent.registry_version
    ) < 0
  )
  const selectedQuickInstallable = selectedAgent
    ? selectedAgent.distribution_type === "binary"
      ? selectedAgent.available
      : selectedAgent.distribution_type === "npx"
    : false
  const selectedIsInstalling = selectedAgent
    ? Boolean(busyBinaryAction[selectedAgent.agent_type])
    : false
  // Same rule getAgentChecks uses: a uvx agent is blocked only when the
  // preflight result is present AND its uv check isn't passing.
  const selectedUvCheck = selectedCurrent?.result?.checks?.find(
    (check) => check.check_id === "uv_available"
  )
  const selectedUvReady =
    selectedAgent?.distribution_type !== "uvx" ||
    !selectedUvCheck ||
    selectedUvCheck.status === "pass"
  const selectedIsSaving = selectedAgent
    ? Boolean(
        savingEnv[selectedAgent.agent_type] ||
        savingConfig[selectedAgent.agent_type]
      )
    : false
  // The Grok save spans config + env + reseed as one action under `grokSaving`
  // (the per-command saving flags clear before the reseed). While it runs, the
  // SHARED env controls below (textarea / Save / enabled switch) mutate the same
  // envText/enabled the Grok save captured, so gate them too — scoped to Grok so
  // other agents are unaffected.
  const selectedGrokSaving = selectedAgent?.agent_type === "grok" && grokSaving
  const selectedIsSavingEnv = selectedAgent
    ? Boolean(savingEnv[selectedAgent.agent_type])
    : false
  const selectedIsSavingConfig = selectedAgent
    ? Boolean(savingConfig[selectedAgent.agent_type])
    : false
  const selectedAgentKind = selectedAgent?.agent_type ?? null

  const selectedModelProviders = useMemo(() => {
    if (!selectedAgent) return []
    return modelProviders.filter(
      (p) => p.agent_type === selectedAgent.agent_type
    )
  }, [modelProviders, selectedAgent])

  // The provider each agent was last bound to, remembered across auth-mode
  // changes. The auth-mode handlers drop `draft.modelProviderId` whenever the
  // mode leaves "model_provider", so that a save in another mode cannot
  // persist a binding. Without this memory, coming BACK to provider mode falls
  // through to the auto-select below, which had no record of the user's own
  // choice. See `providerToRebindTo`.
  const lastBoundProviderRef = useRef<Partial<Record<AgentType, number>>>({})

  const selectedNeedsModelProvider = useMemo(() => {
    if (!selectedDraft) return false
    if (!selectedAgent) return false
    const at = selectedAgent.agent_type
    if (at === "claude_code")
      return selectedDraft.claudeAuthMode === "model_provider"
    if (at === "codex") return selectedDraft.codexAuthMode === "model_provider"
    if (at === "gemini")
      return selectedDraft.geminiAuthMode === "model_provider"
    return false
  }, [selectedAgent, selectedDraft])

  const selectedMissingModelProvider =
    selectedNeedsModelProvider && selectedDraft?.modelProviderId == null
  const selectedConfigText = selectedDraft?.configText ?? ""
  const selectedOpenCodeAuthJsonText = selectedDraft?.openCodeAuthJsonText ?? ""
  // Inline validation for `writable_roots`: codex would accept a relative entry
  // and resolve it against CODEX_HOME, so it is surfaced before the save throws.
  const codexRelativeWritableRoot =
    selectedAgent?.agent_type === "codex" && selectedDraft
      ? firstRelativeWritableRoot(selectedDraft.codexWritableRootsText)
      : null
  const selectedHermesProviderOption =
    selectedAgent?.agent_type === "hermes" && selectedDraft
      ? (HERMES_PROVIDERS.find((p) => p.id === selectedDraft.hermesProvider) ??
        null)
      : null
  const hermesCanUseNativeSetup =
    isDesktop() && getActiveRemoteConnectionId() === null
  const selectedOpenCodeConfig = useMemo(() => {
    if (selectedAgentKind !== "open_code" || !locale) return null
    return extractOpenCodeConfigValues(
      selectedConfigText,
      selectedOpenCodeAuthJsonText
    )
  }, [
    locale,
    selectedAgentKind,
    selectedConfigText,
    selectedOpenCodeAuthJsonText,
  ])
  const openCodeConnected = useMemo(() => {
    if (selectedAgentKind !== "open_code") return []
    return buildConnectedProviders({
      configText: selectedConfigText,
      authJsonText: selectedOpenCodeAuthJsonText,
      catalog: openCodeCatalog,
    })
  }, [
    selectedAgentKind,
    selectedConfigText,
    selectedOpenCodeAuthJsonText,
    openCodeCatalog,
  ])
  const openCodeModelOptions = useMemo(() => {
    const catalogGroups = buildConnectedModelOptions({
      connected: openCodeConnected,
      catalog: openCodeCatalog,
    })
    // Fall back to the config-derived groups before the catalog has loaded.
    return catalogGroups.length > 0
      ? catalogGroups
      : buildOpenCodeModelOptions(selectedOpenCodeConfig)
  }, [openCodeConnected, openCodeCatalog, selectedOpenCodeConfig])
  const openCodeCatalogIds = useMemo(
    () => new Set(openCodeCatalog.map((p) => p.id)),
    [openCodeCatalog]
  )
  // Split connected providers into two single-purpose surfaces:
  //  - well-known (catalog) providers connected via auth.json → top list
  //  - custom OpenAI-compatible endpoints (a `provider.<id>` block NOT in the
  //    catalog) → the bottom "custom provider" editor.
  // The discriminator is `hasConfigBlock && !inCatalog`, so an auth-only
  // well-known provider (no block) stays in the top list even if the catalog
  // fails to load — it can never be misfiled as custom and vanish.
  const openCodeWellKnownConnected = useMemo(
    () => openCodeConnected.filter((p) => !(p.hasConfigBlock && !p.inCatalog)),
    [openCodeConnected]
  )
  const openCodeCustomProviderIds = useMemo(
    () =>
      (selectedOpenCodeConfig?.providerIds ?? []).filter(
        (id) => !openCodeCatalogIds.has(id)
      ),
    [selectedOpenCodeConfig, openCodeCatalogIds]
  )
  // Lazily load the models.dev catalog the first time an OpenCode agent is
  // viewed. Backend resolves live → cache → bundled snapshot, so this never
  // hard-fails; on error we keep an empty catalog (custom-only flow) and allow
  // a retry the next time OpenCode is selected. The ref dedupes so we depend
  // only on `selectedAgentKind` — depending on the loading flag we set here
  // would re-run the effect and cancel its own in-flight request.
  useEffect(() => {
    if (selectedAgentKind !== "open_code") return
    if (openCodeCatalogRequestedRef.current) return
    openCodeCatalogRequestedRef.current = true
    setOpenCodeCatalogLoading(true)
    opencodeProviderCatalog()
      .then((list) => {
        setOpenCodeCatalog(list)
      })
      .catch((err) => {
        console.error("[Settings] opencode catalog load failed:", err)
        openCodeCatalogRequestedRef.current = false
      })
      .finally(() => {
        setOpenCodeCatalogLoading(false)
        setOpenCodeCatalogReady(true)
      })
  }, [selectedAgentKind])

  const selectedLatestRelease =
    selectedAgent && visitLatestRelease?.agentType === selectedAgent.agent_type
      ? visitLatestRelease.release
      : null

  const selectedChecks = useMemo(() => {
    if (!selectedAgent || !locale) return []
    return getAgentChecks(selectedAgent, selectedCurrent, selectedLatestRelease)
  }, [locale, selectedAgent, selectedCurrent, selectedLatestRelease])

  useEffect(() => {
    if (!selectedAgent || selectedChecks.length === 0) return
    setExpandedChecks((prev) => {
      let next = prev
      for (const check of selectedChecks) {
        const key = `${selectedAgent.agent_type}:${check.check_id}`
        if (typeof next[key] !== "undefined") continue
        if (next === prev) next = { ...prev }
        next[key] = check.status !== "pass"
      }
      return next
    })
  }, [selectedAgent, selectedChecks])

  // A passing Version Status card starts folded, and the unreviewed latest
  // release arrives a moment after it, so unfold the card for the offer. A card
  // the user has folded or unfolded by hand is left as they set it.
  useEffect(() => {
    if (!selectedAgent) return
    const offered = selectedChecks.some(
      (check) =>
        check.check_id === "version_status" &&
        check.footerFixes?.some((fix) => fix.kind === "upgrade_latest")
    )
    if (!offered) return
    const key = `${selectedAgent.agent_type}:version_status`
    if (userToggledChecksRef.current.has(key)) return
    setExpandedChecks((prev) => (prev[key] ? prev : { ...prev, [key]: true }))
  }, [selectedAgent, selectedChecks])

  useEffect(() => {
    if (!selectedOpenCodeConfig) {
      if (openCodeProviderId) setOpenCodeProviderId("")
      return
    }
    if (!openCodeProviderId) return
    if (selectedOpenCodeConfig.providerIds.includes(openCodeProviderId)) {
      return
    }
    setOpenCodeProviderId("")
  }, [openCodeProviderId, selectedOpenCodeConfig])

  useEffect(() => {
    if (!openCodeDeleteProviderId) return
    if (!selectedOpenCodeConfig) {
      setOpenCodeDeleteProviderId(null)
      return
    }
    if (
      !selectedOpenCodeConfig.providerIds.includes(openCodeDeleteProviderId)
    ) {
      setOpenCodeDeleteProviderId(null)
    }
  }, [openCodeDeleteProviderId, selectedOpenCodeConfig])

  const updateSelectedDraft = useCallback(
    (updater: (current: AgentDraft) => AgentDraft) => {
      if (!selectedAgent || !selectedDraft) return
      setDrafts((prev) => {
        const current = prev[selectedAgent.agent_type] ?? selectedDraft
        return {
          ...prev,
          [selectedAgent.agent_type]: updater(current),
        }
      })
    },
    [selectedAgent, selectedDraft]
  )

  const handleConfigTextChange = useCallback(
    (nextText: string) => {
      if (!selectedAgent || !selectedDraft) return
      const parseResult = parseConfigJsonText(nextText)
      setConfigErrors((prev) => ({
        ...prev,
        [selectedAgent.agent_type]: parseResult.error,
      }))

      if (parseResult.error) {
        updateSelectedDraft((current) => ({
          ...current,
          configText: nextText,
        }))
        return
      }

      if (selectedAgent.agent_type === "open_code") {
        const openCode = extractOpenCodeConfigValues(
          nextText,
          selectedDraft.openCodeAuthJsonText
        )
        updateSelectedDraft((current) => ({
          ...current,
          configText: nextText,
          model: openCode.model,
        }))
        return
      }

      if (selectedAgent.agent_type === "cline") {
        const cline = extractClineImportantValues(nextText)
        updateSelectedDraft((current) => ({
          ...current,
          configText: nextText,
          clineProvider: cline.provider,
          clineApiKey: cline.apiKey,
          clineModel: cline.model,
          clineBaseUrl: cline.baseUrl,
        }))
        return
      }

      const important = extractImportantConfigValues(
        selectedAgent.agent_type,
        parseEnvText(selectedDraft.envText),
        nextText
      )
      const geminiImportant =
        selectedAgent.agent_type === "gemini"
          ? extractGeminiImportantValues(
              parseEnvText(selectedDraft.envText),
              nextText
            )
          : null
      updateSelectedDraft((current) => ({
        ...current,
        configText: nextText,
        apiBaseUrl: geminiImportant
          ? geminiImportant.apiBaseUrl
          : important.apiBaseUrl,
        apiKey: important.apiKey,
        model: geminiImportant ? geminiImportant.model : important.model,
        geminiAuthMode: geminiImportant
          ? geminiImportant.authMode
          : current.geminiAuthMode,
        geminiApiKey: geminiImportant
          ? geminiImportant.geminiApiKey
          : current.geminiApiKey,
        googleApiKey: geminiImportant
          ? geminiImportant.googleApiKey
          : current.googleApiKey,
        googleCloudProject: geminiImportant
          ? geminiImportant.googleCloudProject
          : current.googleCloudProject,
        googleCloudLocation: geminiImportant
          ? geminiImportant.googleCloudLocation
          : current.googleCloudLocation,
        googleApplicationCredentials: geminiImportant
          ? geminiImportant.googleApplicationCredentials
          : current.googleApplicationCredentials,
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
      }))
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleImportantConfigChange = useCallback(
    (key: ImportantConfigKey, value: string) => {
      if (!selectedAgent || !selectedDraft) return
      const nextDraft = applyImportantFieldToDraft(selectedDraft, key, value)
      const nextJson = patchImportantConfigText(
        selectedAgent.agent_type,
        selectedDraft.configText,
        buildImportantPatchFromDraft(nextDraft)
      )
      if (nextJson.recoveredFromInvalid) {
        toast.warning(t("warnings.nativeJsonRecoveredStructured"))
      }
      setConfigErrors((prev) => ({
        ...prev,
        [selectedAgent.agent_type]: null,
      }))
      updateSelectedDraft((current) => {
        const nextCurrent = applyImportantFieldToDraft(current, key, value)
        return {
          ...nextCurrent,
          envText: patchEnvByImportantKey(
            selectedAgent.agent_type,
            current.envText,
            key,
            value
          ),
          configText: nextJson.configText,
        }
      })
    },
    [selectedAgent, selectedDraft, t, updateSelectedDraft]
  )

  // Toggle a Claude Code hardening flag: write the explicit "1"/"0" value into
  // the native config's `env` (and the DB env overlay in lockstep).
  const handleClaudeEnvFlagChange = useCallback(
    (
      field: "claudeSendAttributionHeader" | "claudeDisableNonessentialTraffic",
      envKey: string,
      enabled: boolean
    ) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "claude_code"
      )
        return
      const value = enabled ? CLAUDE_ENV_FLAG_ON : CLAUDE_ENV_FLAG_OFF
      const next = setClaudeEnvFlagInConfigText(
        selectedDraft.configText,
        envKey,
        value
      )
      if (next.recoveredFromInvalid) {
        toast.warning(t("warnings.nativeJsonRecoveredStructured"))
      }
      setConfigErrors((prev) => ({
        ...prev,
        [selectedAgent.agent_type]: null,
      }))
      updateSelectedDraft((current) => ({
        ...current,
        [field]: enabled,
        // The backend folds native `config.env` into `agent.env`, so keep the
        // DB env overlay (envText) in lockstep — otherwise persistEnv would
        // re-persist a stale value from the overlay. Mirrors
        // handleImportantConfigChange's dual configText + envText write.
        envText: patchEnvText(current.envText, { [envKey]: value }),
        configText: next.configText,
      }))
    },
    [selectedAgent, selectedDraft, t, updateSelectedDraft]
  )

  const handleGrokAuthModeChange = useCallback(
    (nextMode: GrokAuthMethod) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "grok"
      )
        return
      // Record the method knob in env; on subscription strip XAI_API_KEY so the
      // editable env can't override the `grok login` credential (the launch path
      // enforces the same via apply_grok_env_policy). Clearing the draft apiKey
      // keeps the now-hidden key input from resurrecting a stale value.
      updateSelectedDraft((current) => ({
        ...current,
        grokAuthMode: nextMode,
        apiKey: nextMode === "subscription" ? "" : current.apiKey,
        envText: patchEnvText(current.envText, {
          GROK_AUTH_MODE: nextMode,
          ...(nextMode === "subscription" ? { XAI_API_KEY: "" } : {}),
        }),
      }))
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleClaudeAuthModeChange = useCallback(
    (nextMode: ClaudeAuthMode) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "claude_code"
      )
        return

      const keys = importantEnvKeysByAgent("claude_code")
      const allEnvKeys = [...keys.apiBaseUrl, ...keys.apiKey]

      if (nextMode === "official_subscription") {
        // Clear API URL/API Key from env and config
        const envPatch: Record<string, string> = {}
        for (const k of allEnvKeys) envPatch[k] = ""
        // Build clean display config (remove null keys)
        const parsed = parseConfigJsonText(selectedDraft.configText)
        const config: Record<string, unknown> = parsed.error
          ? {}
          : { ...parsed.config }
        delete config.apiBaseUrl
        delete config.apiKey
        if (config.env && typeof config.env === "object") {
          const cfgEnv = { ...(config.env as Record<string, unknown>) }
          for (const k of allEnvKeys) delete cfgEnv[k]
          if (Object.keys(cfgEnv).length > 0) {
            config.env = cfgEnv
          } else {
            delete config.env
          }
        }
        const nextConfigText =
          Object.keys(config).length > 0 ? JSON.stringify(config, null, 2) : ""
        setConfigErrors((prev) => ({
          ...prev,
          [selectedAgent.agent_type]: null,
        }))
        updateSelectedDraft((current) => ({
          ...current,
          claudeAuthMode: nextMode,
          modelProviderId: null,
          apiBaseUrl: "",
          apiKey: "",
          envText: patchEnvText(current.envText, envPatch),
          configText: nextConfigText,
        }))
        return
      }

      // "custom" or "model_provider" — keep existing values, just switch mode
      updateSelectedDraft((current) => ({
        ...current,
        claudeAuthMode: nextMode,
        modelProviderId:
          nextMode === "model_provider" ? current.modelProviderId : null,
      }))
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleModelProviderSelect = useCallback(
    (providerIdStr: string) => {
      if (!selectedAgent || !selectedDraft) return
      const providerId = providerIdStr ? Number(providerIdStr) : null
      if (providerId != null) {
        lastBoundProviderRef.current[selectedAgent.agent_type] = providerId
      }
      const provider = providerId
        ? modelProviders.find((p) => p.id === providerId)
        : null
      const apiUrl = provider?.api_url ?? ""
      const apiKey = provider?.api_key ?? ""
      const agentType = selectedAgent.agent_type

      if (agentType === "claude_code") {
        // Provider's model fields are authoritative: missing/empty keys clear
        // the corresponding draft + env value.
        const claudeModel = parseClaudeProviderModel(provider?.model ?? null)
        const claudeMain = claudeModel.main ?? ""
        const claudeReasoning = claudeModel.reasoning ?? ""
        const claudeHaiku = claudeModel.haiku ?? ""
        const claudeSonnet = claudeModel.sonnet ?? ""
        const claudeOpus = claudeModel.opus ?? ""
        const claudeFable = claudeModel.fable ?? ""
        const claudeCustomOption = claudeModel.customOption ?? ""
        const claudeCustomOptionName = claudeModel.customOptionName ?? ""
        const claudeCustomOptionDescription =
          claudeModel.customOptionDescription ?? ""
        const nextConfigJson = patchImportantConfigText(
          agentType,
          selectedDraft.configText,
          {
            apiBaseUrl: apiUrl,
            apiKey,
            model: selectedDraft.model,
            claudeMainModel: claudeMain,
            claudeReasoningModel: claudeReasoning,
            claudeDefaultHaikuModel: claudeHaiku,
            claudeDefaultSonnetModel: claudeSonnet,
            claudeDefaultOpusModel: claudeOpus,
            claudeDefaultFableModel: claudeFable,
            // The custom model option travels with the provider's model JSON,
            // authoritative like the six model fields: a defined value sets it,
            // an empty/omitted value clears the key from config.env.
            claudeCustomModelOption: claudeCustomOption,
            claudeCustomModelOptionName: claudeCustomOptionName,
            claudeCustomModelOptionDescription: claudeCustomOptionDescription,
          }
        )
        setConfigErrors((prev) => ({
          ...prev,
          [agentType]: null,
        }))
        updateSelectedDraft((current) => {
          let nextEnvText = patchEnvByImportantKey(
            agentType,
            current.envText,
            "apiBaseUrl",
            apiUrl
          )
          nextEnvText = patchEnvByImportantKey(
            agentType,
            nextEnvText,
            "apiKey",
            apiKey
          )
          nextEnvText = patchEnvByImportantKey(
            agentType,
            nextEnvText,
            "claudeMainModel",
            claudeMain
          )
          nextEnvText = patchEnvByImportantKey(
            agentType,
            nextEnvText,
            "claudeReasoningModel",
            claudeReasoning
          )
          nextEnvText = patchEnvByImportantKey(
            agentType,
            nextEnvText,
            "claudeDefaultHaikuModel",
            claudeHaiku
          )
          nextEnvText = patchEnvByImportantKey(
            agentType,
            nextEnvText,
            "claudeDefaultSonnetModel",
            claudeSonnet
          )
          nextEnvText = patchEnvByImportantKey(
            agentType,
            nextEnvText,
            "claudeDefaultOpusModel",
            claudeOpus
          )
          nextEnvText = patchEnvByImportantKey(
            agentType,
            nextEnvText,
            "claudeDefaultFableModel",
            claudeFable
          )
          nextEnvText = patchEnvByImportantKey(
            agentType,
            nextEnvText,
            "claudeCustomModelOption",
            claudeCustomOption
          )
          nextEnvText = patchEnvByImportantKey(
            agentType,
            nextEnvText,
            "claudeCustomModelOptionName",
            claudeCustomOptionName
          )
          nextEnvText = patchEnvByImportantKey(
            agentType,
            nextEnvText,
            "claudeCustomModelOptionDescription",
            claudeCustomOptionDescription
          )
          return {
            ...current,
            modelProviderId: providerId,
            apiBaseUrl: apiUrl,
            apiKey,
            claudeMainModel: claudeMain,
            claudeReasoningModel: claudeReasoning,
            claudeDefaultHaikuModel: claudeHaiku,
            claudeDefaultSonnetModel: claudeSonnet,
            claudeDefaultOpusModel: claudeOpus,
            claudeDefaultFableModel: claudeFable,
            claudeCustomModelOption: claudeCustomOption,
            claudeCustomModelOptionName: claudeCustomOptionName,
            claudeCustomModelOptionDescription: claudeCustomOptionDescription,
            envText: nextEnvText,
            configText: nextConfigJson.configText,
          }
        })
      } else if (agentType === "codex") {
        // The provider stores a structured model config; root `model` is its
        // default slug and we reference the catalog the bind path generates.
        const codexList = parseCodexModelConfig(provider?.model ?? null)
        const codexHasConfig =
          codexList.customs.length > 0 ||
          (codexList.excludedOfficials?.length ?? 0) > 0
        const codexModel = codexList.default ?? codexList.customs[0]?.slug ?? ""
        const nextAuthPatch = patchCodexAuthJsonText(
          selectedDraft.codexAuthJsonText,
          { apiKey, authMode: null }
        )
        const nextAuthJsonText = nextAuthPatch.authJsonText
        // Always pass the provider's model (empty string clears it from the toml).
        let nextConfigTomlText = patchCodexConfigTomlText(
          selectedDraft.codexConfigTomlText,
          {
            modelProvider: CODEX_DEFAULT_MODEL_PROVIDER,
            apiBaseUrl: apiUrl,
            model: codexModel,
          }
        )
        nextConfigTomlText = updateTomlRootStringKey(
          nextConfigTomlText,
          "model_catalog_json",
          codexHasConfig ? "codeg-model-catalog.json" : ""
        )
        const synced = extractCodexImportantValues(
          nextAuthJsonText,
          nextConfigTomlText
        )
        updateSelectedDraft((current) => ({
          ...current,
          modelProviderId: providerId,
          apiBaseUrl: apiUrl,
          apiKey,
          model: codexModel,
          codexModelList: codexList,
          codexAuthJsonText: nextAuthJsonText,
          codexConfigTomlText: nextConfigTomlText,
          codexModelProvider: CODEX_DEFAULT_MODEL_PROVIDER,
          codexProviderOptions: synced.providerOptions,
          envText: patchEnvText(current.envText, {
            OPENAI_API_KEY: apiKey,
            OPENAI_BASE_URL: apiUrl,
            OPENAI_MODEL: codexModel,
          }),
        }))
      } else if (agentType === "gemini") {
        const geminiModel = provider?.model?.trim() ?? ""
        const nextConfigJson = patchGeminiConfigText(selectedDraft.configText, {
          apiBaseUrl: apiUrl,
          geminiApiKey: apiKey,
        })
        setConfigErrors((prev) => ({
          ...prev,
          [agentType]: null,
        }))
        updateSelectedDraft((current) => {
          let nextEnvText = patchGeminiEnvText(current.envText, {
            apiBaseUrl: apiUrl,
            geminiApiKey: apiKey,
          })
          // Always overwrite GEMINI_MODEL with the provider's value (empty
          // string clears it).
          nextEnvText = patchEnvText(nextEnvText, {
            GEMINI_MODEL: geminiModel,
          })
          return {
            ...current,
            modelProviderId: providerId,
            apiBaseUrl: apiUrl,
            apiKey,
            geminiApiKey: apiKey,
            model: geminiModel,
            envText: nextEnvText,
            configText: nextConfigJson.configText,
          }
        })
      } else {
        updateSelectedDraft((current) => ({
          ...current,
          modelProviderId: providerId,
        }))
      }
    },
    [selectedAgent, selectedDraft, modelProviders, updateSelectedDraft]
  )

  // Auto-select a provider when the user switches an agent to
  // "model_provider" auth mode and the draft holds no binding. If the list is
  // empty, the existing "noModelProviderAvailable" hint handles the empty
  // state. The user's own last pick (then the saved binding) wins over the head
  // of the list, because an auth-mode round trip lands here too and rebinding
  // to whichever provider happens to be first copies ITS model names over the
  // one the user was actually on.
  useEffect(() => {
    if (!selectedNeedsModelProvider) return
    if (selectedDraft?.modelProviderId != null) return
    if (!selectedAgent) return
    const target = providerToRebindTo(
      selectedModelProviders,
      lastBoundProviderRef.current[selectedAgent.agent_type],
      selectedAgent.model_provider_id
    )
    if (!target) return
    handleModelProviderSelect(String(target.id))
  }, [
    selectedAgent,
    selectedNeedsModelProvider,
    selectedDraft?.modelProviderId,
    selectedModelProviders,
    handleModelProviderSelect,
  ])

  const handleGeminiFieldChange = useCallback(
    (
      key:
        | "apiBaseUrl"
        | "model"
        | "geminiApiKey"
        | "googleApiKey"
        | "googleCloudProject"
        | "googleCloudLocation"
        | "googleApplicationCredentials",
      value: string
    ) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "gemini"
      )
        return

      const nextValues = {
        authMode: selectedDraft.geminiAuthMode,
        apiBaseUrl: selectedDraft.apiBaseUrl,
        geminiApiKey: selectedDraft.geminiApiKey,
        googleApiKey: selectedDraft.googleApiKey,
        googleCloudProject: selectedDraft.googleCloudProject,
        googleCloudLocation: selectedDraft.googleCloudLocation,
        googleApplicationCredentials:
          selectedDraft.googleApplicationCredentials,
        model: selectedDraft.model,
      }
      nextValues[key] = value
      const normalizedValues = patchGeminiAuthMode(
        nextValues,
        nextValues.authMode
      )

      const nextConfig = patchGeminiConfigText(selectedDraft.configText, {
        apiBaseUrl: normalizedValues.apiBaseUrl,
        model: normalizedValues.model,
        geminiApiKey: normalizedValues.geminiApiKey,
        googleApiKey: normalizedValues.googleApiKey,
        googleCloudProject: normalizedValues.googleCloudProject,
        googleCloudLocation: normalizedValues.googleCloudLocation,
        googleApplicationCredentials:
          normalizedValues.googleApplicationCredentials,
      })
      if (nextConfig.recoveredFromInvalid) {
        toast.warning(t("warnings.nativeJsonRecoveredStructured"))
      }
      setConfigErrors((prev) => ({
        ...prev,
        [selectedAgent.agent_type]: null,
      }))

      updateSelectedDraft((current) => {
        const nextEnvText = patchGeminiEnvText(current.envText, {
          apiBaseUrl: normalizedValues.apiBaseUrl,
          model: normalizedValues.model,
          geminiApiKey: normalizedValues.geminiApiKey,
          googleApiKey: normalizedValues.googleApiKey,
          googleCloudProject: normalizedValues.googleCloudProject,
          googleCloudLocation: normalizedValues.googleCloudLocation,
          googleApplicationCredentials:
            normalizedValues.googleApplicationCredentials,
        })
        return {
          ...current,
          apiBaseUrl: normalizedValues.apiBaseUrl,
          model: normalizedValues.model,
          apiKey:
            normalizedValues.geminiApiKey || normalizedValues.googleApiKey,
          geminiAuthMode: normalizedValues.authMode,
          geminiApiKey: normalizedValues.geminiApiKey,
          googleApiKey: normalizedValues.googleApiKey,
          googleCloudProject: normalizedValues.googleCloudProject,
          googleCloudLocation: normalizedValues.googleCloudLocation,
          googleApplicationCredentials:
            normalizedValues.googleApplicationCredentials,
          envText: nextEnvText,
          configText: nextConfig.configText,
        }
      })
    },
    [selectedAgent, selectedDraft, t, updateSelectedDraft]
  )

  const handleGeminiAuthModeChange = useCallback(
    (nextMode: GeminiAuthMode) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "gemini"
      )
        return

      if (nextMode === "model_provider") {
        // Keep existing values; provider selection will fill API URL/Key
        updateSelectedDraft((current) => ({
          ...current,
          geminiAuthMode: nextMode,
          modelProviderId: current.modelProviderId,
        }))
        return
      }

      const patched = patchGeminiAuthMode(
        {
          authMode: selectedDraft.geminiAuthMode,
          apiBaseUrl: selectedDraft.apiBaseUrl,
          geminiApiKey: selectedDraft.geminiApiKey,
          googleApiKey: selectedDraft.googleApiKey,
          googleCloudProject: selectedDraft.googleCloudProject,
          googleCloudLocation: selectedDraft.googleCloudLocation,
          googleApplicationCredentials:
            selectedDraft.googleApplicationCredentials,
          model: selectedDraft.model,
        },
        nextMode
      )

      const nextConfig = patchGeminiConfigText(selectedDraft.configText, {
        apiBaseUrl: patched.apiBaseUrl,
        model: patched.model,
        geminiApiKey: patched.geminiApiKey,
        googleApiKey: patched.googleApiKey,
        googleCloudProject: patched.googleCloudProject,
        googleCloudLocation: patched.googleCloudLocation,
        googleApplicationCredentials: patched.googleApplicationCredentials,
      })
      if (nextConfig.recoveredFromInvalid) {
        toast.warning(t("warnings.nativeJsonRecoveredStructured"))
      }
      setConfigErrors((prev) => ({
        ...prev,
        [selectedAgent.agent_type]: null,
      }))

      updateSelectedDraft((current) => ({
        ...current,
        geminiAuthMode: patched.authMode,
        modelProviderId: null,
        apiBaseUrl: patched.apiBaseUrl,
        apiKey: patched.geminiApiKey || patched.googleApiKey,
        geminiApiKey: patched.geminiApiKey,
        googleApiKey: patched.googleApiKey,
        googleCloudProject: patched.googleCloudProject,
        googleCloudLocation: patched.googleCloudLocation,
        googleApplicationCredentials: patched.googleApplicationCredentials,
        envText: patchGeminiEnvText(current.envText, {
          apiBaseUrl: patched.apiBaseUrl,
          model: patched.model,
          geminiApiKey: patched.geminiApiKey,
          googleApiKey: patched.googleApiKey,
          googleCloudProject: patched.googleCloudProject,
          googleCloudLocation: patched.googleCloudLocation,
          googleApplicationCredentials: patched.googleApplicationCredentials,
        }),
        configText: nextConfig.configText,
      }))
    },
    [selectedAgent, selectedDraft, t, updateSelectedDraft]
  )

  const handleOpenClawFieldChange = useCallback(
    (
      key: "openClawGatewayUrl" | "openClawGatewayToken" | "openClawSessionKey",
      value: string
    ) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "open_claw"
      )
        return

      const envKeyMap: Record<string, string> = {
        openClawGatewayUrl: OPENCLAW_ENV_KEYS.gatewayUrl,
        openClawGatewayToken: OPENCLAW_ENV_KEYS.gatewayToken,
        openClawSessionKey: OPENCLAW_ENV_KEYS.sessionKey,
      }

      updateSelectedDraft((current) => ({
        ...current,
        [key]: value,
        envText: patchEnvText(current.envText, {
          [envKeyMap[key]]: value,
        }),
      }))
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleHermesFieldChange = useCallback(
    (
      key:
        | "hermesProvider"
        | "apiKey"
        | "model"
        | "apiBaseUrl"
        | "hermesConfigYaml",
      value: string
    ) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "hermes"
      )
        return
      updateSelectedDraft((current) => {
        if (key !== "hermesProvider") {
          return { ...current, [key]: value }
        }
        // Switching provider: the projection only carries the *configured*
        // provider's key, so restore it when returning to that provider and
        // clear otherwise — never carry one provider's secret into another's
        // env var. An empty key field then means "leave the stored key as-is".
        const projected = parseHermesConfig(
          typeof selectedAgent.config_json === "string"
            ? selectedAgent.config_json
            : ""
        )
        const sameAsConfigured = value === projected.provider
        return {
          ...current,
          hermesProvider: value,
          apiKey: sameAsConfigured ? projected.apiKey : "",
          apiBaseUrl: sameAsConfigured ? projected.baseUrl : "",
        }
      })
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleSaveHermesConfig = useCallback(
    async (mode: "structured" | "raw") => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "hermes"
      )
        return
      const agentType = selectedAgent.agent_type
      const draft = selectedDraft
      const providerOption = HERMES_PROVIDERS.find(
        (p) => p.id === draft.hermesProvider
      )
      setSavingConfig((prev) => ({ ...prev, [agentType]: true }))
      try {
        await acpUpdateHermesConfig(
          mode === "raw"
            ? {
                provider: draft.hermesProvider,
                rawConfigYaml: draft.hermesConfigYaml,
              }
            : {
                provider: draft.hermesProvider,
                // Blank key, or a provider with no key field (OAuth / AWS) →
                // null → backend leaves the stored ~/.hermes/.env value
                // untouched (so switching providers can't wipe it).
                apiKey:
                  providerOption?.kind !== "apiKey" || !draft.apiKey.trim()
                    ? null
                    : draft.apiKey,
                model: draft.model,
                baseUrl: providerOption?.needsBaseUrl ? draft.apiBaseUrl : null,
              }
        )
        await refreshAgents()
        // Drop the draft so it rebuilds from the freshly-persisted projection —
        // otherwise the *other* mode (structured fields vs. raw config.yaml)
        // keeps stale content and a later save could overwrite this one.
        setDrafts((prev) => {
          const next = { ...prev }
          delete next[agentType]
          return next
        })
        toast.success(t("toasts.hermesSaved"), {
          description: t("toasts.configSavedHint"),
        })
      } catch (err) {
        console.error("[Settings] save hermes config failed:", err)
        toast.error(t("toasts.saveHermesFailed"), {
          description: toErrorMessage(err),
        })
      } finally {
        setSavingConfig((prev) => ({ ...prev, [agentType]: false }))
      }
    },
    [selectedAgent, selectedDraft, refreshAgents, t]
  )

  // Hermes's interactive setup (`--setup` / `hermes model`) needs a real TTY +
  // browser, so launch it in an external OS terminal on local desktop (the
  // backend builds the exact command). Fall back to copying the displayed
  // command (web / remote, or if the launch fails).
  const runHermesSetupCommand = useCallback(
    async (kind: "setup" | "model", displayCommand: string) => {
      const native = isDesktop() && getActiveRemoteConnectionId() === null
      if (native) {
        try {
          await acpOpenHermesSetupTerminal(kind)
          return
        } catch (err) {
          console.error("[Settings] open hermes setup terminal failed:", err)
        }
      }
      if (displayCommand) {
        const ok = await copyTextToClipboard(displayCommand)
        if (ok) toast.success(t("hermes.commandCopied"))
      }
    },
    [t]
  )

  const handleRevealHermesHome = useCallback(async () => {
    try {
      await acpRevealHermesHome()
    } catch (err) {
      console.error("[Settings] reveal hermes home failed:", err)
      toast.error(toErrorMessage(err))
    }
  }, [])

  const handleClineFieldChange = useCallback(
    (
      key: "clineProvider" | "clineApiKey" | "clineModel" | "clineBaseUrl",
      value: string
    ) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "cline"
      )
        return

      updateSelectedDraft((current) => {
        const next = { ...current, [key]: value }
        // Rebuild config_json from Cline draft fields
        const config: Record<string, unknown> = {}
        config.apiProvider =
          key === "clineProvider" ? value : next.clineProvider
        const apiKey = key === "clineApiKey" ? value : next.clineApiKey
        if (apiKey.trim()) config.apiKey = apiKey.trim()
        const model = key === "clineModel" ? value : next.clineModel
        if (model.trim()) config.model = model.trim()
        const baseUrl = key === "clineBaseUrl" ? value : next.clineBaseUrl
        if (baseUrl.trim()) config.apiBaseUrl = baseUrl.trim()
        next.configText = JSON.stringify(config, null, 2)
        return next
      })
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleOpenCodeConfigPatch = useCallback(
    (mutator: (config: Record<string, unknown>) => void) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "open_code"
      )
        return
      const nextConfig = patchOpenCodeConfigText(
        selectedDraft.configText,
        mutator
      )
      if (nextConfig.recoveredFromInvalid) {
        toast.warning(t("warnings.nativeJsonRecoveredOpenCode"))
      }
      setConfigErrors((prev) => ({
        ...prev,
        [selectedAgent.agent_type]: null,
      }))
      const parsed = extractOpenCodeConfigValues(
        nextConfig.configText,
        selectedDraft.openCodeAuthJsonText
      )
      updateSelectedDraft((current) => ({
        ...current,
        configText: nextConfig.configText,
        model: parsed.model,
      }))
    },
    [selectedAgent, selectedDraft, t, updateSelectedDraft]
  )

  const handleOpenCodeFieldChange = useCallback(
    (key: "model" | "small_model", value: string) => {
      handleOpenCodeConfigPatch((config) => {
        const trimmed = value.trim()
        if (!trimmed) {
          delete config[key]
          return
        }
        config[key] = trimmed
      })
    },
    [handleOpenCodeConfigPatch]
  )

  // Connect a provider from the dialog: sync the draft, then persist both files.
  const applyOpenCodeConnect = useCallback(
    async (
      next: { configText: string; authJsonText: string },
      providerId: string
    ) => {
      if (!selectedAgent || selectedAgent.agent_type !== "open_code") return
      const parsed = extractOpenCodeConfigValues(
        next.configText,
        next.authJsonText
      )
      updateSelectedDraft((current) => ({
        ...current,
        configText: next.configText,
        openCodeAuthJsonText: next.authJsonText,
        model: parsed.model,
      }))
      setConfigErrors((prev) => ({ ...prev, open_code: null }))
      try {
        await persistConfig("open_code", next.configText, {
          openCodeAuthJsonText: next.authJsonText,
        })
        toast.success(t("toasts.providerConnected", { providerId }), {
          description: t("toasts.configSavedHint"),
        })
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err)
        toast.error(t("toasts.connectFailed", { providerId }), {
          description: message,
        })
        throw err
      }
    },
    [selectedAgent, updateSelectedDraft, persistConfig, t]
  )

  const handleOpenCodeDisconnect = useCallback(
    async (providerId: string, hasConfigBlock: boolean) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "open_code"
      )
        return
      const next = disconnectProvider({
        configText: selectedDraft.configText,
        authJsonText: selectedDraft.openCodeAuthJsonText,
        providerId,
        removeConfigBlock: hasConfigBlock,
      })
      const parsed = extractOpenCodeConfigValues(
        next.configText,
        next.authJsonText
      )
      updateSelectedDraft((current) => ({
        ...current,
        configText: next.configText,
        openCodeAuthJsonText: next.authJsonText,
        model: parsed.model,
      }))
      try {
        await persistConfig("open_code", next.configText, {
          openCodeAuthJsonText: next.authJsonText,
        })
        toast.success(t("toasts.providerDisconnected", { providerId }))
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err)
        toast.error(t("toasts.disconnectFailed", { providerId }), {
          description: message,
        })
      }
    },
    [selectedAgent, selectedDraft, updateSelectedDraft, persistConfig, t]
  )

  const handleOpenCodeToggleEnabled = useCallback(
    async (providerId: string, enabled: boolean) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "open_code"
      )
        return
      const nextConfig = setProviderEnabled({
        configText: selectedDraft.configText,
        providerId,
        enabled,
      })
      updateSelectedDraft((current) => ({
        ...current,
        configText: nextConfig,
      }))
      try {
        await persistConfig("open_code", nextConfig, {
          openCodeAuthJsonText: selectedDraft.openCodeAuthJsonText,
        })
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err)
        toast.error(t("toasts.saveOpenCodeFailed"), { description: message })
      }
    },
    [selectedAgent, selectedDraft, updateSelectedDraft, persistConfig, t]
  )

  // Force a fresh models.dev fetch (bypassing the 24h cache) on demand.
  const handleOpenCodeRefreshCatalog = useCallback(async () => {
    setOpenCodeCatalogLoading(true)
    try {
      const list = await opencodeProviderCatalog(true)
      setOpenCodeCatalog(list)
      openCodeCatalogRequestedRef.current = true
      toast.success(t("toasts.catalogRefreshed", { count: list.length }))
    } catch (err) {
      console.error("[Settings] opencode catalog refresh failed:", err)
      toast.error(t("toasts.catalogRefreshFailed"), {
        description: err instanceof Error ? err.message : String(err),
      })
    } finally {
      setOpenCodeCatalogLoading(false)
    }
  }, [t])

  const handleOpenCodeRemoveProvider = useCallback(
    (providerId: string) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "open_code"
      ) {
        return null
      }
      const targetId = providerId.trim()
      if (!targetId) return null

      const nextConfig = patchOpenCodeConfigText(
        selectedDraft.configText,
        (config) => {
          const providerRoot = asObjectRecord(config.provider)
          if (providerRoot) {
            delete providerRoot[targetId]
            if (Object.keys(providerRoot).length === 0) {
              delete config.provider
            }
          }

          const enabledProviders = Array.isArray(config.enabled_providers)
            ? config.enabled_providers
                .filter((item): item is string => typeof item === "string")
                .filter((item) => item !== targetId)
            : []
          if (enabledProviders.length > 0) {
            config.enabled_providers = enabledProviders
          } else {
            delete config.enabled_providers
          }

          const disabledProviders = Array.isArray(config.disabled_providers)
            ? config.disabled_providers
                .filter((item): item is string => typeof item === "string")
                .filter((item) => item !== targetId)
            : []
          if (disabledProviders.length > 0) {
            config.disabled_providers = disabledProviders
          } else {
            delete config.disabled_providers
          }

          // Don't leave model/small_model pointing at the removed provider.
          for (const key of [
            "model",
            "small_model",
            "smallModel",
            "small-model",
          ]) {
            if (modelReferencesProvider(config[key], targetId)) {
              delete config[key]
            }
          }
        }
      )
      if (nextConfig.recoveredFromInvalid) {
        toast.warning(t("warnings.nativeJsonRecoveredOpenCode"))
      }

      const nextAuth = patchOpenCodeAuthJsonText(
        selectedDraft.openCodeAuthJsonText,
        (authObject) => {
          delete authObject[targetId]
        }
      )
      if (nextAuth.recoveredFromInvalid) {
        toast.warning(t("warnings.openCodeAuthRecovered"))
      }

      const nextOpenCode = extractOpenCodeConfigValues(
        nextConfig.configText,
        nextAuth.authJsonText
      )
      const nextDraft = {
        ...selectedDraft,
        configText: nextConfig.configText,
        openCodeAuthJsonText: nextAuth.authJsonText,
        model: nextOpenCode.model,
      }
      setConfigErrors((prev) => ({
        ...prev,
        [selectedAgent.agent_type]: null,
      }))
      setDrafts((prev) => ({
        ...prev,
        [selectedAgent.agent_type]: nextDraft,
      }))
      setOpenCodeProviderId((current) => (current === targetId ? "" : current))
      setOpenCodeNewModelIds((prev) => {
        if (typeof prev[targetId] === "undefined") return prev
        const next = { ...prev }
        delete next[targetId]
        return next
      })
      setOpenCodeModelConfigExpanded((prev) => {
        if (typeof prev[targetId] === "undefined") return prev
        const next = { ...prev }
        delete next[targetId]
        return next
      })
      setOpenCodeModelIdDrafts((prev) => {
        const prefix = `${targetId}:`
        const keys = Object.keys(prev).filter((key) => key.startsWith(prefix))
        if (keys.length === 0) return prev
        const next = { ...prev }
        for (const key of keys) {
          delete next[key]
        }
        return next
      })
      return {
        enabled: nextDraft.enabled,
        envText: nextDraft.envText,
        configText: nextDraft.configText,
        openCodeAuthJsonText: nextDraft.openCodeAuthJsonText,
      }
    },
    [selectedAgent, selectedDraft, t]
  )

  const confirmOpenCodeProviderDelete = useCallback(() => {
    const providerId = openCodeDeleteProviderId?.trim()
    if (!providerId) return
    const removed = handleOpenCodeRemoveProvider(providerId)
    setOpenCodeDeleteProviderId(null)
    if (
      !removed ||
      !selectedAgent ||
      selectedAgent.agent_type !== "open_code"
    ) {
      return
    }
    persistConfig(selectedAgent.agent_type, removed.configText, {
      openCodeAuthJsonText: removed.openCodeAuthJsonText,
    })
      .then(() => {
        toast.success(t("toasts.providerDeleted", { providerId }), {
          description: t("toasts.openCodeConfigSynced"),
        })
      })
      .catch((err) => {
        console.error("[Settings] remove opencode provider failed:", err)
        const message = toErrorMessage(err)
        toast.error(t("toasts.providerDeleteFailed", { providerId }), {
          description: message,
        })
      })
  }, [
    handleOpenCodeRemoveProvider,
    openCodeDeleteProviderId,
    persistConfig,
    selectedAgent,
    t,
  ])

  const handleOpenCodeProviderStatusChange = useCallback(
    (providerId: string, enabled: boolean) => {
      const targetId = providerId.trim()
      if (!targetId) return
      handleOpenCodeConfigPatch((config) => {
        const hadEnabledAllowlist =
          Array.isArray(config.enabled_providers) &&
          config.enabled_providers.length > 0
        const enabledProviders = Array.isArray(config.enabled_providers)
          ? config.enabled_providers
              .filter((item): item is string => typeof item === "string")
              .map((item) => item.trim())
              .filter(Boolean)
          : []
        const disabledProviders = Array.isArray(config.disabled_providers)
          ? config.disabled_providers
              .filter((item): item is string => typeof item === "string")
              .map((item) => item.trim())
              .filter(Boolean)
          : []

        const nextEnabled = new Set(enabledProviders)
        const nextDisabled = new Set(disabledProviders)

        if (enabled) {
          nextDisabled.delete(targetId)
          if (hadEnabledAllowlist) {
            nextEnabled.add(targetId)
          }
        } else {
          nextDisabled.add(targetId)
          if (hadEnabledAllowlist) {
            nextEnabled.delete(targetId)
          }
        }

        const enabledArray = Array.from(nextEnabled)
        const disabledArray = Array.from(nextDisabled)
        if (enabledArray.length > 0) {
          config.enabled_providers = enabledArray
        } else {
          delete config.enabled_providers
        }
        if (disabledArray.length > 0) {
          config.disabled_providers = disabledArray
        } else {
          delete config.disabled_providers
        }
      })
    },
    [handleOpenCodeConfigPatch]
  )

  const handleOpenCodeProviderFieldChange = useCallback(
    (
      providerId: string,
      key: "name" | "api" | "npm" | "baseURL" | "apiKey",
      value: string
    ) => {
      const targetId = providerId.trim()
      if (!targetId) return

      // The API key is a secret: it goes ONLY into auth.json, never into
      // opencode.json. setProviderApiKey also scrubs any stale options.apiKey.
      if (key === "apiKey") {
        if (!selectedDraft) return
        const next = setProviderApiKey({
          configText: selectedDraft.configText,
          authJsonText: selectedDraft.openCodeAuthJsonText,
          providerId: targetId,
          apiKey: value,
        })
        const parsed = extractOpenCodeConfigValues(
          next.configText,
          next.authJsonText
        )
        setConfigErrors((prev) => ({ ...prev, open_code: null }))
        updateSelectedDraft((current) => ({
          ...current,
          configText: next.configText,
          openCodeAuthJsonText: next.authJsonText,
          model: parsed.model,
        }))
        return
      }

      handleOpenCodeConfigPatch((config) => {
        const providerRoot = asObjectRecord(config.provider) ?? {}
        if (!asObjectRecord(config.provider)) {
          config.provider = providerRoot
        }

        const currentProvider = asObjectRecord(providerRoot[targetId]) ?? {}
        if (!asObjectRecord(providerRoot[targetId])) {
          providerRoot[targetId] = currentProvider
        }
        const trimmed = value.trim()
        if (key === "baseURL") {
          const options = asObjectRecord(currentProvider.options) ?? {}
          if (!asObjectRecord(currentProvider.options)) {
            currentProvider.options = options
          }
          if (trimmed) {
            options[key] = trimmed
          } else {
            delete options[key]
          }
          if (Object.keys(options).length === 0) {
            delete currentProvider.options
          }
          return
        }
        if (trimmed) {
          currentProvider[key] = trimmed
        } else {
          delete currentProvider[key]
        }
      })
    },
    [handleOpenCodeConfigPatch, selectedDraft, updateSelectedDraft]
  )

  const handleOpenCodeModelDraftChange = useCallback(
    (providerId: string, value: string) => {
      const targetId = providerId.trim()
      if (!targetId) return
      setOpenCodeNewModelIds((prev) => ({
        ...prev,
        [targetId]: value,
      }))
    },
    []
  )

  const handleOpenCodeAddModel = useCallback(
    (providerId: string) => {
      const targetProviderId = providerId.trim()
      if (!targetProviderId || !selectedOpenCodeConfig) return
      const nextModelId = (openCodeNewModelIds[targetProviderId] ?? "").trim()
      if (!nextModelId) return
      const targetProvider = selectedOpenCodeConfig.providers[targetProviderId]
      if (!targetProvider) return
      if (targetProvider.modelIds.includes(nextModelId)) {
        toast.error(t("errors.modelExists", { modelId: nextModelId }))
        return
      }
      handleOpenCodeConfigPatch((config) => {
        const providerRoot = asObjectRecord(config.provider) ?? {}
        if (!asObjectRecord(config.provider)) {
          config.provider = providerRoot
        }

        const currentProvider =
          asObjectRecord(providerRoot[targetProviderId]) ?? {}
        if (!asObjectRecord(providerRoot[targetProviderId])) {
          providerRoot[targetProviderId] = currentProvider
        }

        const modelsRoot = asObjectRecord(currentProvider.models) ?? {}
        if (!asObjectRecord(currentProvider.models)) {
          currentProvider.models = modelsRoot
        }
        modelsRoot[nextModelId] = {
          name: nextModelId,
        }
      })
      setOpenCodeNewModelIds((prev) => ({
        ...prev,
        [targetProviderId]: "",
      }))
    },
    [handleOpenCodeConfigPatch, openCodeNewModelIds, selectedOpenCodeConfig, t]
  )

  const handleOpenCodeRemoveModel = useCallback(
    (providerId: string, modelId: string) => {
      const targetProviderId = providerId.trim()
      const targetModelId = modelId.trim()
      if (!targetProviderId || !targetModelId) return
      handleOpenCodeConfigPatch((config) => {
        const providerRoot = asObjectRecord(config.provider)
        if (!providerRoot) return
        const currentProvider = asObjectRecord(providerRoot[targetProviderId])
        if (!currentProvider) return
        const modelsRoot = asObjectRecord(currentProvider.models)
        if (!modelsRoot) return
        delete modelsRoot[targetModelId]
        if (Object.keys(modelsRoot).length === 0) {
          delete currentProvider.models
        }
      })
      const draftKey = `${targetProviderId}:${targetModelId}`
      setOpenCodeModelIdDrafts((prev) => {
        if (typeof prev[draftKey] === "undefined") return prev
        const next = { ...prev }
        delete next[draftKey]
        return next
      })
    },
    [handleOpenCodeConfigPatch]
  )

  const handleOpenCodeModelIdDraftChange = useCallback(
    (providerId: string, modelId: string, value: string) => {
      const targetProviderId = providerId.trim()
      const targetModelId = modelId.trim()
      if (!targetProviderId || !targetModelId) return
      const draftKey = `${targetProviderId}:${targetModelId}`
      setOpenCodeModelIdDrafts((prev) => ({
        ...prev,
        [draftKey]: value,
      }))
    },
    []
  )

  const handleOpenCodeModelIdCommit = useCallback(
    (providerId: string, modelId: string) => {
      const targetProviderId = providerId.trim()
      const targetModelId = modelId.trim()
      if (!targetProviderId || !targetModelId || !selectedOpenCodeConfig) return
      const draftKey = `${targetProviderId}:${targetModelId}`
      const rawDraft = openCodeModelIdDrafts[draftKey]
      if (typeof rawDraft !== "string") return
      const nextModelId = rawDraft.trim()

      if (!nextModelId || nextModelId === targetModelId) {
        setOpenCodeModelIdDrafts((prev) => {
          const next = { ...prev }
          delete next[draftKey]
          return next
        })
        return
      }

      if (!/^[A-Za-z0-9_.:-]+$/.test(nextModelId)) {
        toast.error(t("errors.modelIdPattern"))
        return
      }

      const targetProvider = selectedOpenCodeConfig.providers[targetProviderId]
      if (!targetProvider) return
      if (targetProvider.modelIds.includes(nextModelId)) {
        toast.error(t("errors.modelExists", { modelId: nextModelId }))
        return
      }

      handleOpenCodeConfigPatch((config) => {
        const providerRoot = asObjectRecord(config.provider) ?? {}
        if (!asObjectRecord(config.provider)) {
          config.provider = providerRoot
        }
        const currentProvider =
          asObjectRecord(providerRoot[targetProviderId]) ?? {}
        if (!asObjectRecord(providerRoot[targetProviderId])) {
          providerRoot[targetProviderId] = currentProvider
        }
        const modelsRoot = asObjectRecord(currentProvider.models) ?? {}
        if (!asObjectRecord(currentProvider.models)) {
          currentProvider.models = modelsRoot
        }
        const currentModel = asObjectRecord(modelsRoot[targetModelId]) ?? {}
        if (!asObjectRecord(modelsRoot[targetModelId])) return
        delete currentModel.id
        modelsRoot[nextModelId] = currentModel
        delete modelsRoot[targetModelId]
      })

      setOpenCodeModelIdDrafts((prev) => {
        const next = { ...prev }
        delete next[draftKey]
        return next
      })
    },
    [
      handleOpenCodeConfigPatch,
      openCodeModelIdDrafts,
      selectedOpenCodeConfig,
      t,
    ]
  )

  const handleOpenCodeModelFieldChange = useCallback(
    (providerId: string, modelId: string, value: string) => {
      const targetProviderId = providerId.trim()
      const targetModelId = modelId.trim()
      if (!targetProviderId || !targetModelId) return
      handleOpenCodeConfigPatch((config) => {
        const providerRoot = asObjectRecord(config.provider) ?? {}
        if (!asObjectRecord(config.provider)) {
          config.provider = providerRoot
        }
        const currentProvider =
          asObjectRecord(providerRoot[targetProviderId]) ?? {}
        if (!asObjectRecord(providerRoot[targetProviderId])) {
          providerRoot[targetProviderId] = currentProvider
        }
        const modelsRoot = asObjectRecord(currentProvider.models) ?? {}
        if (!asObjectRecord(currentProvider.models)) {
          currentProvider.models = modelsRoot
        }
        const currentModel = asObjectRecord(modelsRoot[targetModelId]) ?? {}
        if (!asObjectRecord(modelsRoot[targetModelId])) {
          modelsRoot[targetModelId] = currentModel
        }
        const trimmed = value.trim()
        if (trimmed) {
          currentModel.name = trimmed
        } else {
          delete currentModel.name
        }
        // Cleanup legacy schema written by earlier versions.
        delete currentModel.id
      })
    },
    [handleOpenCodeConfigPatch]
  )

  const handleCodexConfigTomlTextChange = useCallback(
    (nextText: string) => {
      if (!selectedAgent || selectedAgent.agent_type !== "codex") return
      const important = extractCodexImportantValues(
        selectedDraft?.codexAuthJsonText ?? "",
        nextText
      )
      updateSelectedDraft((current) => ({
        ...current,
        codexConfigTomlText: nextText,
        apiBaseUrl: important.apiBaseUrl,
        apiKey: important.apiKey ?? current.apiKey,
        model: important.model,
        codexModelProvider: important.modelProvider,
        codexProviderOptions: important.providerOptions,
        codexSupportsWebsockets: important.supportsWebsockets,
        codexSkills: important.skills,
        codexDefaultModeRequestUserInput: important.defaultModeRequestUserInput,
        codexServiceTierFast: important.serviceTierFast,
      }))
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleCodexAuthModeChange = useCallback(
    (nextMode: CodexAuthMode) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "codex"
      )
        return

      if (nextMode === "chatgpt_subscription") {
        // Official subscription: set auth_mode to chatgpt, OPENAI_API_KEY to null
        const nextAuth = patchCodexAuthJsonText(
          selectedDraft.codexAuthJsonText,
          { authMode: "chatgpt" }
        )
        const nextAuthJsonText = nextAuth.authJsonText
        let nextConfigTomlText = updateTomlRootStringKey(
          selectedDraft.codexConfigTomlText,
          "model_provider",
          ""
        )
        nextConfigTomlText = removeTomlSection(
          nextConfigTomlText,
          `model_providers.${CODEX_DEFAULT_MODEL_PROVIDER}`
        )
        const synced = extractCodexImportantValues(
          nextAuthJsonText,
          nextConfigTomlText
        )
        updateSelectedDraft((current) => ({
          ...current,
          codexAuthMode: nextMode,
          modelProviderId: null,
          codexAuthJsonText: nextAuthJsonText,
          codexConfigTomlText: nextConfigTomlText,
          envText: patchEnvText(current.envText, {
            OPENAI_API_KEY: "",
            OPENAI_BASE_URL: "",
          }),
          apiBaseUrl: "",
          apiKey: "",
          model: synced.model,
          codexModelProvider: synced.modelProvider,
          codexProviderOptions: synced.providerOptions,
          codexSupportsWebsockets: synced.supportsWebsockets,
          codexSkills: synced.skills,
          codexDefaultModeRequestUserInput: synced.defaultModeRequestUserInput,
          codexServiceTierFast: synced.serviceTierFast,
        }))
        return
      }

      // "api_key" or "model_provider": ensure model_provider = "codeg" in toml
      const nextConfigTomlText = patchCodexConfigTomlText(
        selectedDraft.codexConfigTomlText,
        { modelProvider: CODEX_DEFAULT_MODEL_PROVIDER }
      )
      const nextAuthPatch = patchCodexAuthJsonText(
        selectedDraft.codexAuthJsonText,
        { authMode: null }
      )
      const nextAuthJsonText = nextAuthPatch.authJsonText
      const synced = extractCodexImportantValues(
        nextAuthJsonText,
        nextConfigTomlText
      )
      updateSelectedDraft((current) => ({
        ...current,
        codexAuthMode: nextMode,
        modelProviderId:
          nextMode === "model_provider" ? current.modelProviderId : null,
        codexAuthJsonText: nextAuthJsonText,
        codexConfigTomlText: nextConfigTomlText,
        apiBaseUrl: synced.apiBaseUrl,
        apiKey: synced.apiKey ?? current.apiKey,
        model: synced.model,
        codexModelProvider: CODEX_DEFAULT_MODEL_PROVIDER,
        codexProviderOptions: synced.providerOptions,
        codexSupportsWebsockets: synced.supportsWebsockets,
        codexSkills: synced.skills,
        codexDefaultModeRequestUserInput: synced.defaultModeRequestUserInput,
        codexServiceTierFast: synced.serviceTierFast,
      }))
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleCodexModelListChange = useCallback(
    (next: CodexModelConfig) => {
      const defaultSlug = next.default ?? next.customs[0]?.slug ?? ""
      // `next` arrives already pruned of exclusions that no longer name a
      // listable official, so a plain count is the *effective* customization —
      // codeg only takes over codex's model table when something really deviates.
      const hasCatalog =
        next.customs.length > 0 || (next.excludedOfficials?.length ?? 0) > 0
      updateSelectedDraft((current) => {
        let toml = updateTomlRootStringKey(
          current.codexConfigTomlText,
          "model",
          defaultSlug
        )
        toml = updateTomlRootStringKey(
          toml,
          "model_catalog_json",
          hasCatalog ? "codeg-model-catalog.json" : ""
        )
        return {
          ...current,
          codexModelList: next,
          model: defaultSlug,
          codexConfigTomlText: toml,
        }
      })
    },
    [updateSelectedDraft]
  )

  const handleCodexImportantConfigChange = useCallback(
    (key: "apiBaseUrl" | "apiKey" | "model", value: string) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "codex"
      )
        return
      const nextAuth =
        key === "apiKey"
          ? patchCodexAuthJsonText(selectedDraft.codexAuthJsonText, {
              apiKey: value,
            })
          : {
              authJsonText: selectedDraft.codexAuthJsonText,
              recoveredFromInvalid: false,
            }
      const nextToml =
        key === "apiBaseUrl"
          ? patchCodexConfigTomlText(selectedDraft.codexConfigTomlText, {
              apiBaseUrl: value,
              modelProvider: selectedDraft.codexModelProvider,
            })
          : key === "model"
            ? patchCodexConfigTomlText(selectedDraft.codexConfigTomlText, {
                model: value,
              })
            : selectedDraft.codexConfigTomlText
      if (nextAuth.recoveredFromInvalid) {
        toast.warning(t("warnings.authRecoveredStructured"))
      }
      const synced = extractCodexImportantValues(
        nextAuth.authJsonText,
        nextToml
      )
      updateSelectedDraft((current) => ({
        ...applyImportantFieldToDraft(current, key, value),
        apiBaseUrl: synced.apiBaseUrl,
        apiKey: synced.apiKey ?? current.apiKey,
        model: synced.model,
        codexModelProvider: synced.modelProvider,
        codexProviderOptions: synced.providerOptions,
        codexSupportsWebsockets: synced.supportsWebsockets,
        codexSkills: synced.skills,
        codexDefaultModeRequestUserInput: synced.defaultModeRequestUserInput,
        codexServiceTierFast: synced.serviceTierFast,
        codexAuthJsonText: nextAuth.authJsonText,
        codexConfigTomlText: nextToml,
      }))
    },
    [selectedAgent, selectedDraft, t, updateSelectedDraft]
  )

  const handleCodexSupportsWebsocketsChange = useCallback(
    (enabled: boolean) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "codex"
      )
        return
      const nextToml = patchCodexConfigTomlText(
        selectedDraft.codexConfigTomlText,
        {
          modelProvider: selectedDraft.codexModelProvider,
          supportsWebsockets: enabled,
        }
      )
      const synced = extractCodexImportantValues(
        selectedDraft.codexAuthJsonText,
        nextToml
      )
      updateSelectedDraft((current) => ({
        ...current,
        apiBaseUrl: synced.apiBaseUrl,
        apiKey: synced.apiKey ?? current.apiKey,
        model: synced.model,
        codexModelProvider: synced.modelProvider,
        codexProviderOptions: synced.providerOptions,
        codexSupportsWebsockets: synced.supportsWebsockets,
        codexSkills: synced.skills,
        codexDefaultModeRequestUserInput: synced.defaultModeRequestUserInput,
        codexServiceTierFast: synced.serviceTierFast,
        codexConfigTomlText: nextToml,
      }))
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleCodexSkillsChange = useCallback(
    (enabled: boolean) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "codex"
      )
        return
      const nextToml = patchCodexConfigTomlText(
        selectedDraft.codexConfigTomlText,
        { skills: enabled }
      )
      const synced = extractCodexImportantValues(
        selectedDraft.codexAuthJsonText,
        nextToml
      )
      updateSelectedDraft((current) => ({
        ...current,
        apiBaseUrl: synced.apiBaseUrl,
        apiKey: synced.apiKey ?? current.apiKey,
        model: synced.model,
        codexModelProvider: synced.modelProvider,
        codexProviderOptions: synced.providerOptions,
        codexSupportsWebsockets: synced.supportsWebsockets,
        codexSkills: synced.skills,
        codexDefaultModeRequestUserInput: synced.defaultModeRequestUserInput,
        codexServiceTierFast: synced.serviceTierFast,
        codexConfigTomlText: nextToml,
      }))
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleCodexDefaultModeRequestUserInputChange = useCallback(
    (enabled: boolean) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "codex"
      )
        return
      const nextToml = patchCodexConfigTomlText(
        selectedDraft.codexConfigTomlText,
        { defaultModeRequestUserInput: enabled }
      )
      const synced = extractCodexImportantValues(
        selectedDraft.codexAuthJsonText,
        nextToml
      )
      updateSelectedDraft((current) => ({
        ...current,
        apiBaseUrl: synced.apiBaseUrl,
        apiKey: synced.apiKey ?? current.apiKey,
        model: synced.model,
        codexModelProvider: synced.modelProvider,
        codexProviderOptions: synced.providerOptions,
        codexSupportsWebsockets: synced.supportsWebsockets,
        codexSkills: synced.skills,
        codexDefaultModeRequestUserInput: synced.defaultModeRequestUserInput,
        codexServiceTierFast: synced.serviceTierFast,
        codexConfigTomlText: nextToml,
      }))
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleCodexServiceTierFastChange = useCallback(
    (enabled: boolean) => {
      if (
        !selectedAgent ||
        !selectedDraft ||
        selectedAgent.agent_type !== "codex"
      )
        return
      const nextToml = patchCodexConfigTomlText(
        selectedDraft.codexConfigTomlText,
        { serviceTierFast: enabled }
      )
      const synced = extractCodexImportantValues(
        selectedDraft.codexAuthJsonText,
        nextToml
      )
      updateSelectedDraft((current) => ({
        ...current,
        apiBaseUrl: synced.apiBaseUrl,
        apiKey: synced.apiKey ?? current.apiKey,
        model: synced.model,
        codexModelProvider: synced.modelProvider,
        codexProviderOptions: synced.providerOptions,
        codexSupportsWebsockets: synced.supportsWebsockets,
        codexSkills: synced.skills,
        codexDefaultModeRequestUserInput: synced.defaultModeRequestUserInput,
        codexServiceTierFast: synced.serviceTierFast,
        codexConfigTomlText: nextToml,
      }))
    },
    [selectedAgent, selectedDraft, updateSelectedDraft]
  )

  const handleCodexDeviceLogin = useCallback(async () => {
    setCodexLoginStatus("requesting")
    setCodexLoginError(null)
    setCodexDeviceCode(null)
    codexPollCancelledRef.current = false
    try {
      const resp = await codexRequestDeviceCode()
      setCodexDeviceCode(resp)
      setCodexLoginStatus("polling")
    } catch (err) {
      const msg = toErrorMessage(err)
      setCodexLoginError(msg)
      setCodexLoginStatus("error")
    }
  }, [])

  const cancelCodexDeviceLogin = useCallback(() => {
    codexPollCancelledRef.current = true
    setCodexLoginStatus("idle")
    setCodexDeviceCode(null)
    setCodexLoginError(null)
  }, [])

  useEffect(() => {
    if (codexLoginStatus !== "polling" || !codexDeviceCode) return
    codexPollCancelledRef.current = false
    const pollInterval = (codexDeviceCode.interval || 5) * 1000
    const deadline = Date.now() + 15 * 60 * 1000
    let timer: ReturnType<typeof setTimeout> | null = null
    let active = true

    const poll = async () => {
      if (!active || codexPollCancelledRef.current) return
      if (Date.now() > deadline) {
        setCodexLoginError(t("codex.loginTimeout"))
        setCodexLoginStatus("error")
        setCodexDeviceCode(null)
        return
      }
      try {
        const result = await codexPollDeviceCode({
          deviceAuthId: codexDeviceCode.deviceAuthId,
          userCode: codexDeviceCode.userCode,
        })
        if (!active || codexPollCancelledRef.current) return
        if (result.status === "success") {
          setCodexLoginStatus("success")
          setCodexDeviceCode(null)
          const authJson = JSON.stringify(
            {
              auth_mode: "chatgpt",
              OPENAI_API_KEY: null,
              tokens: {
                id_token: result.idToken,
                access_token: result.accessToken,
                refresh_token: result.refreshToken,
                account_id: result.accountId ?? "",
              },
              last_refresh: new Date().toISOString(),
            },
            null,
            2
          )
          updateSelectedDraft((current) => ({
            ...current,
            codexAuthJsonText: authJson,
          }))
          const draft = drafts.codex
          if (draft) {
            const codexEnvText =
              draft.codexAuthMode === "chatgpt_subscription"
                ? patchEnvText(draft.envText, {
                    OPENAI_API_KEY: "",
                    OPENAI_BASE_URL: "",
                  })
                : draft.envText
            try {
              // Persist sequentially, never in parallel: persistEnv
              // (acp_update_agent_env) rewrites ~/.codex/config.toml to sync the
              // root `model`, while persistConfig writes the full config.toml
              // (including base_url). Running both at once races two
              // read-modify-write cycles on the same file, letting the model
              // sync clobber the just-written base_url. persistConfig runs last
              // so its authoritative config.toml wins.
              await persistEnv(
                "codex",
                draft.enabled,
                codexEnvText,
                draft.modelProviderId
              )
              await persistConfig("codex", draft.configText, {
                codexAuthJsonText: authJson,
                codexConfigTomlText: draft.codexConfigTomlText,
                codexModelCatalog:
                  serializeCodexModelConfig(draft.codexModelList) ?? "",
                codexSandbox: codexSandboxSaveConfig(draft),
              })
            } catch (err) {
              const msg = toErrorMessage(err)
              toast.error(t("codex.loginSaveFailed"), {
                description: msg,
              })
            }
          }
          return
        }
        if (result.status === "error") {
          setCodexLoginError(result.message ?? "Unknown error")
          setCodexLoginStatus("error")
          setCodexDeviceCode(null)
          return
        }
        timer = setTimeout(poll, pollInterval)
      } catch {
        if (!active || codexPollCancelledRef.current) return
        timer = setTimeout(poll, pollInterval)
      }
    }

    timer = setTimeout(poll, pollInterval)
    return () => {
      active = false
      if (timer) clearTimeout(timer)
    }
  }, [
    codexLoginStatus,
    codexDeviceCode,
    drafts.codex,
    persistConfig,
    persistEnv,
    updateSelectedDraft,
    t,
  ])

  useEffect(() => {
    if (selectedAgent?.agent_type !== "codex" && codexLoginStatus !== "idle") {
      cancelCodexDeviceLogin()
    }
  }, [selectedAgent, codexLoginStatus, cancelCodexDeviceLogin])

  if (loadingAgents) {
    return (
      <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
        <Loader2 className="h-4 w-4 mr-2 animate-spin" />
        {t("loadingAgents")}
      </div>
    )
  }

  return (
    <div className="h-full flex flex-col p-3 md:p-4">
      <div className="flex items-center justify-between gap-3 pb-4">
        <div>
          <h2 className="text-base font-semibold">{t("title")}</h2>
          <p className="text-xs text-muted-foreground mt-1">
            {t("description")}
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          className="h-8 text-xs shrink-0"
          onClick={() => setAddCustomOpen(true)}
        >
          <Plus className="h-3.5 w-3.5 mr-1" />
          {t("addCustomAgent")}
        </Button>
      </div>

      <AddCustomAgentDialog
        open={addCustomOpen}
        onOpenChange={setAddCustomOpen}
        onAdded={() => void refreshAgents()}
      />

      {/* Keyed by the id so switching agents never leaks a previous form. */}
      {editCustomAgentId !== null && (
        <AddCustomAgentDialog
          key={editCustomAgentId}
          open
          editRegistryId={editCustomAgentId}
          onOpenChange={(next) => {
            if (!next) setEditCustomAgentId(null)
          }}
          onAdded={() => void refreshAgents()}
        />
      )}

      {loadingError && (
        <div className="mb-3 rounded-md border border-red-500/30 bg-red-500/5 px-3 py-2 text-xs text-red-400">
          {loadingError}
        </div>
      )}

      <div className="flex-1 min-h-0 grid gap-3 lg:grid-cols-[minmax(15rem,20rem)_1fr]">
        <div className="min-h-0 min-w-0 rounded-lg border bg-card flex flex-col overflow-hidden">
          <div className="flex items-center justify-between gap-2 border-b px-3 py-2">
            <span className="text-xs font-medium text-muted-foreground">
              {t("agentList")}
            </span>
            <button
              type="button"
              className={cn(
                "rounded border px-1.5 py-0.5 text-2xs transition-colors",
                onlyInstalled
                  ? "border-primary/40 bg-primary/10 text-primary"
                  : "border-transparent text-muted-foreground hover:bg-muted"
              )}
              aria-pressed={onlyInstalled}
              onClick={() => {
                const next = !onlyInstalled
                setOnlyInstalled(next)
                writeAgentListOnlyInstalled(next)
              }}
            >
              {t("list.onlyInstalled")}
            </button>
          </div>
          <div
            ref={agentListRef}
            className="flex-1 min-h-0 space-y-3 overflow-y-auto p-2"
          >
            <div>
              <div className="px-1 pb-1 text-2xs font-medium text-muted-foreground">
                {t("list.installedGroup", { count: installedAgents.length })}
              </div>
              {installedAgents.length === 0 ? (
                <div className="px-1 text-2xs text-muted-foreground">
                  {t("list.installedEmpty")}
                </div>
              ) : (
                <Reorder.Group
                  as="div"
                  axis="y"
                  values={installedAgents}
                  onReorder={handleReorder}
                  className="space-y-2"
                >
                  {installedAgents.map((agent) => {
                    const current = checkState[agent.agent_type]
                    const isChecking = Boolean(checking[agent.agent_type])
                    const draft =
                      drafts[agent.agent_type] ?? buildAgentDraft(agent)
                    const allChecks = getAgentChecks(agent, current)
                    const displaySummary:
                      | CheckStatus
                      | "unchecked"
                      | "checking" = isChecking
                      ? "checking"
                      : summarizeChecks(allChecks)
                    const hasUpdate = Boolean(
                      agent.installed_version &&
                      agent.registry_version &&
                      hasComparableVersion(agent.registry_version) &&
                      hasComparableVersion(agent.installed_version) &&
                      compareVersion(
                        agent.installed_version,
                        agent.registry_version
                      ) < 0
                    )
                    const checkHint =
                      displaySummary === "fail" || displaySummary === "warn" ? (
                        <span
                          className={cn(
                            "inline-flex shrink-0",
                            displaySummary === "fail"
                              ? "text-red-500"
                              : "text-yellow-500"
                          )}
                          title={
                            displaySummary === "fail"
                              ? t("list.checkFailed")
                              : t("list.checkWarning")
                          }
                          aria-label={
                            displaySummary === "fail"
                              ? t("list.checkFailed")
                              : t("list.checkWarning")
                          }
                        >
                          <AlertCircle className="h-3.5 w-3.5" />
                        </span>
                      ) : null
                    return (
                      <AgentReorderItem
                        key={agent.agent_type}
                        agent={agent}
                        selected={selectedAgentType === agent.agent_type}
                        reordering={reordering}
                        dragging={dragging}
                        onDragStart={(agentType) => {
                          setDragging(agentType)
                        }}
                        onDragEnd={() => {
                          const order = pendingOrderRef.current
                          pendingOrderRef.current = null
                          setDragging(null)
                          if (order && !reordering) {
                            persistReorder(order).catch((err) => {
                              console.error(
                                "[Settings] reorder agents failed:",
                                err
                              )
                            })
                          }
                        }}
                        onSelect={(agentType) => {
                          setSelectedAgentType(agentType)
                        }}
                      >
                        {(startDrag) => (
                          <div className="flex items-center justify-between gap-2 overflow-hidden">
                            <div className="min-w-0 flex items-center gap-2">
                              <button
                                type="button"
                                className="text-muted-foreground cursor-grab active:cursor-grabbing rounded p-0.5 hover:bg-muted"
                                title={t("actions.dragSort")}
                                aria-label={t("actions.dragSortAgent", {
                                  name: agent.name,
                                })}
                                onPointerDown={startDrag}
                                onClick={(event) => {
                                  event.stopPropagation()
                                }}
                                disabled={reordering}
                              >
                                <GripVertical className="h-3.5 w-3.5" />
                              </button>
                              <AgentIcon
                                agentType={agent.agent_type}
                                className="h-4 w-4"
                              />
                              <span className="text-sm font-medium truncate">
                                {agent.name}
                              </span>
                              {draft.enabled && (
                                <span
                                  className="h-2 w-2 rounded-full bg-emerald-500 shrink-0"
                                  aria-label={t("status.agentEnabledAria", {
                                    name: agent.name,
                                  })}
                                  title={t("status.enabled")}
                                />
                              )}
                            </div>

                            <div className="flex items-center gap-1.5 shrink-0">
                              {checkHint}
                              <button
                                type="button"
                                className="inline-flex h-4 w-4 items-center justify-center rounded hover:bg-black/10 dark:hover:bg-white/10"
                                title={t("actions.refreshCheck")}
                                aria-label={t("actions.refreshCheckAgent", {
                                  name: agent.name,
                                })}
                                disabled={isChecking}
                                onClick={(event) => {
                                  event.stopPropagation()
                                  runPreflight(agent.agent_type, true).catch(
                                    (err) => {
                                      console.error(
                                        "[Settings] single preflight failed:",
                                        err
                                      )
                                    }
                                  )
                                }}
                              >
                                {isChecking ? (
                                  <Loader2 className="h-3 w-3 shrink-0 animate-spin" />
                                ) : (
                                  <RefreshCw className="h-3 w-3 shrink-0" />
                                )}
                              </button>
                              <Badge
                                variant="outline"
                                className={cn(
                                  "h-6 px-2 inline-flex items-center text-xs leading-none",
                                  hasUpdate
                                    ? "border-yellow-500/40 bg-yellow-500/10 text-yellow-600 dark:text-yellow-400"
                                    : "border-emerald-500/40 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
                                )}
                              >
                                {hasUpdate
                                  ? t("list.badgeUpdatable")
                                  : t("list.badgeInstalled")}
                              </Badge>
                            </div>
                          </div>
                        )}
                      </AgentReorderItem>
                    )
                  })}
                </Reorder.Group>
              )}
            </div>

            {!onlyInstalled && uninstalledAgents.length > 0 && (
              <div>
                <button
                  type="button"
                  className="flex w-full items-center gap-1 px-1 pb-1 text-2xs font-medium text-muted-foreground"
                  aria-expanded={uninstalledOpen}
                  onClick={() => setUninstalledOpen((open) => !open)}
                >
                  <ChevronDown
                    className={cn(
                      "h-3 w-3 shrink-0 transition-transform",
                      !uninstalledOpen && "-rotate-90"
                    )}
                  />
                  {t("list.uninstalledGroup", {
                    count: uninstalledAgents.length,
                  })}
                </button>
                {uninstalledOpen && (
                  <Reorder.Group
                    as="div"
                    axis="y"
                    values={uninstalledAgents}
                    onReorder={() => {}}
                    className="space-y-2"
                  >
                    {uninstalledAgents.map((agent) => {
                      const current = checkState[agent.agent_type]
                      const isChecking = Boolean(checking[agent.agent_type])
                      const allChecks = getAgentChecks(agent, current)
                      const displaySummary:
                        | CheckStatus
                        | "unchecked"
                        | "checking" = isChecking
                        ? "checking"
                        : summarizeChecks(allChecks)
                      const quickInstallable =
                        agent.distribution_type === "binary"
                          ? agent.available
                          : agent.distribution_type === "npx"
                      const busy = Boolean(busyBinaryAction[agent.agent_type])
                      const checkHint =
                        displaySummary === "fail" ||
                        displaySummary === "warn" ? (
                          <span
                            className={cn(
                              "inline-flex shrink-0",
                              displaySummary === "fail"
                                ? "text-red-500"
                                : "text-yellow-500"
                            )}
                            title={
                              displaySummary === "fail"
                                ? t("list.checkFailed")
                                : t("list.checkWarning")
                            }
                            aria-label={
                              displaySummary === "fail"
                                ? t("list.checkFailed")
                                : t("list.checkWarning")
                            }
                          >
                            <AlertCircle className="h-3.5 w-3.5" />
                          </span>
                        ) : null
                      return (
                        <AgentReorderItem
                          key={agent.agent_type}
                          agent={agent}
                          selected={selectedAgentType === agent.agent_type}
                          reordering
                          dragging={dragging}
                          onDragStart={() => {}}
                          onDragEnd={() => {}}
                          onSelect={(agentType) => {
                            setSelectedAgentType(agentType)
                          }}
                        >
                          {() => (
                            <div className="flex items-center justify-between gap-2 overflow-hidden">
                              <div className="min-w-0 flex items-center gap-2">
                                <AgentIcon
                                  agentType={agent.agent_type}
                                  className="h-4 w-4 opacity-70"
                                />
                                <span className="text-sm truncate text-muted-foreground">
                                  {agent.name}
                                </span>
                              </div>
                              <div className="flex items-center gap-1.5 shrink-0">
                                {checkHint}
                                {quickInstallable ? (
                                  <Button
                                    type="button"
                                    variant="outline"
                                    size="sm"
                                    className="h-6 px-2 text-2xs shrink-0"
                                    disabled={busy}
                                    onClick={(event) => {
                                      event.stopPropagation()
                                      if (
                                        agent.distribution_type === "binary"
                                      ) {
                                        void runBinaryAction(agent, "download")
                                      } else {
                                        void runNpxAction(agent, "install")
                                      }
                                    }}
                                  >
                                    {busy ? (
                                      <Loader2 className="h-3 w-3 shrink-0 animate-spin" />
                                    ) : (
                                      <Download className="h-3 w-3 shrink-0" />
                                    )}
                                    {t("actions.install")}
                                  </Button>
                                ) : (
                                  <Badge
                                    variant="outline"
                                    className="h-6 px-2 text-xs leading-none text-muted-foreground"
                                  >
                                    {t("list.badgeNotInstalled")}
                                  </Badge>
                                )}
                              </div>
                            </div>
                          )}
                        </AgentReorderItem>
                      )
                    })}
                  </Reorder.Group>
                )}
              </div>
            )}
          </div>
        </div>

        <div className="min-h-0 min-w-0 rounded-lg border bg-card">
          {selectedAgent && selectedDraft ? (
            <div className="h-full flex flex-col">
              <div className="border-b px-4 py-3">
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0 flex items-center gap-2">
                    <AgentIcon
                      agentType={selectedAgent.agent_type}
                      className="h-5 w-5"
                    />
                    <h3 className="text-sm font-semibold truncate">
                      {selectedAgent.name}
                    </h3>
                    <Badge variant="outline" className="shrink-0">
                      {selectedAgent.distribution_type}
                    </Badge>
                    {/* Names the thing codeg actually installs, right next to
                        the vendor's name — so the split is visible even before
                        anyone reads the preflight card below. */}
                    {selectedAgent.is_acp_adapter && (
                      <Badge
                        variant="secondary"
                        className="shrink-0"
                        title={t("adapter.badgeHint")}
                      >
                        {t("adapter.badge")}
                      </Badge>
                    )}
                    {isCustomAgentType(selectedAgent.agent_type) && (
                      <Badge variant="secondary" className="shrink-0">
                        {t("customAgentBadge")}
                      </Badge>
                    )}
                  </div>
                  {/* Removing a custom agent lives in the danger row at the
                      bottom of the panel, not here: this line already carries
                      the name, the distribution badge, the Custom badge and
                      the enable switch. */}
                  <div className="flex items-center gap-2 shrink-0">
                    <button
                      type="button"
                      role="switch"
                      aria-checked={selectedDraft.enabled}
                      aria-label={t("status.agentEnabledSwitch", {
                        name: selectedAgent.name,
                      })}
                      title={
                        selectedDraft.enabled
                          ? t("actions.clickDisable", {
                              name: selectedAgent.name,
                            })
                          : t("actions.clickEnable", {
                              name: selectedAgent.name,
                            })
                      }
                      disabled={selectedIsSaving || selectedGrokSaving}
                      className={cn(
                        "relative inline-flex h-5 w-9 items-center rounded-full transition-colors",
                        selectedDraft.enabled
                          ? "bg-primary"
                          : "bg-muted-foreground/30",
                        selectedIsSaving && "cursor-not-allowed opacity-60"
                      )}
                      onClick={() => {
                        const nextEnabled = !selectedDraft.enabled
                        const nextDraft = {
                          ...selectedDraft,
                          enabled: nextEnabled,
                        }
                        setDrafts((prev) => ({
                          ...prev,
                          [selectedAgent.agent_type]: nextDraft,
                        }))
                        persistEnv(
                          selectedAgent.agent_type,
                          nextEnabled,
                          nextDraft.envText,
                          nextDraft.modelProviderId
                        ).catch((err) => {
                          console.error(
                            "[Settings] persist enabled failed:",
                            err
                          )
                          const message = toErrorMessage(err)
                          toast.error(t("toasts.saveAgentSwitchFailed"), {
                            description: message,
                          })
                        })
                      }}
                    >
                      <span
                        className={cn(
                          "inline-block h-4 w-4 rounded-full bg-background shadow-sm transition-transform",
                          selectedDraft.enabled
                            ? "translate-x-4"
                            : "translate-x-0.5"
                        )}
                      />
                    </button>
                  </div>
                </div>
                <p className="mt-2 text-xs text-muted-foreground">
                  {selectedAgent.description}
                </p>
              </div>

              <AgentDiagnosticsDialog
                open={diagnosticsOpen}
                onOpenChange={setDiagnosticsOpen}
                agentType={selectedAgent.agent_type}
              />

              {/* [crewchat] The readiness card — the page's default face —
                  plus the Advanced fold toggle. Layout lives in
                  agent-readiness-card.tsx, the CCR side in
                  use-agent-ccr-binding.ts. */}
              <AgentReadinessCard
                agent={selectedAgent}
                enabled={selectedDraft.enabled}
                install={{
                  installedVersion: selectedAgent.installed_version,
                  recommendedVersion: selectedAgent.registry_version ?? null,
                  hasUpdate: selectedHasUpdate,
                  quickInstallable: selectedQuickInstallable,
                  installing: selectedIsInstalling,
                  uvReady: selectedUvReady,
                }}
                ccr={{
                  kind: selectedCcrKind,
                  state: selectedCcrState,
                  profile: selectedCcrProfile,
                  busy: ccrBinding.busy,
                  onBind: () => {
                    if (selectedCcrKind) void ccrBinding.bind(selectedCcrKind)
                  },
                  onReapply: () => {
                    void ccrBinding.reapply()
                  },
                  onRetry: () => {
                    void ccrBinding.refresh()
                  },
                }}
                installLogs={
                  installStream.status !== "idle" &&
                  streamAgentType === selectedAgent.agent_type
                    ? installStream.logs
                    : null
                }
                installLogEndRef={installLogEndRef}
                onInstall={() => {
                  if (selectedAgent.distribution_type === "binary") {
                    void runBinaryAction(selectedAgent, "download")
                  } else {
                    void runNpxAction(selectedAgent, "install")
                  }
                }}
                onUpgrade={() => {
                  if (selectedAgent.distribution_type === "binary") {
                    void runBinaryAction(selectedAgent, "upgrade")
                  } else {
                    void runNpxAction(selectedAgent, "upgrade")
                  }
                }}
                advancedOpen={advancedOpen}
                onToggleAdvanced={() => setAdvancedOpen((open) => !open)}
              />

              <div
                className={cn(
                  "flex-1 overflow-y-auto p-4 space-y-4",
                  !advancedOpen && "hidden"
                )}
              >
                <p className="text-2xs text-muted-foreground">
                  {t("readiness.advancedNote")}
                </p>
                <div className="space-y-2">
                  {selectedCurrent?.error && (
                    <div className="rounded-md border border-red-500/30 bg-red-500/5 px-3 py-2 text-xs text-red-400 flex items-start gap-2">
                      <AlertCircle className="h-3.5 w-3.5 mt-0.5 shrink-0" />
                      <span className="break-all">{selectedCurrent.error}</span>
                    </div>
                  )}
                  <div className="flex items-center justify-between gap-2">
                    <div className="text-2xs text-muted-foreground flex items-center gap-1">
                      <CheckCircle2 className="h-3 w-3" />
                      {t("preflight.count", { count: selectedChecks.length })}
                    </div>
                    <Button
                      type="button"
                      variant="outline"
                      size="xs"
                      onClick={() => setDiagnosticsOpen(true)}
                    >
                      <Stethoscope className="h-3.5 w-3.5" />
                      {t("actions.diagnose")}
                    </Button>
                  </div>
                  {selectedChecks.length > 0 ? (
                    selectedChecks.map((check) =>
                      renderCheck(selectedAgent, check)
                    )
                  ) : (
                    <div className="text-xs text-muted-foreground">
                      {t("preflight.notRun")}
                    </div>
                  )}
                </div>

                <div className="space-y-2">
                  <label className="text-xs font-medium">{t("envVars")}</label>
                  <div className="relative group">
                    <Textarea
                      value={selectedDraft.envText}
                      onChange={(event) => {
                        updateSelectedDraft((current) => ({
                          ...current,
                          envText: event.target.value,
                        }))
                      }}
                      placeholder={"KEY1=VALUE1\nKEY2=VALUE2"}
                      className="min-h-24"
                      disabled={selectedGrokSaving}
                    />
                    <div className="pointer-events-none absolute inset-0 rounded-md bg-background/10 backdrop-blur-[3px] transition-opacity duration-200 group-focus-within:opacity-0" />
                  </div>
                  {/*
                    Backed by the same `envText` draft as the textarea above,
                    not self-persisting: saving on toggle would also commit
                    whatever unsaved edits the textarea happens to hold. One
                    Save button owns both.
                  */}
                  <div className="flex items-start justify-between gap-3 rounded-md border bg-muted/10 p-3">
                    <div className="space-y-1">
                      <label className="text-xs font-medium">
                        {t("hostTools.label")}
                      </label>
                      <p className="text-2xs text-muted-foreground">
                        {t("hostTools.description")}
                      </p>
                    </div>
                    <Switch
                      checked={hostToolsAgentModeEnabled(selectedDraft.envText)}
                      onCheckedChange={(checked) => {
                        updateSelectedDraft((current) => ({
                          ...current,
                          envText: setHostToolsAgentMode(
                            current.envText,
                            checked
                          ),
                        }))
                      }}
                      disabled={selectedGrokSaving}
                      aria-label={t("hostTools.label")}
                    />
                  </div>
                  <div className="flex justify-end">
                    <Button
                      size="sm"
                      onClick={() => {
                        persistEnv(
                          selectedAgent.agent_type,
                          selectedDraft.enabled,
                          selectedDraft.envText,
                          selectedDraft.modelProviderId
                        )
                          .then(() => {
                            toast.success(t("toasts.configSaved"), {
                              description: t("toasts.configSavedHint"),
                            })
                          })
                          .catch((err) => {
                            console.error("[Settings] save env failed:", err)
                            const message = toErrorMessage(err)
                            toast.error(t("toasts.saveEnvFailed"), {
                              description: message,
                            })
                          })
                      }}
                      disabled={selectedIsSavingEnv || selectedGrokSaving}
                    >
                      {selectedIsSavingEnv ? (
                        <>
                          <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          {t("actions.saving")}
                        </>
                      ) : (
                        <>
                          <Save className="h-3.5 w-3.5" />
                          {t("actions.saveEnvVars")}
                        </>
                      )}
                    </Button>
                  </div>
                </div>

                {selectedAgent.agent_type === "codex" ? (
                  <div className="space-y-3 rounded-md border bg-muted/10 p-3">
                    <div>
                      <label className="text-xs font-medium">
                        {t("configManagement")}
                      </label>
                      <p className="mt-1 text-2xs text-muted-foreground">
                        {t("codex.configDescription")}
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        {t("codex.authMode")}
                      </label>
                      <Select
                        value={selectedDraft.codexAuthMode}
                        onValueChange={(value) => {
                          if (
                            CODEX_AUTH_MODES.includes(value as CodexAuthMode)
                          ) {
                            handleCodexAuthModeChange(value as CodexAuthMode)
                          }
                        }}
                      >
                        <SelectTrigger className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent align="start">
                          {/* [crewchat] "model_provider" stays hidden: the model
                              layer is CCR and per-role models live on the Agents
                              page. The upstream branch below is left dormant —
                              see NOTICE.md for the deferred full removal. */}
                          {CODEX_AUTH_MODES.filter(
                            (mode) => mode !== "model_provider"
                          ).map((mode) => (
                            <SelectItem key={mode} value={mode}>
                              {mode === "chatgpt_subscription"
                                ? t("authModeOfficialSubscription")
                                : t("authModeCustomEndpoint")}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <p className="text-2xs text-muted-foreground">
                        {selectedDraft.codexAuthMode === "chatgpt_subscription"
                          ? t("codex.chatgptSubscriptionHint")
                          : selectedDraft.codexAuthMode === "model_provider"
                            ? t("modelProviderHint")
                            : t("authModeCustomEndpointHint")}
                      </p>
                    </div>

                    {selectedDraft.codexAuthMode === "chatgpt_subscription" && (
                      <div className="space-y-2">
                        {hasCodexChatgptTokens(
                          selectedDraft.codexAuthJsonText
                        ) &&
                          codexLoginStatus !== "polling" &&
                          codexLoginStatus !== "requesting" && (
                            <div className="flex items-center gap-1.5 text-xs text-green-600">
                              <CheckCircle2 className="h-3 w-3" />
                              {t("codex.loggedIn")}
                            </div>
                          )}
                        {codexLoginStatus === "idle" && (
                          <Button
                            onClick={handleCodexDeviceLogin}
                            size="sm"
                            variant="outline"
                          >
                            {hasCodexChatgptTokens(
                              selectedDraft.codexAuthJsonText
                            )
                              ? t("codex.loginRelogin")
                              : t("codex.loginButton")}
                          </Button>
                        )}
                        {codexLoginStatus === "requesting" && (
                          <div className="flex items-center gap-2 text-xs text-muted-foreground">
                            <Loader2 className="h-3 w-3 animate-spin" />
                            {t("codex.loginRequesting")}
                          </div>
                        )}
                        {codexLoginStatus === "polling" && codexDeviceCode && (
                          <div className="space-y-2 rounded-md border p-3">
                            <p className="text-xs">{t("codex.loginStep1")}</p>
                            <button
                              type="button"
                              className="text-xs text-primary underline cursor-pointer"
                              onClick={() =>
                                openUrl(codexDeviceCode.verificationUrl)
                              }
                            >
                              {codexDeviceCode.verificationUrl}
                            </button>
                            <p className="text-xs mt-1">
                              {t("codex.loginStep2")}
                            </p>
                            <div className="flex items-center gap-2">
                              <code className="rounded bg-muted px-2 py-1 text-sm font-mono font-bold tracking-widest">
                                {codexDeviceCode.userCode}
                              </code>
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-7 w-7 p-0"
                                onClick={async () => {
                                  const ok = await copyTextToClipboard(
                                    codexDeviceCode.userCode
                                  )
                                  if (ok) {
                                    toast.success(t("codex.loginCodeCopied"))
                                  }
                                }}
                              >
                                <Copy className="h-3 w-3" />
                              </Button>
                            </div>
                            <div className="flex items-center gap-2 text-xs text-muted-foreground mt-1">
                              <Loader2 className="h-3 w-3 animate-spin" />
                              {t("codex.loginPolling")}
                            </div>
                            <Button
                              size="sm"
                              variant="outline"
                              onClick={cancelCodexDeviceLogin}
                            >
                              {t("codex.loginCancel")}
                            </Button>
                          </div>
                        )}
                        {codexLoginStatus === "success" && (
                          <div className="flex items-center gap-1.5 text-xs text-green-600">
                            <CheckCircle2 className="h-3 w-3" />
                            {t("codex.loginSuccess")}
                          </div>
                        )}
                        {codexLoginStatus === "error" && (
                          <div className="space-y-1.5">
                            <p className="text-xs text-destructive">
                              {t("codex.loginFailed", {
                                message: codexLoginError ?? "Unknown error",
                              })}
                            </p>
                            <Button
                              onClick={handleCodexDeviceLogin}
                              size="sm"
                              variant="outline"
                            >
                              {t("codex.loginRetry")}
                            </Button>
                          </div>
                        )}
                      </div>
                    )}

                    {selectedDraft.codexAuthMode === "model_provider" && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          {t("selectModelProvider")}
                        </label>
                        {selectedModelProviders.length > 0 ? (
                          <Select
                            value={
                              selectedDraft.modelProviderId != null
                                ? String(selectedDraft.modelProviderId)
                                : ""
                            }
                            onValueChange={handleModelProviderSelect}
                          >
                            <SelectTrigger className="w-full">
                              <SelectValue
                                placeholder={t("selectModelProvider")}
                              />
                            </SelectTrigger>
                            <SelectContent align="start">
                              {selectedModelProviders.map((provider) => (
                                <SelectItem
                                  key={provider.id}
                                  value={String(provider.id)}
                                >
                                  {provider.name}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        ) : (
                          <p className="text-2xs text-muted-foreground">
                            {t("noModelProviderAvailable")}
                          </p>
                        )}
                      </div>
                    )}

                    {(selectedDraft.codexAuthMode === "api_key" ||
                      selectedDraft.codexAuthMode === "model_provider") && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          API URL
                        </label>
                        <Input
                          value={selectedDraft.apiBaseUrl}
                          readOnly={
                            selectedDraft.codexAuthMode === "model_provider"
                          }
                          onChange={(event) => {
                            handleCodexImportantConfigChange(
                              "apiBaseUrl",
                              event.target.value
                            )
                          }}
                          placeholder="https://api.openai.com/v1"
                        />
                      </div>
                    )}

                    {(selectedDraft.codexAuthMode === "api_key" ||
                      selectedDraft.codexAuthMode === "model_provider") && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          API Key
                        </label>
                        <div className="flex items-center gap-2">
                          <Input
                            type={
                              showApiKeys[selectedAgent.agent_type]
                                ? "text"
                                : "password"
                            }
                            value={selectedDraft.apiKey}
                            readOnly={
                              selectedDraft.codexAuthMode === "model_provider"
                            }
                            onChange={(event) => {
                              handleCodexImportantConfigChange(
                                "apiKey",
                                event.target.value
                              )
                            }}
                            placeholder="sk-..."
                          />
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              setShowApiKeys((prev) => ({
                                ...prev,
                                [selectedAgent.agent_type]:
                                  !prev[selectedAgent.agent_type],
                              }))
                            }}
                            title={
                              showApiKeys[selectedAgent.agent_type]
                                ? t("actions.hideApiKey")
                                : t("actions.showApiKey")
                            }
                          >
                            {showApiKeys[selectedAgent.agent_type] ? (
                              <EyeOff className="h-3.5 w-3.5" />
                            ) : (
                              <Eye className="h-3.5 w-3.5" />
                            )}
                          </Button>
                        </div>
                      </div>
                    )}

                    {(selectedDraft.codexAuthMode === "api_key" ||
                      selectedDraft.codexAuthMode === "model_provider") && (
                      <div className="space-y-1.5">
                        <CodexModelListEditor
                          value={selectedDraft.codexModelList}
                          onChange={handleCodexModelListChange}
                          readOnly={
                            selectedDraft.codexAuthMode === "model_provider"
                          }
                        />
                      </div>
                    )}

                    <div className="space-y-1.5">
                      <div className="flex items-center justify-between rounded-md border px-3 py-2">
                        <label className="text-2xs text-muted-foreground">
                          {t("codex.enableWebsocket")}
                        </label>
                        <Switch
                          checked={selectedDraft.codexSupportsWebsockets}
                          onCheckedChange={handleCodexSupportsWebsocketsChange}
                          aria-label={t("codex.enableWebsocketAria")}
                        />
                      </div>
                    </div>

                    <div className="space-y-1.5">
                      <div className="flex items-center justify-between rounded-md border px-3 py-2">
                        <label className="text-2xs text-muted-foreground">
                          {t("codex.enableSkills")}
                        </label>
                        <Switch
                          checked={selectedDraft.codexSkills}
                          onCheckedChange={handleCodexSkillsChange}
                          aria-label={t("codex.enableSkillsAria")}
                        />
                      </div>
                    </div>

                    {/* `[features].default_mode_request_user_input` — without
                        it codex refuses its own `request_user_input` tool
                        outside Plan mode, so codeg's question cards never
                        appear in an ordinary turn (openai/codex#24750). */}
                    <div className="space-y-1.5">
                      <div className="flex items-center justify-between rounded-md border px-3 py-2">
                        <label className="text-2xs text-muted-foreground">
                          {t("codex.enableDefaultModeRequestUserInput")}
                        </label>
                        <Switch
                          checked={
                            selectedDraft.codexDefaultModeRequestUserInput
                          }
                          onCheckedChange={
                            handleCodexDefaultModeRequestUserInputChange
                          }
                          aria-label={t(
                            "codex.enableDefaultModeRequestUserInputAria"
                          )}
                        />
                      </div>
                      <p className="text-3xs text-muted-foreground">
                        {t("codex.enableDefaultModeRequestUserInputHint")}
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <div className="flex items-center justify-between rounded-md border px-3 py-2">
                        <label className="text-2xs text-muted-foreground">
                          {t("codex.enableFast")}
                        </label>
                        <Switch
                          checked={selectedDraft.codexServiceTierFast}
                          onCheckedChange={handleCodexServiceTierFastChange}
                          aria-label={t("codex.enableFastAria")}
                        />
                      </div>
                    </div>

                    {/* ---- Sandbox & approvals (config.toml thread defaults) ----
                        These govern the turns codex starts by itself: /goal,
                        /review, /compact. Ordinary prompts carry the composer
                        preset's own policy per turn and ignore these keys. */}
                    <div className="space-y-2 rounded-md border px-3 py-2.5">
                      <div className="space-y-1">
                        <p className="text-2xs font-medium">
                          {t("codex.sandboxGroupTitle")}
                        </p>
                        <p className="text-3xs text-muted-foreground">
                          {t("codex.sandboxGroupHint")}
                        </p>
                      </div>

                      {selectedDraft.codexSandboxShadowed ? (
                        <p className="text-3xs text-yellow-500">
                          {t("codex.sandboxShadowedWarning")}
                        </p>
                      ) : null}
                      {selectedDraft.codexSandboxHasPermissionsTable &&
                      !selectedDraft.codexSandboxShadowed ? (
                        <p className="text-3xs text-yellow-500">
                          {t("codex.sandboxPermissionsTableWarning")}
                        </p>
                      ) : null}

                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          {t("codex.approvalPolicyLabel")}
                        </label>
                        <Select
                          value={
                            selectedDraft.codexApprovalPolicy ||
                            CODEX_SANDBOX_UNSET_OPTION
                          }
                          onValueChange={(value) => {
                            updateSelectedDraft((current) => ({
                              ...current,
                              codexApprovalPolicy:
                                value === CODEX_SANDBOX_UNSET_OPTION
                                  ? CODEX_SANDBOX_UNSET
                                  : (value as CodexApprovalPolicyChoice),
                            }))
                          }}
                        >
                          <SelectTrigger className="h-8 text-xs">
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent align="start">
                            <SelectItem value={CODEX_SANDBOX_UNSET_OPTION}>
                              {t("codex.approvalPolicyUnset")}
                            </SelectItem>
                            {CODEX_APPROVAL_POLICY_VALUES.map((value) => (
                              <SelectItem key={value} value={value}>
                                {t(`codex.approvalPolicy_${value}`)}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                        {/* `untrusted` has no equivalent in codex-acp's three
                            approval presets, so an ACP session cannot honor it
                            (#442). Say so where the user picks it, rather than
                            letting it look effective. */}
                        {selectedDraft.codexApprovalPolicy === "untrusted" ? (
                          <p className="text-3xs text-yellow-500">
                            {t("codex.approvalPolicyUntrustedAcpWarning")}
                          </p>
                        ) : null}
                      </div>

                      {selectedDraft.codexApprovalPolicy === "granular" ? (
                        <div className="space-y-1 rounded-md border border-dashed px-2.5 py-2">
                          <p className="text-3xs text-muted-foreground">
                            {t("codex.granularHint")}
                          </p>
                          {CODEX_GRANULAR_KEYS.map((key) => (
                            <div
                              className="flex items-center justify-between gap-2 py-0.5"
                              key={key}
                            >
                              <label className="text-2xs text-muted-foreground">
                                {t(`codex.granular_${key}`)}
                              </label>
                              <Switch
                                checked={selectedDraft.codexGranular[key]}
                                onCheckedChange={(checked) => {
                                  updateSelectedDraft((current) => ({
                                    ...current,
                                    codexGranular: {
                                      ...current.codexGranular,
                                      [key]: checked,
                                    },
                                  }))
                                }}
                                aria-label={t(`codex.granular_${key}`)}
                              />
                            </div>
                          ))}
                        </div>
                      ) : null}

                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          {t("codex.sandboxModeLabel")}
                        </label>
                        <Select
                          disabled={selectedDraft.codexSandboxShadowed}
                          value={
                            selectedDraft.codexSandboxMode ||
                            CODEX_SANDBOX_UNSET_OPTION
                          }
                          onValueChange={(value) => {
                            updateSelectedDraft((current) => ({
                              ...current,
                              codexSandboxMode:
                                value === CODEX_SANDBOX_UNSET_OPTION
                                  ? CODEX_SANDBOX_UNSET
                                  : (value as CodexSandboxModeChoice),
                            }))
                          }}
                        >
                          <SelectTrigger className="h-8 text-xs">
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent align="start">
                            <SelectItem value={CODEX_SANDBOX_UNSET_OPTION}>
                              {t("codex.sandboxModeUnset")}
                            </SelectItem>
                            {CODEX_SANDBOX_MODE_VALUES.map((value) => (
                              <SelectItem key={value} value={value}>
                                {t(`codex.sandboxMode_${value}`)}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                        <p className="text-3xs text-muted-foreground">
                          {t("codex.sandboxModeHint")}
                        </p>
                        {/* Sandbox mode is what codeg maps onto the session's
                            starting approval preset (#442), so it reaches
                            ordinary prompts even though approval_policy does
                            not. Worth stating next to the control that does it. */}
                        {codexSandboxSeedsAcpPreset(
                          selectedDraft.codexSandboxShadowed
                        ) ? (
                          <p className="text-3xs text-muted-foreground">
                            {t("codex.sandboxModeSeedsPresetHint")}
                          </p>
                        ) : null}
                        {/* codex-acp 1.7.0–1.13.x gave their `read-only`
                            preset a workspace-write sandbox and re-send that
                            policy every turn, so an ACP session on one of them
                            cannot honor a read-only sandbox. 2.0.0 restored it,
                            but launch prefers an adapter on PATH, which may be
                            older — so the caveat stays, stated as a version
                            note rather than a warning about the pinned one. */}
                        {showsCodexReadOnlyAcpWarning(
                          selectedDraft.codexSandboxMode,
                          selectedDraft.codexSandboxShadowed
                        ) ? (
                          <p className="text-3xs text-muted-foreground">
                            {t("codex.sandboxModeReadOnlyAcpWarning")}
                          </p>
                        ) : null}
                      </div>

                      {codexWorkspaceWriteApplies(
                        selectedDraft.codexSandboxMode
                      ) && !selectedDraft.codexSandboxShadowed ? (
                        <div className="space-y-2 rounded-md border border-dashed px-2.5 py-2">
                          <div className="space-y-1">
                            <label className="text-2xs text-muted-foreground">
                              {t("codex.writableRootsLabel")}
                            </label>
                            <Textarea
                              className="min-h-16 font-mono text-2xs"
                              spellCheck={false}
                              value={selectedDraft.codexWritableRootsText}
                              onChange={(event) => {
                                const next = event.target.value
                                updateSelectedDraft((current) => ({
                                  ...current,
                                  codexWritableRootsText: next,
                                }))
                              }}
                              placeholder={"/Users/me/shared\n/srv/cache"}
                            />
                            {codexRelativeWritableRoot ? (
                              <p className="text-3xs text-red-500">
                                {t("codex.sandboxRootsRelativeError", {
                                  path: codexRelativeWritableRoot,
                                })}
                              </p>
                            ) : (
                              <p className="text-3xs text-muted-foreground">
                                {t("codex.writableRootsHint")}
                              </p>
                            )}
                          </div>
                          <div className="flex items-center justify-between gap-2">
                            <label className="text-2xs text-muted-foreground">
                              {t("codex.networkAccessLabel")}
                            </label>
                            <Switch
                              checked={selectedDraft.codexNetworkAccess}
                              onCheckedChange={(checked) => {
                                updateSelectedDraft((current) => ({
                                  ...current,
                                  codexNetworkAccess: checked,
                                }))
                              }}
                              aria-label={t("codex.networkAccessLabel")}
                            />
                          </div>
                          <div className="flex items-center justify-between gap-2">
                            <label className="text-2xs text-muted-foreground">
                              {t("codex.excludeTmpdirLabel")}
                            </label>
                            <Switch
                              checked={selectedDraft.codexExcludeTmpdirEnvVar}
                              onCheckedChange={(checked) => {
                                updateSelectedDraft((current) => ({
                                  ...current,
                                  codexExcludeTmpdirEnvVar: checked,
                                }))
                              }}
                              aria-label={t("codex.excludeTmpdirLabel")}
                            />
                          </div>
                          <div className="flex items-center justify-between gap-2">
                            <label className="text-2xs text-muted-foreground">
                              {t("codex.excludeSlashTmpLabel")}
                            </label>
                            <Switch
                              checked={selectedDraft.codexExcludeSlashTmp}
                              onCheckedChange={(checked) => {
                                updateSelectedDraft((current) => ({
                                  ...current,
                                  codexExcludeSlashTmp: checked,
                                }))
                              }}
                              aria-label={t("codex.excludeSlashTmpLabel")}
                            />
                          </div>
                        </div>
                      ) : null}
                    </div>

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        {t("codex.configTomlNative")}
                      </label>
                      <Textarea
                        value={selectedDraft.codexConfigTomlText}
                        onChange={(event) => {
                          handleCodexConfigTomlTextChange(event.target.value)
                        }}
                        placeholder={`disable_response_storage = true
model = "gpt-6-astra"
model_reasoning_effort = "high"
model_provider = "codeg"

[features]
responses_websockets_v2 = true

[model_providers.codeg]
base_url = "https://api.openai.com/v1"
supports_websockets = true`}
                        className="min-h-40 max-h-80 font-mono text-xs"
                      />
                    </div>

                    <div className="flex justify-end">
                      <Button
                        size="sm"
                        onClick={() => {
                          if (selectedMissingModelProvider) {
                            toast.error(t("toasts.modelProviderRequired"))
                            return
                          }
                          const codexEnvText =
                            selectedDraft.codexAuthMode ===
                            "chatgpt_subscription"
                              ? patchEnvText(selectedDraft.envText, {
                                  OPENAI_API_KEY: "",
                                  OPENAI_BASE_URL: "",
                                })
                              : selectedDraft.envText
                          // Persist sequentially, never in parallel: persistEnv
                          // (acp_update_agent_env) rewrites ~/.codex/config.toml
                          // to sync the root `model`, while persistConfig writes
                          // the full config.toml including base_url. Running both
                          // at once races two read-modify-write cycles on the same
                          // file, letting the model sync clobber the just-written
                          // base_url (the API key in auth.json is unaffected, so
                          // the key saves but the URL silently does not).
                          // persistConfig runs last so its authoritative
                          // config.toml wins.
                          persistEnv(
                            selectedAgent.agent_type,
                            selectedDraft.enabled,
                            codexEnvText,
                            selectedDraft.modelProviderId
                          )
                            .then(() =>
                              persistConfig(
                                selectedAgent.agent_type,
                                selectedDraft.configText,
                                {
                                  codexAuthJsonText:
                                    selectedDraft.codexAuthJsonText,
                                  codexConfigTomlText:
                                    selectedDraft.codexConfigTomlText,
                                  codexModelCatalog:
                                    serializeCodexModelConfig(
                                      selectedDraft.codexModelList
                                    ) ?? "",
                                  codexSandbox:
                                    codexSandboxSaveConfig(selectedDraft),
                                }
                              )
                            )
                            .then(() => {
                              toast.success(t("toasts.codexSaved"), {
                                description: t("toasts.configSavedHint"),
                              })
                            })
                            .catch((err) => {
                              console.error(
                                "[Settings] save codex native config failed:",
                                err
                              )
                              const message = toErrorMessage(err)
                              toast.error(t("toasts.saveCodexNativeFailed"), {
                                description: message,
                              })
                            })
                        }}
                        disabled={selectedIsSavingEnv || selectedIsSavingConfig}
                      >
                        {selectedIsSavingEnv || selectedIsSavingConfig ? (
                          <>
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            {t("actions.saving")}
                          </>
                        ) : (
                          <>
                            <Save className="h-3.5 w-3.5" />
                            {t("actions.saveCodexConfig")}
                          </>
                        )}
                      </Button>
                    </div>
                  </div>
                ) : selectedAgent.agent_type === "gemini" ? (
                  <div className="space-y-3 rounded-md border bg-muted/10 p-3">
                    <div>
                      <label className="text-xs font-medium">
                        {t("gemini.authConfig")}
                      </label>
                      <p className="mt-1 text-2xs text-muted-foreground">
                        {t("gemini.authConfigDescription")}
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        {t("gemini.authMode")}
                      </label>
                      <Select
                        value={selectedDraft.geminiAuthMode}
                        onValueChange={(value) => {
                          if (
                            GEMINI_AUTH_MODES.includes(value as GeminiAuthMode)
                          ) {
                            handleGeminiAuthModeChange(value as GeminiAuthMode)
                          }
                        }}
                      >
                        <SelectTrigger className="w-full">
                          <SelectValue
                            placeholder={t("gemini.selectAuthMode")}
                          />
                        </SelectTrigger>
                        <SelectContent align="start">
                          {/* [crewchat] "model_provider" hidden — same reason as
                              the codex panel above. */}
                          {GEMINI_AUTH_MODES.filter(
                            (mode) => mode !== "model_provider"
                          ).map((mode) => (
                            <SelectItem key={mode} value={mode}>
                              {geminiAuthModeLabel(mode)}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <p className="text-2xs text-muted-foreground">
                        {geminiAuthModeHint(selectedDraft.geminiAuthMode)}
                      </p>
                    </div>

                    {selectedDraft.geminiAuthMode === "model_provider" && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          {t("selectModelProvider")}
                        </label>
                        {selectedModelProviders.length > 0 ? (
                          <Select
                            value={
                              selectedDraft.modelProviderId != null
                                ? String(selectedDraft.modelProviderId)
                                : ""
                            }
                            onValueChange={handleModelProviderSelect}
                          >
                            <SelectTrigger className="w-full">
                              <SelectValue
                                placeholder={t("selectModelProvider")}
                              />
                            </SelectTrigger>
                            <SelectContent align="start">
                              {selectedModelProviders.map((provider) => (
                                <SelectItem
                                  key={provider.id}
                                  value={String(provider.id)}
                                >
                                  {provider.name}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        ) : (
                          <p className="text-2xs text-muted-foreground">
                            {t("noModelProviderAvailable")}
                          </p>
                        )}
                      </div>
                    )}

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        Model
                      </label>
                      <Input
                        value={selectedDraft.model}
                        readOnly={
                          selectedDraft.geminiAuthMode === "model_provider"
                        }
                        onChange={(event) => {
                          handleGeminiFieldChange("model", event.target.value)
                        }}
                        placeholder="gemini-3-pro-preview"
                      />
                      <p className="text-2xs text-muted-foreground">
                        {t("modelHintDefault")}
                      </p>
                    </div>

                    {(selectedDraft.geminiAuthMode === "custom" ||
                      selectedDraft.geminiAuthMode === "model_provider") && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          GOOGLE_GEMINI_BASE_URL
                        </label>
                        <Input
                          value={selectedDraft.apiBaseUrl}
                          readOnly={
                            selectedDraft.geminiAuthMode === "model_provider"
                          }
                          onChange={(event) => {
                            handleGeminiFieldChange(
                              "apiBaseUrl",
                              event.target.value
                            )
                          }}
                          placeholder="https://your-gemini-endpoint.example.com"
                        />
                      </div>
                    )}

                    {(selectedDraft.geminiAuthMode === "custom" ||
                      selectedDraft.geminiAuthMode === "gemini_api_key" ||
                      selectedDraft.geminiAuthMode === "model_provider" ||
                      selectedDraft.geminiAuthMode === "vertex_api_key") && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          {selectedDraft.geminiAuthMode === "vertex_api_key"
                            ? "GOOGLE_API_KEY"
                            : "GEMINI_API_KEY"}
                        </label>
                        <div className="flex items-center gap-2">
                          <Input
                            type={
                              showApiKeys[selectedAgent.agent_type]
                                ? "text"
                                : "password"
                            }
                            value={
                              selectedDraft.geminiAuthMode === "vertex_api_key"
                                ? selectedDraft.googleApiKey
                                : selectedDraft.geminiApiKey
                            }
                            readOnly={
                              selectedDraft.geminiAuthMode === "model_provider"
                            }
                            onChange={(event) => {
                              if (
                                selectedDraft.geminiAuthMode ===
                                "vertex_api_key"
                              ) {
                                handleGeminiFieldChange(
                                  "googleApiKey",
                                  event.target.value
                                )
                                return
                              }
                              handleGeminiFieldChange(
                                "geminiApiKey",
                                event.target.value
                              )
                            }}
                            placeholder="AIza..."
                          />
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              setShowApiKeys((prev) => ({
                                ...prev,
                                [selectedAgent.agent_type]:
                                  !prev[selectedAgent.agent_type],
                              }))
                            }}
                            title={
                              showApiKeys[selectedAgent.agent_type]
                                ? t("actions.hideKey")
                                : t("actions.showKey")
                            }
                          >
                            {showApiKeys[selectedAgent.agent_type] ? (
                              <EyeOff className="h-3.5 w-3.5" />
                            ) : (
                              <Eye className="h-3.5 w-3.5" />
                            )}
                          </Button>
                        </div>
                      </div>
                    )}

                    {(selectedDraft.geminiAuthMode === "vertex_adc" ||
                      selectedDraft.geminiAuthMode ===
                        "vertex_service_account" ||
                      selectedDraft.geminiAuthMode === "vertex_api_key") && (
                      <div className="grid gap-3 md:grid-cols-2">
                        <div className="space-y-1.5">
                          <label className="text-2xs text-muted-foreground">
                            GOOGLE_CLOUD_PROJECT
                          </label>
                          <Input
                            value={selectedDraft.googleCloudProject}
                            onChange={(event) => {
                              handleGeminiFieldChange(
                                "googleCloudProject",
                                event.target.value
                              )
                            }}
                            placeholder="my-gcp-project-id"
                          />
                        </div>
                        <div className="space-y-1.5">
                          <label className="text-2xs text-muted-foreground">
                            GOOGLE_CLOUD_LOCATION
                          </label>
                          <Input
                            value={selectedDraft.googleCloudLocation}
                            onChange={(event) => {
                              handleGeminiFieldChange(
                                "googleCloudLocation",
                                event.target.value
                              )
                            }}
                            placeholder="global / us-central1"
                          />
                        </div>
                      </div>
                    )}

                    {selectedDraft.geminiAuthMode ===
                      "vertex_service_account" && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          GOOGLE_APPLICATION_CREDENTIALS
                        </label>
                        <Input
                          value={selectedDraft.googleApplicationCredentials}
                          onChange={(event) => {
                            handleGeminiFieldChange(
                              "googleApplicationCredentials",
                              event.target.value
                            )
                          }}
                          placeholder="/path/to/service-account.json"
                        />
                      </div>
                    )}

                    <div className="flex items-center justify-between gap-2">
                      <Button
                        type="button"
                        size="sm"
                        variant="outline"
                        onClick={() => {
                          openUrl(
                            "https://geminicli.com/docs/get-started/authentication/"
                          ).catch((err) => {
                            console.error(
                              "[Settings] open gemini auth doc failed:",
                              err
                            )
                          })
                        }}
                      >
                        {t("gemini.viewAuthDoc")}
                      </Button>
                      <Button
                        size="sm"
                        onClick={() => {
                          if (selectedMissingModelProvider) {
                            toast.error(t("toasts.modelProviderRequired"))
                            return
                          }
                          Promise.all([
                            persistEnv(
                              selectedAgent.agent_type,
                              selectedDraft.enabled,
                              selectedDraft.envText,
                              selectedDraft.modelProviderId
                            ),
                            persistConfig(
                              selectedAgent.agent_type,
                              selectedDraft.configText
                            ),
                          ])
                            .then(() => {
                              toast.success(t("toasts.geminiSaved"), {
                                description: t("toasts.configSavedHint"),
                              })
                            })
                            .catch((err) => {
                              console.error(
                                "[Settings] save gemini config failed:",
                                err
                              )
                              const message = toErrorMessage(err)
                              toast.error(t("toasts.saveGeminiFailed"), {
                                description: message,
                              })
                            })
                        }}
                        disabled={selectedIsSavingEnv || selectedIsSavingConfig}
                      >
                        {selectedIsSavingEnv || selectedIsSavingConfig ? (
                          <>
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            {t("actions.saving")}
                          </>
                        ) : (
                          <>
                            <Save className="h-3.5 w-3.5" />
                            {t("actions.saveGeminiConfig")}
                          </>
                        )}
                      </Button>
                    </div>
                  </div>
                ) : selectedAgent.agent_type === "open_code" ? (
                  <div className="space-y-3 rounded-md border bg-muted/10 p-3">
                    <div>
                      <label className="text-xs font-medium">
                        {t("openCode.configManagement")}
                      </label>
                      <p className="mt-1 text-2xs text-muted-foreground">
                        {t("openCode.configDescription")}
                      </p>
                    </div>

                    <div className="grid gap-3 md:grid-cols-2">
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          {t("openCode.mainModel")}
                        </label>
                        <OpenCodeModelCombobox
                          value={selectedOpenCodeConfig?.model ?? ""}
                          onValueChange={(v) =>
                            handleOpenCodeFieldChange("model", v)
                          }
                          groups={openCodeModelOptions}
                          placeholder="provider/model-id"
                        />
                      </div>
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          {t("openCode.smallModel")}
                        </label>
                        <OpenCodeModelCombobox
                          value={selectedOpenCodeConfig?.smallModel ?? ""}
                          onValueChange={(v) =>
                            handleOpenCodeFieldChange("small_model", v)
                          }
                          groups={openCodeModelOptions}
                          placeholder="provider/model-id"
                        />
                      </div>
                    </div>

                    <div className="space-y-2 rounded-md border bg-background/60 p-3">
                      <div className="flex items-center justify-between gap-2">
                        <label className="text-2xs font-medium">
                          {t("openCode.providerManagement")}
                        </label>
                        <div className="text-2xs text-muted-foreground">
                          {t("openCode.providerCount", {
                            count:
                              selectedOpenCodeConfig?.providerIds.length ?? 0,
                          })}
                        </div>
                      </div>

                      <div className="flex flex-wrap items-center gap-2">
                        <Button
                          type="button"
                          size="sm"
                          onClick={() => {
                            setOpenCodeEditProviderId(null)
                            setOpenCodeConnectOpen(true)
                          }}
                        >
                          <Plug className="h-3.5 w-3.5" />
                          {t("openCode.connectProvider")}
                        </Button>
                        <Button
                          type="button"
                          size="sm"
                          variant="outline"
                          onClick={() => {
                            void handleOpenCodeRefreshCatalog()
                          }}
                          disabled={openCodeCatalogLoading}
                          title={t("openCode.refreshCatalog")}
                        >
                          <RefreshCw
                            className={cn(
                              "h-3.5 w-3.5",
                              openCodeCatalogLoading && "animate-spin"
                            )}
                          />
                          {t("openCode.refreshCatalog")}
                        </Button>
                        {openCodeCatalogLoading &&
                          openCodeCatalog.length === 0 && (
                            <span className="inline-flex items-center gap-1 text-2xs text-muted-foreground">
                              <Loader2 className="h-3 w-3 animate-spin" />
                              {t("openCode.connect.loading")}
                            </span>
                          )}
                      </div>

                      {openCodeWellKnownConnected.length === 0 ? (
                        <div className="text-2xs text-muted-foreground">
                          {t("openCode.noConnectedProviders")}
                        </div>
                      ) : (
                        <div className="space-y-1.5">
                          <label className="text-2xs font-medium">
                            {t("openCode.connectedProviders")}
                          </label>
                          <div className="space-y-1.5">
                            {openCodeWellKnownConnected.map((provider) => (
                              <div
                                key={provider.id}
                                className="flex flex-wrap items-center justify-between gap-2 rounded-md border bg-muted/20 px-2.5 py-1.5"
                              >
                                <div className="flex min-w-0 flex-1 items-center gap-2">
                                  <span className="truncate text-xs font-medium">
                                    {provider.name}
                                  </span>
                                  <span className="text-3xs text-muted-foreground">
                                    {provider.id}
                                  </span>
                                  <Badge variant="outline" className="text-3xs">
                                    {provider.authKind === "oauth"
                                      ? t("openCode.authKindOauth")
                                      : provider.authKind === "api"
                                        ? t("openCode.authKindApi")
                                        : t("openCode.authKindNone")}
                                  </Badge>
                                  {!provider.inCatalog && (
                                    <Badge
                                      variant="secondary"
                                      className="text-3xs"
                                    >
                                      {t("openCode.customBadge")}
                                    </Badge>
                                  )}
                                </div>
                                <div className="flex items-center gap-2.5">
                                  <Switch
                                    checked={provider.enabled}
                                    onCheckedChange={(checked) => {
                                      void handleOpenCodeToggleEnabled(
                                        provider.id,
                                        checked
                                      )
                                    }}
                                    aria-label={t(
                                      "openCode.providerEnabledState",
                                      { providerId: provider.id }
                                    )}
                                  />
                                  {provider.authKind !== "oauth" && (
                                    <Button
                                      type="button"
                                      size="xs"
                                      variant="ghost"
                                      onClick={() => {
                                        // Top list is well-known only → the
                                        // guided dialog edits the key/base URL.
                                        setOpenCodeEditProviderId(provider.id)
                                        setOpenCodeConnectOpen(true)
                                      }}
                                    >
                                      {t("openCode.editConfig")}
                                    </Button>
                                  )}
                                  <Button
                                    type="button"
                                    size="xs"
                                    variant="outline"
                                    onClick={() => {
                                      void handleOpenCodeDisconnect(
                                        provider.id,
                                        provider.hasConfigBlock
                                      )
                                    }}
                                  >
                                    {t("openCode.disconnect")}
                                  </Button>
                                </div>
                              </div>
                            ))}
                          </div>
                        </div>
                      )}

                      <OpenCodeConnectDialog
                        open={openCodeConnectOpen}
                        onOpenChange={(o) => {
                          setOpenCodeConnectOpen(o)
                          if (!o) setOpenCodeEditProviderId(null)
                        }}
                        catalog={openCodeCatalog}
                        catalogLoading={openCodeCatalogLoading}
                        configText={selectedDraft.configText}
                        authJsonText={selectedDraft.openCodeAuthJsonText}
                        editProviderId={openCodeEditProviderId}
                        onConnect={applyOpenCodeConnect}
                      />

                      <OpenCodeCustomProviderDialog
                        open={openCodeCustomOpen}
                        onOpenChange={setOpenCodeCustomOpen}
                        existingProviderIds={
                          selectedOpenCodeConfig?.providerIds ?? []
                        }
                        catalogIds={openCodeCatalog.map((p) => p.id)}
                        configText={selectedDraft.configText}
                        authJsonText={selectedDraft.openCodeAuthJsonText}
                        onConnect={applyOpenCodeConnect}
                      />

                      <div className="space-y-1 border-t pt-2">
                        <div className="flex items-center justify-between gap-2">
                          <div className="text-2xs font-medium text-muted-foreground">
                            {t("openCode.advancedProviderConfig")}
                          </div>
                          <Button
                            type="button"
                            size="xs"
                            variant="outline"
                            onClick={() => setOpenCodeCustomOpen(true)}
                            disabled={
                              openCodeCatalogLoading || !openCodeCatalogReady
                            }
                            title={
                              openCodeCatalogLoading || !openCodeCatalogReady
                                ? t("openCode.connect.loading")
                                : undefined
                            }
                          >
                            <Plus className="h-3.5 w-3.5" />
                            {t("openCode.addCustomProvider")}
                          </Button>
                        </div>
                        <p className="text-3xs text-muted-foreground">
                          {t("openCode.customProviderConfigHint")}
                        </p>
                      </div>

                      {openCodeCustomProviderIds.length === 0 ? (
                        <div className="text-2xs text-muted-foreground">
                          {t("openCode.emptyProvider")}
                        </div>
                      ) : (
                        <div className="space-y-2">
                          {openCodeCustomProviderIds.map((providerId) => {
                            if (!selectedOpenCodeConfig) return null
                            const provider =
                              selectedOpenCodeConfig.providers[providerId]
                            if (!provider) return null
                            const expanded = openCodeProviderId === providerId
                            const isDisabled =
                              selectedOpenCodeConfig.disabledProviders.includes(
                                providerId
                              ) ||
                              (selectedOpenCodeConfig.enabledProviders.length >
                                0 &&
                                !selectedOpenCodeConfig.enabledProviders.includes(
                                  providerId
                                ))
                            return (
                              <Collapsible
                                key={providerId}
                                open={expanded}
                                onOpenChange={(open) => {
                                  setOpenCodeProviderId(open ? providerId : "")
                                }}
                              >
                                <div className="rounded-md border bg-muted/20">
                                  <div className="flex items-center justify-between gap-2 px-2.5 py-2">
                                    <button
                                      type="button"
                                      className="flex min-w-0 flex-1 items-center gap-2 text-left"
                                      onClick={() => {
                                        setOpenCodeProviderId((current) =>
                                          current === providerId
                                            ? ""
                                            : providerId
                                        )
                                      }}
                                    >
                                      <ChevronDown
                                        className={cn(
                                          "h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform",
                                          expanded && "rotate-180"
                                        )}
                                      />
                                      <span className="truncate text-xs font-medium">
                                        {providerId}
                                      </span>
                                      <span className="text-2xs text-muted-foreground">
                                        models: {provider.modelCount}
                                      </span>
                                    </button>
                                    <div className="flex items-center gap-3">
                                      <span className="text-2xs text-muted-foreground">
                                        {isDisabled
                                          ? t("status.disabled")
                                          : t("status.enabled")}
                                      </span>
                                      <Switch
                                        checked={!isDisabled}
                                        onCheckedChange={(checked) => {
                                          handleOpenCodeProviderStatusChange(
                                            providerId,
                                            checked
                                          )
                                        }}
                                        aria-label={t(
                                          "openCode.providerEnabledState",
                                          { providerId }
                                        )}
                                        title={
                                          isDisabled
                                            ? t("actions.clickEnable", {
                                                name: providerId,
                                              })
                                            : t("actions.clickDisable", {
                                                name: providerId,
                                              })
                                        }
                                      />
                                      <Button
                                        type="button"
                                        size="xs"
                                        variant="outline"
                                        onClick={() => {
                                          setOpenCodeDeleteProviderId(
                                            providerId
                                          )
                                        }}
                                      >
                                        {t("actions.delete")}
                                      </Button>
                                    </div>
                                  </div>

                                  <CollapsibleContent className="px-2.5 pb-2.5">
                                    <div className="grid gap-3 border-t pt-2.5 md:grid-cols-2">
                                      <div className="space-y-1.5">
                                        <label className="text-2xs text-muted-foreground">
                                          provider.name
                                        </label>
                                        <Input
                                          value={provider.name}
                                          onChange={(event) => {
                                            handleOpenCodeProviderFieldChange(
                                              providerId,
                                              "name",
                                              event.target.value
                                            )
                                          }}
                                          placeholder="My Provider"
                                        />
                                      </div>
                                      <div className="space-y-1.5">
                                        <label className="text-2xs text-muted-foreground">
                                          provider.npm
                                        </label>
                                        <Select
                                          value={
                                            provider.npm.trim()
                                              ? provider.npm
                                              : OPENCODE_PROVIDER_NPM_OPTIONS[0]
                                                  .value
                                          }
                                          onValueChange={(value) => {
                                            handleOpenCodeProviderFieldChange(
                                              providerId,
                                              "npm",
                                              value
                                            )
                                          }}
                                        >
                                          <SelectTrigger className="w-full">
                                            <SelectValue
                                              placeholder={t(
                                                "openCode.selectProviderNpm"
                                              )}
                                            />
                                          </SelectTrigger>
                                          <SelectContent align="start">
                                            {buildOpenCodeNpmOptions(
                                              provider.npm
                                            ).map((npmOption) => (
                                              <SelectItem
                                                key={npmOption}
                                                value={npmOption}
                                              >
                                                {npmOption}
                                              </SelectItem>
                                            ))}
                                          </SelectContent>
                                        </Select>
                                      </div>
                                      <div className="space-y-1.5">
                                        <label className="text-2xs text-muted-foreground">
                                          provider.api
                                        </label>
                                        <Input
                                          value={provider.api}
                                          onChange={(event) => {
                                            handleOpenCodeProviderFieldChange(
                                              providerId,
                                              "api",
                                              event.target.value
                                            )
                                          }}
                                          placeholder="openai.responses"
                                        />
                                      </div>
                                      <div className="space-y-1.5">
                                        <label className="text-2xs text-muted-foreground">
                                          provider.options.baseURL
                                        </label>
                                        <Input
                                          value={provider.baseUrl}
                                          onChange={(event) => {
                                            handleOpenCodeProviderFieldChange(
                                              providerId,
                                              "baseURL",
                                              event.target.value
                                            )
                                          }}
                                          placeholder="https://api.example.com/v1"
                                        />
                                      </div>
                                      <div className="space-y-1.5 md:col-span-2">
                                        <label className="text-2xs text-muted-foreground">
                                          provider.options.apiKey
                                        </label>
                                        <div className="flex items-center gap-2">
                                          <Input
                                            type={
                                              showApiKeys[
                                                selectedAgent.agent_type
                                              ]
                                                ? "text"
                                                : "password"
                                            }
                                            value={provider.apiKey}
                                            onChange={(event) => {
                                              handleOpenCodeProviderFieldChange(
                                                providerId,
                                                "apiKey",
                                                event.target.value
                                              )
                                            }}
                                            placeholder="sk-..."
                                          />
                                          <Button
                                            type="button"
                                            variant="outline"
                                            size="sm"
                                            onClick={() => {
                                              setShowApiKeys((prev) => ({
                                                ...prev,
                                                [selectedAgent.agent_type]:
                                                  !prev[
                                                    selectedAgent.agent_type
                                                  ],
                                              }))
                                            }}
                                            title={
                                              showApiKeys[
                                                selectedAgent.agent_type
                                              ]
                                                ? t("actions.hideKey")
                                                : t("actions.showKey")
                                            }
                                          >
                                            {showApiKeys[
                                              selectedAgent.agent_type
                                            ] ? (
                                              <EyeOff className="h-3.5 w-3.5" />
                                            ) : (
                                              <Eye className="h-3.5 w-3.5" />
                                            )}
                                          </Button>
                                        </div>
                                      </div>
                                    </div>
                                    <Collapsible
                                      open={Boolean(
                                        openCodeModelConfigExpanded[providerId]
                                      )}
                                      onOpenChange={(open) => {
                                        setOpenCodeModelConfigExpanded(
                                          (prev) => ({
                                            ...prev,
                                            [providerId]: open,
                                          })
                                        )
                                      }}
                                    >
                                      <div className="mt-3 rounded-md border bg-background/50 p-2.5">
                                        <button
                                          type="button"
                                          className="flex w-full items-center justify-between gap-2 text-left"
                                          onClick={() => {
                                            setOpenCodeModelConfigExpanded(
                                              (prev) => ({
                                                ...prev,
                                                [providerId]: !prev[providerId],
                                              })
                                            )
                                          }}
                                        >
                                          <div className="flex items-center gap-2">
                                            <ChevronDown
                                              className={cn(
                                                "h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform",
                                                openCodeModelConfigExpanded[
                                                  providerId
                                                ] && "rotate-180"
                                              )}
                                            />
                                            <span className="text-2xs font-medium">
                                              {t("openCode.modelManagement")}
                                            </span>
                                          </div>
                                          <span className="text-2xs text-muted-foreground">
                                            {t("openCode.modelCount", {
                                              count: provider.modelCount,
                                            })}
                                          </span>
                                        </button>
                                        <CollapsibleContent className="pt-2">
                                          <p className="text-2xs text-muted-foreground">
                                            {t("openCode.modelDescription")}
                                          </p>

                                          <div className="mt-2 flex flex-wrap items-center gap-2">
                                            <Input
                                              value={
                                                openCodeNewModelIds[
                                                  providerId
                                                ] ?? ""
                                              }
                                              onChange={(event) => {
                                                handleOpenCodeModelDraftChange(
                                                  providerId,
                                                  event.target.value
                                                )
                                              }}
                                              className="w-[15rem]"
                                              placeholder="new-model-id"
                                            />
                                            <Button
                                              type="button"
                                              size="sm"
                                              variant="outline"
                                              onClick={() => {
                                                handleOpenCodeAddModel(
                                                  providerId
                                                )
                                              }}
                                            >
                                              {t("openCode.addModel")}
                                            </Button>
                                          </div>

                                          {provider.modelIds.length === 0 ? (
                                            <div className="mt-2 text-2xs text-muted-foreground">
                                              {t("openCode.emptyModel")}
                                            </div>
                                          ) : (
                                            <div className="mt-2 space-y-1">
                                              <div className="flex items-center gap-2 px-1 text-3xs text-muted-foreground">
                                                <div className="min-w-0 flex-1">
                                                  {t("openCode.modelId")}
                                                </div>
                                                <div className="min-w-0 flex-1">
                                                  {t("openCode.modelName")}
                                                </div>
                                                <div className="size-8 shrink-0" />
                                              </div>
                                              {provider.modelIds.map(
                                                (modelId) => {
                                                  const model =
                                                    provider.models[modelId]
                                                  if (!model) return null
                                                  const modelDraftKey = `${providerId}:${modelId}`
                                                  return (
                                                    <div
                                                      key={`${providerId}:${modelId}`}
                                                      className="flex items-center gap-2"
                                                    >
                                                      <Input
                                                        value={
                                                          openCodeModelIdDrafts[
                                                            modelDraftKey
                                                          ] ?? model.id
                                                        }
                                                        onChange={(event) => {
                                                          handleOpenCodeModelIdDraftChange(
                                                            providerId,
                                                            modelId,
                                                            event.target.value
                                                          )
                                                        }}
                                                        onBlur={() => {
                                                          handleOpenCodeModelIdCommit(
                                                            providerId,
                                                            modelId
                                                          )
                                                        }}
                                                        {...ime.props}
                                                        onKeyDown={(event) => {
                                                          if (
                                                            ime.isComposing(
                                                              event
                                                            )
                                                          )
                                                            return
                                                          if (
                                                            event.key ===
                                                            "Enter"
                                                          ) {
                                                            event.preventDefault()
                                                            handleOpenCodeModelIdCommit(
                                                              providerId,
                                                              modelId
                                                            )
                                                            event.currentTarget.blur()
                                                            return
                                                          }
                                                          if (
                                                            event.key ===
                                                            "Escape"
                                                          ) {
                                                            setOpenCodeModelIdDrafts(
                                                              (prev) => {
                                                                if (
                                                                  typeof prev[
                                                                    modelDraftKey
                                                                  ] ===
                                                                  "undefined"
                                                                ) {
                                                                  return prev
                                                                }
                                                                const next = {
                                                                  ...prev,
                                                                }
                                                                delete next[
                                                                  modelDraftKey
                                                                ]
                                                                return next
                                                              }
                                                            )
                                                            event.currentTarget.blur()
                                                          }
                                                        }}
                                                        className="h-8 min-w-0 flex-1"
                                                        placeholder="model.id"
                                                      />
                                                      <Input
                                                        value={model.name}
                                                        onChange={(event) => {
                                                          handleOpenCodeModelFieldChange(
                                                            providerId,
                                                            modelId,
                                                            event.target.value
                                                          )
                                                        }}
                                                        className="h-8 min-w-0 flex-1"
                                                        placeholder="model.name"
                                                      />
                                                      <Button
                                                        type="button"
                                                        size="icon-sm"
                                                        variant="ghost"
                                                        className="shrink-0 text-muted-foreground hover:text-destructive"
                                                        aria-label={t(
                                                          "openCode.deleteModel",
                                                          { modelId }
                                                        )}
                                                        title={t(
                                                          "openCode.deleteModel",
                                                          { modelId }
                                                        )}
                                                        onClick={() => {
                                                          handleOpenCodeRemoveModel(
                                                            providerId,
                                                            modelId
                                                          )
                                                        }}
                                                      >
                                                        <Minus className="h-3.5 w-3.5" />
                                                      </Button>
                                                    </div>
                                                  )
                                                }
                                              )}
                                            </div>
                                          )}
                                        </CollapsibleContent>
                                      </div>
                                    </Collapsible>
                                    <div className="mt-3 flex justify-end">
                                      <Button
                                        type="button"
                                        size="sm"
                                        onClick={() => {
                                          persistConfig(
                                            selectedAgent.agent_type,
                                            selectedDraft.configText,
                                            {
                                              openCodeAuthJsonText:
                                                selectedDraft.openCodeAuthJsonText,
                                            }
                                          )
                                            .then(() => {
                                              toast.success(
                                                t("toasts.providerSaved", {
                                                  providerId,
                                                }),
                                                {
                                                  description: `${t("toasts.openCodeConfigSynced")} ${t("toasts.configSavedHint")}`,
                                                }
                                              )
                                            })
                                            .catch((err) => {
                                              console.error(
                                                "[Settings] save opencode provider failed:",
                                                err
                                              )
                                              const message =
                                                err instanceof Error
                                                  ? err.message
                                                  : String(err)
                                              toast.error(
                                                t("toasts.saveProviderFailed", {
                                                  providerId,
                                                }),
                                                {
                                                  description: message,
                                                }
                                              )
                                            })
                                        }}
                                        disabled={selectedIsSavingConfig}
                                      >
                                        {selectedIsSavingConfig ? (
                                          <>
                                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                                            {t("actions.saving")}
                                          </>
                                        ) : (
                                          <>
                                            <Save className="h-3.5 w-3.5" />
                                            {t("actions.saveCurrentProvider")}
                                          </>
                                        )}
                                      </Button>
                                    </div>
                                  </CollapsibleContent>
                                </div>
                              </Collapsible>
                            )
                          })}
                        </div>
                      )}
                    </div>

                    {/*
                      The editor owns the `permission` key and hands back a
                      whole rewritten document, so it goes through the same
                      path as the raw JSON box below — draft-only, like the
                      model fields above, with the card's Save button doing
                      the write to opencode.json.
                    */}
                    <OpenCodePermissionsSection
                      configText={selectedDraft.configText}
                      onChange={handleConfigTextChange}
                      disabled={selectedIsSavingConfig}
                    />

                    {/*
                      Same contract as the permissions editor above: it owns a
                      disjoint set of top-level keys, rewrites the whole
                      document, and leaves the write to this card's Save button.
                    */}
                    <OpenCodeBehaviorSection
                      configText={selectedDraft.configText}
                      onChange={handleConfigTextChange}
                      disabled={selectedIsSavingConfig}
                    />

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        {t("openCode.nativeJsonConfig")}
                      </label>
                      <Textarea
                        value={selectedDraft.configText}
                        onChange={(event) => {
                          handleConfigTextChange(event.target.value)
                        }}
                        placeholder={`{
  "$schema": "https://opencode.ai/config.json",
  "model": "google/gemini-3-pro-preview",
  "provider": {
    "google": {
      "options": {
        "baseURL": "https://generativelanguage.googleapis.com/v1beta"
      }
    }
  }
}`}
                        className="min-h-44 max-h-96 overflow-y-auto font-mono text-xs"
                      />
                      {selectedConfigError && (
                        <div className="rounded-md border border-red-500/30 bg-red-500/5 px-2.5 py-1.5 text-2xs text-red-400">
                          {selectedConfigError}
                        </div>
                      )}
                    </div>

                    <div className="flex justify-end">
                      <Button
                        size="sm"
                        onClick={() => {
                          persistConfig(
                            selectedAgent.agent_type,
                            selectedDraft.configText,
                            {
                              openCodeAuthJsonText:
                                selectedDraft.openCodeAuthJsonText,
                            }
                          )
                            .then(() => {
                              toast.success(t("toasts.openCodeSaved"), {
                                description: t("toasts.configSavedHint"),
                              })
                            })
                            .catch((err) => {
                              console.error(
                                "[Settings] save opencode config failed:",
                                err
                              )
                              const message = toErrorMessage(err)
                              toast.error(t("toasts.saveOpenCodeFailed"), {
                                description: message,
                              })
                            })
                        }}
                        disabled={selectedIsSavingConfig}
                      >
                        {selectedIsSavingConfig ? (
                          <>
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            {t("actions.saving")}
                          </>
                        ) : (
                          <>
                            <Save className="h-3.5 w-3.5" />
                            {t("actions.saveOpenCodeConfig")}
                          </>
                        )}
                      </Button>
                    </div>
                  </div>
                ) : selectedAgent.agent_type === "cline" ? (
                  <div className="space-y-3 rounded-md border bg-muted/10 p-3">
                    <div>
                      <label className="text-xs font-medium">Cline</label>
                      <p className="mt-1 text-2xs text-muted-foreground">
                        {t("cline.configDescription")}
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        Provider
                      </label>
                      <Select
                        value={selectedDraft.clineProvider}
                        onValueChange={(value) => {
                          handleClineFieldChange("clineProvider", value)
                        }}
                      >
                        <SelectTrigger className="h-8 text-xs">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectGroup>
                            <SelectLabel>{t("cline.signInGroup")}</SelectLabel>
                            {CLINE_SIGNIN_PROVIDERS.map((p) => (
                              <SelectItem key={p.value} value={p.value}>
                                {p.label}
                              </SelectItem>
                            ))}
                          </SelectGroup>
                          <SelectGroup>
                            <SelectLabel>{t("cline.byoGroup")}</SelectLabel>
                            {CLINE_BYO_PROVIDERS.map((p) => (
                              <SelectItem key={p.value} value={p.value}>
                                {p.label}
                              </SelectItem>
                            ))}
                          </SelectGroup>
                          {/* A provider configured outside codeg (e.g. `cline
                              auth bedrock`) keeps its own row, so opening this
                              panel can't silently retarget the store. */}
                          {!isClineSignInProvider(
                            selectedDraft.clineProvider
                          ) &&
                            !CLINE_BYO_PROVIDERS.some(
                              (p) => p.value === selectedDraft.clineProvider
                            ) && (
                              <SelectGroup>
                                <SelectLabel>
                                  {t("cline.externalGroup")}
                                </SelectLabel>
                                <SelectItem value={selectedDraft.clineProvider}>
                                  {selectedDraft.clineProvider}
                                </SelectItem>
                              </SelectGroup>
                            )}
                        </SelectContent>
                      </Select>
                    </div>

                    {isClineSignInProvider(selectedDraft.clineProvider) ? (
                      // Sign-in: cline holds the credential, so there is nothing
                      // here to fill in — only the command that obtains it. The
                      // launch path deliberately exports no provider/key pair
                      // for these, leaving `tryRestoreAuth` to find the token.
                      <div className="space-y-1.5">
                        <p className="text-2xs text-muted-foreground">
                          {t("cline.signInHint")}
                        </p>
                        <div className="flex items-center gap-2">
                          <code className="flex-1 overflow-x-auto rounded bg-muted px-2 py-1 text-2xs font-mono whitespace-nowrap">
                            {clineAuthCommand(selectedDraft.clineProvider)}
                          </code>
                          <Button
                            type="button"
                            variant="ghost"
                            size="sm"
                            className="h-7 w-7 shrink-0 p-0"
                            onClick={async () => {
                              const ok = await copyTextToClipboard(
                                clineAuthCommand(selectedDraft.clineProvider)
                              )
                              if (ok) toast.success(t("grok.commandCopied"))
                            }}
                            title={t("grok.copyCommand")}
                            aria-label={t("grok.copyCommand")}
                          >
                            <Copy className="h-3 w-3" />
                          </Button>
                        </div>
                      </div>
                    ) : (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          API Key
                        </label>
                        <div className="flex items-center gap-2">
                          <Input
                            type={
                              showApiKeys[selectedAgent.agent_type]
                                ? "text"
                                : "password"
                            }
                            value={selectedDraft.clineApiKey}
                            onChange={(event) => {
                              handleClineFieldChange(
                                "clineApiKey",
                                event.target.value
                              )
                            }}
                            placeholder="sk-..."
                          />
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              setShowApiKeys((prev) => ({
                                ...prev,
                                [selectedAgent.agent_type]:
                                  !prev[selectedAgent.agent_type],
                              }))
                            }}
                            title={
                              showApiKeys[selectedAgent.agent_type]
                                ? t("actions.hideApiKey")
                                : t("actions.showApiKey")
                            }
                          >
                            {showApiKeys[selectedAgent.agent_type] ? (
                              <EyeOff className="h-3.5 w-3.5" />
                            ) : (
                              <Eye className="h-3.5 w-3.5" />
                            )}
                          </Button>
                        </div>
                      </div>
                    )}

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        Model
                      </label>
                      <Input
                        value={selectedDraft.clineModel}
                        onChange={(event) => {
                          handleClineFieldChange(
                            "clineModel",
                            event.target.value
                          )
                        }}
                        placeholder="claude-sonnet-5-5"
                      />
                    </div>

                    {/* Endpoint, like the key, is cline's own for a sign-in
                        provider — its account service, not something to point
                        elsewhere. */}
                    {!isClineSignInProvider(selectedDraft.clineProvider) && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          API URL
                        </label>
                        {/* Cline stores this as `settings.baseUrl`, which its
                            own schema validates as a full URL — and an
                            OpenAI-compatible endpoint wants the version suffix,
                            so the placeholder shows the whole shape. */}
                        <Input
                          value={selectedDraft.clineBaseUrl}
                          onChange={(event) => {
                            handleClineFieldChange(
                              "clineBaseUrl",
                              event.target.value
                            )
                          }}
                          placeholder="https://api.openai.com/v1"
                        />
                      </div>
                    )}

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        {t("nativeJsonConfig")} (config)
                      </label>
                      <Textarea
                        value={selectedDraft.configText}
                        onChange={(event) => {
                          handleConfigTextChange(event.target.value)
                        }}
                        className="min-h-24 font-mono text-xs"
                        placeholder={`{
  "apiProvider": "anthropic",
  "apiKey": "sk-...",
  "model": "claude-sonnet-5-5"
}`}
                      />
                      {selectedConfigError && (
                        <div className="rounded-md border border-red-500/30 bg-red-500/5 px-2.5 py-1.5 text-2xs text-red-400">
                          {selectedConfigError}
                        </div>
                      )}
                    </div>

                    <div className="flex items-center justify-end gap-2">
                      <Button
                        size="sm"
                        onClick={() => {
                          persistConfig(
                            selectedAgent.agent_type,
                            selectedDraft.configText
                          )
                            .then(() => {
                              toast.success(t("toasts.clineSaved"), {
                                description: t("toasts.configSavedHint"),
                              })
                            })
                            .catch((err) => {
                              console.error(
                                "[Settings] save cline config failed:",
                                err
                              )
                              const message = toErrorMessage(err)
                              toast.error(t("toasts.saveClineFailed"), {
                                description: message,
                              })
                            })
                        }}
                        disabled={selectedIsSavingConfig}
                      >
                        {selectedIsSavingConfig ? (
                          <>
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            {t("actions.saving")}
                          </>
                        ) : (
                          <>
                            <Save className="h-3.5 w-3.5" />
                            {t("actions.saveClineConfig")}
                          </>
                        )}
                      </Button>
                    </div>
                  </div>
                ) : selectedAgent.agent_type === "open_claw" ? (
                  <div className="space-y-3 rounded-md border bg-muted/10 p-3">
                    <div>
                      <label className="text-xs font-medium">
                        {t("openClaw.gatewayConfig")}
                      </label>
                      <p className="mt-1 text-2xs text-muted-foreground">
                        {t("openClaw.gatewayDescription")}
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        Gateway URL
                      </label>
                      <Input
                        value={selectedDraft.openClawGatewayUrl}
                        onChange={(event) => {
                          handleOpenClawFieldChange(
                            "openClawGatewayUrl",
                            event.target.value
                          )
                        }}
                        placeholder="wss://gateway-host:18789"
                      />
                      <p className="text-2xs text-muted-foreground">
                        {t("openClaw.gatewayUrlHint")}
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        Gateway Token
                      </label>
                      <div className="flex items-center gap-2">
                        <Input
                          type={
                            showApiKeys[selectedAgent.agent_type]
                              ? "text"
                              : "password"
                          }
                          value={selectedDraft.openClawGatewayToken}
                          onChange={(event) => {
                            handleOpenClawFieldChange(
                              "openClawGatewayToken",
                              event.target.value
                            )
                          }}
                          placeholder={t("openClaw.gatewayTokenPlaceholder")}
                        />
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            setShowApiKeys((prev) => ({
                              ...prev,
                              [selectedAgent.agent_type]:
                                !prev[selectedAgent.agent_type],
                            }))
                          }}
                          title={
                            showApiKeys[selectedAgent.agent_type]
                              ? t("actions.hideToken")
                              : t("actions.showToken")
                          }
                        >
                          {showApiKeys[selectedAgent.agent_type] ? (
                            <EyeOff className="h-3.5 w-3.5" />
                          ) : (
                            <Eye className="h-3.5 w-3.5" />
                          )}
                        </Button>
                      </div>
                      <p className="text-2xs text-muted-foreground">
                        {t("openClaw.gatewayTokenHint")}
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        Session Key
                      </label>
                      <Input
                        value={selectedDraft.openClawSessionKey}
                        onChange={(event) => {
                          handleOpenClawFieldChange(
                            "openClawSessionKey",
                            event.target.value
                          )
                        }}
                        placeholder="agent:main:main"
                      />
                      <p className="text-2xs text-muted-foreground">
                        {t("openClaw.sessionKeyHint")}
                      </p>
                    </div>

                    <div className="flex items-center justify-end gap-2">
                      <Button
                        size="sm"
                        onClick={() => {
                          Promise.all([
                            persistEnv(
                              selectedAgent.agent_type,
                              selectedDraft.enabled,
                              selectedDraft.envText,
                              selectedDraft.modelProviderId
                            ),
                            persistConfig(
                              selectedAgent.agent_type,
                              selectedDraft.configText
                            ),
                          ])
                            .then(() => {
                              toast.success(t("toasts.openClawSaved"), {
                                description: t("toasts.configSavedHint"),
                              })
                            })
                            .catch((err) => {
                              console.error(
                                "[Settings] save openclaw config failed:",
                                err
                              )
                              const message = toErrorMessage(err)
                              toast.error(t("toasts.saveOpenClawFailed"), {
                                description: message,
                              })
                            })
                        }}
                        disabled={selectedIsSavingEnv || selectedIsSavingConfig}
                      >
                        {selectedIsSavingEnv || selectedIsSavingConfig ? (
                          <>
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            {t("actions.saving")}
                          </>
                        ) : (
                          <>
                            <Save className="h-3.5 w-3.5" />
                            {t("actions.saveOpenClawConfig")}
                          </>
                        )}
                      </Button>
                    </div>
                  </div>
                ) : selectedAgent.agent_type === "hermes" ? (
                  <div className="space-y-3 rounded-md border bg-muted/10 p-3">
                    <div>
                      <label className="text-xs font-medium">
                        {t("hermes.configManagement")}
                      </label>
                      <p className="mt-1 text-2xs text-muted-foreground">
                        {t("hermes.configDescription")}
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        {t("hermes.providerLabel")}
                      </label>
                      <Select
                        value={selectedDraft.hermesProvider}
                        onValueChange={(value) =>
                          handleHermesFieldChange("hermesProvider", value)
                        }
                        disabled={selectedIsSavingConfig}
                      >
                        <SelectTrigger className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent align="start">
                          {/* Preserve an existing config's provider in the list
                              even when it's outside the curated table, so the
                              dropdown shows the real value instead of going blank. */}
                          {selectedDraft.hermesProvider &&
                            !HERMES_PROVIDERS.some(
                              (p) => p.id === selectedDraft.hermesProvider
                            ) && (
                              <SelectItem value={selectedDraft.hermesProvider}>
                                {selectedDraft.hermesProvider}
                              </SelectItem>
                            )}
                          {(
                            [
                              ["apiKey", t("hermes.groupApiKey")],
                              ["oauth", t("hermes.groupOauth")],
                              ["aws", t("hermes.groupAws")],
                            ] as const
                          ).map(([kind, groupLabel]) => {
                            const items = HERMES_PROVIDERS.filter(
                              (p) => p.kind === kind
                            )
                            if (items.length === 0) return null
                            return (
                              <SelectGroup key={kind}>
                                <SelectLabel>{groupLabel}</SelectLabel>
                                {items.map((provider) => (
                                  <SelectItem
                                    key={provider.id}
                                    value={provider.id}
                                  >
                                    {provider.label}
                                  </SelectItem>
                                ))}
                              </SelectGroup>
                            )
                          })}
                        </SelectContent>
                      </Select>
                      <p className="text-2xs text-muted-foreground">
                        {t("hermes.providerHint")}
                      </p>
                    </div>

                    {selectedHermesProviderOption?.kind === "apiKey" && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          API Key
                        </label>
                        <div className="flex items-center gap-2">
                          <Input
                            type={
                              showApiKeys[selectedAgent.agent_type]
                                ? "text"
                                : "password"
                            }
                            value={selectedDraft.apiKey}
                            onChange={(event) =>
                              handleHermesFieldChange(
                                "apiKey",
                                event.target.value
                              )
                            }
                            placeholder="sk-..."
                            disabled={selectedIsSavingConfig}
                          />
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              setShowApiKeys((prev) => ({
                                ...prev,
                                [selectedAgent.agent_type]:
                                  !prev[selectedAgent.agent_type],
                              }))
                            }}
                            title={
                              showApiKeys[selectedAgent.agent_type]
                                ? t("actions.hideApiKey")
                                : t("actions.showApiKey")
                            }
                          >
                            {showApiKeys[selectedAgent.agent_type] ? (
                              <EyeOff className="h-3.5 w-3.5" />
                            ) : (
                              <Eye className="h-3.5 w-3.5" />
                            )}
                          </Button>
                        </div>
                        <p className="text-2xs text-muted-foreground">
                          {t("hermes.apiKeyHint")}
                        </p>
                      </div>
                    )}

                    {selectedHermesProviderOption?.needsBaseUrl && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          API URL
                        </label>
                        <Input
                          value={selectedDraft.apiBaseUrl}
                          onChange={(event) =>
                            handleHermesFieldChange(
                              "apiBaseUrl",
                              event.target.value
                            )
                          }
                          placeholder="https://api.example.com/v1"
                          disabled={selectedIsSavingConfig}
                        />
                      </div>
                    )}

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        {t("hermes.modelName")}
                      </label>
                      <Input
                        value={selectedDraft.model}
                        onChange={(event) =>
                          handleHermesFieldChange("model", event.target.value)
                        }
                        placeholder="moonshotai/kimi-k2"
                        disabled={selectedIsSavingConfig}
                      />
                    </div>

                    {selectedHermesProviderOption?.kind === "oauth" && (
                      <p className="text-2xs text-muted-foreground">
                        {t("hermes.oauthHint")}
                      </p>
                    )}

                    {selectedHermesProviderOption?.kind === "aws" && (
                      <p className="text-2xs text-muted-foreground">
                        {t("hermes.awsHint")}
                      </p>
                    )}

                    {!selectedHermesProviderOption && (
                      <p className="text-2xs text-amber-600 dark:text-amber-500">
                        {t("hermes.unsupportedProvider")}
                      </p>
                    )}

                    <div className="flex justify-end">
                      <Button
                        size="sm"
                        onClick={() => handleSaveHermesConfig("structured")}
                        disabled={
                          selectedIsSavingConfig ||
                          !selectedHermesProviderOption
                        }
                      >
                        {selectedIsSavingConfig ? (
                          <>
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            {t("actions.saving")}
                          </>
                        ) : (
                          <>
                            <Save className="h-3.5 w-3.5" />
                            {t("actions.saveHermesConfig")}
                          </>
                        )}
                      </Button>
                    </div>

                    <div className="space-y-2 rounded-md border p-3">
                      <div>
                        <label className="text-2xs font-medium">
                          {t("hermes.setupTitle")}
                        </label>
                        <p className="mt-1 text-2xs text-muted-foreground">
                          {t("hermes.setupHint")}
                        </p>
                      </div>
                      {hermesCanUseNativeSetup && (
                        <div className="flex flex-wrap items-center gap-2">
                          <Button
                            size="sm"
                            variant="outline"
                            onClick={() =>
                              runHermesSetupCommand(
                                "setup",
                                selectedDraft.hermesSetupCommand
                              )
                            }
                          >
                            <Wrench className="h-3.5 w-3.5" />
                            {t("hermes.runSetup")}
                          </Button>
                          <Button
                            size="sm"
                            variant="outline"
                            onClick={() =>
                              runHermesSetupCommand(
                                "model",
                                selectedDraft.hermesModelCommand
                              )
                            }
                          >
                            {t("hermes.configureModel")}
                          </Button>
                          <Button
                            size="sm"
                            variant="outline"
                            onClick={handleRevealHermesHome}
                          >
                            {t("hermes.openConfigFolder")}
                          </Button>
                        </div>
                      )}
                      {selectedDraft.hermesSetupCommand && (
                        <div className="flex items-center gap-2">
                          <code className="flex-1 overflow-x-auto rounded bg-muted px-2 py-1 text-2xs font-mono whitespace-nowrap">
                            {selectedDraft.hermesSetupCommand}
                          </code>
                          <Button
                            size="sm"
                            variant="ghost"
                            className="h-7 w-7 shrink-0 p-0"
                            onClick={async () => {
                              const ok = await copyTextToClipboard(
                                selectedDraft.hermesSetupCommand
                              )
                              if (ok) {
                                toast.success(t("hermes.commandCopied"))
                              }
                            }}
                            title={t("hermes.copyCommand")}
                          >
                            <Copy className="h-3 w-3" />
                          </Button>
                        </div>
                      )}
                    </div>

                    <details className="rounded-md border p-3">
                      <summary className="cursor-pointer text-2xs font-medium text-muted-foreground">
                        {t("hermes.advancedTitle")}
                      </summary>
                      <div className="mt-2 space-y-2">
                        <p className="text-2xs text-muted-foreground">
                          {t("hermes.rawConfigHint")}
                        </p>
                        <Textarea
                          value={selectedDraft.hermesConfigYaml}
                          onChange={(event) =>
                            handleHermesFieldChange(
                              "hermesConfigYaml",
                              event.target.value
                            )
                          }
                          placeholder={`model:\n  provider: openrouter\n  default: moonshotai/kimi-k2`}
                          className="min-h-40 max-h-80 font-mono text-xs"
                          disabled={selectedIsSavingConfig}
                        />
                        <div className="flex justify-end">
                          <Button
                            size="sm"
                            variant="outline"
                            onClick={() => handleSaveHermesConfig("raw")}
                            disabled={selectedIsSavingConfig}
                          >
                            {selectedIsSavingConfig ? (
                              <>
                                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                                {t("actions.saving")}
                              </>
                            ) : (
                              <>
                                <Save className="h-3.5 w-3.5" />
                                {t("hermes.saveRawConfig")}
                              </>
                            )}
                          </Button>
                        </div>
                      </div>
                    </details>
                  </div>
                ) : selectedAgent.agent_type === "code_buddy" ? (
                  <CodeBuddyConfigPanel
                    agent={selectedAgent}
                    saving={Boolean(savingEnv[selectedAgent.agent_type])}
                    onSave={(env, enabled) =>
                      persistEnv(
                        selectedAgent.agent_type,
                        enabled,
                        envMapToText(env),
                        selectedAgent.model_provider_id
                      )
                    }
                  />
                ) : selectedAgent.agent_type === "kimi_code" ? (
                  <KimiCodeConfigPanel
                    agent={selectedAgent}
                    onSaved={refreshAgents}
                  />
                ) : selectedAgent.agent_type === "pi" ? (
                  <PiConfigPanel
                    agent={selectedAgent}
                    saving={Boolean(savingEnv[selectedAgent.agent_type])}
                    onSaveEnv={(env, enabled) =>
                      persistEnv(
                        selectedAgent.agent_type,
                        enabled,
                        envMapToText(env),
                        selectedAgent.model_provider_id
                      )
                    }
                    onSaved={refreshAgents}
                  />
                ) : selectedAgent.agent_type === "cursor" ? (
                  <CursorConfigPanel
                    agent={selectedAgent}
                    saving={Boolean(savingEnv[selectedAgent.agent_type])}
                    onSaveEnv={(env, enabled) =>
                      persistEnv(
                        selectedAgent.agent_type,
                        enabled,
                        envMapToText(env),
                        selectedAgent.model_provider_id
                      )
                    }
                    onSaved={refreshAgents}
                    onAffectedSessions={reportAffectedSessions}
                  />
                ) : selectedAgent.agent_type === "deepseek" ? (
                  <>
                    <DeepSeekConfigPanel
                      agent={selectedAgent}
                      saving={Boolean(savingEnv[selectedAgent.agent_type])}
                      onSaveEnv={(env, enabled) =>
                        persistEnv(
                          selectedAgent.agent_type,
                          enabled,
                          envMapToText(env),
                          selectedAgent.model_provider_id,
                          // The keys this panel owns, folded into the raw
                          // editor's draft (which the enable switch persists
                          // wholesale) so the two can never disagree.
                          // `DEEPSEEK_ACP_MODEL` is NOT one of them — the raw
                          // editor owns it, and folding it in would overwrite a
                          // model line being typed there.
                          {
                            DEEPSEEK_API_KEY: env.DEEPSEEK_API_KEY,
                            DEEPSEEK_BASE_URL: env.DEEPSEEK_BASE_URL,
                            DEEPSEEK_ACP_PROVIDER: env.DEEPSEEK_ACP_PROVIDER,
                          }
                        )
                      }
                    />
                    {/* The deployment's model catalog. It lives in the harness'
                      own `settings.yaml`, not in the agent env, so it has its
                      own section and its own save — and it is what the
                      composer's per-session model selector gets to choose
                      from. */}
                    <DeepSeekModelListEditor
                      launchModel={selectedAgent.env.DEEPSEEK_ACP_MODEL}
                    />
                  </>
                ) : selectedAgent.agent_type === "qoder" ? (
                  <QoderConfigPanel
                    agent={selectedAgent}
                    saving={Boolean(savingEnv[selectedAgent.agent_type])}
                    onSaveEnv={(env, enabled) =>
                      persistEnv(
                        selectedAgent.agent_type,
                        enabled,
                        envMapToText(env),
                        selectedAgent.model_provider_id,
                        // The one key this panel owns, folded into the raw
                        // editor's draft. That draft is persisted WHOLESALE by
                        // the enable switch and the generic env Save button, so
                        // without this a saved token would be silently deleted
                        // the moment either one fires. `undefined` (the token
                        // field was cleared) deletes the line, which is the
                        // outcome clearing it asks for.
                        {
                          QODER_PERSONAL_ACCESS_TOKEN:
                            env.QODER_PERSONAL_ACCESS_TOKEN,
                        }
                      )
                    }
                    onSaved={refreshAgents}
                    onAffectedSessions={reportAffectedSessions}
                  />
                ) : selectedAgent.agent_type === "antigravity" ? (
                  <AntigravityConfigPanel
                    agent={selectedAgent}
                    saving={Boolean(savingEnv[selectedAgent.agent_type])}
                    onSaveEnv={(env, enabled) =>
                      persistEnv(
                        selectedAgent.agent_type,
                        enabled,
                        envMapToText(env),
                        selectedAgent.model_provider_id,
                        // The keys this panel owns, folded into the raw
                        // editor's draft. That draft is persisted WHOLESALE by
                        // the enable switch and the generic env Save button, so
                        // without this a saved auth method would be silently
                        // deleted the moment either one fires. `undefined` (the
                        // method does not use that credential) deletes the
                        // line, which is exactly what switching methods asks
                        // for.
                        Object.fromEntries(
                          ANTIGRAVITY_ENV_KEYS.map((key) => [key, env[key]])
                        )
                      )
                    }
                    onSaved={refreshAgents}
                  />
                ) : selectedAgent.agent_type === "grok" ? (
                  <div className="space-y-3 rounded-md border bg-muted/10 p-3">
                    <div>
                      <label className="text-xs font-medium">
                        {t("configManagement")}
                      </label>
                      <p className="mt-1 text-2xs text-muted-foreground">
                        {t("grok.configDescription")}
                      </p>
                    </div>

                    {/* Structured controls — mode + reasoning effort */}
                    <div className="grid gap-3 md:grid-cols-2">
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          {t("grok.permissionModeLabel")}
                        </label>
                        <Select
                          value={selectedDraft.grokPermissionMode || GROK_UNSET}
                          disabled={grokSaving}
                          onValueChange={(value) =>
                            updateSelectedDraft((current) => ({
                              ...current,
                              grokPermissionMode:
                                value === GROK_UNSET ? "" : value,
                            }))
                          }
                        >
                          <SelectTrigger
                            className="w-full"
                            aria-label={t("grok.permissionModeLabel")}
                          >
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            <SelectItem value={GROK_UNSET}>
                              {t("grok.optionDefault")}
                            </SelectItem>
                            <SelectItem value="default">
                              {t("grok.permissionDefault")}
                            </SelectItem>
                            <SelectItem value="acceptEdits">
                              {t("grok.permissionAcceptEdits")}
                            </SelectItem>
                            <SelectItem value="auto">
                              {t("grok.permissionAuto")}
                            </SelectItem>
                            <SelectItem value="bypassPermissions">
                              {t("grok.permissionAlwaysApprove")}
                            </SelectItem>
                          </SelectContent>
                        </Select>
                      </div>

                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          {t("grok.reasoningEffortLabel")}
                        </label>
                        <Select
                          value={
                            selectedDraft.grokReasoningEffort || GROK_UNSET
                          }
                          disabled={grokSaving}
                          onValueChange={(value) =>
                            updateSelectedDraft((current) => ({
                              ...current,
                              grokReasoningEffort:
                                value === GROK_UNSET ? "" : value,
                            }))
                          }
                        >
                          <SelectTrigger
                            className="w-full"
                            aria-label={t("grok.reasoningEffortLabel")}
                          >
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            <SelectItem value={GROK_UNSET}>
                              {t("grok.optionDefault")}
                            </SelectItem>
                            <SelectItem value="low">
                              {t("grok.effortLow")}
                            </SelectItem>
                            <SelectItem value="medium">
                              {t("grok.effortMedium")}
                            </SelectItem>
                            <SelectItem value="high">
                              {t("grok.effortHigh")}
                            </SelectItem>
                            <SelectItem value="xhigh">
                              {t("grok.effortXhigh")}
                            </SelectItem>
                          </SelectContent>
                        </Select>
                      </div>
                    </div>

                    {/* Authentication — method selector + method-specific body.
                        Mirrors the Cursor panel: an explicit choice between the
                        `grok login` subscription and an XAI_API_KEY, recognized
                        on load via inferGrokMode and recorded as GROK_AUTH_MODE. */}
                    <div className="space-y-2.5 rounded-md border p-2.5">
                      <div className="space-y-1.5">
                        <label className="text-2xs font-medium">
                          {t("grok.authTitle")}
                        </label>
                        <Select
                          value={selectedDraft.grokAuthMode}
                          disabled={grokSaving}
                          onValueChange={(value) =>
                            handleGrokAuthModeChange(value as GrokAuthMethod)
                          }
                        >
                          <SelectTrigger
                            className="w-full"
                            aria-label={t("grok.authMode")}
                          >
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            <SelectItem value="subscription">
                              {t("authModeOfficialSubscription")}
                            </SelectItem>
                            <SelectItem value="api_key">
                              {t("grok.authModeApiKey")}
                            </SelectItem>
                            <SelectItem value="custom">
                              {t("grok.authModeCustom")}
                            </SelectItem>
                          </SelectContent>
                        </Select>
                        <p className="text-2xs text-muted-foreground">
                          {selectedDraft.grokAuthMode === "subscription"
                            ? t("grok.subscriptionHint")
                            : selectedDraft.grokAuthMode === "custom"
                              ? t("grok.authModeCustomHint")
                              : t("grok.authModeApiKeyHint")}
                        </p>
                      </div>

                      {selectedDraft.grokAuthMode === "subscription" ? (
                        // Subscription: a copyable `grok login` command. Its
                        // session lives in ~/.grok/auth.json (untouched here); the
                        // launch path strips any inherited XAI_API_KEY.
                        <div className="space-y-1.5">
                          <p className="text-2xs text-muted-foreground">
                            {t("grok.loginHint")}
                          </p>
                          <div className="flex items-center gap-2">
                            <code className="flex-1 overflow-x-auto rounded bg-muted px-2 py-1 text-2xs font-mono whitespace-nowrap">
                              {GROK_LOGIN_COMMAND}
                            </code>
                            <Button
                              type="button"
                              variant="ghost"
                              size="sm"
                              className="h-7 w-7 shrink-0 p-0"
                              onClick={async () => {
                                const ok =
                                  await copyTextToClipboard(GROK_LOGIN_COMMAND)
                                if (ok) toast.success(t("grok.commandCopied"))
                              }}
                              title={t("grok.copyCommand")}
                              aria-label={t("grok.copyCommand")}
                            >
                              <Copy className="h-3 w-3" />
                            </Button>
                          </div>
                        </div>
                      ) : selectedDraft.grokAuthMode === "api_key" ? (
                        // API key: the non-interactive XAI_API_KEY credential.
                        <div className="space-y-1.5">
                          <label className="text-2xs text-muted-foreground">
                            XAI_API_KEY
                          </label>
                          <div className="flex items-center gap-2">
                            <Input
                              type={
                                showApiKeys[selectedAgent.agent_type]
                                  ? "text"
                                  : "password"
                              }
                              value={selectedDraft.apiKey}
                              onChange={(event) =>
                                handleImportantConfigChange(
                                  "apiKey",
                                  event.target.value
                                )
                              }
                              placeholder="xai-..."
                              aria-label="XAI_API_KEY"
                              name="grok-xai-api-key"
                              autoComplete="off"
                              spellCheck={false}
                              disabled={grokSaving}
                            />
                            <Button
                              type="button"
                              variant="outline"
                              size="sm"
                              disabled={grokSaving}
                              onClick={() =>
                                setShowApiKeys((prev) => ({
                                  ...prev,
                                  [selectedAgent.agent_type]:
                                    !prev[selectedAgent.agent_type],
                                }))
                              }
                              aria-label={
                                showApiKeys[selectedAgent.agent_type]
                                  ? t("actions.hideApiKey")
                                  : t("actions.showApiKey")
                              }
                              title={
                                showApiKeys[selectedAgent.agent_type]
                                  ? t("actions.hideApiKey")
                                  : t("actions.showApiKey")
                              }
                            >
                              {showApiKeys[selectedAgent.agent_type] ? (
                                <EyeOff className="h-3.5 w-3.5" />
                              ) : (
                                <Eye className="h-3.5 w-3.5" />
                              )}
                            </Button>
                          </div>
                          <p className="text-2xs text-muted-foreground">
                            {selectedDraft.apiKey.trim()
                              ? t("grok.authKeyConfigured")
                              : t("grok.authKeyMissing")}
                          </p>
                        </div>
                      ) : null}
                    </div>

                    {/* Custom model (BYO endpoint) → [model.<id>] + [models].default.
                        Only shown (and saved) in the `custom` auth method: a model
                        id registers a custom Grok model as the default; the other
                        methods omit the codeg-managed block. */}
                    {selectedDraft.grokAuthMode === "custom" ? (
                      <div className="space-y-2.5 rounded-md border p-2.5">
                        <div>
                          <label className="text-2xs font-medium">
                            {t("grok.customModelTitle")}
                          </label>
                          <p className="mt-1 text-2xs text-muted-foreground">
                            {t("grok.customModelHint")}
                          </p>
                        </div>

                        <div className="space-y-1.5">
                          <label className="text-2xs text-muted-foreground">
                            {t("grok.customModelIdLabel")}
                          </label>
                          <Input
                            value={selectedDraft.grokCustomModelId}
                            onChange={(event) =>
                              updateSelectedDraft((current) => ({
                                ...current,
                                grokCustomModelId: event.target.value,
                              }))
                            }
                            placeholder={t("grok.customModelIdPlaceholder")}
                            aria-label={t("grok.customModelIdLabel")}
                            autoComplete="off"
                            spellCheck={false}
                            disabled={grokSaving}
                          />
                          <p className="text-2xs text-muted-foreground">
                            {t("grok.customModelIdHint")}
                          </p>
                        </div>

                        <div className="grid gap-3 md:grid-cols-2">
                          <div className="space-y-1.5">
                            <label className="text-2xs text-muted-foreground">
                              {t("grok.customBaseUrlLabel")}
                            </label>
                            <Input
                              value={selectedDraft.grokCustomBaseUrl}
                              onChange={(event) =>
                                updateSelectedDraft((current) => ({
                                  ...current,
                                  grokCustomBaseUrl: event.target.value,
                                }))
                              }
                              placeholder={t("grok.customBaseUrlPlaceholder")}
                              aria-label={t("grok.customBaseUrlLabel")}
                              autoComplete="off"
                              spellCheck={false}
                              disabled={grokSaving}
                            />
                          </div>
                          <div className="space-y-1.5">
                            <label className="text-2xs text-muted-foreground">
                              {t("grok.customApiBackendLabel")}
                            </label>
                            <Select
                              value={
                                selectedDraft.grokCustomApiBackend ||
                                GROK_DEFAULT_API_BACKEND
                              }
                              disabled={grokSaving}
                              onValueChange={(value) =>
                                updateSelectedDraft((current) => ({
                                  ...current,
                                  grokCustomApiBackend: value,
                                }))
                              }
                            >
                              <SelectTrigger
                                className="w-full"
                                aria-label={t("grok.customApiBackendLabel")}
                              >
                                <SelectValue />
                              </SelectTrigger>
                              <SelectContent>
                                <SelectItem value="responses">
                                  {t("grok.backendResponses")}
                                </SelectItem>
                                <SelectItem value="chat_completions">
                                  {t("grok.backendChatCompletions")}
                                </SelectItem>
                                <SelectItem value="messages">
                                  {t("grok.backendMessages")}
                                </SelectItem>
                              </SelectContent>
                            </Select>
                          </div>
                        </div>

                        <div className="space-y-1.5">
                          <label className="text-2xs text-muted-foreground">
                            {t("grok.customApiKeyLabel")}
                          </label>
                          <div className="flex items-center gap-2">
                            <Input
                              type={showGrokCustomKey ? "text" : "password"}
                              value={selectedDraft.grokCustomApiKey}
                              onChange={(event) =>
                                updateSelectedDraft((current) => ({
                                  ...current,
                                  grokCustomApiKey: event.target.value,
                                }))
                              }
                              placeholder="xai-..."
                              aria-label={t("grok.customApiKeyLabel")}
                              name="grok-custom-api-key"
                              autoComplete="off"
                              spellCheck={false}
                              disabled={grokSaving}
                            />
                            <Button
                              type="button"
                              variant="outline"
                              size="sm"
                              disabled={grokSaving}
                              onClick={() =>
                                setShowGrokCustomKey((prev) => !prev)
                              }
                              aria-label={
                                showGrokCustomKey
                                  ? t("actions.hideApiKey")
                                  : t("actions.showApiKey")
                              }
                              title={
                                showGrokCustomKey
                                  ? t("actions.hideApiKey")
                                  : t("actions.showApiKey")
                              }
                            >
                              {showGrokCustomKey ? (
                                <EyeOff className="h-3.5 w-3.5" />
                              ) : (
                                <Eye className="h-3.5 w-3.5" />
                              )}
                            </Button>
                          </div>
                          <p className="text-2xs text-muted-foreground">
                            {t("grok.customApiKeyHint")}
                          </p>
                        </div>

                        <div className="space-y-1.5">
                          <label className="text-2xs text-muted-foreground">
                            {t("grok.customContextWindowLabel")}
                          </label>
                          <Input
                            type="number"
                            inputMode="numeric"
                            value={selectedDraft.grokCustomContextWindow}
                            onChange={(event) =>
                              updateSelectedDraft((current) => ({
                                ...current,
                                grokCustomContextWindow: event.target.value,
                              }))
                            }
                            placeholder="500000"
                            aria-label={t("grok.customContextWindowLabel")}
                            disabled={grokSaving}
                          />
                          <p className="text-2xs text-muted-foreground">
                            {t("grok.customContextWindowHint")}
                          </p>
                        </div>
                      </div>
                    ) : null}

                    {/* Compaction — session-global auto-compact threshold. */}
                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        {t("grok.autoCompactLabel")}
                      </label>
                      <Input
                        type="number"
                        inputMode="numeric"
                        min={0}
                        max={100}
                        value={selectedDraft.grokAutoCompactThreshold}
                        onChange={(event) =>
                          updateSelectedDraft((current) => ({
                            ...current,
                            grokAutoCompactThreshold: event.target.value,
                          }))
                        }
                        placeholder="85"
                        aria-label={t("grok.autoCompactLabel")}
                        disabled={grokSaving}
                      />
                      <p className="text-2xs text-muted-foreground">
                        {t("grok.autoCompactHint")}
                      </p>
                    </div>

                    {/* Advanced escape hatch — config.toml keys other than the
                        controls above. Saved together with those controls by the
                        single Save below (the structured settings merge onto this
                        text, format-preservingly, when it was edited). */}
                    <Collapsible
                      open={grokAdvancedOpen}
                      onOpenChange={setGrokAdvancedOpen}
                    >
                      <CollapsibleTrigger asChild>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="h-7 gap-1 px-1 text-2xs text-muted-foreground"
                        >
                          <ChevronRight
                            className={cn(
                              "h-3.5 w-3.5 transition-transform",
                              grokAdvancedOpen && "rotate-90"
                            )}
                          />
                          {t("grok.advancedToggle")}
                        </Button>
                      </CollapsibleTrigger>
                      <CollapsibleContent className="space-y-1.5 pt-2">
                        <label className="text-2xs text-muted-foreground">
                          {t("grok.configTomlNative")}
                        </label>
                        <Textarea
                          value={selectedDraft.grokConfigTomlText}
                          onChange={(event) => {
                            const nextText = event.target.value
                            updateSelectedDraft((current) => ({
                              ...current,
                              grokConfigTomlText: nextText,
                            }))
                          }}
                          placeholder={t("grok.configTomlPlaceholder")}
                          className="min-h-40 max-h-80 font-mono text-xs"
                          spellCheck={false}
                          aria-label={t("grok.configTomlNative")}
                          disabled={grokSaving}
                        />
                        <p className="text-2xs text-muted-foreground">
                          {t("grok.configTomlHint")}
                        </p>
                      </CollapsibleContent>
                    </Collapsible>

                    {/* A single Save persists every surface together: the
                        structured controls merge (format-preserving) onto the raw
                        text when it was edited, else onto the current on-disk file,
                        then XAI_API_KEY is written. One action → no independent save
                        that could drop the other surface's unsaved edits. */}
                    <div className="flex justify-end">
                      <Button
                        size="sm"
                        onClick={async () => {
                          setGrokSaving(true)
                          try {
                            // Config first, so a malformed raw edit fails before
                            // the API key is touched. buildGrokSaveOptions sends
                            // the raw text only when the user actually edited it
                            // (else the merge runs against fresh on-disk config).
                            await persistConfig(
                              selectedAgent.agent_type,
                              selectedDraft.configText,
                              buildGrokSaveOptions(
                                selectedDraft,
                                selectedAgent.grok_config_toml
                              )
                            )
                            // Independent second write (the API key lives in env).
                            // If it fails after config committed, report that
                            // partial outcome honestly rather than a blanket fail.
                            try {
                              await persistEnv(
                                selectedAgent.agent_type,
                                selectedDraft.enabled,
                                selectedDraft.envText,
                                selectedDraft.modelProviderId
                              )
                            } catch (envErr) {
                              console.error(
                                "[Settings] save grok api key failed:",
                                envErr
                              )
                              await reseedGrokDraft()
                              toast.error(t("toasts.saveGrokApiKeyFailed"), {
                                description: toErrorMessage(envErr),
                              })
                              return
                            }
                            await reseedGrokDraft()
                            toast.success(t("toasts.grokSaved"), {
                              description: t("toasts.configSavedHint"),
                            })
                          } catch (err) {
                            console.error(
                              "[Settings] save grok settings failed:",
                              err
                            )
                            toast.error(t("toasts.saveGrokNativeFailed"), {
                              description: toErrorMessage(err),
                            })
                          } finally {
                            setGrokSaving(false)
                          }
                        }}
                        disabled={
                          grokSaving ||
                          selectedIsSavingConfig ||
                          selectedIsSavingEnv
                        }
                      >
                        {grokSaving ? (
                          <>
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            {t("actions.saving")}
                          </>
                        ) : (
                          <>
                            <Save className="h-3.5 w-3.5" />
                            {t("actions.saveGrokConfig")}
                          </>
                        )}
                      </Button>
                    </div>
                  </div>
                ) : isCustomAgentType(selectedAgent.agent_type) ? (
                  // A custom agent is driven purely by the ACP protocol: codeg
                  // knows nothing about its config file layout or auth model,
                  // so the generic "config management" editor below would be
                  // offering to write a file that may not exist in a format it
                  // cannot know. Environment variables (above) are the one
                  // channel that works for every agent, so they are the whole
                  // surface — plus the skills declaration and removing the
                  // agent.
                  // All four blocks share the settings-card vocabulary
                  // (`SettingCard` / `SettingRow`, as in the task settings
                  // dialog) so the panel reads as one stack of settings rather
                  // than four differently-shaped boxes.
                  <>
                    <SettingCard>
                      <SettingRow
                        icon={Pencil}
                        title={t("customAgentEdit")}
                        description={t("customAgentEditHint")}
                        control={
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() =>
                              setEditCustomAgentId(
                                customAgentId(selectedAgent.agent_type)
                              )
                            }
                          >
                            <Pencil className="h-3.5 w-3.5" />
                            {t("customAgentEdit")}
                          </Button>
                        }
                      />
                    </SettingCard>
                    <CustomAgentSkillsToggle
                      registryId={customAgentId(selectedAgent.agent_type) ?? ""}
                    />
                    <CustomAgentMcpToggle
                      registryId={customAgentId(selectedAgent.agent_type) ?? ""}
                    />
                    {/* The one destructive action keeps its own tinting — the
                        card shape is shared, the color is the warning. */}
                    <SettingCard className="border-destructive/30 bg-destructive/5">
                      <SettingRow
                        icon={Trash2}
                        title={
                          <span className="text-destructive">
                            {t("customAgentRemove")}
                          </span>
                        }
                        description={t("customAgentRemoveHint")}
                        control={
                          <Button
                            variant="outline"
                            size="sm"
                            className="text-destructive hover:text-destructive"
                            disabled={removingCustomAgent}
                            onClick={() => setRemoveConfirmAgent(selectedAgent)}
                          >
                            {removingCustomAgent ? (
                              <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            ) : (
                              <Trash2 className="h-3.5 w-3.5" />
                            )}
                            {t("customAgentRemove")}
                          </Button>
                        }
                      />
                    </SettingCard>
                  </>
                ) : (
                  <div className="space-y-3 rounded-md border bg-muted/10 p-3">
                    <div>
                      <label className="text-xs font-medium">
                        {t("configManagement")}
                      </label>
                      <p className="mt-1 text-2xs text-muted-foreground">
                        {selectedAgent.agent_type === "claude_code"
                          ? t("generalConfigDescriptionClaude")
                          : t("generalConfigDescriptionDefault")}
                      </p>
                    </div>

                    {selectedAgent.agent_type === "claude_code" && (
                      <div className="space-y-1.5">
                        <label className="text-2xs text-muted-foreground">
                          {t("claude.authMode")}
                        </label>
                        <Select
                          value={selectedDraft.claudeAuthMode}
                          onValueChange={(value) => {
                            if (
                              CLAUDE_AUTH_MODES.includes(
                                value as ClaudeAuthMode
                              )
                            ) {
                              handleClaudeAuthModeChange(
                                value as ClaudeAuthMode
                              )
                            }
                          }}
                        >
                          <SelectTrigger className="w-full">
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent align="start">
                            <SelectItem value="official_subscription">
                              {t("authModeOfficialSubscription")}
                            </SelectItem>
                            <SelectItem value="custom">
                              {t("authModeCustomEndpoint")}
                            </SelectItem>
                            {/* [crewchat] "model_provider" hidden — models are
                                chosen on the Agents page, providers live in
                                CCR. Upstream option left unreachable. */}
                          </SelectContent>
                        </Select>
                        <p className="text-2xs text-muted-foreground">
                          {selectedDraft.claudeAuthMode ===
                          "official_subscription"
                            ? t("claude.officialSubscriptionHint")
                            : selectedDraft.claudeAuthMode === "custom"
                              ? t("authModeCustomEndpointHint")
                              : t("modelProviderHint")}
                        </p>
                      </div>
                    )}

                    {selectedAgent.agent_type === "claude_code" &&
                      selectedDraft.claudeAuthMode === "model_provider" && (
                        <div className="space-y-1.5">
                          <label className="text-2xs text-muted-foreground">
                            {t("selectModelProvider")}
                          </label>
                          {selectedModelProviders.length > 0 ? (
                            <Select
                              value={
                                selectedDraft.modelProviderId != null
                                  ? String(selectedDraft.modelProviderId)
                                  : ""
                              }
                              onValueChange={handleModelProviderSelect}
                            >
                              <SelectTrigger className="w-full">
                                <SelectValue
                                  placeholder={t("selectModelProvider")}
                                />
                              </SelectTrigger>
                              <SelectContent align="start">
                                {selectedModelProviders.map((provider) => (
                                  <SelectItem
                                    key={provider.id}
                                    value={String(provider.id)}
                                  >
                                    {provider.name}
                                  </SelectItem>
                                ))}
                              </SelectContent>
                            </Select>
                          ) : (
                            <p className="text-2xs text-muted-foreground">
                              {t("noModelProviderAvailable")}
                            </p>
                          )}
                        </div>
                      )}

                    {(selectedAgent.agent_type !== "claude_code" ||
                      selectedDraft.claudeAuthMode === "custom" ||
                      selectedDraft.claudeAuthMode === "model_provider") && (
                      <>
                        {importantFieldsFor(selectedAgent.agent_type)
                          .apiBaseUrl && (
                          <div className="space-y-1.5">
                            <label className="text-2xs text-muted-foreground">
                              API URL
                            </label>
                            <Input
                              value={selectedDraft.apiBaseUrl}
                              readOnly={
                                selectedAgent.agent_type === "claude_code" &&
                                selectedDraft.claudeAuthMode ===
                                  "model_provider"
                              }
                              onChange={(event) => {
                                handleImportantConfigChange(
                                  "apiBaseUrl",
                                  event.target.value
                                )
                              }}
                              placeholder="https://api.example.com"
                            />
                          </div>
                        )}

                        {importantFieldsFor(selectedAgent.agent_type)
                          .apiKey && (
                          <div className="space-y-1.5">
                            <label className="text-2xs text-muted-foreground">
                              API Key
                            </label>
                            <div className="flex items-center gap-2">
                              <Input
                                type={
                                  showApiKeys[selectedAgent.agent_type]
                                    ? "text"
                                    : "password"
                                }
                                value={selectedDraft.apiKey}
                                readOnly={
                                  selectedAgent.agent_type === "claude_code" &&
                                  selectedDraft.claudeAuthMode ===
                                    "model_provider"
                                }
                                onChange={(event) => {
                                  handleImportantConfigChange(
                                    "apiKey",
                                    event.target.value
                                  )
                                }}
                                placeholder="sk-..."
                              />
                              <Button
                                type="button"
                                variant="outline"
                                size="sm"
                                onClick={() => {
                                  setShowApiKeys((prev) => ({
                                    ...prev,
                                    [selectedAgent.agent_type]:
                                      !prev[selectedAgent.agent_type],
                                  }))
                                }}
                                title={
                                  showApiKeys[selectedAgent.agent_type]
                                    ? t("actions.hideApiKey")
                                    : t("actions.showApiKey")
                                }
                              >
                                {showApiKeys[selectedAgent.agent_type] ? (
                                  <EyeOff className="h-3.5 w-3.5" />
                                ) : (
                                  <Eye className="h-3.5 w-3.5" />
                                )}
                              </Button>
                            </div>
                          </div>
                        )}
                      </>
                    )}

                    {selectedAgent.agent_type === "claude_code" ? (
                      <div className="space-y-2">
                        <div className="grid gap-3 md:grid-cols-2">
                          <div className="space-y-1.5">
                            <label className="text-2xs text-muted-foreground">
                              {t("claude.mainModel")}
                            </label>
                            <Input
                              value={selectedDraft.claudeMainModel}
                              readOnly={
                                selectedDraft.claudeAuthMode ===
                                "model_provider"
                              }
                              onChange={(event) => {
                                handleImportantConfigChange(
                                  "claudeMainModel",
                                  event.target.value
                                )
                              }}
                              placeholder="claude-sonnet-5-5"
                            />
                          </div>
                          <div className="space-y-1.5">
                            <label className="text-2xs text-muted-foreground">
                              {t("claude.reasoningModel")}
                            </label>
                            <Input
                              value={selectedDraft.claudeReasoningModel}
                              readOnly={
                                selectedDraft.claudeAuthMode ===
                                "model_provider"
                              }
                              onChange={(event) => {
                                handleImportantConfigChange(
                                  "claudeReasoningModel",
                                  event.target.value
                                )
                              }}
                              placeholder="claude-opus-5-5"
                            />
                          </div>
                          <div className="space-y-1.5">
                            <label className="text-2xs text-muted-foreground">
                              {t("claude.haikuDefaultModel")}
                            </label>
                            <Input
                              value={selectedDraft.claudeDefaultHaikuModel}
                              readOnly={
                                selectedDraft.claudeAuthMode ===
                                "model_provider"
                              }
                              onChange={(event) => {
                                handleImportantConfigChange(
                                  "claudeDefaultHaikuModel",
                                  event.target.value
                                )
                              }}
                              placeholder="claude-haiku-5-5"
                            />
                          </div>
                          <div className="space-y-1.5">
                            <label className="text-2xs text-muted-foreground">
                              {t("claude.sonnetDefaultModel")}
                            </label>
                            <Input
                              value={selectedDraft.claudeDefaultSonnetModel}
                              readOnly={
                                selectedDraft.claudeAuthMode ===
                                "model_provider"
                              }
                              onChange={(event) => {
                                handleImportantConfigChange(
                                  "claudeDefaultSonnetModel",
                                  event.target.value
                                )
                              }}
                              placeholder="claude-sonnet-5-5"
                            />
                          </div>
                          <div className="space-y-1.5">
                            <label className="text-2xs text-muted-foreground">
                              {t("claude.opusDefaultModel")}
                            </label>
                            <Input
                              value={selectedDraft.claudeDefaultOpusModel}
                              readOnly={
                                selectedDraft.claudeAuthMode ===
                                "model_provider"
                              }
                              onChange={(event) => {
                                handleImportantConfigChange(
                                  "claudeDefaultOpusModel",
                                  event.target.value
                                )
                              }}
                              placeholder="claude-opus-5-5"
                            />
                          </div>
                          <div className="space-y-1.5">
                            <label className="text-2xs text-muted-foreground">
                              {t("claude.fableDefaultModel")}
                            </label>
                            <Input
                              value={selectedDraft.claudeDefaultFableModel}
                              readOnly={
                                selectedDraft.claudeAuthMode ===
                                "model_provider"
                              }
                              onChange={(event) => {
                                handleImportantConfigChange(
                                  "claudeDefaultFableModel",
                                  event.target.value
                                )
                              }}
                              placeholder="claude-fable-5-1"
                            />
                          </div>
                        </div>
                        <p className="text-2xs text-muted-foreground">
                          {t("modelHintDefault")}
                        </p>
                        <div className="space-y-2 border-t border-border/60 pt-3">
                          <div className="grid gap-3 md:grid-cols-2">
                            <div className="space-y-1.5 md:col-span-2">
                              <label className="text-2xs text-muted-foreground">
                                {t("claude.customModelOption")}
                              </label>
                              <Input
                                value={selectedDraft.claudeCustomModelOption}
                                readOnly={
                                  selectedDraft.claudeAuthMode ===
                                  "model_provider"
                                }
                                onChange={(event) => {
                                  handleImportantConfigChange(
                                    "claudeCustomModelOption",
                                    event.target.value
                                  )
                                }}
                                placeholder="my-gateway/claude-opus-5-5"
                              />
                            </div>
                            <div className="space-y-1.5">
                              <label className="text-2xs text-muted-foreground">
                                {t("claude.customModelOptionName")}
                              </label>
                              <Input
                                value={
                                  selectedDraft.claudeCustomModelOptionName
                                }
                                readOnly={
                                  selectedDraft.claudeAuthMode ===
                                  "model_provider"
                                }
                                onChange={(event) => {
                                  handleImportantConfigChange(
                                    "claudeCustomModelOptionName",
                                    event.target.value
                                  )
                                }}
                                placeholder="Gateway Opus"
                              />
                            </div>
                            <div className="space-y-1.5">
                              <label className="text-2xs text-muted-foreground">
                                {t("claude.customModelOptionDescription")}
                              </label>
                              <Input
                                value={
                                  selectedDraft.claudeCustomModelOptionDescription
                                }
                                readOnly={
                                  selectedDraft.claudeAuthMode ===
                                  "model_provider"
                                }
                                onChange={(event) => {
                                  handleImportantConfigChange(
                                    "claudeCustomModelOptionDescription",
                                    event.target.value
                                  )
                                }}
                                placeholder="Routed via custom gateway"
                              />
                            </div>
                          </div>
                          <p className="text-2xs text-muted-foreground">
                            {t("claude.customModelOptionHint")}
                          </p>
                        </div>
                        <div className="space-y-1.5">
                          <div className="flex items-center justify-between rounded-md border px-3 py-2">
                            <label className="text-2xs text-muted-foreground">
                              {t("claude.sendAttributionHeader")}
                            </label>
                            <Switch
                              checked={
                                selectedDraft.claudeSendAttributionHeader
                              }
                              onCheckedChange={(checked) => {
                                handleClaudeEnvFlagChange(
                                  "claudeSendAttributionHeader",
                                  CLAUDE_ATTRIBUTION_HEADER_ENV_KEY,
                                  checked
                                )
                              }}
                              aria-label={t("claude.sendAttributionHeaderAria")}
                            />
                          </div>
                        </div>
                        <div className="space-y-1.5">
                          <div className="flex items-center justify-between rounded-md border px-3 py-2">
                            <label className="text-2xs text-muted-foreground">
                              {t("claude.disableNonessentialTraffic")}
                            </label>
                            <Switch
                              checked={
                                selectedDraft.claudeDisableNonessentialTraffic
                              }
                              onCheckedChange={(checked) => {
                                handleClaudeEnvFlagChange(
                                  "claudeDisableNonessentialTraffic",
                                  CLAUDE_NONESSENTIAL_TRAFFIC_ENV_KEY,
                                  checked
                                )
                              }}
                              aria-label={t(
                                "claude.disableNonessentialTrafficAria"
                              )}
                            />
                          </div>
                        </div>
                      </div>
                    ) : (
                      importantFieldsFor(selectedAgent.agent_type).model && (
                        <div className="space-y-1.5">
                          <label className="text-2xs text-muted-foreground">
                            Model
                          </label>
                          <Input
                            value={selectedDraft.model}
                            readOnly={selectedDraft.modelProviderId != null}
                            onChange={(event) => {
                              handleImportantConfigChange(
                                "model",
                                event.target.value
                              )
                            }}
                            placeholder="gpt-6-astra / claude-sonnet-5-5 / gemini-3.1-pro-preview"
                          />
                        </div>
                      )
                    )}

                    <div className="space-y-1.5">
                      <label className="text-2xs text-muted-foreground">
                        {t("nativeJsonConfig")}
                      </label>
                      <Textarea
                        value={selectedDraft.configText}
                        onChange={(event) => {
                          handleConfigTextChange(event.target.value)
                        }}
                        placeholder={`{
  "apiBaseUrl": "https://api.example.com",
  "apiKey": "sk-...",
  "model": "gpt-6-astra",
  "env": {
    "CUSTOM_KEY": "VALUE"
  }
}`}
                        className="min-h-36 font-mono text-xs"
                      />
                      {selectedConfigError && (
                        <div className="rounded-md border border-red-500/30 bg-red-500/5 px-2.5 py-1.5 text-2xs text-red-400">
                          {selectedConfigError}
                        </div>
                      )}
                    </div>

                    <div className="flex justify-end">
                      <Button
                        size="sm"
                        onClick={() => {
                          if (selectedMissingModelProvider) {
                            toast.error(t("toasts.modelProviderRequired"))
                            return
                          }
                          // When a Claude provider is bound, the on-disk config
                          // loaded into configText may carry stale model keys
                          // (e.g. a leftover custom model option) from before the
                          // binding — re-derive them from the provider so
                          // persistConfig cannot write a stale value back over
                          // the backend bind cascade (invalid JSON passes through
                          // so persistConfig still surfaces the error). Sequence
                          // env→config (never parallel): persistEnv also rewrites
                          // config.env on the backend, so concurrent writes would
                          // interleave two writers of ~/.claude/settings.json.
                          let configToSave = configTextForClaudeSave(
                            selectedDraft.configText,
                            selectedAgent.agent_type,
                            selectedDraft.modelProviderId,
                            modelProviders.find(
                              (p) => p.id === selectedDraft.modelProviderId
                            )
                          )
                          // Materialize the Claude hardening toggles so the shown
                          // default positions are actually applied on save —
                          // writing the explicit "1"/"0" into both the native
                          // config `env` and the DB env overlay — regardless of
                          // whether the user touched the switches. Invalid JSON is
                          // left untouched so persistConfig surfaces the error.
                          let envToSave = selectedDraft.envText
                          if (selectedAgent.agent_type === "claude_code") {
                            const materialized =
                              materializeClaudeHardeningFlags(
                                configToSave,
                                envToSave,
                                {
                                  sendAttributionHeader:
                                    selectedDraft.claudeSendAttributionHeader,
                                  disableNonessentialTraffic:
                                    selectedDraft.claudeDisableNonessentialTraffic,
                                }
                              )
                            configToSave = materialized.configText
                            envToSave = materialized.envText
                          }
                          persistEnv(
                            selectedAgent.agent_type,
                            selectedDraft.enabled,
                            envToSave,
                            selectedDraft.modelProviderId
                          )
                            .then(() =>
                              persistConfig(
                                selectedAgent.agent_type,
                                configToSave
                              )
                            )
                            .then(() => {
                              // Reflect the provider-authoritative rewrite AND the
                              // materialized hardening flags in the editors so the
                              // textareas don't show stale values until reload —
                              // and so a later env-only save doesn't persist a
                              // stale envText that drops the flags from the DB
                              // overlay. Each inner guard preserves an edit the
                              // user typed while the save was in flight.
                              const syncedConfig =
                                configToSave !== selectedDraft.configText
                                  ? normalizeConfigText(configToSave)
                                  : null
                              const syncEnv =
                                envToSave !== selectedDraft.envText
                              if (syncedConfig !== null || syncEnv) {
                                updateSelectedDraft((current) => {
                                  let next = current
                                  if (
                                    syncedConfig !== null &&
                                    current.configText ===
                                      selectedDraft.configText
                                  ) {
                                    next = {
                                      ...next,
                                      configText: syncedConfig,
                                    }
                                  }
                                  if (
                                    syncEnv &&
                                    current.envText === selectedDraft.envText
                                  ) {
                                    next = { ...next, envText: envToSave }
                                  }
                                  return next
                                })
                              }
                              toast.success(t("toasts.configSaved"), {
                                description: t("toasts.configSavedHint"),
                              })
                            })
                            .catch((err) => {
                              console.error(
                                "[Settings] save config management failed:",
                                err
                              )
                              const message = toErrorMessage(err)
                              toast.error(
                                t("toasts.saveConfigManagementFailed"),
                                {
                                  description: message,
                                }
                              )
                            })
                        }}
                        disabled={selectedIsSavingEnv || selectedIsSavingConfig}
                      >
                        {selectedIsSavingEnv || selectedIsSavingConfig ? (
                          <>
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            {t("actions.saving")}
                          </>
                        ) : (
                          <>
                            <Save className="h-3.5 w-3.5" />
                            {t("actions.saveConfigManagement")}
                          </>
                        )}
                      </Button>
                    </div>
                  </div>
                )}
              </div>
            </div>
          ) : (
            <div className="h-full flex items-center justify-center text-xs text-muted-foreground">
              {t("emptyNoAgent")}
            </div>
          )}
        </div>
      </div>

      <AlertDialog
        open={Boolean(openCodeDeleteProviderId)}
        onOpenChange={(open) => {
          if (!open) setOpenCodeDeleteProviderId(null)
        }}
      >
        <AlertDialogContent size="sm">
          <AlertDialogHeader>
            <AlertDialogTitle>
              {t("dialogs.confirmDeleteProvider", {
                providerId: openCodeDeleteProviderId ?? "",
              })}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {t("dialogs.confirmDeleteProviderDescription")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={selectedIsSaving}>
              {t("actions.cancel")}
            </AlertDialogCancel>
            <Button
              variant="destructive"
              onClick={confirmOpenCodeProviderDelete}
              disabled={selectedIsSaving}
            >
              {selectedIsSaving ? (
                <>
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  {t("actions.deleting")}
                </>
              ) : (
                <>
                  <Trash2 className="h-3.5 w-3.5" />
                  {t("actions.confirmDelete")}
                </>
              )}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={Boolean(uninstallConfirmAgent)}
        onOpenChange={(open) => {
          if (!open) setUninstallConfirmAgent(null)
        }}
      >
        <AlertDialogContent size="sm">
          <AlertDialogHeader>
            <AlertDialogTitle>
              {t("dialogs.confirmUninstall", {
                name: uninstallConfirmAgent?.name ?? "Agent",
              })}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {t("dialogs.confirmUninstallDescription")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel
              disabled={
                uninstallConfirmAgent
                  ? Boolean(busyBinaryAction[uninstallConfirmAgent.agent_type])
                  : false
              }
            >
              {t("actions.cancel")}
            </AlertDialogCancel>
            <Button
              variant="destructive"
              onClick={confirmUninstall}
              disabled={
                uninstallConfirmAgent
                  ? Boolean(busyBinaryAction[uninstallConfirmAgent.agent_type])
                  : false
              }
            >
              {uninstallConfirmAgent &&
              busyBinaryAction[uninstallConfirmAgent.agent_type] ? (
                <>
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  {t("actions.uninstalling")}
                </>
              ) : (
                <>
                  <Trash2 className="h-3.5 w-3.5" />
                  {t("actions.confirmUninstall")}
                </>
              )}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={Boolean(removeConfirmAgent)}
        onOpenChange={(open) => {
          if (!open) setRemoveConfirmAgent(null)
        }}
      >
        <AlertDialogContent size="sm">
          <AlertDialogHeader>
            <AlertDialogTitle>{t("customAgentRemove")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("customAgentRemoveConfirm", {
                name: removeConfirmAgent?.name ?? "Agent",
              })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={removingCustomAgent}>
              {t("actions.cancel")}
            </AlertDialogCancel>
            <Button
              variant="destructive"
              onClick={confirmRemoveCustomAgent}
              disabled={removingCustomAgent}
            >
              {removingCustomAgent ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Trash2 className="h-3.5 w-3.5" />
              )}
              {t("customAgentRemove")}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={Boolean(customInstallAgent)}
        onOpenChange={(open) => {
          if (!open) setCustomInstallAgent(null)
        }}
      >
        <AlertDialogContent size="sm">
          <AlertDialogHeader>
            <AlertDialogTitle>
              {t("dialogs.customInstallTitle", {
                name: customInstallAgent?.name ?? "Agent",
              })}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {t("dialogs.customInstallDescription")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <div className="space-y-1.5">
            <label
              htmlFor="custom-version-input"
              className="text-xs font-medium"
            >
              {t("dialogs.customInstallVersionLabel")}
            </label>
            <Input
              id="custom-version-input"
              autoFocus
              value={customVersionInput}
              placeholder={customInstallAgent?.registry_version ?? "1.0.0"}
              onChange={(e) => setCustomVersionInput(e.target.value)}
              {...ime.props}
              onKeyDown={(e) => {
                if (ime.isComposing(e)) return
                if (
                  e.key === "Enter" &&
                  isValidCustomVersion(customVersionInput)
                ) {
                  e.preventDefault()
                  confirmCustomInstall()
                }
              }}
            />
            {customVersionInput.trim() !== "" &&
              !isValidCustomVersion(customVersionInput) && (
                <p className="text-2xs text-red-500">
                  {t("dialogs.customInstallInvalid")}
                </p>
              )}
          </div>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("actions.cancel")}</AlertDialogCancel>
            <Button
              onClick={confirmCustomInstall}
              disabled={!isValidCustomVersion(customVersionInput)}
            >
              <PackagePlus className="h-3.5 w-3.5" />
              {t("dialogs.customInstallSubmit")}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={Boolean(upgradeLatestTarget)}
        onOpenChange={(open) => {
          if (!open) setUpgradeLatestTarget(null)
        }}
      >
        <AlertDialogContent size="sm">
          <AlertDialogHeader>
            <AlertDialogTitle>
              {t("dialogs.confirmUpgradeToLatest", {
                name: upgradeLatestTarget?.agent.name ?? "Agent",
                version: upgradeLatestTarget?.version ?? "",
              })}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {t("dialogs.confirmUpgradeToLatestDescription", {
                name: upgradeLatestTarget?.agent.name ?? "Agent",
                recommendedVersion:
                  upgradeLatestTarget?.agent.registry_version ?? "",
              })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("actions.cancel")}</AlertDialogCancel>
            <Button
              onClick={confirmUpgradeLatest}
              disabled={anyBinaryActionBusy}
            >
              <Wrench className="h-3.5 w-3.5" />
              {t("actions.upgrade")}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <OpencodePluginsModal
        open={pluginModalOpen}
        onOpenChange={setPluginModalOpen}
        onCompleted={() => {
          if (pluginModalAgent) {
            runPreflight(pluginModalAgent)
          }
          setPluginModalAgent(null)
        }}
      />
    </div>
  )
}
