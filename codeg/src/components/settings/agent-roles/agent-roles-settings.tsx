"use client"

import { useCallback, useEffect, useMemo, useState } from "react"
import { Loader2, Plus, Trash2 } from "lucide-react"
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
  deleteAgentRole,
  listAgentRoles,
  saveAgentRole,
  type AgentRole,
} from "@/lib/agent-roles"
import { acpListAgentSkills, acpListAgents, mcpScanLocal } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import { ccrGetConfig } from "@/lib/ccr"
import type { AcpAgentInfo, AgentType, LocalMcpServer } from "@/lib/types"
import { cn } from "@/lib/utils"

import {
  buildModelGroups,
  createEmptyRole,
  type ModelGroup,
  type SkillsPanelState,
} from "./helpers"
import { RoleEditor } from "./role-editor"

/** The roster page: list on the left, the role editor on the right. */
export function AgentRolesSettings() {
  const t = useTranslations("AgentRoles")

  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [roles, setRoles] = useState<AgentRole[]>([])
  const [draft, setDraft] = useState<AgentRole | null>(null)
  const [busy, setBusy] = useState(false)
  const [deleteOpen, setDeleteOpen] = useState(false)

  const [bodies, setBodies] = useState<AcpAgentInfo[]>([])
  const [modelGroups, setModelGroups] = useState<ModelGroup[]>([])
  const [mcpServers, setMcpServers] = useState<LocalMcpServer[]>([])
  const [skills, setSkills] = useState<SkillsPanelState>({
    loading: false,
    result: null,
  })

  useEffect(() => {
    let cancelled = false
    async function load() {
      try {
        const [roleList, agentList, scan] = await Promise.all([
          listAgentRoles(),
          acpListAgents(),
          mcpScanLocal(),
        ])
        if (cancelled) return
        setRoles(roleList)
        setBodies(agentList)
        setMcpServers(scan.servers ?? [])
        setLoadError(null)
      } catch (error) {
        if (!cancelled) setLoadError(toErrorMessage(error))
      } finally {
        if (!cancelled) setLoading(false)
      }
      // CCR is best-effort: a dead gateway must not block the page.
      try {
        const config = await ccrGetConfig(1)
        if (!cancelled) setModelGroups(buildModelGroups(config))
      } catch {
        if (!cancelled) setModelGroups([])
      }
    }
    void load()
    return () => {
      cancelled = true
    }
  }, [])

  const agentType = draft?.agent_type ?? ""
  useEffect(() => {
    if (!agentType) {
      setSkills({ loading: false, result: null })
      return
    }
    let cancelled = false
    setSkills((current) => ({ loading: true, result: current.result }))
    acpListAgentSkills({ agentType: agentType as AgentType })
      .then((result) => {
        if (!cancelled) setSkills({ loading: false, result })
      })
      .catch(() => {
        if (!cancelled) setSkills({ loading: false, result: null })
      })
    return () => {
      cancelled = true
    }
  }, [agentType])

  const bodyNames = useMemo(() => {
    const map = new Map<string, string>()
    for (const body of bodies) map.set(body.agent_type, body.name)
    return map
  }, [bodies])

  const patchDraft = useCallback((patch: Partial<AgentRole>) => {
    setDraft((current) => (current ? { ...current, ...patch } : current))
  }, [])

  const handleSave = useCallback(async () => {
    if (!draft) return
    setBusy(true)
    try {
      const saved = await saveAgentRole(draft)
      setRoles((current) => {
        const exists = current.some((role) => role.id === saved.id)
        return exists
          ? current.map((role) => (role.id === saved.id ? saved : role))
          : [...current, saved]
      })
      setDraft(saved)
      toast.success(t("saved"))
    } catch (error) {
      toast.error(t("saveFailed", { detail: toErrorMessage(error) }))
    } finally {
      setBusy(false)
    }
  }, [draft, t])

  const handleDelete = useCallback(async () => {
    const id = draft?.id
    if (!id) return
    setBusy(true)
    try {
      await deleteAgentRole(id)
      setRoles((current) => current.filter((role) => role.id !== id))
      setDraft(null)
      toast.success(t("deleted"))
    } catch (error) {
      toast.error(t("saveFailed", { detail: toErrorMessage(error) }))
    } finally {
      setBusy(false)
      setDeleteOpen(false)
    }
  }, [draft?.id, t])

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        <Loader2 className="mr-2 h-4 w-4 animate-spin" />
        {t("loading")}
      </div>
    )
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="space-y-1 p-3 pb-1 md:p-4 md:pb-1">
        <h1 className="text-sm font-semibold">{t("sectionTitle")}</h1>
        <p className="text-xs text-muted-foreground">
          {t("sectionDescription")}
        </p>
        {loadError ? (
          <p className="text-xs text-destructive">
            {t("loadFailed", { detail: loadError })}
          </p>
        ) : null}
      </div>

      <div className="flex min-h-0 flex-1 gap-3 p-3 pt-2 md:p-4 md:pt-2">
        <aside className="flex w-60 shrink-0 flex-col gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="justify-start"
            onClick={() => setDraft(createEmptyRole())}
          >
            <Plus className="h-3.5 w-3.5" />
            {t("newAgent")}
          </Button>
          <ScrollArea className="min-h-0 flex-1 rounded-xl border bg-card">
            {roles.length === 0 ? (
              <p className="p-3 text-xs text-muted-foreground">
                {t("emptyHint")}
              </p>
            ) : (
              <div className="p-1.5">
                {roles.map((role) => (
                  <button
                    key={role.id}
                    type="button"
                    onClick={() => setDraft({ ...role })}
                    className={cn(
                      "w-full rounded-md px-2 py-1.5 text-left transition-colors hover:bg-muted/50",
                      draft?.id === role.id && "bg-muted"
                    )}
                  >
                    <span className="block truncate text-xs font-medium">
                      {role.name}
                    </span>
                    <span className="block truncate text-2xs text-muted-foreground">
                      {bodyNames.get(role.agent_type) ?? role.agent_type}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </ScrollArea>
        </aside>

        <section className="min-w-0 flex-1">
          <ScrollArea className="h-full rounded-xl border bg-card">
            {draft ? (
              <div className="p-4">
                <RoleEditor
                  draft={draft}
                  bodies={bodies}
                  modelGroups={modelGroups}
                  skills={skills}
                  mcpServers={mcpServers}
                  busy={busy}
                  canDelete={Boolean(draft.id)}
                  onChange={patchDraft}
                  onSave={() => {
                    void handleSave()
                  }}
                  onDelete={() => setDeleteOpen(true)}
                />
              </div>
            ) : (
              <div className="flex h-full items-center justify-center p-6 text-xs text-muted-foreground">
                {t("emptyHint")}
              </div>
            )}
          </ScrollArea>
        </section>
      </div>

      <AlertDialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("deleteTitle")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("deleteDescription")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("cancel")}</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                void handleDelete()
              }}
            >
              <Trash2 className="h-3.5 w-3.5" />
              {t("delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
