/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import type { IProvider } from '@/common/config/storage';
import { hasSpecificModelCapability } from '@/renderer/utils/model/modelCapabilities';

/**
 * Cache for provider available models to avoid repeated computation.
 */
const available_modelsCache = new Map<string, string[]>();

/**
 * Get all available primary models for a provider (with cache).
 * Filters out disabled models based on model_enabled state.
 * @param provider - Provider configuration
 * @returns Array of available primary model names
 */
export const getAvailableModels = (provider: IProvider): string[] => {
  // 启停状态挂在模型自身上。
  const cacheKey = `${provider.id}-${(provider.models || [])
    .map((m) => `${m.id}:${m.enabled ? 1 : 0}`)
    .join(',')}`;

  if (available_modelsCache.has(cacheKey)) {
    return available_modelsCache.get(cacheKey)!;
  }

  const result: string[] = [];
  for (const model of provider.models || []) {
    if (!model.enabled) continue;

    const functionCalling = hasSpecificModelCapability(provider, model.id, 'function_calling');
    const excluded = hasSpecificModelCapability(provider, model.id, 'excludeFromPrimary');

    if ((functionCalling === true || functionCalling === undefined) && excluded !== true) {
      result.push(model.id);
    }
  }

  available_modelsCache.set(cacheKey, result);
  return result;
};

/**
 * Check if a provider has any available primary conversation models (efficient version).
 * @param provider - Provider configuration
 * @returns true if the provider has available models
 */
export const hasAvailableModels = (provider: IProvider): boolean => {
  return getAvailableModels(provider).length > 0;
};
