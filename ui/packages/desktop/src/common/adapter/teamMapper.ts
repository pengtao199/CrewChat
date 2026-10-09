/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import type {
  BackendTeammateStatus,
  TeamAgent,
  TeamContextResetAvailability,
  TeamContextResetCapability,
  TeammateRole,
  TeammateStatus,
  TTeam,
  WorkspaceMode,
} from '../types/team/teamTypes';

// ── Parameter types for team API calls ─────────────────────────────────

/**
 * Fields the backend actually consumes when creating a team member. The
 * runtime backend / conversation type are derived server-side from the
 * agent, so callers only supply agent identity, role, and model.
 */
export type TeamAgentInput = Pick<TeamAgent, 'role' | 'agent_name' | 'agent_id' | 'model'>;

export type ICreateTeamParams = {
  user_id: string;
  name: string;
  workspace: string;
  workspace_mode: WorkspaceMode;
  agents: TeamAgentInput[];
};

export type IAddTeamAgentParams = {
  team_id: string;
  agent: TeamAgentInput;
};

// ── Backend → Frontend ─────────────────────────────────────────────────

const VALID_ROLES = new Set<TeammateRole>(['leader', 'teammate']);
const VALID_WORKSPACE_MODES = new Set<WorkspaceMode>(['shared', 'isolated']);
const VALID_CONTEXT_RESET_AVAILABILITY = new Set<TeamContextResetAvailability>([
  'ready',
  'initializing',
  'busy',
  'dormant',
  'failed',
  'removing',
  'session_stopped',
  'unsupported',
  'leader_not_targetable',
]);

function toRole(raw: string | undefined): TeammateRole {
  if (raw === 'lead') return 'leader';
  return VALID_ROLES.has(raw as TeammateRole) ? (raw as TeammateRole) : 'teammate';
}

export function normalizeTeamStatus(raw: BackendTeammateStatus | undefined): TeammateStatus {
  const statusMap: Record<string, TeammateStatus> = {
    pending: 'pending',
    idle: 'idle',
    working: 'active',
    thinking: 'active',
    tool_use: 'active',
    completed: 'completed',
    error: 'failed',
    // Dormant (leader-only warmup): pass through so the badge can render an
    // asleep state distinct from idle, instead of collapsing to idle.
    dormant: 'dormant',
  };
  return statusMap[raw ?? ''] ?? 'idle';
}

function toWorkspaceMode(raw: string | undefined): WorkspaceMode {
  return VALID_WORKSPACE_MODES.has(raw as WorkspaceMode) ? (raw as WorkspaceMode) : 'shared';
}

function toContextResetCapability(raw: unknown): TeamContextResetCapability {
  if (typeof raw !== 'object' || raw === null) {
    throw new Error('Invalid team context-reset capability');
  }
  const capability = raw as Record<string, unknown>;
  if (
    typeof capability.supported !== 'boolean' ||
    !VALID_CONTEXT_RESET_AVAILABILITY.has(capability.availability as TeamContextResetAvailability)
  ) {
    throw new Error('Invalid team context-reset capability');
  }
  return {
    supported: capability.supported,
    availability: capability.availability as TeamContextResetAvailability,
  };
}

export function fromBackendAgent(raw: unknown): TeamAgent {
  const r = (raw ?? {}) as Record<string, unknown>;
  const agentType = (r.agent_type as string | undefined) ?? (r.backend as string | undefined) ?? '';
  const backend = (r.agent_backend as string | undefined) ?? (r.backend as string | undefined) ?? agentType;
  return {
    slot_id: (r.slot_id as string | undefined) ?? '',
    conversation_id: (r.conversation_id as string | undefined) ?? '',
    role: toRole(r.role as string | undefined),
    agent_backend: backend,
    icon: r.icon as string | undefined,
    agent_name:
      (r.agent_name as string | undefined) ??
      (r.agent_name as string | undefined) ??
      (r.name as string | undefined) ??
      '',
    status: normalizeTeamStatus(r.status as BackendTeammateStatus | undefined),
    cli_path: r.cli_path as string | undefined,
    agent_id: r.agent_id as string | undefined,
    model: r.model as string | undefined,
    pending_confirmations: (r.pending_confirmations ?? r.pendingConfirmations ?? 0) as number,
    context_reset: toContextResetCapability(r.context_reset),
  };
}

export function fromBackendTeam(raw: unknown): TTeam {
  const r = (raw ?? {}) as Record<string, unknown>;
  const rawAgents = Array.isArray(r.agents)
    ? (r.agents as unknown[])
    : Array.isArray(r.agents)
      ? (r.agents as unknown[])
      : [];
  const agents = rawAgents.map(fromBackendAgent);
  const leaderAgentId =
    (r.leader_agent_id as string | undefined) ?? (r.leader_agent_id as string | undefined) ?? '';
  return {
    id: (r.id as string | undefined) ?? '',
    user_id: (r.user_id as string | undefined) ?? '',
    name: (r.name as string | undefined) ?? '',
    workspace: (r.workspace as string | undefined) ?? '',
    workspace_mode: toWorkspaceMode(r.workspace_mode as string | undefined),
    leader_agent_id: leaderAgentId,
    agents,
    leader_agent_id: leaderAgentId,
    agents: agents,
    session_mode: r.session_mode as string | undefined,
    created_at: (r.created_at as number | undefined) ?? 0,
    updated_at: (r.updated_at as number | undefined) ?? 0,
  };
}

export function fromBackendTeamList(raw: unknown): TTeam[] {
  return Array.isArray(raw) ? (raw as unknown[]).map(fromBackendTeam) : [];
}

export function fromBackendTeamOptional(raw: unknown): TTeam | null {
  return raw == null ? null : fromBackendTeam(raw);
}

// ── Frontend → Backend ─────────────────────────────────────────────────

export function toBackendAgent(a: TeamAgentInput): Record<string, unknown> {
  if (!a.agent_id) {
    throw new Error('agent_id is required');
  }

  return {
    name: a.agent_name,
    role: a.role === 'leader' ? 'lead' : a.role,
    model: a.model || 'default',
    agent_id: a.agent_id,
  };
}
