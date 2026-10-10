import { ipcBridge } from '@/common';
import type { Agent } from '@/common/types/agent/agentTypes';
import type { ManagerStatus } from '@/common/adapter/ipcBridge';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { mutate as swrMutate } from 'swr';
import { MANAGER_ID, uniqueWorkers } from '../utils/groupRoster';

export type GuidChatMode = 'single' | 'group';

export const useGuidTeamSelection = (agents: Agent[]) => {
  const [mode, setMode] = useState<GuidChatMode>('single');
  const [workerIds, setWorkerIds] = useState<string[]>([]);
  const [manager, setManager] = useState<ManagerStatus | null>(null);
  const [managerLoading, setManagerLoading] = useState(false);

  const reloadManager = useCallback(async () => {
    setManagerLoading(true);
    try {
      setManager(await ipcBridge.crewchatAgents.manager.invoke());
    } catch {
      setManager(null);
    } finally {
      setManagerLoading(false);
    }
  }, []);

  useEffect(() => {
    if (mode === 'group') void reloadManager();
  }, [mode, reloadManager]);

  const workers = useMemo(
    () =>
      agents.filter(
        (agent) =>
          agent.id !== MANAGER_ID &&
          agent.enabled !== false &&
          agent.team_selectable !== false
      ),
    [agents]
  );

  const toggleWorker = useCallback((agentId: string) => {
    setWorkerIds((current) =>
      current.includes(agentId) ? current.filter((id) => id !== agentId) : uniqueWorkers([...current, agentId])
    );
  }, []);

  const removeWorker = useCallback((agentId: string) => {
    setWorkerIds((current) => current.filter((id) => id !== agentId));
  }, []);

  const restoreManager = useCallback(async () => {
    setManagerLoading(true);
    try {
      setManager(await ipcBridge.crewchatAgents.restoreManager.invoke());
      await swrMutate('agents.list');
    } catch {
      await reloadManager();
    } finally {
      setManagerLoading(false);
    }
  }, [reloadManager]);

  return {
    mode,
    setMode,
    workerIds,
    workers,
    toggleWorker,
    removeWorker,
    manager,
    managerLoading,
    managerReady: Boolean(manager?.ready),
    reloadManager,
    restoreManager,
  };
};
