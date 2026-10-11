"use client"

import { useMemo } from "react"
import { Loader2, Save, Trash2 } from "lucide-react"
import { useTranslations } from "next-intl"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Textarea } from "@/components/ui/textarea"
import type { AgentRole } from "@/lib/agent-roles"
import type { AcpAgentInfo, LocalMcpServer } from "@/lib/types"

import {
  resolveModelProvider,
  summarizeMcpServer,
  type ModelGroup,
  type SkillsPanelState,
} from "./helpers"
import {
  SearchMultiSelect,
  type SearchMultiSelectOption,
} from "./search-multi-select"

interface RoleEditorProps {
  draft: AgentRole
  bodies: AcpAgentInfo[]
  modelGroups: ModelGroup[]
  skills: SkillsPanelState
  mcpServers: LocalMcpServer[]
  busy: boolean
  canDelete: boolean
  onChange: (patch: Partial<AgentRole>) => void
  onSave: () => void
  onDelete: () => void
}

export function RoleEditor({
  draft,
  bodies,
  modelGroups,
  skills,
  mcpServers,
  busy,
  canDelete,
  onChange,
  onSave,
  onDelete,
}: RoleEditorProps) {
  const t = useTranslations("AgentRoles")

  // Provider row of the model picker: the stored pair when it still exists in
  // CCR, else derived from the model, else empty. Never written back on open —
  // it is persisted once the user actually picks a provider or a model.
  const providerValue = useMemo(
    () => resolveModelProvider(modelGroups, draft.model_provider, draft.model),
    [modelGroups, draft.model_provider, draft.model]
  )
  const modelOptions = useMemo(() => {
    const group = modelGroups.find((item) => item.provider === providerValue)
    const models = group?.models ?? []
    // A stored model CCR no longer lists stays pickable, so opening an old role
    // never silently swaps its model for a placeholder.
    if (draft.model && !models.includes(draft.model)) {
      return [draft.model, ...models]
    }
    return models
  }, [modelGroups, providerValue, draft.model])

  const skillOptions: SearchMultiSelectOption[] = (
    skills.result?.skills ?? []
  ).map((skill) => ({
    id: skill.id,
    label: skill.name,
    description: skill.description ?? undefined,
    badge: skill.read_only ? t("skillReadOnly") : undefined,
  }))
  const mcpOptions: SearchMultiSelectOption[] = mcpServers.map((server) => ({
    id: server.id,
    label: server.id,
    description: summarizeMcpServer(server.spec) || undefined,
  }))

  const skillsLoading = skills.loading && !skills.result
  const skillPlaceholder = !draft.agent_type
    ? t("skillPickBodyFirst")
    : skillsLoading
      ? t("loading")
      : skills.result && !skills.result.supported
        ? t("skillUnsupported")
        : skillOptions.length === 0
          ? t("skillEmpty")
          : t("skillSelectPlaceholder")
  const skillDisabled =
    !draft.agent_type || skillsLoading || skillOptions.length === 0

  const mcpPlaceholder =
    mcpOptions.length === 0 ? t("mcpEmpty") : t("mcpSelectPlaceholder")

  const canSave =
    Boolean(draft.name.trim()) && Boolean(draft.agent_type) && !busy

  return (
    <div className="space-y-6">
      <section className="space-y-3">
        <h3 className="text-sm font-semibold">{t("basicSectionTitle")}</h3>
        <div className="space-y-3">
          <div className="space-y-1.5">
            <Label htmlFor="agent-role-name">{t("nameLabel")}</Label>
            <Input
              id="agent-role-name"
              value={draft.name}
              placeholder={t("namePlaceholder")}
              onChange={(event) => onChange({ name: event.target.value })}
            />
          </div>
          <div className="space-y-1.5">
            <Label>{t("bodyLabel")}</Label>
            <Select
              value={draft.agent_type}
              onValueChange={(value) => onChange({ agent_type: value })}
            >
              <SelectTrigger className="w-full">
                <SelectValue placeholder={t("bodyPlaceholder")} />
              </SelectTrigger>
              <SelectContent>
                {bodies.map((body) => (
                  <SelectItem key={body.agent_type} value={body.agent_type}>
                    {body.enabled
                      ? body.name
                      : `${body.name} ${t("bodyDisabled")}`}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-1.5">
            <Label>{t("modelLabel")}</Label>
            <div className="grid grid-cols-2 gap-2">
              <Select
                value={providerValue}
                onValueChange={(value) =>
                  onChange({ model_provider: value, model: "" })
                }
                disabled={modelGroups.length === 0}
              >
                <SelectTrigger className="w-full">
                  <SelectValue placeholder={t("providerPlaceholder")} />
                </SelectTrigger>
                <SelectContent>
                  {modelGroups.map((group) => (
                    <SelectItem key={group.provider} value={group.provider}>
                      {group.provider}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Select
                value={draft.model}
                onValueChange={(value) =>
                  onChange({ model: value, model_provider: providerValue })
                }
                disabled={modelGroups.length === 0 || !providerValue}
              >
                <SelectTrigger className="w-full">
                  <SelectValue placeholder={t("modelPlaceholder")} />
                </SelectTrigger>
                <SelectContent>
                  {modelOptions.map((model) => (
                    <SelectItem key={model} value={model}>
                      {model}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            {modelGroups.length === 0 ? (
              <p className="text-2xs text-muted-foreground">
                {t("modelUnavailable")}
              </p>
            ) : null}
          </div>
        </div>
      </section>

      <section className="space-y-2">
        <div className="space-y-1">
          <h3 className="text-sm font-semibold">{t("skillSectionTitle")}</h3>
          <p className="text-xs leading-5 text-muted-foreground">
            {t("skillHint")}
          </p>
        </div>
        <SearchMultiSelect
          options={skillOptions}
          selected={draft.skills}
          onChange={(next) => onChange({ skills: next })}
          placeholder={skillPlaceholder}
          disabled={skillDisabled}
        />
      </section>

      <section className="space-y-2">
        <div className="space-y-1">
          <h3 className="text-sm font-semibold">{t("mcpSectionTitle")}</h3>
          <p className="text-xs leading-5 text-muted-foreground">
            {t("mcpHint")}
          </p>
        </div>
        <SearchMultiSelect
          options={mcpOptions}
          selected={draft.mcp}
          onChange={(next) => onChange({ mcp: next })}
          placeholder={mcpPlaceholder}
          disabled={mcpOptions.length === 0}
        />
      </section>

      <section className="space-y-2">
        <div className="space-y-1">
          <h3 className="text-sm font-semibold">{t("soulLabel")}</h3>
          <p className="text-xs leading-5 text-muted-foreground">
            {t("soulPlaceholder")}
          </p>
        </div>
        <Textarea
          value={draft.soul}
          className="min-h-32 font-mono text-xs"
          onChange={(event) => onChange({ soul: event.target.value })}
        />
      </section>

      <section className="space-y-2">
        <div className="space-y-1">
          <h3 className="text-sm font-semibold">{t("rulesLabel")}</h3>
          <p className="text-xs leading-5 text-muted-foreground">
            {t("rulesPlaceholder")}
          </p>
        </div>
        <Textarea
          value={draft.rules}
          className="min-h-32 font-mono text-xs"
          onChange={(event) => onChange({ rules: event.target.value })}
        />
      </section>

      <div className="flex items-center justify-between border-t pt-4">
        {canDelete ? (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={onDelete}
            disabled={busy}
          >
            <Trash2 className="h-3.5 w-3.5" />
            {t("delete")}
          </Button>
        ) : (
          <span />
        )}
        <Button type="button" size="sm" onClick={onSave} disabled={!canSave}>
          {busy ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <Save className="h-3.5 w-3.5" />
          )}
          {t("save")}
        </Button>
      </div>
    </div>
  )
}
