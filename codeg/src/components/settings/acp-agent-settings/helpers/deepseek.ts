import type { AcpAgentInfo } from "@/lib/types"
import { DEEPSEEK_PANEL_ENV_KEYS } from "../../deepseek-config-panel"

import { findEnvValue } from "./config-text"
import { parseEnvText, patchEnvText } from "./env"
import { importantEnvKeysByAgent } from "./important-config"
import type { AgentDraft } from "./types"

/**
 * Fold the DeepSeek panel's own env keys, as they are actually persisted, into
 * an existing draft. Everything else in the draft — other keys, and any
 * unsaved edit to them — is left exactly as it was.
 *
 * Returns the draft unchanged when nothing moved, so this never invalidates a
 * memo or restarts a render for a no-op refresh.
 */
export function rebaseDeepSeekDraft(
  draft: AgentDraft,
  agent: AcpAgentInfo
): AgentDraft {
  // Decide on the VALUES, before rewriting anything, so an unrelated refresh
  // leaves the draft object (and its text) untouched.
  //
  // Mirrors `patchEnvText`'s own rule exactly: an empty persisted value means
  // DELETE the key, so `KEY=` present in the draft while the agent has no such
  // key IS a difference — the enable switch persists the draft wholesale, and
  // an empty `DEEPSEEK_BASE_URL` is not "use the default", it is an empty
  // endpoint.
  const current = parseEnvText(draft.envText)
  const patch: Record<string, string | undefined> = {}
  let moved = false
  for (const key of DEEPSEEK_PANEL_ENV_KEYS) {
    patch[key] = agent.env[key]
    const next = (agent.env[key] ?? "").trim()
    const present = key in current
    if (next ? current[key] !== next : present) moved = true
  }
  if (!moved) return draft
  const envText = patchEnvText(draft.envText, patch)
  if (envText === draft.envText) return draft
  const keys = importantEnvKeysByAgent("deepseek")
  const merged = parseEnvText(envText)
  return {
    ...draft,
    envText,
    apiBaseUrl: findEnvValue(merged, keys.apiBaseUrl),
    apiKey: findEnvValue(merged, keys.apiKey),
    model: findEnvValue(merged, keys.model),
  }
}
