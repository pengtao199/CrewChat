/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

// Mirror of aionui-api-types/src/agent.rs.
// Any shape change on either side requires a same-PR update on the other.

export type AgentSource = 'builtin' | 'generated' | 'user';
export type AgentAgentStatus = 'missing' | 'online' | 'offline' | 'unchecked';
export type AgentAgentSource = 'internal' | 'builtin' | 'extension' | 'custom';

export type AgentAgent = {
  type: string;
  source: AgentAgentSource;
  acp_backend?: string;
};

export function agentRuntimeKey(agent?: Pick<Agent, 'agent'> | null): string {
  return agent?.agent?.acp_backend || agent?.agent?.type || '';
}

export function isAionrsAgent(agent?: Pick<Agent, 'agent'> | null): boolean {
  return agent?.agent?.type === 'aionrs';
}

export interface Agent {
  id: string;
  source: AgentSource;
  name: string;
  name_i18n: Record<string, string>;
  description?: string;
  description_i18n: Record<string, string>;
  avatar?: string;
  enabled: boolean;
  sort_order: number;
  agent_id: string;
  agent?: AgentAgent;
  enabled_skills: string[];
  custom_skill_names: string[];
  disabled_builtin_skills: string[];
  context?: string;
  context_i18n: Record<string, string>;
  prompts: string[];
  prompts_i18n: Record<string, string[]>;
  models: string[];
  last_used_at?: number;
  agent_status: AgentAgentStatus;
  agent_status_message?: string;
  team_selectable: boolean;
  team_block_reason?: string;
  deletable: boolean;
}

export interface AgentProfile {
  name: string;
  name_i18n: Record<string, string>;
  description?: string;
  description_i18n: Record<string, string>;
  avatar?: string;
}

export interface AgentState {
  enabled: boolean;
  sort_order: number;
  last_used_at?: number;
}

export interface AgentEngine {
  agent_id: string;
  agent?: AgentAgent;
}

export interface AgentRules {
  content: string;
  storage_mode: string;
}

export interface AgentPrompts {
  recommended: string[];
  recommended_i18n: Record<string, string[]>;
}

export interface AgentDefaultScalar {
  mode: string;
  value?: string;
}

export interface AgentDefaultList {
  mode: string;
  value: string[];
}

export interface AgentDefaults {
  model: AgentDefaultScalar;
  permission: AgentDefaultScalar;
  thought_level: AgentDefaultScalar;
  skills: AgentDefaultList;
  mcps: AgentDefaultList;
}

export interface AgentDefaultsRequest {
  model?: AgentDefaultScalar;
  permission?: AgentDefaultScalar;
  thought_level?: AgentDefaultScalar;
  skills?: AgentDefaultList;
  mcps?: AgentDefaultList;
}

export interface AgentCapabilities {
  default_skill_ids: string[];
  custom_skill_names: string[];
  default_disabled_builtin_skill_ids: string[];
}

export interface AgentPreferences {
  last_model_id?: string;
  last_permission_value?: string;
  last_thought_level_value?: string;
  last_skill_ids: string[];
  last_disabled_builtin_skill_ids: string[];
  last_mcp_ids: string[];
}

export interface AgentDetail {
  id: string;
  source: AgentSource;
  agent_status: AgentAgentStatus;
  agent_status_message?: string;
  team_selectable: boolean;
  team_block_reason?: string;
  deletable: boolean;
  profile: AgentProfile;
  state: AgentState;
  engine: AgentEngine;
  rules: AgentRules;
  prompts: AgentPrompts;
  defaults: AgentDefaults;
  capabilities: AgentCapabilities;
  preferences: AgentPreferences;
}

export interface CreateAgentRequest {
  id?: string;
  name: string;
  description?: string;
  avatar?: string;
  agent_id?: string;
  enabled_skills?: string[];
  custom_skill_names?: string[];
  disabled_builtin_skills?: string[];
  prompts?: string[];
  models?: string[];
  name_i18n?: Record<string, string>;
  description_i18n?: Record<string, string>;
  prompts_i18n?: Record<string, string[]>;
  recommended_prompts?: string[];
  recommended_prompts_i18n?: Record<string, string[]>;
  defaults?: AgentDefaultsRequest;
}

export type UpdateAgentRequest = Partial<Omit<CreateAgentRequest, 'id'>> & {
  id: string;
};

export interface SetAgentStateRequest {
  id: string;
  enabled?: boolean;
  sort_order?: number;
  last_used_at?: number;
}

export interface ImportAgentsRequest {
  agents: CreateAgentRequest[];
}

export interface ImportError {
  id: string;
  error: string;
}

export interface ImportAgentsResult {
  imported: number;
  skipped: number;
  failed: number;
  errors: ImportError[];
}
