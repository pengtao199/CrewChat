import React from 'react';
import { Robot } from '@icon-park/react';
import { resolveAgentAvatar } from '@renderer/utils/model/agentAvatar';
import ThemedLogo from '@/renderer/components/agent/ThemedLogo';
import { resolveAgentName } from '@renderer/utils/model/agentDisplay';
import { agentRuntimeKey, type Agent } from '@/common/types/agent/agentTypes';

/** Team leader selector entry derived from the unified agent catalog. */
export type TeamAgentOption = {
  id: string;
  name: string;
  /** Execution backend (claude, gemini, qwen, …). */
  backend?: string;
  /** Avatar token — a backend-resolved URL or an emoji. */
  icon?: string;
  /** Whether this agent can currently be used in team mode. */
  team_selectable?: boolean;
  /** Why this agent cannot currently be used in team mode. */
  team_block_reason?: string;
};

export function agentToOption(agent: Agent, localeKey = 'en-US'): TeamAgentOption {
  return {
    id: agent.id,
    name: resolveAgentName(agent, localeKey, agent.name),
    backend: agentRuntimeKey(agent),
    icon: agent.avatar,
    team_selectable: agent.team_selectable,
    team_block_reason: agent.team_block_reason,
  };
}

export function agentKey(agent: TeamAgentOption): string {
  return agent.id;
}

export function agentFromId(
  agentId: string,
  allAgents: TeamAgentOption[]
): TeamAgentOption | undefined {
  return allAgents.find((agent) => agentKey(agent) === agentId);
}

/** Filter agents to only those supported in team mode. */
export function filterTeamSupportedAgents(agents: TeamAgentOption[]): TeamAgentOption[] {
  return agents;
}

type AgentOptionLabelProps = {
  agent: TeamAgentOption;
  size?: 'compact' | 'large';
  muted?: boolean;
};

export const AgentOptionLabel: React.FC<AgentOptionLabelProps> = ({
  agent,
  size = 'compact',
  muted = false,
}) => {
  const avatar = resolveAgentAvatar(agent.icon);
  const isLarge = size === 'large';
  const iconSize = isLarge ? 18 : 16;
  const avatarToneClass = muted ? 'bg-fill-1 text-t-tertiary opacity-75' : 'bg-fill-2 text-t-primary';
  const avatarClass = isLarge
    ? `flex h-30px w-30px shrink-0 items-center justify-center rounded-8px ${avatarToneClass}`
    : `flex h-32px w-32px shrink-0 items-center justify-center rounded-8px ${avatarToneClass}`;
  const nameClass = muted ? 'text-t-tertiary' : 'text-t-primary';
  const avatarNode =
    avatar.kind === 'image' ? (
      <ThemedLogo
        src={avatar.value}
        alt={agent.name}
        style={{ width: iconSize, height: iconSize, objectFit: 'contain' }}
      />
    ) : avatar.kind === 'emoji' ? (
      <span style={{ fontSize: isLarge ? 18 : 14, lineHeight: `${iconSize}px` }}>{avatar.value}</span>
    ) : (
      <Robot size={String(iconSize)} />
    );

  return (
    <div className={isLarge ? 'flex min-w-0 items-center gap-12px' : 'flex min-w-0 items-center gap-8px'}>
      <span className={avatarClass} data-testid='agent-avatar'>
        {avatarNode}
      </span>
      <span
        data-testid='agent-option-name'
        className={
          isLarge ? `min-w-0 truncate text-14px font-500 leading-21px ${nameClass}` : `min-w-0 truncate ${nameClass}`
        }
      >
        {agent.name}
      </span>
    </div>
  );
};
