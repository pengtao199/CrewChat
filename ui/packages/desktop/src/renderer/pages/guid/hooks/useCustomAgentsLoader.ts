/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import type { Agent } from '@/common/types/agent/agentTypes';
import { useEffect } from 'react';
import useSWR, { mutate as swrMutate } from 'swr';

type UseCustomAgentsLoaderResult = {
  /**
   * Preset agent catalog returned by the backend — merged builtin + user +
   * extension, already sorted. This is the list the Guid pill bar and the
   * Settings list render.
   */
  agents: Agent[];
};

/**
 * Loads the agent catalog consumed by Guid. Phase 2 removes `/api/agents`
 * as a user-facing candidate source, so this hook intentionally exposes only
 * the agent list shared with settings/conversation flows.
 */
export const useCustomAgentsLoader = (): UseCustomAgentsLoaderResult => {
  // Preset agents share their own cache so settings / guid / conversation
  // all see the same list without duplicate HTTP calls.
  const { data: agentList } = useSWR('agents.list', async () => {
    try {
      return await ipcBridge.agents.list.invoke();
    } catch (error) {
      console.error('Failed to load agents:', error);
      return [] as Agent[];
    }
  });
  const agents = agentList ?? [];

  useEffect(() => {
    void swrMutate('agents.list');
  }, []);

  return {
    agents,
  };
};
