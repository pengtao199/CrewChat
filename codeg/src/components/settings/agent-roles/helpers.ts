// Pure helpers for the Agents settings page — no React, no transport.

import type { AgentRole, BodyModelFace } from "@/lib/agent-roles"
import {
  ccrAgentKindForBody,
  pickCcrProfileForKind,
  type CcrConfig,
  type CcrProfileSummary,
} from "@/lib/ccr"
import type { AgentOptionsSnapshot, AgentSkillsListResult } from "@/lib/types"

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
    permission_mode: "",
    auto_approve: false,
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

/**
 * [crewchat] The permission presets a body advertises, as select options
 * (Phase 4.6). Empty when the body published none — the editor then offers only
 * "follow the CLI's own default", which is the honest choice: an Agent that
 * says nothing about permissions must not silently change what the CLI does.
 */
export function permissionModeOptions(
  snapshot: AgentOptionsSnapshot | null
): Array<{ id: string; label: string }> {
  const modes = snapshot?.modes?.available_modes ?? []
  return modes.map((mode) => ({ id: mode.id, label: mode.name }))
}

/**
 * [crewchat] The roster ROW's verdict on a role's model pick — what used to be
 * invisible until you opened the role.
 *
 * `unknown` is the honest default for everything we cannot judge: no face read
 * yet, a body with no model configured, or an undetermined face (CCR
 * unreachable, which the CCR badge on the same row already reports). Only
 * `unsupported` and `unreachable` are worth a badge, and they are exactly the
 * two cases where the Agents page would otherwise look like it works.
 */
export type RoleModelFaceState =
  | "unknown"
  | "unsupported"
  | "unreachable"
  | "ok"

export function roleModelFaceState(
  face: BodyModelFace | null | undefined,
  provider: string,
  model: string
): RoleModelFaceState {
  if (!face) return "unknown"
  if (!face.supported) return "unsupported"
  // No model configured is the role's own business (the CLI default applies),
  // and an empty list means "could not determine" — neither is a lie to flag.
  if (!model.trim() || face.models.length === 0) return "unknown"
  return faceIncludesModel(face, provider, model) ? "ok" : "unreachable"
}

/**
 * [crewchat] Is the stored `(provider, model)` pair one the engine's face
 * actually offers?
 *
 * The roster may hold the provider as an id or as a display name (the backend
 * accepts both), so either form counts as a match. An EMPTY provider matches on
 * the model alone — the backend resolves a provider-less model across CCR's
 * providers, so the page must not call that unreachable just because the stored
 * provider is empty.
 *
 * `false` for an unsupported engine (no overlay ⇒ nothing is reachable) and for
 * an undetermined face (`models: []`, CCR unreachable) — in the second case the
 * caller must use `supported` to tell "cannot know" from "not offered".
 */
export function faceIncludesModel(
  face: BodyModelFace | null,
  provider: string,
  model: string
): boolean {
  if (!face?.supported) return false
  const wantedModel = model.trim()
  if (!wantedModel) return false
  const wantedProvider = provider.trim()
  return face.models.some(
    (option) =>
      option.model === wantedModel &&
      (!wantedProvider ||
        option.provider === wantedProvider ||
        option.provider_id === wantedProvider)
  )
}
