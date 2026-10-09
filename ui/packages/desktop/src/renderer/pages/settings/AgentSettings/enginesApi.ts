/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * Calls into our own namespace (/api/crewchat/*) rather than the upstream
 * bridge, since these endpoints exist only in this fork.
 */

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    credentials: 'include',
    headers: { 'Content-Type': 'application/json' },
    ...init,
  });
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return (await response.json()) as T;
}

/** One concrete installation of a CLI on this machine. */
export type EngineInstall = {
  path: string;
  version: string | null;
  runnable: boolean;
  error: string | null;
  source: string;
  is_path_default: boolean;
};

/** Status the backend has already resolved for the row (AGENTS/ui.md: no UI logic). */
export type EngineStatus = 'ready' | 'not_installed' | 'broken' | 'conflict';

/** One engine row: cc-switch's scan + version check merged with our executor metadata. */
export type EngineView = {
  id: string;
  name: string;
  kind: string;
  is_body: boolean;
  agent_ids: string[];
  status: EngineStatus;
  installed: boolean;
  version: string | null;
  latest_version: string | null;
  has_update: boolean;
  broken: boolean;
  conflict: boolean;
  error: string | null;
  path: string | null;
  source: string | null;
  runnable: boolean;
  installs: EngineInstall[];
  update_command: string;
  anchored: boolean;
  unmanaged: boolean;
};

/** SWR key for the engines view. Re-entering the page reads the cache; only an
 * explicit refresh (or a lifecycle action) triggers a new scan. */
export const ENGINES_SWR_KEY = 'crewchat.engines';

export const fetchEngines = (refresh = false) =>
  request<EngineView[]>(`/api/crewchat/engines${refresh ? '?refresh=true' : ''}`);

export const installEngines = (tools: string[]) =>
  request<{ success: boolean }>('/api/crewchat/engines/install', {
    method: 'POST',
    body: JSON.stringify({ tools }),
  });

export const updateEngines = (tools: string[]) =>
  request<{ success: boolean }>('/api/crewchat/engines/update', {
    method: 'POST',
    body: JSON.stringify({ tools }),
  });
