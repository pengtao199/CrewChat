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

/** One engine row: cc-switch's scan merged with our executor metadata. */
export type EngineView = {
  id: string;
  name: string;
  kind: string;
  is_body: boolean;
  agent_ids: string[];
  installed: boolean;
  version: string | null;
  path: string | null;
  source: string | null;
  runnable: boolean;
  conflict: boolean;
  installs: EngineInstall[];
  update_command: string;
  anchored: boolean;
  unmanaged: boolean;
};

export type GatewayStatus = {
  available: boolean;
  alive: boolean;
  running: boolean;
  channels: Record<string, boolean>;
};

export type ProviderOption = { id: string; name: string; models: string[] };

export const fetchEngines = () => request<EngineView[]>('/api/crewchat/engines');

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

export const fetchGateway = () => request<GatewayStatus>('/api/crewchat/gateway');

export const fetchProviders = () => request<ProviderOption[]>('/api/providers');

export const setGatewayMode = (app: string, enabled: boolean, providerId?: string) =>
  request<{ success: boolean; channels: Record<string, boolean> }>('/api/crewchat/gateway/mode', {
    method: 'POST',
    body: JSON.stringify({ app, enabled, provider_id: providerId }),
  });
