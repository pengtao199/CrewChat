// Pure helpers for the Agents settings page — no React, no transport.

import type { AgentRole } from "@/lib/agent-roles"
import {
  ccrAgentKindForBody,
  pickCcrProfileForKind,
  type CcrConfig,
  type CcrProfileSummary,
} from "@/lib/ccr"
import type { AgentSkillsListResult } from "@/lib/types"

/** A brand-new, unsaved role. `id` stays empty until the server assigns one. */
export function createEmptyRole(): AgentRole {
  return {
    id: "",
    name: "",
    soul: "",
    rules: "",
    agent_type: "",
    model: "",
    model_provider: "",
    skills: [],
    mcp: [],
    created_at: null,
    updated_at: null,
  }
}

export interface ModelGroup {
  provider: string
  models: string[]
}

/**
 * CCR providers → grouped model options for the picker. Providers without a
 * model list are skipped; duplicate model ids inside one provider are merged.
 */
export function buildModelGroups(
  config: CcrConfig | null | undefined
): ModelGroup[] {
  const providers = Array.isArray(config?.Providers) ? config.Providers : []
  const groups: ModelGroup[] = []
  for (const provider of providers) {
    const models = Array.from(
      new Set(
        (Array.isArray(provider.models) ? provider.models : [])
          .map((model) => String(model).trim())
          .filter(Boolean)
      )
    )
    if (models.length === 0) continue
    groups.push({
      provider: String(provider.name || provider.id || "provider"),
      models,
    })
  }
  return groups
}

/** One-line summary of a local MCP server spec (URL, or command + args). */
export function summarizeMcpServer(
  spec: Record<string, unknown> | null | undefined
): string {
  if (!spec) return ""
  const url = typeof spec.url === "string" ? spec.url.trim() : ""
  if (url) return url
  const command = typeof spec.command === "string" ? spec.command.trim() : ""
  if (!command) return ""
  const args = Array.isArray(spec.args)
    ? spec.args.map((arg) => String(arg))
    : []
  return [command, ...args].join(" ")
}

/** Toggle one id in a list without mutating the input. */
export function toggleId(list: string[], id: string): string[] {
  return list.includes(id) ? list.filter((item) => item !== id) : [...list, id]
}

/**
 * Which provider the model picker should open on: the stored one when it still
 * exists in CCR, else the provider that currently lists the stored model, else
 * nothing (the pair is stale and the model picker starts empty).
 */
export function resolveModelProvider(
  groups: ModelGroup[],
  modelProvider: string,
  model: string
): string {
  if (
    modelProvider &&
    groups.some((group) => group.provider === modelProvider)
  ) {
    return modelProvider
  }
  const hit = groups.find((group) => group.models.includes(model))
  return hit?.provider ?? ""
}

/** Local-skill panel state for the currently picked body. */
export interface SkillsPanelState {
  loading: boolean
  result: AgentSkillsListResult | null
}

/** [crewchat] The CCR binding state of a role's engine (body). Mirrors the
 * CLI page's readiness card: "unsupported" outranks "unreachable" because a
 * CLI outside CCR's registry can never bind, whether or not the server is
 * alive. "loading" while the page's single profile read is still unanswered,
 * and for the empty body of an unsaved role. */
export type RoleCcrState =
  | "loading"
  | "bound"
  | "bindable"
  | "unsupported"
  | "unreachable"

export function ccrStateForBody(
  bodyAgentType: string,
  profiles: CcrProfileSummary[] | null,
  unreachable: boolean
): RoleCcrState {
  if (!bodyAgentType) return "loading"
  const kind = ccrAgentKindForBody(bodyAgentType)
  if (!kind) return "unsupported"
  if (unreachable) return "unreachable"
  if (profiles === null) return "loading"
  return pickCcrProfileForKind(profiles, kind) ? "bound" : "bindable"
}

/** The configured profiles out of one CCR config read. */
export function profilesFromConfig(
  config: CcrConfig | null | undefined
): CcrProfileSummary[] {
  const profiles = config?.profile?.profiles
  return Array.isArray(profiles) ? profiles : []
}
