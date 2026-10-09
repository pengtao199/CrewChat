/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { isAionrsAgent, type Agent } from '@/common/types/agent/agentTypes';
import type { IChannelAgentBindingRead, IChannelAgentBindingWrite } from '@/common/types/channel/channel';

/**
 * Channel settings UI consumes backend-normalized agent bindings.
 * Legacy backend/custom-agent migration is handled by aionCore during channel
 * settings reads; renderer-side selection must only trust canonical
 * `agent_id` bindings.
 */
export type ChannelAgentBinding = IChannelAgentBindingRead | undefined;

export type ResolvedChannelAgentSelection = {
  agentId?: string;
  hasBrokenSavedAgent: boolean;
};

export function getDefaultChannelAgent(agents: Agent[]): Agent | undefined {
  return (
    agents.find((agent) => agent.source === 'generated' && isAionrsAgent(agent)) ||
    agents.find((agent) => isAionrsAgent(agent)) ||
    agents[0]
  );
}

export function resolveChannelAgentId(saved: ChannelAgentBinding, agents: Agent[]): string | undefined {
  return resolveChannelAgentSelection(saved, agents).agentId;
}

export function resolveChannelAgentSelection(
  saved: ChannelAgentBinding,
  agents: Agent[]
): ResolvedChannelAgentSelection {
  if (!saved) {
    return {
      agentId: getDefaultChannelAgent(agents)?.id,
      hasBrokenSavedAgent: false,
    };
  }

  const explicitAgentId = typeof saved.agent_id === 'string' ? saved.agent_id : undefined;

  if (explicitAgentId && agents.some((agent) => agent.id === explicitAgentId)) {
    return {
      agentId: explicitAgentId,
      hasBrokenSavedAgent: false,
    };
  }

  if (explicitAgentId || saved.custom_agent_id || saved.backend || saved.agent_type) {
    return {
      agentId: undefined,
      hasBrokenSavedAgent: true,
    };
  }

  return {
    agentId: getDefaultChannelAgent(agents)?.id,
    hasBrokenSavedAgent: false,
  };
}

export function buildChannelAgentBinding(agent: Agent): IChannelAgentBindingWrite {
  return {
    agent_id: agent.id,
  };
}
