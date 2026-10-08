/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * Calls into our own namespace (/api/crewchat/*) rather than the upstream
 * bridge, since these endpoints exist only in this fork.
 */

import type { AgentSpec, ComposerOptions } from './types';

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    credentials: 'include',
    headers: { 'Content-Type': 'application/json' },
    ...init,
  });
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return (await response.json()) as T;
}

export const fetchAgents = () => request<{ agents: AgentSpec[]; agents_dir: string }>('/api/crewchat/agents');

export const fetchOptions = () => request<ComposerOptions>('/api/crewchat/agents/options');

export const saveAgent = (id: string, spec: AgentSpec) =>
  request<AgentSpec>(`/api/crewchat/agents/${encodeURIComponent(id)}`, {
    method: 'PUT',
    body: JSON.stringify(spec),
  });

export const removeAgent = (id: string) =>
  request<{ success: boolean }>(`/api/crewchat/agents/${encodeURIComponent(id)}`, { method: 'DELETE' });

export const fetchModels = (providerId: string) =>
  request<{ models: string[]; success: boolean; message?: string }>('/api/providers/fetch-models', {
    method: 'POST',
    body: JSON.stringify({ provider_id: providerId }),
  });
