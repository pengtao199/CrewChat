import React, { useMemo, useState } from 'react';
import { Button, Empty, Spin, Tooltip } from '@arco-design/web-react';
import { Plus } from '@icon-park/react';
import { useTranslation } from 'react-i18next';
import { AionSearchInput, AionInlineSearchInput } from '@renderer/components/base';
import { AgentOptionLabel, agentKey, type TeamAgentOption } from '../agentSelectUtils';

type Props = {
  agents: TeamAgentOption[];
  onSelect: (agent: TeamAgentOption) => void;
  disabled?: boolean;
  pendingAgentId?: string;
  testIdPrefix?: string;
  density?: 'compact' | 'modal';
  className?: string;
  footer?: React.ReactNode;
  /**
   * 搜索框样式：'default' 用常驻搜索栏样式（AionSearchInput，34px 描边）；
   * 'inline' 用下拉列表专用的柔和样式（AionInlineSearchInput，浅灰填充无边框）。
   * 点击展开的下拉列表传 'inline'，常驻列表用默认。
   */
  searchVariant?: 'default' | 'inline';
};

const TeamAgentPicker: React.FC<Props> = ({
  agents,
  onSelect,
  disabled = false,
  pendingAgentId,
  testIdPrefix = 'team-agent-picker',
  density = 'compact',
  className,
  footer,
  searchVariant = 'default',
}) => {
  const { t } = useTranslation();
  const isModalDensity = density === 'modal';
  const isInlineSearch = searchVariant === 'inline';
  const [query, setQuery] = useState('');
  const searchPlaceholder = t('team.create.searchPlaceholder', { defaultValue: 'Search' });
  const filteredAgents = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return agents;
    return agents.filter((agent) => agent.name.toLowerCase().includes(q));
  }, [agents, query]);

  return (
    <div className={`flex min-h-0 flex-col ${isModalDensity ? 'gap-12px' : ''} ${className ?? ''}`}>
      <div
        className={isModalDensity ? undefined : 'bg-dialog-fill-0 px-12px pt-12px'}
        data-testid={`${testIdPrefix}-search-shell`}
      >
        {isInlineSearch ? (
          <AionInlineSearchInput
            className='w-full'
            value={query}
            onChange={setQuery}
            placeholder={searchPlaceholder}
            data-testid={`${testIdPrefix}-search`}
          />
        ) : (
          <AionSearchInput
            className='w-full'
            value={query}
            onChange={setQuery}
            placeholder={searchPlaceholder}
            data-testid={`${testIdPrefix}-search`}
          />
        )}
      </div>
      <div
        data-testid={`${testIdPrefix}-picker-body`}
        className={
          isModalDensity
            ? 'min-h-0 flex-1 overflow-y-auto rounded-8px bg-dialog-fill-0'
            : 'max-h-300px overflow-y-auto border-b border-border-1 bg-dialog-fill-0 px-8px py-10px'
        }
      >
        {filteredAgents.length === 0 ? (
          <Empty description={t('team.create.noSearchResults', { defaultValue: 'No results found' })} />
        ) : (
          <div className={isModalDensity ? 'flex flex-col gap-6px' : 'flex flex-col gap-6px'}>
            {filteredAgents.map((agent, index) => {
              const rowKey = `${agentKey(agent)}-${index}`;
              const rowBlocked = agent.team_selectable === false;
              const rowDisabled = disabled || rowBlocked;
              const isPending = pendingAgentId === agent.id;
              const blockReason = rowBlocked
                ? agent.team_block_reason ||
                  t('settings.agentTeamUnsupported', {
                    defaultValue: 'This agent cannot be used in team mode right now.',
                  })
                : undefined;
              const rowBaseClassName = isModalDensity
                ? '!h-44px !justify-start !rounded-8px !px-8px !py-0'
                : '!h-48px !justify-start !rounded-8px !px-6px !py-0';
              const rowStateClassName = rowDisabled
                ? '!cursor-not-allowed !text-t-tertiary hover:!bg-transparent'
                : 'hover:!bg-fill-2';
              const rowClassName = `w-full ${rowBaseClassName} ${rowStateClassName}`;
              const rowContentClassName = `flex min-w-0 w-full flex-1 items-center justify-between gap-12px ${
                rowDisabled ? 'cursor-not-allowed text-t-tertiary' : ''
              }`;
              const addIconBoxClassName = isModalDensity ? 'h-30px w-30px' : 'h-32px w-32px';
              const addIcon = (
                <span
                  className={`flex ${addIconBoxClassName} shrink-0 items-center justify-center ${
                    rowDisabled ? 'text-t-quaternary' : 'text-t-secondary'
                  }`}
                  data-testid='team-agent-picker-add-icon'
                >
                  {/* Keep the fixed-size icon box; swap the plus for an inline spinner while this
                      row's add is in flight so the row geometry never shifts. Using Arco Button's
                      own `loading` prop would inject a spinner at the start of the flex content and
                      break the row layout. */}
                  {isPending ? (
                    <Spin size={isModalDensity ? 16 : 14} />
                  ) : (
                    <Plus theme='outline' size={isModalDensity ? '16' : '14'} fill='currentColor' />
                  )}
                </span>
              );
              const row = blockReason ? (
                <Button
                  long
                  type='text'
                  className={rowClassName}
                  aria-disabled='true'
                  tabIndex={-1}
                  data-testid={`${testIdPrefix}-option-${agentKey(agent)}`}
                >
                  <div className={rowContentClassName} data-testid='team-agent-picker-row-content'>
                    <div
                      className='min-w-0 flex flex-1 flex-col items-start overflow-hidden'
                      data-testid='team-agent-picker-text'
                    >
                      <AgentOptionLabel agent={agent} size={isModalDensity ? 'large' : 'compact'} muted />
                    </div>
                    {addIcon}
                  </div>
                </Button>
              ) : (
                <Button
                  long
                  type='text'
                  disabled={disabled || isPending}
                  className={rowClassName}
                  onClick={() => onSelect(agent)}
                  data-testid={`${testIdPrefix}-option-${agentKey(agent)}`}
                >
                  <div className={rowContentClassName} data-testid='team-agent-picker-row-content'>
                    <div
                      className='min-w-0 flex flex-1 flex-col items-start overflow-hidden'
                      data-testid='team-agent-picker-text'
                    >
                      <AgentOptionLabel
                        agent={agent}
                        size={isModalDensity ? 'large' : 'compact'}
                        muted={disabled}
                      />
                    </div>
                    {addIcon}
                  </div>
                </Button>
              );
              return (
                <div key={rowKey}>
                  {blockReason ? (
                    // The picker can live inside a high z-index dropdown that is portaled to
                    // document.body (e.g. the mobile add-member dropdown at z-index ~10020).
                    // Mount the tooltip on document.body too — otherwise it stays trapped in the
                    // modal's stacking context and its z-index can't beat the sibling dropdown —
                    // and raise its z-index above the dropdown so the block reason shows on top.
                    <Tooltip
                      content={blockReason}
                      getPopupContainer={() => document.body}
                      triggerProps={{ style: { zIndex: 10060 } }}
                    >
                      {row}
                    </Tooltip>
                  ) : (
                    row
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
      {footer ? (
        <div
          className='bg-dialog-fill-0 px-14px py-10px text-12px font-600 leading-18px text-t-tertiary'
          data-testid={`${testIdPrefix}-footer`}
        >
          {footer}
        </div>
      ) : null}
    </div>
  );
};

export default TeamAgentPicker;
