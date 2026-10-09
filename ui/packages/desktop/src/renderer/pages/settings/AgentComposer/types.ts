import type { IProvider } from '@/common/types/provider/providerApi';

/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * Shapes served by /api/crewchat/agents*. Kept page-private because no other
 * page consumes the composer contract.
 */

export type AgentSpec = {
  id: string;
  name: string;
  emoji: string;
  brain: string;
  body: string;
  role: string;
  goal: string;
  backstory: string;
  skills: string[];
  mcps: string[];
  memory: boolean;
  max_iter: number;
  reasoning: boolean;
};

export type ComposerOptions = {
  providers: IProvider[];
  bodies: Array<{ id: string; label: string; available: boolean; has_builtin_brain: boolean }>;
  skills: string[];
  mcps: string[];
  plugins: string[];
};
