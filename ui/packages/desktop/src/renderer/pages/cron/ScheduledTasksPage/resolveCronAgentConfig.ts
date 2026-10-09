/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import type { ICronAgentConfigWrite } from '@/common/adapter/ipcBridge';
import { isAionrsAgent, type Agent } from '@/common/types/agent/agentTypes';
import { resolveAgentName } from '@renderer/utils/model/agentDisplay';

type SelectedAionrsProvider = {
  id?: string;
  name?: string;
};

type ResolveCronAgentConfigInput = {
  agentValue: string;
  presetAgents: Agent[];
  selectedAionrsProvider?: SelectedAionrsProvider;
  model_id?: string;
  config_options?: Record<string, string>;
  workspace?: string;
  localeKey?: string;
  getMode: (agent: Agent) => string | undefined;
  aionrsModelRequiredMessage: string;
};

type ResolveCronAgentConfigResult = {
  agent_config: ICronAgentConfigWrite | undefined;
};

export function resolveCronAgentConfig(input: ResolveCronAgentConfigInput): ResolveCronAgentConfigResult {
  const {
    agentValue,
    presetAgents,
    selectedAionrsProvider,
    model_id,
    config_options,
    workspace,
    localeKey = 'en-US',
    getMode,
    aionrsModelRequiredMessage,
  } = input;

  const colonIdx = agentValue.indexOf(':');
  const prefixedId = colonIdx >= 0 ? agentValue.substring(colonIdx + 1) : agentValue;
  const agentSelection = presetAgents.find((item) => item.id === prefixedId || item.id === agentValue);
  if (!agentSelection) {
    throw new Error('agent_id is required');
  }

  let agent_config: ICronAgentConfigWrite | undefined;

  const agent = agentSelection;
  const agentName = resolveAgentName(agent, localeKey, agent.name);
  const mode = getMode(agent);

  if (isAionrsAgent(agent)) {
    if (!selectedAionrsProvider?.id || !model_id) {
      throw new Error(aionrsModelRequiredMessage);
    }
    agent_config = {
      name: agentName,
      agent_id: agent.id,
      mode,
      model_id,
      model: {
        provider_id: selectedAionrsProvider.id,
        model: model_id,
        use_model: model_id,
      },
      workspace,
    };
  } else {
    agent_config = {
      name: agentName,
      agent_id: agent.id,
      mode,
      model_id,
      config_options,
      workspace,
    };
  }

  return { agent_config };
}
