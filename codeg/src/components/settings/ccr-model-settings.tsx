"use client"

import { useCallback, useEffect, useMemo, useState } from "react"
import {
  ArrowDown,
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  Download,
  Eye,
  EyeOff,
  FileJson,
  ListChecks,
  Loader2,
  Pencil,
  PlugZap,
  Plus,
  RefreshCw,
  Search,
  Server,
  Settings2,
  Trash2,
  X,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"

import {
  AlertDialog,
  AlertDialogAction,
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
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import {
  CCR_PROTOCOLS,
  ccrGetConfig,
  ccrGetProviderAccountSnapshots,
  ccrGetProviderPresets,
  ccrGatewayStatus,
  ccrHealth,
  ccrProbeProvider,
  ccrRestartGateway,
  ccrSaveConfig,
  ccrStartGateway,
  ccrStopGateway,
  type CcrAccountMeter,
  type CcrAccountSnapshot,
  type CcrConfig,
  type CcrCredential,
  type CcrGatewayStatus,
  type CcrHealth,
  type CcrProbeResult,
  type CcrProvider,
  type CcrProviderPreset,
} from "@/lib/ccr"
import { readDisabledModels, writeDisabledModels } from "@/lib/ccr-model-memory"

// ---------------------------------------------------------------------------
// Draft types & helpers
// ---------------------------------------------------------------------------

interface CredentialDraft {
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

interface ProviderDraft {
  presetId: string
  name: string
  baseUrl: string
  type: string
  modelsText: string
  credentials: CredentialDraft[]
}

function emptyCredential(): CredentialDraft {
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

function emptyDraft(): ProviderDraft {
  return {
    presetId: "custom",
    name: "",
    baseUrl: "",
    type: "openai_chat_completions",
    modelsText: "",
    credentials: [emptyCredential()],
  }
}

function providerBaseUrl(provider: CcrProvider): string {
  return provider.api_base_url ?? provider.baseUrl ?? ""
}

function providerModels(provider: CcrProvider): string[] {
  return Array.isArray(provider.models) ? provider.models : []
}

function providerCredentials(provider: CcrProvider): CcrCredential[] {
  if (Array.isArray(provider.credentials) && provider.credentials.length > 0) {
    return provider.credentials
  }
  const single = provider.api_key ?? provider.apiKey ?? provider.apikey ?? ""
  return single ? [{ name: "Key 1", api_key: single }] : []
}

function credentialKey(credential: CcrCredential): string {
  return credential.api_key ?? credential.apiKey ?? credential.apikey ?? ""
}

function credentialName(credential: CcrCredential, index: number): string {
  const name = (credential.name ?? credential.label ?? "").trim()
  return name || `Key ${index + 1}`
}

function credentialPriority(credential: CcrCredential, index: number): number {
  return Number.isFinite(credential.priority)
    ? Number(credential.priority)
    : index + 1
}

function keyTail(key: string): string {
  const value = key.trim()
  if (!value) return ""
  return value.length <= 8 ? value : `…${value.slice(-6)}`
}

function limitsSummary(limits: Record<string, unknown> | undefined): string {
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

function meterText(meter: CcrAccountMeter): string {
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

function headerUsageBadge(snapshots: CcrAccountSnapshot[]): string {
  for (const snapshot of snapshots) {
    const meters = snapshot.meters ?? []
    for (const meter of meters) {
      const text = meterText(meter)
      if (text) {
        return `${meter.label}: ${text}`
      }
    }
  }
  return ""
}

function parseModels(text: string): string[] {
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

function parseLimits(text: string): Record<string, unknown> | undefined {
  const trimmed = text.trim()
  if (!trimmed) return undefined
  const parsed: unknown = JSON.parse(trimmed)
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error("limits-not-object")
  }
  return parsed as Record<string, unknown>
}

function slugId(name: string): string {
  const base = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32)
  return `${base || "provider"}-${Date.now().toString(36)}`
}

function errorText(error: unknown): string {
  const raw = error as Record<string, unknown>
  if (typeof raw?.message === "string") {
    return raw.message
  }
  return error instanceof Error ? error.message : String(error)
}

function draftFromProvider(provider: CcrProvider): ProviderDraft {
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

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export function CcrModelSettings() {
  const t = useTranslations("CcrModelSettings")
  const [health, setHealth] = useState<CcrHealth | null>(null)
  const [gateway, setGateway] = useState<CcrGatewayStatus | null>(null)
  const [config, setConfig] = useState<CcrConfig | null>(null)
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState("")
  const [error, setError] = useState<string | null>(null)

  const [gatewayOpen, setGatewayOpen] = useState(true)
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [snapshots, setSnapshots] = useState<
    Record<string, CcrAccountSnapshot[]>
  >({})
  const [snapshotBusy, setSnapshotBusy] = useState<Record<string, boolean>>({})

  const [dialogOpen, setDialogOpen] = useState(false)
  const [editingIndex, setEditingIndex] = useState<number | null>(null)
  const [draft, setDraft] = useState<ProviderDraft>(emptyDraft)
  const [dialogError, setDialogError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [testing, setTesting] = useState(false)
  const [testResult, setTestResult] = useState<
    { ok: true; result: CcrProbeResult } | { ok: false; error: string } | null
  >(null)
  const [presets, setPresets] = useState<CcrProviderPreset[] | null>(null)

  const [importOpen, setImportOpen] = useState(false)
  const [importText, setImportText] = useState("")
  const [importError, setImportError] = useState<string | null>(null)

  const [pickerIndex, setPickerIndex] = useState<number | null>(null)
  const [pickerDetected, setPickerDetected] = useState<string[]>([])
  const [pickerLoading, setPickerLoading] = useState(false)
  const [pickerError, setPickerError] = useState<string | null>(null)
  const [pickerSearchLeft, setPickerSearchLeft] = useState("")
  const [pickerSearchRight, setPickerSearchRight] = useState("")
  const [customModel, setCustomModel] = useState("")
  const [modelBusy, setModelBusy] = useState("")

  const [deleteIndex, setDeleteIndex] = useState<number | null>(null)
  const [toggleBusy, setToggleBusy] = useState("")

  const reload = useCallback(async () => {
    try {
      const [nextHealth, nextGateway, nextConfig] = await Promise.all([
        ccrHealth(),
        ccrGatewayStatus(),
        ccrGetConfig(),
      ])
      setHealth(nextHealth)
      setGateway(nextGateway)
      setConfig(nextConfig)
      setError(null)
    } catch (err: unknown) {
      setError(errorText(err))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    reload().catch(console.error)
  }, [reload])

  const providers = useMemo(() => config?.Providers ?? [], [config])

  const providerKey = useCallback(
    (provider: CcrProvider, index: number): string =>
      provider.id ?? `${provider.name}::${index}`,
    []
  )

  const loadSnapshots = useCallback(
    async (key: string, providerName: string, force = false) => {
      setSnapshotBusy((prev) => ({ ...prev, [key]: true }))
      try {
        const list = await ccrGetProviderAccountSnapshots(providerName, {
          forceRefresh: force,
        })
        setSnapshots((prev) => ({ ...prev, [key]: list }))
      } catch {
        setSnapshots((prev) => ({ ...prev, [key]: [] }))
      } finally {
        setSnapshotBusy((prev) => ({ ...prev, [key]: false }))
      }
    },
    []
  )

  const toggleExpand = useCallback(
    (key: string, providerName: string) => {
      if (expanded.has(key)) {
        const next = new Set(expanded)
        next.delete(key)
        setExpanded(next)
        return
      }
      const next = new Set(expanded)
      next.add(key)
      setExpanded(next)
      if (!snapshots[key]) {
        loadSnapshots(key, providerName).catch(console.error)
      }
    },
    [expanded, loadSnapshots, snapshots]
  )

  const runGatewayAction = useCallback(
    async (action: "start" | "stop" | "restart") => {
      setBusy(action)
      try {
        if (action === "start") {
          await ccrStartGateway()
        } else if (action === "stop") {
          await ccrStopGateway()
        } else {
          await ccrRestartGateway()
        }
        setGateway(await ccrGatewayStatus())
      } catch (err: unknown) {
        toast.error(errorText(err), { description: t("gatewayActionFailed") })
      } finally {
        setBusy("")
      }
    },
    [t]
  )

  const loadPresets = useCallback(async () => {
    try {
      setPresets(await ccrGetProviderPresets())
    } catch {
      setPresets([])
    }
  }, [])

  const openAdd = useCallback(() => {
    setEditingIndex(null)
    setDraft(emptyDraft())
    setDialogError(null)
    setTestResult(null)
    setDialogOpen(true)
    if (presets === null) {
      loadPresets().catch(console.error)
    }
  }, [loadPresets, presets])

  const openEdit = useCallback(
    (index: number) => {
      const provider = providers[index]
      if (!provider) return
      setEditingIndex(index)
      setDraft(draftFromProvider(provider))
      setDialogError(null)
      setTestResult(null)
      setDialogOpen(true)
      if (presets === null) {
        loadPresets().catch(console.error)
      }
    },
    [loadPresets, presets, providers]
  )

  const applyPreset = useCallback(
    (presetId: string) => {
      if (presetId === "custom") {
        setDraft((prev) => ({ ...prev, presetId: "custom" }))
        return
      }
      const preset = presets?.find((item) => item.id === presetId)
      if (!preset) return
      const endpoint = preset.endpoints?.[0]
      setDraft((prev) => ({
        ...prev,
        presetId,
        name: preset.name,
        baseUrl: endpoint?.baseUrl ?? prev.baseUrl,
        type: endpoint?.protocols?.[0] ?? prev.type,
        modelsText: (preset.defaultModels ?? []).join("\n"),
      }))
    },
    [presets]
  )

  const updateCredentialRow = useCallback(
    (index: number, patch: Partial<CredentialDraft>) => {
      setDraft((prev) => ({
        ...prev,
        credentials: prev.credentials.map((row, i) =>
          i === index ? { ...row, ...patch } : row
        ),
      }))
    },
    []
  )

  const addCredentialRow = useCallback(() => {
    setDraft((prev) => ({
      ...prev,
      credentials: [...prev.credentials, emptyCredential()],
    }))
  }, [])

  const removeCredentialRow = useCallback((index: number) => {
    setDraft((prev) => ({
      ...prev,
      credentials: prev.credentials.filter((_, i) => i !== index),
    }))
  }, [])

  const moveCredentialRow = useCallback((index: number, delta: number) => {
    setDraft((prev) => {
      const target = index + delta
      if (target < 0 || target >= prev.credentials.length) return prev
      const rows = [...prev.credentials]
      const [moved] = rows.splice(index, 1)
      rows.splice(target, 0, moved)
      return { ...prev, credentials: rows }
    })
  }, [])

  const testConnection = useCallback(async () => {
    const baseUrl = draft.baseUrl.trim()
    if (!baseUrl) {
      setTestResult({ ok: false, error: t("baseUrlRequired") })
      return
    }
    const apiKey =
      draft.credentials.map((row) => row.apiKey.trim()).find(Boolean) ?? ""
    setTesting(true)
    setTestResult(null)
    try {
      const result = await ccrProbeProvider({
        baseUrl,
        apiKey: apiKey || undefined,
        mode: "connectivity",
        models: parseModels(draft.modelsText).slice(0, 3),
        forceRefresh: true,
      })
      setTestResult({ ok: true, result })
    } catch (err: unknown) {
      setTestResult({ ok: false, error: errorText(err) })
    } finally {
      setTesting(false)
    }
  }, [draft, t])

  const confirmImport = useCallback(() => {
    try {
      const parsed: unknown = JSON.parse(importText)
      const rawList = Array.isArray(parsed)
        ? parsed
        : ((parsed as Record<string, unknown>)?.credentials as unknown[])
      if (!Array.isArray(rawList)) {
        setImportError(t("importInvalid"))
        return
      }
      const rows = rawList.map((item): CredentialDraft => {
        const record = item as Record<string, unknown>
        const key =
          typeof record.api_key === "string"
            ? record.api_key
            : typeof record.apiKey === "string"
              ? record.apiKey
              : typeof record.apikey === "string"
                ? record.apikey
                : ""
        return {
          name: typeof record.name === "string" ? record.name : "",
          apiKey: key,
          priority:
            typeof record.priority === "number" ? String(record.priority) : "",
          weight:
            typeof record.weight === "number" ? String(record.weight) : "",
          limitsText: record.limits
            ? JSON.stringify(record.limits, null, 2)
            : "",
          revealKey: false,
          enabled: record.enabled !== false,
          advancedOpen: Boolean(
            record.limits ||
            typeof record.priority === "number" ||
            typeof record.weight === "number"
          ),
        }
      })
      if (rows.length === 0) {
        setImportError(t("importInvalid"))
        return
      }
      setDraft((prev) => ({
        ...prev,
        credentials: [...prev.credentials, ...rows],
      }))
      setImportOpen(false)
      setImportText("")
      setImportError(null)
    } catch {
      setImportError(t("importInvalid"))
    }
  }, [importText, t])

  const submitDraft = useCallback(async () => {
    const name = draft.name.trim()
    if (!name) {
      setDialogError(t("nameRequired"))
      return
    }
    const baseUrl = draft.baseUrl.trim()
    if (!baseUrl) {
      setDialogError(t("baseUrlRequired"))
      return
    }
    const rows = draft.credentials.filter(
      (row) => row.apiKey.trim() || row.name.trim()
    )
    if (!rows.some((row) => row.apiKey.trim())) {
      setDialogError(t("keysRequired"))
      return
    }

    let credentials: CcrCredential[]
    try {
      credentials = rows.map((row) => {
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
    } catch {
      setDialogError(t("limitsInvalid"))
      return
    }

    setSaving(true)
    setDialogError(null)
    try {
      const current = config ?? (await ccrGetConfig())
      const list = [...(current.Providers ?? [])]
      const existing = editingIndex !== null ? list[editingIndex] : undefined
      const entry: CcrProvider = existing
        ? { ...existing }
        : { id: slugId(name), name, enabled: true }
      entry.name = name
      entry.type = draft.type
      entry.api_base_url = baseUrl
      delete entry.baseUrl
      delete entry.api_key
      delete entry.apiKey
      delete entry.apikey
      entry.models = parseModels(draft.modelsText)
      entry.credentials = credentials

      if (editingIndex === null) {
        list.push(entry)
      } else {
        list[editingIndex] = entry
      }
      await ccrSaveConfig({ ...current, Providers: list })
      toast.success(t("saved"))
      setDialogOpen(false)
      await reload()
    } catch (err: unknown) {
      setDialogError(errorText(err))
    } finally {
      setSaving(false)
    }
  }, [config, draft, editingIndex, reload, t])

  const toggleCredentialEnabled = useCallback(
    async (providerIndex: number, credentialIndex: number) => {
      const key = `${providerIndex}:${credentialIndex}`
      setToggleBusy(key)
      try {
        const current = config ?? (await ccrGetConfig())
        const list = [...(current.Providers ?? [])]
        const provider = list[providerIndex]
        if (!provider) return
        const credentials = providerCredentials(provider).map((credential) => ({
          ...credential,
        }))
        const target = credentials[credentialIndex]
        if (!target) return
        target.enabled = target.enabled === false
        const entry: CcrProvider = { ...provider, credentials }
        delete entry.api_key
        delete entry.apiKey
        delete entry.apikey
        list[providerIndex] = entry
        await ccrSaveConfig({ ...current, Providers: list })
        await reload()
      } catch (err: unknown) {
        toast.error(errorText(err))
      } finally {
        setToggleBusy("")
      }
    },
    [config, reload]
  )

  const confirmDelete = useCallback(async () => {
    if (deleteIndex === null) return
    try {
      const current = config ?? (await ccrGetConfig())
      const list = [...(current.Providers ?? [])]
      list.splice(deleteIndex, 1)
      await ccrSaveConfig({ ...current, Providers: list })
      toast.success(t("deleted"))
      setDeleteIndex(null)
      await reload()
    } catch (err: unknown) {
      toast.error(errorText(err))
    }
  }, [config, deleteIndex, reload, t])

  const providerFirstKey = useCallback((provider: CcrProvider): string => {
    const keys = providerCredentials(provider)
      .map((credential) => credentialKey(credential).trim())
      .filter(Boolean)
    return keys[0] ?? ""
  }, [])

  const detectModels = useCallback(
    async (provider: CcrProvider) => {
      setPickerLoading(true)
      setPickerError(null)
      try {
        const result = await ccrProbeProvider({
          baseUrl: providerBaseUrl(provider),
          apiKey: providerFirstKey(provider) || undefined,
          mode: "models",
          forceRefresh: true,
        })
        const models = [
          ...new Set(
            (result.models ?? []).map((model) => model.trim()).filter(Boolean)
          ),
        ]
        setPickerDetected(models.sort((a, b) => a.localeCompare(b)))
      } catch (err: unknown) {
        setPickerDetected([])
        setPickerError(errorText(err))
      } finally {
        setPickerLoading(false)
      }
    },
    [providerFirstKey]
  )

  const openPicker = useCallback(
    (index: number) => {
      const provider = providers[index]
      if (!provider) return
      setPickerIndex(index)
      setPickerSearchLeft("")
      setPickerSearchRight("")
      setCustomModel("")
      setPickerDetected([])
      detectModels(provider).catch(console.error)
    },
    [detectModels, providers]
  )

  const toggleModel = useCallback(
    async (index: number, model: string, enable: boolean) => {
      const provider = providers[index]
      if (!provider) return
      setModelBusy(`toggle:${model}`)
      try {
        const key = providerKey(provider, index)
        const current = config ?? (await ccrGetConfig())
        const list = [...(current.Providers ?? [])]
        const target = list[index]
        if (!target) return
        const enabled = providerModels(target)
        const disabled = readDisabledModels(key)
        const nextEnabled = enable
          ? enabled.includes(model)
            ? enabled
            : [...enabled, model]
          : enabled.filter((item) => item !== model)
        const nextDisabled = enable
          ? disabled.filter((item) => item !== model)
          : disabled.includes(model)
            ? disabled
            : [...disabled, model]
        list[index] = { ...target, models: nextEnabled }
        await ccrSaveConfig({ ...current, Providers: list })
        writeDisabledModels(key, nextDisabled)
        await reload()
      } catch (err: unknown) {
        toast.error(errorText(err))
      } finally {
        setModelBusy("")
      }
    },
    [config, providerKey, providers, reload]
  )

  const fetchModels = useCallback(
    async (index: number) => {
      const provider = providers[index]
      if (!provider) return
      const key = providerKey(provider, index)
      setModelBusy(`fetch:${key}`)
      try {
        const result = await ccrProbeProvider({
          baseUrl: providerBaseUrl(provider),
          apiKey: providerFirstKey(provider) || undefined,
          mode: "models",
          forceRefresh: true,
        })
        const detected = [
          ...new Set(
            (result.models ?? []).map((model) => model.trim()).filter(Boolean)
          ),
        ]
        const current = config ?? (await ccrGetConfig())
        const list = [...(current.Providers ?? [])]
        const target = list[index]
        if (!target) return
        const enabled = providerModels(target)
        const known = new Set([...enabled, ...readDisabledModels(key)])
        const added = detected.filter((model) => !known.has(model))
        if (added.length === 0) {
          toast.info(t("fetchModelsEmpty"))
          return
        }
        list[index] = { ...target, models: [...enabled, ...added] }
        await ccrSaveConfig({ ...current, Providers: list })
        toast.success(t("fetchModelsDone", { count: added.length }))
        await reload()
      } catch (err: unknown) {
        toast.error(errorText(err), { description: t("detectFailed") })
      } finally {
        setModelBusy("")
      }
    },
    [config, providerFirstKey, providerKey, providers, reload, t]
  )

  const addModelFromPicker = useCallback(
    async (model: string) => {
      if (pickerIndex === null) return
      const provider = providers[pickerIndex]
      if (!provider) return
      if (providerModels(provider).includes(model)) return
      const key = providerKey(provider, pickerIndex)
      setModelBusy(`add:${model}`)
      try {
        const current = config ?? (await ccrGetConfig())
        const list = [...(current.Providers ?? [])]
        const target = list[pickerIndex]
        if (!target) return
        list[pickerIndex] = {
          ...target,
          models: [...providerModels(target), model],
        }
        await ccrSaveConfig({ ...current, Providers: list })
        writeDisabledModels(
          key,
          readDisabledModels(key).filter((item) => item !== model)
        )
        await reload()
      } catch (err: unknown) {
        toast.error(errorText(err))
      } finally {
        setModelBusy("")
      }
    },
    [config, pickerIndex, providerKey, providers, reload]
  )

  const removeModelFromPicker = useCallback(
    async (model: string) => {
      if (pickerIndex === null) return
      const provider = providers[pickerIndex]
      if (!provider) return
      const key = providerKey(provider, pickerIndex)
      setModelBusy(`remove:${model}`)
      try {
        const current = config ?? (await ccrGetConfig())
        const list = [...(current.Providers ?? [])]
        const target = list[pickerIndex]
        if (!target) return
        list[pickerIndex] = {
          ...target,
          models: providerModels(target).filter((item) => item !== model),
        }
        await ccrSaveConfig({ ...current, Providers: list })
        writeDisabledModels(
          key,
          readDisabledModels(key).filter((item) => item !== model)
        )
        await reload()
      } catch (err: unknown) {
        toast.error(errorText(err))
      } finally {
        setModelBusy("")
      }
    },
    [config, pickerIndex, providerKey, providers, reload]
  )

  const addCustomModel = useCallback(() => {
    const model = customModel.trim()
    if (!model) return
    addModelFromPicker(model)
      .then(() => setCustomModel(""))
      .catch(console.error)
  }, [addModelFromPicker, customModel])

  const gatewayState = gateway?.state ?? "unknown"
  const gatewayRunning = gatewayState === "running"
  const stateLabel = gatewayRunning
    ? t("gatewayRunning")
    : gatewayState === "stopped"
      ? t("gatewayStopped")
      : t("gatewayError")

  const pickerProvider =
    pickerIndex !== null ? providers[pickerIndex] : undefined
  const pickerEnabled = pickerProvider ? providerModels(pickerProvider) : []
  const searchLeft = pickerSearchLeft.trim().toLowerCase()
  const searchRight = pickerSearchRight.trim().toLowerCase()
  const filteredDetected = pickerDetected.filter((model) =>
    model.toLowerCase().includes(searchLeft)
  )
  const filteredAdded = pickerEnabled.filter((model) =>
    model.toLowerCase().includes(searchRight)
  )

  return (
    <ScrollArea className="h-full">
      <section className="space-y-3 px-3 pt-3 md:px-4 md:pt-4">
        <div>
          <h1 className="text-sm font-semibold">{t("sectionTitle")}</h1>
          <p className="text-sm text-muted-foreground">
            {t("sectionDescription")}
          </p>
        </div>
      </section>

      {error && (
        <section className="mt-3 px-3 md:px-4">
          <div className="rounded-md border border-red-500/30 bg-red-500/5 px-3 py-2 text-xs text-red-400">
            {error}
          </div>
        </section>
      )}

      {/* ----- gateway info（可折叠） ----- */}
      <section className="mt-4 px-3 md:px-4">
        <button
          type="button"
          onClick={() => setGatewayOpen((prev) => !prev)}
          className="flex w-full items-center justify-between gap-2 text-left"
        >
          <h2 className="text-xs font-medium text-muted-foreground">
            {t("statusTitle")}
          </h2>
          <span className="flex items-center gap-2">
            <Badge
              variant={gatewayRunning ? "secondary" : "destructive"}
              className="text-3xs px-1.5 py-0"
            >
              {stateLabel}
            </Badge>
            <ChevronDown
              className={`h-3.5 w-3.5 text-muted-foreground transition-transform ${
                gatewayOpen ? "rotate-180" : ""
              }`}
            />
          </span>
        </button>
        {gatewayOpen && (
          <div className="mt-2 space-y-2 rounded-md border px-3 py-2.5">
            <div className="grid gap-x-6 gap-y-1.5 text-xs sm:grid-cols-2">
              <span className="text-muted-foreground">
                {t("connectionLabel")}:
                <span className="ml-1.5 font-mono text-foreground">
                  {health?.url ?? "-"}
                </span>
              </span>
              <span className="text-muted-foreground">
                {t("gatewayEndpointLabel")}:
                <span className="ml-1.5 font-mono text-foreground">
                  {gateway?.coreEndpoint ?? gateway?.endpoint ?? "-"}
                </span>
              </span>
              <span className="text-muted-foreground">
                {t("tokenLabel")}:
                <span className="ml-1.5 text-foreground">
                  {health?.tokenConfigured
                    ? t("tokenConfigured")
                    : t("tokenMissing")}
                </span>
              </span>
              <span className="text-muted-foreground">
                {t("pidLabel")}:
                <span className="ml-1.5 font-mono text-foreground">
                  {gateway?.pid ?? "-"}
                </span>
              </span>
              <span className="text-muted-foreground sm:col-span-2">
                {t("startedAtLabel")}:
                <span className="ml-1.5 text-foreground">
                  {gateway?.lastStartedAt
                    ? new Date(gateway.lastStartedAt).toLocaleString()
                    : "-"}
                </span>
              </span>
            </div>
            {gateway?.lastError && (
              <p className="text-2xs text-red-400">
                {t("lastError")}: {gateway.lastError}
              </p>
            )}
            <div className="flex flex-wrap gap-1.5">
              <Button
                size="sm"
                variant="outline"
                className="h-7 text-xs"
                onClick={() => {
                  setBusy("reload")
                  reload().finally(() => setBusy(""))
                }}
                disabled={busy !== ""}
              >
                {busy === "reload" ? (
                  <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                ) : (
                  <RefreshCw className="mr-1 h-3.5 w-3.5" />
                )}
                {t("refresh")}
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-7 text-xs"
                onClick={() => runGatewayAction("start")}
                disabled={busy !== "" || gatewayRunning}
              >
                {busy === "start" && (
                  <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                )}
                {t("start")}
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-7 text-xs"
                onClick={() => runGatewayAction("stop")}
                disabled={busy !== "" || !gatewayRunning}
              >
                {busy === "stop" && (
                  <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                )}
                {t("stop")}
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-7 text-xs"
                onClick={() => runGatewayAction("restart")}
                disabled={busy !== ""}
              >
                {busy === "restart" && (
                  <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                )}
                {t("restart")}
              </Button>
            </div>
          </div>
        )}
      </section>

      {/* ----- providers ----- */}
      <section className="mt-5 space-y-2 px-3 md:px-4">
        <div className="flex items-center justify-between gap-2">
          <h2 className="text-xs font-medium text-muted-foreground">
            {t("providersTitle")}
            {providers.length > 0 && (
              <span className="ml-1.5">({providers.length})</span>
            )}
          </h2>
          <Button
            size="sm"
            className="h-8 text-xs"
            onClick={openAdd}
            disabled={loading || !!error}
          >
            <Plus className="mr-1 h-3.5 w-3.5" />
            {t("addProvider")}
          </Button>
        </div>

        {loading ? (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : providers.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
            <Server className="mb-2 h-8 w-8 opacity-40" />
            <span className="text-xs">{t("providersEmpty")}</span>
          </div>
        ) : (
          <div className="space-y-2">
            {providers.map((provider, index) => {
              const key = providerKey(provider, index)
              const isOpen = expanded.has(key)
              const credentials = providerCredentials(provider)
              const models = providerModels(provider)
              const disabledModels = readDisabledModels(key)
              const listedModels = [
                ...new Set([...models, ...disabledModels]),
              ].sort((a, b) => a.localeCompare(b))
              const usage = headerUsageBadge(snapshots[key] ?? [])
              const providerSnapshots = snapshots[key] ?? []
              return (
                <div key={key} className="rounded-md border">
                  <div className="flex items-center justify-between gap-3 px-3 py-2.5">
                    <button
                      type="button"
                      className="flex min-w-0 flex-1 items-center gap-2 text-left"
                      onClick={() => toggleExpand(key, provider.name)}
                    >
                      <ChevronRight
                        className={`h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform ${
                          isOpen ? "rotate-90" : ""
                        }`}
                      />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-sm font-medium">
                          {provider.name}
                        </span>
                        <span className="block truncate font-mono text-xs text-muted-foreground">
                          {providerBaseUrl(provider)}
                        </span>
                      </span>
                      <span className="flex shrink-0 flex-wrap items-center gap-1">
                        {provider.type && (
                          <Badge
                            variant="secondary"
                            className="text-3xs px-1.5 py-0"
                          >
                            {provider.type}
                          </Badge>
                        )}
                        <Badge
                          variant="outline"
                          className="text-3xs px-1.5 py-0"
                        >
                          {t("keysCount", { count: credentials.length })}
                        </Badge>
                        {usage && (
                          <Badge
                            variant="outline"
                            className="max-w-40 truncate text-3xs px-1.5 py-0"
                          >
                            {usage}
                          </Badge>
                        )}
                      </span>
                    </button>
                    <div className="flex shrink-0 gap-1">
                      <Button
                        size="icon"
                        variant="ghost"
                        className="h-7 w-7"
                        onClick={() => openEdit(index)}
                      >
                        <Pencil className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        size="icon"
                        variant="ghost"
                        className="h-7 w-7 text-destructive"
                        onClick={() => setDeleteIndex(index)}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  </div>

                  {isOpen && (
                    <div className="space-y-3 border-t px-3 py-3">
                      {/* 凭据概览（只读 + 启用开关） */}
                      <div>
                        <h3 className="mb-1.5 text-xs font-medium text-muted-foreground">
                          {t("credentialsTitle")}
                        </h3>
                        {credentials.length === 0 ? (
                          <p className="text-xs text-muted-foreground">
                            {t("credentialsEmpty")}
                          </p>
                        ) : (
                          <div className="rounded-md border">
                            {credentials.map((credential, credentialIndex) => (
                              <div
                                key={`${credential.id ?? credentialIndex}`}
                                className="flex items-center justify-between gap-3 border-b px-2.5 py-1.5 text-xs last:border-b-0"
                              >
                                <span className="flex min-w-0 items-center gap-2">
                                  <span className="w-5 shrink-0 text-right font-mono text-muted-foreground">
                                    {credentialPriority(
                                      credential,
                                      credentialIndex
                                    )}
                                  </span>
                                  <span className="truncate">
                                    {credentialName(
                                      credential,
                                      credentialIndex
                                    )}
                                  </span>
                                  <span className="shrink-0 font-mono text-muted-foreground">
                                    {keyTail(credentialKey(credential))}
                                  </span>
                                  {Number.isFinite(credential.weight) &&
                                    Number(credential.weight) !== 1 && (
                                      <Badge
                                        variant="outline"
                                        className="text-3xs px-1.5 py-0"
                                      >
                                        w{String(credential.weight)}
                                      </Badge>
                                    )}
                                  {limitsSummary(credential.limits) && (
                                    <Badge
                                      variant="outline"
                                      className="text-3xs px-1.5 py-0"
                                    >
                                      {limitsSummary(credential.limits)}
                                    </Badge>
                                  )}
                                </span>
                                <Switch
                                  checked={credential.enabled !== false}
                                  disabled={
                                    toggleBusy === `${index}:${credentialIndex}`
                                  }
                                  onCheckedChange={() =>
                                    toggleCredentialEnabled(
                                      index,
                                      credentialIndex
                                    )
                                  }
                                />
                              </div>
                            ))}
                          </div>
                        )}
                      </div>

                      {/* 用量 */}
                      <div>
                        <div className="mb-1.5 flex items-center justify-between">
                          <h3 className="text-xs font-medium text-muted-foreground">
                            {t("usageTitle")}
                          </h3>
                          <Button
                            size="icon"
                            variant="ghost"
                            className="h-6 w-6"
                            title={t("refreshUsage")}
                            disabled={snapshotBusy[key] === true}
                            onClick={() =>
                              loadSnapshots(key, provider.name, true)
                            }
                          >
                            {snapshotBusy[key] ? (
                              <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            ) : (
                              <RefreshCw className="h-3.5 w-3.5" />
                            )}
                          </Button>
                        </div>
                        {providerSnapshots.length === 0 ? (
                          <p className="text-xs text-muted-foreground">
                            {snapshotBusy[key] ? t("testing") : t("usageEmpty")}
                          </p>
                        ) : (
                          <div className="space-y-2">
                            {providerSnapshots.map(
                              (snapshot, snapshotIndex) => (
                                <div
                                  key={`${snapshot.credentialId ?? snapshotIndex}`}
                                  className="rounded-md border px-2.5 py-2"
                                >
                                  <div className="flex items-center justify-between gap-2 text-xs">
                                    <span className="truncate">
                                      {snapshot.credentialLabel ??
                                        snapshot.provider}
                                    </span>
                                    <Badge
                                      variant={
                                        snapshot.status === "ok"
                                          ? "secondary"
                                          : "destructive"
                                      }
                                      className="text-3xs px-1.5 py-0"
                                    >
                                      {snapshot.status}
                                    </Badge>
                                  </div>
                                  {(snapshot.meters ?? []).map((meter) => (
                                    <div
                                      key={meter.id}
                                      className="mt-1 flex items-center justify-between gap-2 text-xs"
                                    >
                                      <span className="truncate text-muted-foreground">
                                        {meter.label}
                                        {meter.window
                                          ? ` (${meter.window})`
                                          : ""}
                                      </span>
                                      <span className="shrink-0 font-mono">
                                        {meterText(meter)}
                                      </span>
                                    </div>
                                  ))}
                                  {snapshot.message && (
                                    <p className="mt-1 text-2xs text-muted-foreground">
                                      {snapshot.message}
                                    </p>
                                  )}
                                </div>
                              )
                            )}
                          </div>
                        )}
                      </div>

                      {/* 模型 */}
                      <div>
                        <div className="mb-1.5 flex items-center justify-between">
                          <h3 className="text-xs font-medium text-muted-foreground">
                            {t("modelsTitle")}
                            {listedModels.length > 0 && (
                              <span className="ml-1.5">
                                ({listedModels.length})
                              </span>
                            )}
                          </h3>
                          <Button
                            size="sm"
                            variant="ghost"
                            className="h-6 text-xs"
                            onClick={() => openPicker(index)}
                          >
                            <ListChecks className="mr-1 h-3.5 w-3.5" />
                            {t("selectModels")}
                          </Button>
                        </div>
                        {listedModels.length === 0 ? (
                          <p className="text-xs text-muted-foreground">
                            {t("modelsEmpty")}
                          </p>
                        ) : (
                          <div className="space-y-1">
                            {listedModels.map((model) => (
                              <div
                                key={model}
                                className="flex items-center justify-between gap-3 rounded-md border px-2.5 py-1.5"
                              >
                                <span className="truncate font-mono text-xs">
                                  {model}
                                </span>
                                <Switch
                                  checked={models.includes(model)}
                                  disabled={modelBusy !== ""}
                                  onCheckedChange={(checked) =>
                                    toggleModel(index, model, checked)
                                  }
                                />
                              </div>
                            ))}
                          </div>
                        )}
                        <Button
                          size="sm"
                          variant="outline"
                          className="mt-2 h-7 text-xs"
                          disabled={modelBusy !== ""}
                          onClick={() => fetchModels(index)}
                        >
                          {modelBusy === `fetch:${key}` ? (
                            <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <Download className="mr-1 h-3.5 w-3.5" />
                          )}
                          {t("fetchModels")}
                        </Button>
                      </div>
                    </div>
                  )}
                </div>
              )
            })}
          </div>
        )}
      </section>

      <footer className="mt-6 px-3 pb-4 md:px-4">
        <p className="text-2xs text-muted-foreground">{t("footerNote")}</p>
      </footer>

      {/* ----- 添加 / 编辑供应商（配置） ----- */}
      <Dialog
        open={dialogOpen}
        onOpenChange={(open) => {
          if (!open) setDialogOpen(false)
        }}
      >
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>
              {editingIndex === null ? t("addProvider") : t("editProvider")}
            </DialogTitle>
          </DialogHeader>
          <div className="max-h-[65vh] space-y-4 overflow-y-auto pr-1">
            <div className="space-y-1.5">
              <label className="text-xs font-medium">{t("presetLabel")}</label>
              <Select value={draft.presetId} onValueChange={applyPreset}>
                <SelectTrigger className="h-8 text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="custom">{t("presetCustom")}</SelectItem>
                  {(presets ?? []).map((preset) => (
                    <SelectItem key={preset.id} value={preset.id}>
                      {preset.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-2xs text-muted-foreground">
                {t("presetHint")}
              </p>
            </div>

            <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
              <div className="space-y-1.5">
                <label className="text-xs font-medium">{t("nameLabel")}</label>
                <Input
                  value={draft.name}
                  onChange={(event) =>
                    setDraft((prev) => ({ ...prev, name: event.target.value }))
                  }
                  placeholder="OpenCode Go"
                />
              </div>
              <div className="space-y-1.5">
                <label className="text-xs font-medium">
                  {t("baseUrlLabel")}
                </label>
                <Input
                  value={draft.baseUrl}
                  onChange={(event) =>
                    setDraft((prev) => ({
                      ...prev,
                      baseUrl: event.target.value,
                    }))
                  }
                  placeholder="https://api.example.com/v1"
                />
              </div>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">
                {t("protocolLabel")}
              </label>
              <Select
                value={draft.type}
                onValueChange={(value) =>
                  setDraft((prev) => ({ ...prev, type: value }))
                }
              >
                <SelectTrigger className="h-8 text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {CCR_PROTOCOLS.map((protocol) => (
                    <SelectItem key={protocol} value={protocol}>
                      {protocol}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            {/* 凭据池 */}
            <div className="space-y-2">
              <div className="flex items-center justify-between">
                <label className="text-xs font-medium">
                  {t("credentialsTitle")}
                </label>
                <div className="flex gap-1">
                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7 text-xs"
                    onClick={() => {
                      setImportText("")
                      setImportError(null)
                      setImportOpen(true)
                    }}
                  >
                    <FileJson className="mr-1 h-3.5 w-3.5" />
                    {t("importKeys")}
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7 text-xs"
                    onClick={addCredentialRow}
                  >
                    <Plus className="mr-1 h-3.5 w-3.5" />
                    {t("addKey")}
                  </Button>
                </div>
              </div>
              <div className="space-y-2">
                {draft.credentials.map((row, rowIndex) => (
                  <div
                    key={rowIndex}
                    className="space-y-2 rounded-md border px-2.5 py-2"
                  >
                    <div className="flex items-center gap-1.5">
                      <Input
                        value={row.name}
                        onChange={(event) =>
                          updateCredentialRow(rowIndex, {
                            name: event.target.value,
                          })
                        }
                        placeholder={`Key ${rowIndex + 1}`}
                        className="h-8 w-28 shrink-0 text-xs"
                      />
                      <div className="relative min-w-0 flex-1">
                        <Input
                          type={row.revealKey ? "text" : "password"}
                          value={row.apiKey}
                          onChange={(event) =>
                            updateCredentialRow(rowIndex, {
                              apiKey: event.target.value,
                            })
                          }
                          placeholder={t("apiKeyLabel")}
                          className="h-8 pr-8 text-xs"
                        />
                        <button
                          type="button"
                          className="absolute right-1.5 top-1/2 -translate-y-1/2 text-muted-foreground"
                          title={row.revealKey ? t("hideKey") : t("showKey")}
                          onClick={() =>
                            updateCredentialRow(rowIndex, {
                              revealKey: !row.revealKey,
                            })
                          }
                        >
                          {row.revealKey ? (
                            <EyeOff className="h-3.5 w-3.5" />
                          ) : (
                            <Eye className="h-3.5 w-3.5" />
                          )}
                        </button>
                      </div>
                      <Button
                        size="icon"
                        variant="ghost"
                        className="h-7 w-7 shrink-0"
                        title={t("advancedOptions")}
                        onClick={() =>
                          updateCredentialRow(rowIndex, {
                            advancedOpen: !row.advancedOpen,
                          })
                        }
                      >
                        <Settings2 className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        size="icon"
                        variant="ghost"
                        className="h-7 w-7 shrink-0"
                        disabled={rowIndex === 0}
                        onClick={() => moveCredentialRow(rowIndex, -1)}
                      >
                        <ArrowUp className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        size="icon"
                        variant="ghost"
                        className="h-7 w-7 shrink-0"
                        disabled={rowIndex === draft.credentials.length - 1}
                        onClick={() => moveCredentialRow(rowIndex, 1)}
                      >
                        <ArrowDown className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        size="icon"
                        variant="ghost"
                        className="h-7 w-7 shrink-0 text-destructive"
                        disabled={draft.credentials.length <= 1}
                        onClick={() => removeCredentialRow(rowIndex)}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                    {row.advancedOpen && (
                      <div className="space-y-2 border-t pt-2">
                        <div className="grid grid-cols-2 gap-2">
                          <div className="space-y-1">
                            <label className="text-2xs text-muted-foreground">
                              {t("priorityLabel")}
                            </label>
                            <Input
                              value={row.priority}
                              onChange={(event) =>
                                updateCredentialRow(rowIndex, {
                                  priority: event.target.value,
                                })
                              }
                              placeholder={String(rowIndex + 1)}
                              className="h-7 text-xs"
                            />
                          </div>
                          <div className="space-y-1">
                            <label className="text-2xs text-muted-foreground">
                              {t("weightLabel")}
                            </label>
                            <Input
                              value={row.weight}
                              onChange={(event) =>
                                updateCredentialRow(rowIndex, {
                                  weight: event.target.value,
                                })
                              }
                              placeholder="1"
                              className="h-7 text-xs"
                            />
                          </div>
                        </div>
                        <div className="space-y-1">
                          <label className="text-2xs text-muted-foreground">
                            {t("limitsLabel")}
                          </label>
                          <Textarea
                            value={row.limitsText}
                            onChange={(event) =>
                              updateCredentialRow(rowIndex, {
                                limitsText: event.target.value,
                              })
                            }
                            placeholder={t("limitsPlaceholder")}
                            className="min-h-14 font-mono text-xs"
                          />
                        </div>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">{t("modelsLabel")}</label>
              <Textarea
                value={draft.modelsText}
                onChange={(event) =>
                  setDraft((prev) => ({
                    ...prev,
                    modelsText: event.target.value,
                  }))
                }
                placeholder={"mimo-v2.6-pro\ndeepseek-v4-flash"}
                className="min-h-20 font-mono text-xs"
              />
            </div>

            <div className="space-y-1.5">
              <Button
                size="sm"
                variant="outline"
                className="h-7 text-xs"
                disabled={testing}
                onClick={testConnection}
              >
                {testing ? (
                  <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                ) : (
                  <PlugZap className="mr-1 h-3.5 w-3.5" />
                )}
                {testing ? t("testing") : t("testConnection")}
              </Button>
              {testResult?.ok === false && (
                <p className="text-2xs text-red-400">
                  {t("testFailed")}: {testResult.error}
                </p>
              )}
              {testResult?.ok === true && (
                <div className="space-y-0.5 text-2xs text-muted-foreground">
                  {(testResult.result.protocols ?? []).map((result) => (
                    <p key={`${result.protocol}-${result.endpoint}`}>
                      {result.supported ? "✓" : "✗"} {result.protocol}
                      {typeof result.status === "number"
                        ? ` · ${result.status}`
                        : ""}
                      {result.message ? ` · ${result.message}` : ""}
                    </p>
                  ))}
                  {(testResult.result.models ?? []).length > 0 && (
                    <p>
                      {t("testModelsFound", {
                        count: (testResult.result.models ?? []).length,
                      })}
                    </p>
                  )}
                </div>
              )}
            </div>

            {dialogError && (
              <div className="rounded-md border border-red-500/30 bg-red-500/5 px-3 py-2 text-xs text-red-400">
                {dialogError}
              </div>
            )}
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setDialogOpen(false)}
              disabled={saving}
            >
              {t("cancel")}
            </Button>
            <Button onClick={submitDraft} disabled={saving}>
              {saving && <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />}
              {editingIndex === null ? t("create") : t("save")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* ----- 导入 JSON ----- */}
      <Dialog
        open={importOpen}
        onOpenChange={(open) => {
          if (!open) setImportOpen(false)
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t("importTitle")}</DialogTitle>
          </DialogHeader>
          <div className="space-y-2">
            <p className="text-xs text-muted-foreground">
              {t("importDescription")}
            </p>
            <Textarea
              value={importText}
              onChange={(event) => setImportText(event.target.value)}
              placeholder={'[{"name":"Key 1","api_key":"sk-..."}]'}
              className="min-h-32 font-mono text-xs"
            />
            {importError && (
              <p className="text-2xs text-red-400">{importError}</p>
            )}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setImportOpen(false)}>
              {t("cancel")}
            </Button>
            <Button onClick={confirmImport}>{t("import")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* ----- 选择模型（双栏） ----- */}
      <Dialog
        open={pickerIndex !== null}
        onOpenChange={(open) => {
          if (!open) setPickerIndex(null)
        }}
      >
        <DialogContent className="sm:max-w-3xl">
          <DialogHeader>
            <DialogTitle>{t("selectModels")}</DialogTitle>
          </DialogHeader>
          <p className="text-xs text-muted-foreground">
            {t("pickerDescription")}
          </p>
          <div className="grid max-h-[60vh] grid-cols-1 gap-3 overflow-y-auto md:grid-cols-2">
            <div className="rounded-md border">
              <div className="flex items-center justify-between gap-2 border-b px-2.5 py-2">
                <div>
                  <p className="text-xs font-medium">{t("detectedTitle")}</p>
                  <p className="text-2xs text-muted-foreground">
                    {t("detectedHint")}
                  </p>
                </div>
                <div className="flex items-center gap-1.5">
                  <Button
                    size="icon"
                    variant="ghost"
                    className="h-6 w-6"
                    title={t("refresh")}
                    disabled={pickerLoading}
                    onClick={() => {
                      const provider =
                        pickerIndex !== null
                          ? providers[pickerIndex]
                          : undefined
                      if (provider) {
                        detectModels(provider).catch(console.error)
                      }
                    }}
                  >
                    {pickerLoading ? (
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <RefreshCw className="h-3.5 w-3.5" />
                    )}
                  </Button>
                  <Badge variant="outline" className="text-3xs px-1.5 py-0">
                    {pickerDetected.length}
                  </Badge>
                </div>
              </div>
              <div className="space-y-1.5 p-2">
                <div className="relative">
                  <Search className="absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    value={pickerSearchLeft}
                    onChange={(event) =>
                      setPickerSearchLeft(event.target.value)
                    }
                    placeholder={t("searchDetected")}
                    className="h-7 pl-7 text-xs"
                  />
                </div>
                {pickerError && (
                  <p className="text-2xs text-red-400">{pickerError}</p>
                )}
                <div className="space-y-1">
                  {filteredDetected.map((model) => {
                    const added = pickerEnabled.includes(model)
                    return (
                      <button
                        key={model}
                        type="button"
                        disabled={added || modelBusy !== ""}
                        onClick={() => addModelFromPicker(model)}
                        className="flex w-full items-center justify-between gap-2 rounded-md border px-2.5 py-1.5 text-left text-xs disabled:opacity-60"
                      >
                        <span className="truncate font-mono">{model}</span>
                        {added ? (
                          <span className="flex shrink-0 items-center gap-1 text-muted-foreground">
                            <Check className="h-3.5 w-3.5" />
                            {t("alreadyAdded")}
                          </span>
                        ) : (
                          <ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                        )}
                      </button>
                    )
                  })}
                </div>
              </div>
            </div>
            <div className="rounded-md border">
              <div className="flex items-center justify-between gap-2 border-b px-2.5 py-2">
                <div>
                  <p className="text-xs font-medium">{t("addedTitle")}</p>
                  <p className="text-2xs text-muted-foreground">
                    {t("addedHint")}
                  </p>
                </div>
                <Badge variant="outline" className="text-3xs px-1.5 py-0">
                  {pickerEnabled.length}
                </Badge>
              </div>
              <div className="space-y-1.5 p-2">
                <div className="flex gap-1.5">
                  <Input
                    value={customModel}
                    onChange={(event) => setCustomModel(event.target.value)}
                    placeholder={t("customPlaceholder")}
                    className="h-7 min-w-0 flex-1 text-xs"
                  />
                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7 shrink-0 text-xs"
                    disabled={!customModel.trim() || modelBusy !== ""}
                    onClick={addCustomModel}
                  >
                    <Plus className="mr-1 h-3.5 w-3.5" />
                    {t("addCustom")}
                  </Button>
                </div>
                <div className="relative">
                  <Search className="absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    value={pickerSearchRight}
                    onChange={(event) =>
                      setPickerSearchRight(event.target.value)
                    }
                    placeholder={t("searchAdded")}
                    className="h-7 pl-7 text-xs"
                  />
                </div>
                <div className="space-y-1">
                  {filteredAdded.map((model) => (
                    <div
                      key={model}
                      className="flex items-center justify-between gap-2 rounded-md border px-2.5 py-1.5 text-xs"
                    >
                      <span className="truncate font-mono">{model}</span>
                      <Button
                        size="icon"
                        variant="ghost"
                        className="h-6 w-6 shrink-0 text-destructive"
                        title={t("removeModel")}
                        disabled={modelBusy !== ""}
                        onClick={() => removeModelFromPicker(model)}
                      >
                        <X className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  ))}
                </div>
              </div>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setPickerIndex(null)}>
              {t("close")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* ----- 删除供应商 ----- */}
      <AlertDialog
        open={deleteIndex !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteIndex(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("deleteConfirmTitle")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("deleteConfirmMessage", {
                name:
                  deleteIndex !== null
                    ? (providers[deleteIndex]?.name ?? "")
                    : "",
              })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("cancel")}</AlertDialogCancel>
            <AlertDialogAction onClick={confirmDelete}>
              {t("delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </ScrollArea>
  )
}
