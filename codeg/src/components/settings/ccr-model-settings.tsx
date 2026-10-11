"use client"

import { useCallback, useEffect, useMemo, useState } from "react"
import { Loader2, Plus, Server } from "lucide-react"
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
import { Button } from "@/components/ui/button"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  ccrGetConfig,
  ccrGetProviderAccountSnapshots,
  ccrGetProviderPresets,
  ccrGatewayStatus,
  ccrHealth,
  ccrRestartGateway,
  ccrSaveConfig,
  ccrStartGateway,
  ccrStopGateway,
  type CcrAccountSnapshot,
  type CcrConfig,
  type CcrGatewayStatus,
  type CcrHealth,
  type CcrProvider,
  type CcrProviderPreset,
} from "@/lib/ccr"
import { readDisabledModels, writeDisabledModels } from "@/lib/ccr-model-memory"
import { GatewayCard } from "./ccr/gateway-card"
import { ProviderCard } from "./ccr/provider-card"
import { ProviderDialog } from "./ccr/provider-dialog"
import {
  type ProviderDraft,
  buildProviderEntry,
  draftFromProvider,
  emptyDraft,
  errorText,
  providerCredentials,
  providerModels,
} from "./ccr/helpers"

export function CcrModelSettings() {
  const t = useTranslations("CcrModelSettings")

  const [health, setHealth] = useState<CcrHealth | null>(null)
  const [gateway, setGateway] = useState<CcrGatewayStatus | null>(null)
  const [config, setConfig] = useState<CcrConfig | null>(null)
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState("")
  const [error, setError] = useState<string | null>(null)

  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [snapshots, setSnapshots] = useState<
    Record<string, CcrAccountSnapshot[]>
  >({})
  const [snapshotBusy, setSnapshotBusy] = useState<Record<string, boolean>>({})

  const [dialogOpen, setDialogOpen] = useState(false)
  const [editingIndex, setEditingIndex] = useState<number | null>(null)
  const [dialogDraft, setDialogDraft] = useState<ProviderDraft>(emptyDraft)
  const [presets, setPresets] = useState<CcrProviderPreset[] | null>(null)

  const [deleteIndex, setDeleteIndex] = useState<number | null>(null)
  const [toggleBusy, setToggleBusy] = useState("")
  const [modelBusy, setModelBusy] = useState("")

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
    (provider: CcrProvider, index: number) => {
      const key = providerKey(provider, index)
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
        loadSnapshots(key, provider.name).catch(console.error)
      }
    },
    [expanded, loadSnapshots, providerKey, snapshots]
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

  const requestPresets = useCallback(() => {
    if (presets === null) {
      loadPresets().catch(console.error)
    }
  }, [loadPresets, presets])

  const openAdd = useCallback(() => {
    setEditingIndex(null)
    setDialogDraft(emptyDraft())
    setDialogOpen(true)
  }, [])

  const openEdit = useCallback(
    (index: number) => {
      const provider = providers[index]
      if (!provider) return
      setEditingIndex(index)
      setDialogDraft(draftFromProvider(provider))
      setDialogOpen(true)
    },
    [providers]
  )

  const submitDraft = useCallback(
    async (draft: ProviderDraft) => {
      const current = config ?? (await ccrGetConfig())
      const list = [...(current.Providers ?? [])]
      const existing = editingIndex !== null ? list[editingIndex] : undefined
      const entry = buildProviderEntry(existing, draft)
      if (editingIndex === null) {
        list.push(entry)
      } else {
        list[editingIndex] = entry
      }
      await ccrSaveConfig({ ...current, Providers: list })
      toast.success(t("saved"))
      setDialogOpen(false)
      await reload()
    },
    [config, editingIndex, reload, t]
  )

  const toggleCredentialEnabled = useCallback(
    async (providerIndex: number, credentialIndex: number) => {
      setToggleBusy(`${providerIndex}:${credentialIndex}`)
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

      <GatewayCard
        health={health}
        gateway={gateway}
        busy={busy}
        onRefresh={() => {
          setBusy("reload")
          reload().finally(() => setBusy(""))
        }}
        onAction={(action) => {
          runGatewayAction(action).catch(console.error)
        }}
      />

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
              return (
                <ProviderCard
                  key={key}
                  provider={provider}
                  index={index}
                  expanded={expanded.has(key)}
                  snapshots={snapshots[key]}
                  snapshotBusy={snapshotBusy[key] === true}
                  modelBusy={modelBusy}
                  toggleBusy={toggleBusy}
                  disabledModels={readDisabledModels(key)}
                  onToggleExpand={() => toggleExpand(provider, index)}
                  onEdit={() => openEdit(index)}
                  onDelete={() => setDeleteIndex(index)}
                  onToggleModel={(model, enable) => {
                    toggleModel(index, model, enable).catch(console.error)
                  }}
                  onToggleCredential={(credentialIndex) => {
                    toggleCredentialEnabled(index, credentialIndex).catch(
                      console.error
                    )
                  }}
                  onLoadSnapshots={(force) => {
                    loadSnapshots(key, provider.name, force).catch(
                      console.error
                    )
                  }}
                />
              )
            })}
          </div>
        )}
      </section>

      <footer className="mt-6 px-3 pb-4 md:px-4">
        <p className="text-2xs text-muted-foreground">{t("footerNote")}</p>
      </footer>

      <ProviderDialog
        open={dialogOpen}
        editing={editingIndex !== null}
        initialDraft={dialogDraft}
        presets={presets}
        onOpenChange={setDialogOpen}
        onRequestPresets={requestPresets}
        onSubmit={submitDraft}
      />

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
