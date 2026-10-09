/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import type { ICronJob } from '@/common/adapter/ipcBridge';
import type { AgentLogoMap } from '@renderer/utils/model/agentLogo';
import { resolveAgentLogo } from '@renderer/utils/model/agentLogo';
import { resolveAgentAvatar } from '@renderer/utils/model/agentAvatar';
import type { Agent } from '@/common/types/agent/agentTypes';

function normalizeAgentBackend(agent: string | undefined): string | undefined {
  if (!agent) return undefined;
  return agent.replace(/^cli:/, '').replace(/^preset:/, '');
}

function resolveCronAgentId(config: ICronJob['metadata']['agent_config']): string | undefined {
  return config?.agent_id;
}

/**
 * Resolve the display name and logo for a cron job's agent.
 *
 * Agent-backed jobs display from the agent catalog. Non-agent
 * legacy jobs fall back to the derived runtime type.
 */
export function getJobAgentMeta(
  job: ICronJob,
  presetAgents: Agent[],
  logos: AgentLogoMap
): { name?: string; logo?: string | null; emoji?: string; agentFallback?: boolean } {
  const config = job.metadata.agent_config;
  const agentId = resolveCronAgentId(config);
  if (agentId) {
    const agent = presetAgents.find((item) => item.id === agentId);
    if (!agent) {
      return { name: config?.name || normalizeAgentBackend(job.metadata.agent_type), agentFallback: true };
    }

    const rawType = normalizeAgentBackend(job.metadata.agent_type);
    const displayName = agent.name || rawType;
    const avatar = resolveAgentAvatar(agent.avatar);
    if (avatar.kind === 'image') {
      return { name: displayName, logo: avatar.value };
    }
    if (avatar.kind === 'emoji') {
      return { name: displayName, emoji: avatar.value };
    }

    return { name: displayName, agentFallback: true };
  }

  const rawType = normalizeAgentBackend(job.metadata.agent_type);
  if (!rawType) return {};
  const logoBackend = rawType;

  if (rawType === 'acp') {
    return {
      name: config?.name || rawType,
      logo: resolveAgentLogo(logos, { backend: logoBackend }),
    };
  }

  return {
    name: config?.name || rawType,
    logo: resolveAgentLogo(logos, { backend: logoBackend }),
  };
}
