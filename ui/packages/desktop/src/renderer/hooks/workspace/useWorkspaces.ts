/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * Workspaces (projects) — a folder is a first-class workspace, managed from the
 * sidebar. Agents are global; a workspace only says *where* a session runs.
 */

import { ipcBridge } from '@/common';
import type { Workspace } from '@/common/adapter/ipcBridge';
import { useCallback } from 'react';
import useSWR from 'swr';

const CURRENT_KEY = 'crewchat.currentWorkspace';

export const WORKSPACES_SWR_KEY = 'crewchat.workspaces';

export type CurrentWorkspace = { id: string; name: string; path: string };

export function getCurrentWorkspace(): CurrentWorkspace | null {
  try {
    const raw = localStorage.getItem(CURRENT_KEY);
    return raw ? (JSON.parse(raw) as CurrentWorkspace) : null;
  } catch {
    return null;
  }
}

export function setCurrentWorkspace(workspace: CurrentWorkspace | null): void {
  try {
    if (workspace) localStorage.setItem(CURRENT_KEY, JSON.stringify(workspace));
    else localStorage.removeItem(CURRENT_KEY);
  } catch {
    /* storage unavailable — non-fatal */
  }
}

const fetchWorkspaces = async (): Promise<Workspace[]> => (await ipcBridge.workspaces.list.invoke()) ?? [];

export function useWorkspaces() {
  const { data, mutate, isLoading } = useSWR<Workspace[]>(WORKSPACES_SWR_KEY, fetchWorkspaces, {
    revalidateOnFocus: false,
    shouldRetryOnError: false,
  });
  const workspaces = data ?? [];

  const create = useCallback(
    async (name: string, path: string) => {
      const created = await ipcBridge.workspaces.create.invoke({ name, path });
      await mutate();
      return created;
    },
    [mutate]
  );

  const remove = useCallback(
    async (id: string) => {
      await ipcBridge.workspaces.remove.invoke({ id });
      await mutate();
    },
    [mutate]
  );

  return { workspaces, isLoading, mutate, create, remove };
}
