/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import type { Agent } from '@/common/types/agent/agentTypes';
import { resolveLocaleKey } from '@/common/utils';
import AgentAvatar from '@/renderer/pages/settings/AgentCatalog/AgentAvatar';
import type { ManagedAgent } from '@/renderer/utils/model/agentTypes';
import { Right } from '@icon-park/react';
import { Tooltip, Typography } from '@arco-design/web-react';
import React from 'react';
import { useTranslation } from 'react-i18next';
import { formatNameList } from '@/renderer/services/i18n/list';
import useSWR from 'swr';

const BOUND_ASSISTANTS_SWR_KEY = 'agents.boundAgents.list';

/**
 * Agents bound to this managed agent. `agent_id` is the backend
 * `agent_metadata.id`; runtime backend labels are intentionally ignored here
 * because multiple agent rows can share a backend.
 */
export const getBoundAgents = (agent: Pick<ManagedAgent, 'id'>, agents: Agent[]): Agent[] =>
  agents.filter((agent) => agent.agent_id === agent.id);

/**
 * Read-only agent catalog for the Agent settings surface. Shares a single
 * SWR cache so the list page and every repair page reuse one fetch.
 */
export const useAgentsForAgents = (): { agents: Agent[]; isLoading: boolean } => {
  const { data, isLoading } = useSWR<Agent[]>(BOUND_ASSISTANTS_SWR_KEY, () => ipcBridge.agents.list.invoke());
  return { agents: data ?? [], isLoading };
};

const agentLabel = (agent: Agent, localeKey: string): string =>
  agent.name_i18n?.[localeKey] || agent.name;

/**
 * Compact overlapping avatar stack shown on an agent list row to surface which
 * agents depend on it. Caps at `max` avatars and renders a "+N" pill for
 * the remainder so a high-fan-out engine (e.g. aionrs) stays readable.
 */
export const BoundAgentStack: React.FC<{ agents: Agent[]; max?: number }> = ({ agents, max = 4 }) => {
  const { t, i18n } = useTranslation();
  const localeKey = resolveLocaleKey(i18n.language);
  if (agents.length === 0) return null;

  const shown = agents.slice(0, max);
  const overflow = agents.length - shown.length;
  const tooltip = formatNameList(
    agents.map((a) => agentLabel(a, localeKey)),
    i18n.language
  );

  return (
    <Tooltip
      content={t('settings.agentManagement.boundAgentsTooltip', { count: agents.length, names: tooltip })}
    >
      <div className='flex items-center' data-testid='agent-bound-stack'>
        {shown.map((agent, index) => (
          <div
            key={agent.id}
            className='overflow-hidden rounded-full border-2 border-solid border-bg-2'
            style={{ marginInlineStart: index === 0 ? 0 : -7, zIndex: shown.length - index }}
          >
            <AgentAvatar agent={agent} size={22} />
          </div>
        ))}
        {overflow > 0 && (
          <div
            className='flex items-center justify-center rounded-full border-2 border-solid border-bg-2 bg-fill-3 text-9px font-600 text-t-secondary'
            style={{ width: 22, height: 22, marginInlineStart: -7 }}
          >
            +{overflow}
          </div>
        )}
      </div>
    </Tooltip>
  );
};

/**
 * Full bound-agent list for the agent configuration page. Each row shows
 * avatar + name and navigates to that agent's detail/editor on click.
 */
export const BoundAgentList: React.FC<{
  agents: Agent[];
  onOpenAgent: (agentId: string) => void;
}> = ({ agents, onOpenAgent }) => {
  const { t, i18n } = useTranslation();
  const localeKey = resolveLocaleKey(i18n.language);

  if (agents.length === 0) {
    return (
      <div
        className='rounded-8px bg-aou-2 px-12px py-16px text-center text-12px text-t-tertiary'
        data-testid='agent-bound-empty'
      >
        {t('settings.agentManagement.boundAgentsEmpty')}
      </div>
    );
  }

  return (
    <div className='flex flex-col gap-4px' data-testid='agent-bound-list'>
      {agents.map((agent) => (
        <div
          key={agent.id}
          className='group flex cursor-pointer items-center gap-10px rounded-8px px-12px py-10px transition-colors hover:bg-fill-1'
          data-testid={`agent-bound-row-${agent.id}`}
          onClick={() => onOpenAgent(agent.id)}
        >
          <AgentAvatar agent={agent} size={26} />
          <Typography.Text className='flex-1 truncate text-13px font-500 text-t-primary'>
            {agentLabel(agent, localeKey)}
          </Typography.Text>
          <span className='flex items-center gap-2px text-12px text-t-tertiary group-hover:text-t-secondary'>
            {t('settings.agentManagement.viewAgent')}
            <Right size={13} fill='currentColor' />
          </span>
        </div>
      ))}
    </div>
  );
};
