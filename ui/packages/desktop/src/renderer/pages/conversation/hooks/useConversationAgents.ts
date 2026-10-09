/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { useMemo } from 'react';
import useSWR, { mutate } from 'swr';
import { ipcBridge } from '@/common';
import type { Agent } from '@/common/types/agent/agentTypes';
import { useAgentOrder } from '@/renderer/hooks/agent/useAgentOrder';
import { selectableAgents } from '@/renderer/utils/model/agentSelection';

export type UseConversationAgentsResult = {
  presetAgents: Agent[];
  isLoading: boolean;
  refresh: () => Promise<void>;
};

export const useConversationAgents = (): UseConversationAgentsResult => {
  const { agentOrder } = useAgentOrder();
  const { data: agents, isLoading } = useSWR('agents.list', async () => {
    try {
      return await ipcBridge.agents.list.invoke();
    } catch (error) {
      console.error('Failed to load agents for conversation flows:', error);
      return [] as Agent[];
    }
  });

  // Memoize the selectable list so effects depending on `presetAgents`
  // don't re-fire on every render. SWR returns the same `agents`
  // reference between renders, so the memo only recomputes on real updates.
  // The enabled-order preference is shared with settings, Guid, teams, and
  // scheduled tasks. Without one, `selectableAgents` preserves the legacy
  // CLI → user → official ordering.
  const presetAgents = useMemo(
    () => selectableAgents(agents ?? [], agentOrder),
    [agentOrder, agents]
  );

  return {
    presetAgents,
    isLoading,
    refresh: async () => {
      await mutate('agents.list');
    },
  };
};
