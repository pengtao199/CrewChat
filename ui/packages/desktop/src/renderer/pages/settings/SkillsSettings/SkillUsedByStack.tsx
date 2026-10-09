/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * SkillUsedByStack — compact overlapping avatar stack shown on a skill card
 * to surface which agents have the skill attached. Mirrors the agent
 * page's BoundAgentStack, capped with a "+N" pill.
 */

import type { Agent } from '@/common/types/agent/agentTypes';
import { resolveLocaleKey } from '@/common/utils';
import AgentAvatar from '@/renderer/pages/settings/AgentCatalog/AgentAvatar';
import { Tooltip } from '@arco-design/web-react';
import React from 'react';
import { useTranslation } from 'react-i18next';
import { formatNameList } from '@/renderer/services/i18n/list';

/**
 * Agents that have this skill attached (enabled or custom list).
 * The live API may omit the skill arrays on some agents, so guard both.
 */
export const getAgentsUsingSkill = (skillName: string, agents: Agent[]): Agent[] =>
  agents.filter(
    (a) => (a.enabled_skills ?? []).includes(skillName) || (a.custom_skill_names ?? []).includes(skillName)
  );

const SkillUsedByStack: React.FC<{ agents: Agent[]; max?: number }> = ({ agents, max = 4 }) => {
  const { t, i18n } = useTranslation();
  const localeKey = resolveLocaleKey(i18n.language);
  if (agents.length === 0) return null;

  const shown = agents.slice(0, max);
  const overflow = agents.length - shown.length;
  const names = formatNameList(
    agents.map((a) => a.name_i18n?.[localeKey] || a.name),
    i18n.language
  );

  return (
    <Tooltip
      content={t('settings.skillsHub.usedByTooltip', {
        count: agents.length,
        names,
        defaultValue: `Used by ${agents.length} agent(s): ${names}`,
      })}
    >
      <div className='flex items-center' data-testid='skill-used-by-stack'>
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

export default SkillUsedByStack;
