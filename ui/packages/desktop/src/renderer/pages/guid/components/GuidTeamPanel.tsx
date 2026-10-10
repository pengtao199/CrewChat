import { Button, Input, Tag, Tooltip } from '@arco-design/web-react';
import { CloseSmall, Plus } from '@icon-park/react';
import type { Agent } from '@/common/types/agent/agentTypes';
import type { ManagerStatus } from '@/common/adapter/ipcBridge';
import StatusBadge from '@/renderer/components/base/StatusBadge';
import React, { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';

type GuidTeamPanelProps = {
  manager: ManagerStatus | null;
  managerLoading: boolean;
  workers: Agent[];
  workerIds: string[];
  localeKey: string;
  hasWorkspace: boolean;
  onToggleWorker: (agentId: string) => void;
  onRemoveWorker: (agentId: string) => void;
  onRestoreManager: () => void;
};

const GuidTeamPanel: React.FC<GuidTeamPanelProps> = ({
  manager,
  managerLoading,
  workers,
  workerIds,
  localeKey,
  hasWorkspace,
  onToggleWorker,
  onRemoveWorker,
  onRestoreManager,
}) => {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [query, setQuery] = useState('');
  const selected = useMemo(
    () => workerIds.map((id) => workers.find((agent) => agent.id === id)).filter((agent): agent is Agent => Boolean(agent)),
    [workerIds, workers]
  );
  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return workers.filter((agent) => {
      if (!needle) return true;
      const label = agent.name_i18n?.[localeKey] || agent.name;
      return label.toLowerCase().includes(needle) || agent.id.toLowerCase().includes(needle);
    });
  }, [localeKey, query, workers]);

  const managerTone = !manager?.exists ? 'blocked' : manager.ready ? 'ready' : 'warning';
  const managerHint = !manager?.exists
    ? t('conversation.group.managerMissing')
    : manager.ready
      ? t('conversation.group.managerReady')
      : t('conversation.group.managerUnconfigured');

  return (
    <div className='mx-auto mb-16px flex w-full max-w-720px flex-col gap-12px'>
      <div className='flex flex-col gap-10px rounded-12px border border-solid border-border-2 bg-fill-1 px-12px py-12px'>
        <div className='flex items-center justify-between gap-8px'>
          <div className='flex min-w-0 items-center gap-8px'>
            <span className='text-16px'>🧭</span>
            <span className='truncate text-13px font-medium text-t-primary'>{t('conversation.group.manager')}</span>
            <StatusBadge tone={managerTone} label={managerHint} />
          </div>
          {!manager?.exists ? (
            <Button size='mini' type='outline' loading={managerLoading} onClick={onRestoreManager}>
              {t('conversation.group.restore')}
            </Button>
          ) : !manager.ready ? (
            <Button size='mini' type='outline' onClick={() => navigate('/settings/agents')}>
              {t('conversation.group.configure')}
            </Button>
          ) : null}
        </div>
        <p className='m-0 text-12px text-t-tertiary'>{t('conversation.group.managerHint')}</p>
      </div>

      <div className='flex flex-col gap-10px rounded-12px border border-solid border-border-2 bg-2 px-12px py-12px'>
        <div className='flex items-center justify-between gap-8px'>
          <span className='text-13px font-medium text-t-primary'>
            {t('conversation.group.workers', { count: workerIds.length })}
          </span>
          {!hasWorkspace ? (
            <span className='text-12px text-t-tertiary'>{t('conversation.group.needWorkspace')}</span>
          ) : null}
        </div>
        {selected.length ? (
          <div className='flex flex-wrap gap-8px'>
            {selected.map((agent) => (
              <Tag
                key={agent.id}
                closable
                closeIcon={<CloseSmall theme='outline' size={12} />}
                onClose={() => onRemoveWorker(agent.id)}
              >
                {agent.name_i18n?.[localeKey] || agent.name}
              </Tag>
            ))}
          </div>
        ) : (
          <span className='text-12px text-t-tertiary'>{t('conversation.group.needMember')}</span>
        )}
        <Input
          size='small'
          value={query}
          onChange={setQuery}
          placeholder={t('conversation.group.search')}
          allowClear
        />
        <div className='flex max-h-220px flex-col gap-6px overflow-auto'>
          {filtered.map((agent) => {
            const picked = workerIds.includes(agent.id);
            const label = agent.name_i18n?.[localeKey] || agent.name;
            const blocked = agent.agent_status && agent.agent_status !== 'online';
            const row = (
              <Button
                key={agent.id}
                long
                type='text'
                disabled={Boolean(blocked)}
                className='!h-40px !justify-start !rounded-8px !px-8px'
                onClick={() => onToggleWorker(agent.id)}
              >
                <span className='flex w-full items-center justify-between gap-8px'>
                  <span className='truncate text-13px text-t-primary'>
                    {agent.avatar ? `${agent.avatar} ` : ''}
                    {label}
                  </span>
                  {picked ? (
                    <StatusBadge tone='ready' label={t('conversation.group.selected')} />
                  ) : (
                    <Plus theme='outline' size={14} />
                  )}
                </span>
              </Button>
            );
            return blocked ? (
              <Tooltip key={agent.id} content={agent.agent_status_message || t('conversation.group.unavailable')}>
                {row}
              </Tooltip>
            ) : (
              row
            );
          })}
        </div>
      </div>
    </div>
  );
};

export default GuidTeamPanel;
