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
import { Switch } from "@/components/ui/switch"
import { useAgentOptions } from "@/components/automations/use-agent-options"
import type { AgentRole, BodyModelFace } from "@/lib/agent-roles"
import type { AcpAgentInfo, AgentType, LocalMcpServer } from "@/lib/types"

import {
  faceIncludesModel,
  permissionModeOptions,
  resolveModelProvider,
  summarizeMcpServer,
  type ModelGroup,
  type RoleCcrState,
  type SkillsPanelState,
} from "./helpers"
import {
  SearchMultiSelect,
  type SearchMultiSelectOption,
} from "./search-multi-select"

/** Radix reserves the empty string for "nothing selected", so the row meaning
 *  "follow the CLI's own default" needs a sentinel; it maps back to `""` (the
 *  roster's "no override") on save. */
const PERMISSION_MODE_DEFAULT = "__cli_default__"

interface RoleEditorProps {
  draft: AgentRole
  bodies: AcpAgentInfo[]
  modelGroups: ModelGroup[]
  skills: SkillsPanelState
  mcpServers: LocalMcpServer[]
  busy: boolean
  canDelete: boolean
  /** [crewchat] CCR binding state of the draft's engine — gates the model
   * picker and decides which hint (bind / unsupported / retry) shows below
   * it. */
  ccrState: RoleCcrState
  /**
   * [crewchat] What THIS engine's gateway face offers. `null` while it is being
   * read. An unsupported engine (`supported: false`) cannot be pinned at all,
   * so the picker locks and says why instead of offering a choice the launcher
   * would ignore.
   */
  modelFace: BodyModelFace | null
  onRetryCcr: () => void
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
  ccrState,
  modelFace,
  onRetryCcr,
  onChange,
  onSave,
  onDelete,
}: RoleEditorProps) {
  const t = useTranslations("AgentRoles")

  // [crewchat] Phase 4.6 — the body's advertised permission presets. A probe is
  // a transient CLI spawn, so it waits for a body to be chosen; a body that
  // publishes none leaves the user with "follow the CLI's own default", which
  // is the honest option (it changes nothing).
  const { snapshot: agentOptions, loading: optionsLoading } = useAgentOptions(
    (draft.agent_type || "") as AgentType,
    null,
    Boolean(draft.agent_type)
  )
  const modeOptions = useMemo(
    () => permissionModeOptions(agentOptions),
    [agentOptions]
  )

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

  // [crewchat] The model picker is CCR's face on this page: it gates on the
  // engine's binding state so a role can't be pointed at a model the engine
  // cannot reach yet. "bound" (and the transient "loading") leaves it open.
  const modelGated =
    ccrState === "bindable" ||
    ccrState === "unsupported" ||
    ccrState === "unreachable"
  // [crewchat] The second gate is about the ENGINE, not CCR's binding: a body
  // with no overlay cannot be pinned to any model, so its picker is locked and
  // explained. `supported: true` with an empty list means "could not determine"
  // (CCR unreachable) — that case keeps the picker open and says nothing extra,
  // because the CCR hint below already covers it.
  const engineUnpinnable = modelFace != null && !modelFace.supported
  const modelPickerDisabled =
    modelGroups.length === 0 || modelGated || engineUnpinnable
  // [crewchat] A stored pair the engine's face does not offer is what makes the
  // roster page lie today: the value saves, and the launcher quietly runs the
  // CLI's own model. Say so, without touching the pick (an old role must keep
  // the model it has).
  const modelUnreachable =
    !engineUnpinnable &&
    modelFace?.supported === true &&
    Boolean(draft.model) &&
    !faceIncludesModel(modelFace, providerValue, draft.model)
  const modelGateText =
    ccrState === "bindable"
      ? t("ccrGatedBindable")
      : ccrState === "unreachable"
        ? t("ccrGatedUnreachable")
        : t("ccrGatedUnsupported")
  const goCliPage = () => {
    // [crewchat] Plain location navigation (like the settings shell's own
    // transitions) carrying the query string; `agent` preselects this body on
    // the CLI page.
    if (typeof window === "undefined") return
    const search = window.location.search
    const query = `agent=${encodeURIComponent(draft.agent_type)}`
    window.location.assign(
      `/settings/agents?${query}${search ? `&${search.slice(1)}` : ""}`
    )
  }

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
                disabled={modelPickerDisabled}
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
                disabled={
                  modelGroups.length === 0 ||
                  !providerValue ||
                  modelGated ||
                  engineUnpinnable
                }
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
            {engineUnpinnable ? (
              // The engine itself is the blocker, so this hint outranks the CCR
              // one: binding CCR would not help.
              <p className="text-2xs text-muted-foreground">
                {t("modelUnsupported")}
                {modelFace?.reason ? (
                  <span className="ml-1 opacity-70" title={modelFace.reason}>
                    （{modelFace.reason}）
                  </span>
                ) : null}
              </p>
            ) : modelUnreachable ? (
              <p className="text-2xs text-amber-500">{t("modelUnreachable")}</p>
            ) : null}
            {modelGated && !engineUnpinnable ? (
              <p className="flex flex-wrap items-center gap-2 text-2xs text-muted-foreground">
                <span>{modelGateText}</span>
                {ccrState === "bindable" && (
                  <button
                    type="button"
                    className="text-primary hover:underline"
                    onClick={goCliPage}
                  >
                    {t("ccrGoCliPage")} →
                  </button>
                )}
                {ccrState === "unreachable" && (
                  <button
                    type="button"
                    className="text-primary hover:underline"
                    onClick={onRetryCcr}
                  >
                    {t("ccrRetry")}
                  </button>
                )}
              </p>
            ) : modelGroups.length === 0 ? (
              <p className="text-2xs text-muted-foreground">
                {t("modelUnavailable")}
              </p>
            ) : null}
          </div>
        </div>
      </section>

      {/* [crewchat] Phase 4.6 — permissions belong to the AGENT, not the
          conversation: the preset its sessions start in, and whether they may
          ask at all. Both were previously decided in the chat (a composer mode
          picker plus a permission card per request), which interrupted work and
          made the same Agent behave differently from one conversation to the
          next. */}
      <section className="space-y-3">
        <div className="space-y-1">
          <h3 className="text-sm font-semibold">
            {t("permissionSectionTitle")}
          </h3>
          <p className="text-xs leading-5 text-muted-foreground">
            {t("permissionSectionHint")}
          </p>
        </div>
        <div className="space-y-1.5">
          <Label>{t("permissionModeLabel")}</Label>
          <Select
            value={draft.permission_mode || PERMISSION_MODE_DEFAULT}
            onValueChange={(value) =>
              onChange({
                permission_mode: value === PERMISSION_MODE_DEFAULT ? "" : value,
              })
            }
          >
            <SelectTrigger className="w-full">
              <SelectValue placeholder={t("permissionModeDefault")} />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={PERMISSION_MODE_DEFAULT}>
                {t("permissionModeDefault")}
              </SelectItem>
              {modeOptions.map((option) => (
                <SelectItem key={option.id} value={option.id}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <p className="text-2xs text-muted-foreground">
            {optionsLoading
              ? t("permissionModeLoading")
              : t("permissionModeHint")}
          </p>
        </div>
        <div className="flex items-start gap-2">
          <Switch
            id="agent-role-auto-approve"
            checked={draft.auto_approve}
            onCheckedChange={(checked) => onChange({ auto_approve: checked })}
          />
          <div className="space-y-0.5">
            <Label htmlFor="agent-role-auto-approve">
              {t("autoApproveLabel")}
            </Label>
            <p className="text-2xs text-muted-foreground">
              {t("autoApproveHint")}
            </p>
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
