/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import type { Agent } from '@/common/types/agent/agentTypes';

type AgentNameSource = Pick<Agent, 'id' | 'name' | 'name_i18n'>;

export function resolveAgentName(
  agent: AgentNameSource | null | undefined,
  localeKey: string,
  fallback = 'Agent'
): string {
  if (!agent) {
    return fallback;
  }

  const localizedName = agent.name_i18n?.[localeKey] || agent.name_i18n?.['en-US'];
  return localizedName?.trim() || agent.name?.trim() || agent.id || fallback;
}
