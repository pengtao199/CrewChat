/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import type { Agent } from '@/common/types/agent/agentTypes';

/**
 * Single source of truth for which agents appear in a *selection* list
 * (home pill bar, team creation, scheduled-task dropdown, …) and in what order.
 *
 * Rules (see PRD F-AHM-06 / F-AHM-07):
 *  - Only enabled agents are selectable.
 *  - A stored enabled-order preference takes priority across every source.
 *  - Without a preference, preserve the legacy bare CLI → user → official
 *    order so an upgrade does not reshuffle an existing user's picker.
 *  - New agents missing from a stored preference append in legacy order.
 *
 * Note: a bare CLI agent surfaces with `source === 'generated'`.
 */

/** Legacy group weight — lower comes first. Bare CLI < user-created < official. */
const sourceGroupWeight = (source: string): number => {
  switch (source) {
    case 'generated':
      return 0;
    case 'user':
      return 1;
    case 'builtin':
      return 2;
    default:
      return 1;
  }
};

const compareLegacyAgentOrder = (left: Agent, right: Agent): number => {
  const groupDelta = sourceGroupWeight(left.source) - sourceGroupWeight(right.source);
  if (groupDelta !== 0) return groupDelta;

  const orderDelta = left.sort_order - right.sort_order;
  if (orderDelta !== 0) return orderDelta;

  return left.id.localeCompare(right.id);
};

/**
 * Return enabled agents in the user's preferred cross-source order.
 * Stale IDs and duplicates in `preferredOrder` are ignored.
 */
export const selectableAgents = (agents: Agent[], preferredOrder?: readonly string[]): Agent[] => {
  const legacyOrdered = agents
    .filter((agent) => agent.enabled !== false)
    .toSorted(compareLegacyAgentOrder);

  if (!preferredOrder || preferredOrder.length === 0) {
    return legacyOrdered;
  }

  const enabledById = new Map(legacyOrdered.map((agent) => [agent.id, agent]));
  const orderedAgents: Agent[] = [];
  const includedIds = new Set<string>();

  for (const agentId of preferredOrder) {
    const agent = enabledById.get(agentId);
    if (!agent || includedIds.has(agentId)) continue;
    includedIds.add(agentId);
    orderedAgents.push(agent);
  }

  for (const agent of legacyOrdered) {
    if (includedIds.has(agent.id)) continue;
    includedIds.add(agent.id);
    orderedAgents.push(agent);
  }

  return orderedAgents;
};

/** Build the persisted enabled order after an agent is toggled. */
export const agentOrderAfterToggle = (
  agents: Agent[],
  preferredOrder: readonly string[],
  agentId: string,
  enabled: boolean
): string[] => {
  const currentOrder = selectableAgents(agents, preferredOrder)
    .map((agent) => agent.id)
    .filter((id) => id !== agentId);

  return enabled ? [...currentOrder, agentId] : currentOrder;
};
