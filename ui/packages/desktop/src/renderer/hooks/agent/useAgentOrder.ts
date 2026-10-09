/**
 * @license
 * Copyright 2026 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { configService } from '@/common/config/configService';
import { useConfig } from '@/renderer/hooks/config/useConfig';
import { useCallback, useMemo } from 'react';

export const ASSISTANT_ENABLED_ORDER_CONFIG_KEY = 'agents.enabledOrder' as const;

export function normalizeAgentOrder(value: readonly string[] | undefined): string[] {
  if (!Array.isArray(value)) return [];

  const seen = new Set<string>();
  const normalizedOrder: string[] = [];
  for (const id of value) {
    if (typeof id !== 'string') continue;
    const normalizedId = id.trim();
    if (!normalizedId || seen.has(normalizedId)) continue;
    seen.add(normalizedId);
    normalizedOrder.push(normalizedId);
  }
  return normalizedOrder;
}

/**
 * Persist an agent order through the shared client-preferences endpoint.
 * `configService.set` updates its reactive cache before the request completes,
 * so restore the previous cache value if the request fails.
 */
export async function persistAgentOrder(nextOrder: readonly string[]): Promise<void> {
  const previousOrder = configService.get(ASSISTANT_ENABLED_ORDER_CONFIG_KEY);
  const normalizedOrder = normalizeAgentOrder(nextOrder);

  try {
    await configService.set(ASSISTANT_ENABLED_ORDER_CONFIG_KEY, normalizedOrder);
  } catch (error) {
    configService.setLocal(ASSISTANT_ENABLED_ORDER_CONFIG_KEY, previousOrder);
    throw error;
  }
}

export function useAgentOrder(): {
  agentOrder: string[];
  setAgentOrder: (nextOrder: readonly string[]) => Promise<void>;
} {
  const [configuredOrder] = useConfig(ASSISTANT_ENABLED_ORDER_CONFIG_KEY);
  const agentOrder = useMemo(() => normalizeAgentOrder(configuredOrder), [configuredOrder]);
  const setAgentOrder = useCallback((nextOrder: readonly string[]) => persistAgentOrder(nextOrder), []);

  return { agentOrder, setAgentOrder };
}
