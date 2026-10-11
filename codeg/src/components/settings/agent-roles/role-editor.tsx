"use client"

import type { ReactNode } from "react"
import { Loader2, Save, Trash2 } from "lucide-react"
import { useTranslations } from "next-intl"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Textarea } from "@/components/ui/textarea"
import type { AgentRole } from "@/lib/agent-roles"
import type { AcpAgentInfo, LocalMcpServer } from "@/lib/types"

import {
  summarizeMcpServer,
  toggleId,
  type ModelGroup,
  type SkillsPanelState,
} from "./helpers"

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

function HintRow({
  text,
  loading = false,
}: {
  text: string
  loading?: boolean
}) {
  return (
    <div className="flex items-center gap-2 px-2 py-2 text-2xs text-muted-foreground">
      {loading ? <Loader2 className="h-3 w-3 animate-spin" /> : null}
      <span className="min-w-0">{text}</span>
    </div>
  )
}

function CheckRow({
  checked,
  title,
  subtitle,
  badge,
  onToggle,
}: {
  checked: boolean
  title: string
  subtitle?: string
  badge?: string
  onToggle: () => void
}) {
  return (
    <div
      role="button"
      tabIndex={0}
      aria-pressed={checked}
      onClick={onToggle}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault()
          onToggle()
        }
      }}
      className="flex w-full cursor-pointer items-start gap-2 px-2 py-2 text-left hover:bg-accent"
    >
      <Checkbox
        checked={checked}
        tabIndex={-1}
        aria-hidden
        className="pointer-events-none mt-0.5"
      />
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-1.5 text-xs font-medium">
          <span className="truncate">{title}</span>
          {badge ? (
            <Badge variant="outline" className="shrink-0 text-2xs font-normal">
              {badge}
            </Badge>
          ) : null}
        </span>
        {subtitle ? (
          <span className="mt-0.5 block truncate text-2xs text-muted-foreground">
            {subtitle}
          </span>
        ) : null}
      </span>
    </div>
  )
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
  const skillItems = skills.result?.skills ?? []
  const canSave =
    Boolean(draft.name.trim()) && Boolean(draft.agent_type) && !busy

  let skillBody: ReactNode
  if (!draft.agent_type) {
    skillBody = <HintRow text={t("skillPickBodyFirst")} />
  } else if (skills.loading && !skills.result) {
    skillBody = <HintRow text={t("loading")} loading />
  } else if (skills.result && !skills.result.supported) {
    skillBody = (
      <HintRow text={skills.result.message || t("skillUnsupported")} />
    )
  } else if (skillItems.length === 0) {
    skillBody = <HintRow text={t("skillEmpty")} />
  } else {
    skillBody = (
      <div className="divide-y">
        {skillItems.map((skill) => (
          <CheckRow
            key={skill.id}
            checked={draft.skills.includes(skill.id)}
            title={skill.name}
            subtitle={skill.description ?? undefined}
            badge={skill.read_only ? t("skillReadOnly") : undefined}
            onToggle={() =>
              onChange({ skills: toggleId(draft.skills, skill.id) })
            }
          />
        ))}
      </div>
    )
  }

  return (
    <div className="space-y-4">
      <section className="space-y-1.5">
        <Label htmlFor="agent-role-name">{t("nameLabel")}</Label>
        <Input
          id="agent-role-name"
          value={draft.name}
          placeholder={t("namePlaceholder")}
          onChange={(event) => onChange({ name: event.target.value })}
        />
      </section>

      <section className="space-y-3">
        <h2 className="text-xs font-medium text-muted-foreground">
          {t("documentsTitle")}
        </h2>
        <div className="space-y-1.5">
          <Label htmlFor="agent-role-soul">{t("soulLabel")}</Label>
          <Textarea
            id="agent-role-soul"
            value={draft.soul}
            placeholder={t("soulPlaceholder")}
            className="min-h-32 font-mono text-xs"
            onChange={(event) => onChange({ soul: event.target.value })}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="agent-role-rules">{t("rulesLabel")}</Label>
          <Textarea
            id="agent-role-rules"
            value={draft.rules}
            placeholder={t("rulesPlaceholder")}
            className="min-h-32 font-mono text-xs"
            onChange={(event) => onChange({ rules: event.target.value })}
          />
        </div>
      </section>

      <section className="space-y-3">
        <h2 className="text-xs font-medium text-muted-foreground">
          {t("bindingTitle")}
        </h2>
        <div className="grid gap-3 md:grid-cols-2">
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
            <Select
              value={draft.model}
              onValueChange={(value) => onChange({ model: value })}
              disabled={modelGroups.length === 0}
            >
              <SelectTrigger className="w-full">
                <SelectValue
                  placeholder={
                    modelGroups.length === 0
                      ? t("modelUnavailable")
                      : t("modelPlaceholder")
                  }
                />
              </SelectTrigger>
              <SelectContent>
                {modelGroups.map((group) => (
                  <SelectGroup key={group.provider}>
                    <SelectLabel>{group.provider}</SelectLabel>
                    {group.models.map((model) => (
                      <SelectItem
                        key={`${group.provider}::${model}`}
                        value={model}
                      >
                        {model}
                      </SelectItem>
                    ))}
                  </SelectGroup>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>
      </section>

      <section className="space-y-1.5">
        <h2 className="text-xs font-medium text-muted-foreground">
          {t("skillSectionTitle")}
        </h2>
        <p className="text-2xs text-muted-foreground">{t("skillHint")}</p>
        <div className="rounded-md border">{skillBody}</div>
      </section>

      <section className="space-y-1.5">
        <h2 className="text-xs font-medium text-muted-foreground">
          {t("mcpSectionTitle")}
        </h2>
        <p className="text-2xs text-muted-foreground">{t("mcpHint")}</p>
        <div className="rounded-md border">
          {mcpServers.length === 0 ? (
            <HintRow text={t("mcpEmpty")} />
          ) : (
            <div className="divide-y">
              {mcpServers.map((server) => (
                <CheckRow
                  key={server.id}
                  checked={draft.mcp.includes(server.id)}
                  title={server.id}
                  subtitle={summarizeMcpServer(server.spec) || undefined}
                  onToggle={() =>
                    onChange({ mcp: toggleId(draft.mcp, server.id) })
                  }
                />
              ))}
            </div>
          )}
        </div>
      </section>

      <div className="flex items-center justify-between pt-1">
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
