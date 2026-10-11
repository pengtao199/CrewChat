"use client"

import { useCallback, useEffect, useMemo, useState } from "react"
import { Loader2, Pencil, Plus, RefreshCw, Server, Trash2 } from "lucide-react"
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
import { Textarea } from "@/components/ui/textarea"
import {
  CCR_PROTOCOLS,
  ccrGatewayStatus,
  ccrGetConfig,
  ccrHealth,
  ccrRestartGateway,
  ccrSaveConfig,
  ccrStartGateway,
  ccrStopGateway,
  type CcrConfig,
  type CcrGatewayStatus,
  type CcrHealth,
  type CcrProvider,
} from "@/lib/ccr"

interface ProviderDraft {
  name: string
  baseUrl: string
  apiKey: string
  type: string
  modelsText: string
}

const EMPTY_DRAFT: ProviderDraft = {
  name: "",
  baseUrl: "",
  apiKey: "",
  type: "openai_chat_completions",
  modelsText: "",
}

function providerBaseUrl(provider: CcrProvider): string {
  return provider.baseUrl ?? provider.api_base_url ?? ""
}

function providerModels(provider: CcrProvider): string[] {
  return Array.isArray(provider.models) ? provider.models : []
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

export function CcrModelSettings() {
  const t = useTranslations("CcrModelSettings")
  const [health, setHealth] = useState<CcrHealth | null>(null)
  const [gateway, setGateway] = useState<CcrGatewayStatus | null>(null)
  const [config, setConfig] = useState<CcrConfig | null>(null)
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState("")
  const [error, setError] = useState<string | null>(null)

  const [dialogOpen, setDialogOpen] = useState(false)
  const [editingIndex, setEditingIndex] = useState<number | null>(null)
  const [draft, setDraft] = useState<ProviderDraft>(EMPTY_DRAFT)
  const [dialogError, setDialogError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [deleteIndex, setDeleteIndex] = useState<number | null>(null)

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

  const allModels = useMemo(() => {
    const rows: Array<{ model: string; provider: string }> = []
    for (const provider of providers) {
      for (const model of providerModels(provider)) {
        rows.push({ model, provider: provider.name })
      }
    }
    return rows.sort((a, b) => a.model.localeCompare(b.model))
  }, [providers])

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
        const status = await ccrGatewayStatus()
        setGateway(status)
      } catch (err: unknown) {
        toast.error(errorText(err), { description: t("gatewayActionFailed") })
      } finally {
        setBusy("")
      }
    },
    [t]
  )

  const openAdd = useCallback(() => {
    setEditingIndex(null)
    setDraft(EMPTY_DRAFT)
    setDialogError(null)
    setDialogOpen(true)
  }, [])

  const openEdit = useCallback(
    (index: number) => {
      const provider = providers[index]
      if (!provider) return
      setEditingIndex(index)
      setDraft({
        name: provider.name,
        baseUrl: providerBaseUrl(provider),
        apiKey: "",
        type: provider.type ?? "openai_chat_completions",
        modelsText: providerModels(provider).join("\n"),
      })
      setDialogError(null)
      setDialogOpen(true)
    },
    [providers]
  )

  const submitDraft = useCallback(async () => {
    const name = draft.name.trim()
    if (!name) {
      setDialogError(t("nameRequired"))
      return
    }
    const models = parseModels(draft.modelsText)
    const baseUrl = draft.baseUrl.trim()
    const apiKey = draft.apiKey.trim()

    setSaving(true)
    setDialogError(null)
    try {
      const current = config ?? (await ccrGetConfig())
      const list = [...(current.Providers ?? [])]
      if (editingIndex === null) {
        const entry: CcrProvider = {
          id: slugId(name),
          name,
          type: draft.type,
          enabled: true,
          models,
        }
        if (baseUrl) entry.baseUrl = baseUrl
        if (apiKey) entry.apiKey = apiKey
        list.push(entry)
      } else {
        const existing = list[editingIndex]
        if (!existing) throw new Error(t("saveFailed"))
        const entry: CcrProvider = {
          ...existing,
          name,
          type: draft.type,
          models,
        }
        if (baseUrl) entry.baseUrl = baseUrl
        if (apiKey) entry.apiKey = apiKey
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

  const gatewayState = gateway?.state ?? "unknown"
  const gatewayRunning = gatewayState === "running"

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

      <section className="mt-4 space-y-2 px-3 md:px-4">
        <h2 className="text-xs font-medium text-muted-foreground">
          {t("statusTitle")}
        </h2>
        <div className="space-y-2 rounded-md border px-3 py-2.5">
          <div className="flex flex-wrap items-center gap-x-6 gap-y-1.5 text-xs">
            <span className="text-muted-foreground">
              {t("connectionLabel")}:
              <span className="ml-1.5 font-mono text-foreground">
                {health?.url ?? "-"}
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
            <span className="flex items-center gap-1.5 text-muted-foreground">
              {t("gatewayLabel")}:
              <Badge
                variant={gatewayRunning ? "secondary" : "destructive"}
                className="text-3xs px-1.5 py-0"
              >
                {gatewayRunning
                  ? t("gatewayRunning")
                  : gatewayState === "stopped"
                    ? t("gatewayStopped")
                    : t("gatewayError")}
              </Badge>
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
      </section>

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
            {providers.map((provider, index) => (
              <div
                key={`${provider.id ?? provider.name}-${index}`}
                className="flex items-center justify-between gap-3 rounded-md border px-3 py-2.5"
              >
                <div className="flex min-w-0 flex-1 items-center gap-3">
                  <div className="min-w-0 space-y-0.5">
                    <div className="truncate text-sm font-medium">
                      {provider.name}
                    </div>
                    <div className="truncate font-mono text-xs text-muted-foreground">
                      {providerBaseUrl(provider)}
                    </div>
                  </div>
                  <div className="flex shrink-0 gap-1">
                    {provider.type && (
                      <Badge
                        variant="secondary"
                        className="text-3xs px-1.5 py-0"
                      >
                        {provider.type}
                      </Badge>
                    )}
                    <Badge variant="outline" className="text-3xs px-1.5 py-0">
                      {t("modelCount", {
                        count: providerModels(provider).length,
                      })}
                    </Badge>
                  </div>
                </div>
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
            ))}
          </div>
        )}
      </section>

      <section className="mt-5 space-y-2 px-3 md:px-4">
        <h2 className="text-xs font-medium text-muted-foreground">
          {t("modelsTitle")}
          {allModels.length > 0 && (
            <span className="ml-1.5">({allModels.length})</span>
          )}
        </h2>
        {allModels.length === 0 ? (
          <p className="py-4 text-center text-xs text-muted-foreground">
            {t("modelsEmpty")}
          </p>
        ) : (
          <div className="rounded-md border">
            {allModels.map((row) => (
              <div
                key={`${row.provider}/${row.model}`}
                className="flex items-center justify-between gap-3 border-b px-3 py-1.5 text-xs last:border-b-0"
              >
                <span className="truncate font-mono">{row.model}</span>
                <span className="shrink-0 text-muted-foreground">
                  {row.provider}
                </span>
              </div>
            ))}
          </div>
        )}
      </section>

      <footer className="mt-6 px-3 pb-4 md:px-4">
        <p className="text-2xs text-muted-foreground">{t("footerNote")}</p>
      </footer>

      <Dialog
        open={dialogOpen}
        onOpenChange={(open) => {
          if (!open) setDialogOpen(false)
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>
              {editingIndex === null ? t("addProvider") : t("editProvider")}
            </DialogTitle>
          </DialogHeader>
          <div className="space-y-4">
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
              <label className="text-xs font-medium">{t("baseUrlLabel")}</label>
              <Input
                value={draft.baseUrl}
                onChange={(event) =>
                  setDraft((prev) => ({ ...prev, baseUrl: event.target.value }))
                }
                placeholder="https://api.example.com/v1"
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-medium">{t("apiKeyLabel")}</label>
              <Input
                type="password"
                value={draft.apiKey}
                onChange={(event) =>
                  setDraft((prev) => ({ ...prev, apiKey: event.target.value }))
                }
                placeholder={
                  editingIndex === null ? "sk-..." : t("apiKeyKeepHint")
                }
              />
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
