/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import { agentRuntimeKey, type AgentDetail } from '@/common/types/agent/agentTypes';

/**
 * Resolve the `model` value a team agent should send to `POST /api/teams`.
 *
 * Backend `service.rs` consumes `input.model` verbatim with no default, so an
 * empty or backend-name-only value (e.g. "gemini") ends up persisted as
 * `use_model: null`. Downstream, GeminiSendBox / AionrsSendBox gate the
 * textarea on `current_model?.useModel` and render disabled. See mnemo #297.
 *
 * This resolver reads agent-owned defaults first and then falls back to
 * backend-safe defaults when the selected agent has no explicit model.
 *
 * For ACP backends (claude, codex, acp) the model is resolved from the
 * agent's handshake data or cached model info so the backend receives a
 * valid model ID (e.g. "claude-sonnet-4-5-20250514") instead of the bare
 * backend name.
 */
export async function resolveDefaultTeamAgentModel(params: {
  agent_id?: string;
  agent_backend?: string;
}): Promise<string> {
  const { agent_id, agent_backend } = params;

  const agentDetail = await resolveAgentDetail(agent_id);
  if (agentDetail) {
    const agentModel = resolveAgentModel(agentDetail);
    if (agentModel) {
      return agentModel;
    }

    return resolveBackendDefaultModel(agentRuntimeKey({ agent: agentDetail.engine.agent }));
  }

  return resolveBackendDefaultModel(agent_backend);
}

async function resolveAgentDetail(agent_id?: string): Promise<AgentDetail | undefined> {
  if (!agent_id) return undefined;

  try {
    const detail = (await ipcBridge.agents.get.invoke({ id: agent_id })) as AgentDetail | null;
    return detail ?? undefined;
  } catch {
    return undefined;
  }
}

function resolveAgentModel(detail: AgentDetail): string | undefined {
  if (detail.defaults.model.mode === 'fixed' && detail.defaults.model.value) {
    return detail.defaults.model.value;
  }

  if (detail.defaults.model.mode === 'auto' && detail.preferences.last_model_id) {
    return detail.preferences.last_model_id;
  }

  return undefined;
}

function resolveBackendDefaultModel(agent_backend?: string): Promise<string> {
  if (agent_backend === 'gemini') {
    return resolveGeminiDefaultModel();
  }

  if (agent_backend === 'aionrs') {
    return resolveAionrsDefaultModel();
  }

  if (agent_backend === 'antigravity') {
    return resolveAntigravityDefaultModel();
  }

  return resolveAcpDefaultModel(agent_backend ?? 'acp');
}

async function resolveAcpDefaultModel(_agent_backend: string): Promise<string> {
  return 'default';
}

async function resolveAntigravityDefaultModel(): Promise<string> {
  // An empty model means "no --model flag": team provisioning persists it
  // verbatim, the session layer filters empty ids to None, and agy runs on
  // its own default model — the exact path direct chat already uses. The
  // 'default' placeholder is not a real agy model id and fails every team
  // turn with UserLlmProviderModelNotFound while agy's discovery is empty.
  return '';
}

async function resolveGeminiDefaultModel(): Promise<string> {
  // The legacy 'gemini.defaultModel' config key has been removed after the
  // Gemini → ACP consolidation. Always fall back to the 'auto' alias.
  return 'auto';
}

async function resolveAionrsDefaultModel(): Promise<string> {
  return 'default';
}
