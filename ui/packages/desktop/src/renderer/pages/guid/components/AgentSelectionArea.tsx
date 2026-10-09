/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import styles from '../index.module.css';
import { agentRuntimeKey, type Agent } from '@/common/types/agent/agentTypes';
import { Down, Robot } from '@icon-park/react';
import { Button } from '@arco-design/web-react';
import { AionSearchInput } from '@/renderer/components/base';
import { useAgentOrder } from '@/renderer/hooks/agent/useAgentOrder';
import { useManagedAgentRuntimeCatalog } from '@/renderer/hooks/agent/useManagedAgents';
import { managedAgentSearchText } from '@/renderer/utils/model/agentTypes';
import React, { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { resolveAgentAvatar } from '@/renderer/utils/model/agentAvatar';
import ThemedLogo from '@/renderer/components/agent/ThemedLogo';
import { selectableAgents } from '@/renderer/utils/model/agentSelection';
import { useTranslation } from 'react-i18next';

/**
 * Mirrors the Agent settings search: matches the agent's own name and
 * description plus the joined runtime-agent haystack (backend/command/binary),
 * so both surfaces find the same agents for the same query.
 */
export function agentMatchesSearch(
  agent: Agent,
  localeKey: string,
  query: string,
  agentSearchText?: string
): boolean {
  const searchableText = [
    agent.name,
    agent.name_i18n?.[localeKey],
    agent.description,
    agent.description_i18n?.[localeKey],
    agentSearchText,
  ]
    .filter(Boolean)
    .join(' ')
    .toLowerCase();
  return searchableText.includes(query);
}

export function resolveAgentVisibleLimit(width: number): number {
  if (width >= 720) return 4;
  if (width >= 600) return 3;
  if (width >= 460) return 2;
  return 1;
}

export function hasTruncatedAgentLabels(root: HTMLElement | null): boolean {
  if (!root) return false;
  return Array.from(root.querySelectorAll<HTMLElement>('[data-agent-label="true"]')).some(
    (element) => element.scrollWidth > element.clientWidth + 1
  );
}

type AgentSelectionAreaProps = {
  selectedAgentId?: string | null;
  agents: Agent[];
  localeKey: string;
  maxVisibleAgents?: number;
  onSelectAgent: (agentId: string) => void;
};

const AgentSelectionArea: React.FC<AgentSelectionAreaProps> = ({
  selectedAgentId,
  agents,
  localeKey,
  maxVisibleAgents = 4,
  onSelectAgent,
}) => {
  const { t } = useTranslation();
  const { agentOrder } = useAgentOrder();
  const [moreVisible, setMoreVisible] = useState(false);
  const [search, setSearch] = useState('');
  const [availableWidth, setAvailableWidth] = useState(() => (typeof window === 'undefined' ? 800 : window.innerWidth));
  const containerRef = useRef<HTMLDivElement>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const hoverOpenTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const hoverCloseTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const selectedId = selectedAgentId || undefined;
  const widthVisibleLimit = Math.min(Math.max(1, maxVisibleAgents), resolveAgentVisibleLimit(availableWidth));
  const [adaptiveVisibleLimit, setAdaptiveVisibleLimit] = useState(widthVisibleLimit);
  const visibleLimit = Math.min(widthVisibleLimit, adaptiveVisibleLimit);
  const enabledAgents = useMemo(
    () => selectableAgents(agents, agentOrder),
    [agentOrder, agents]
  );

  useEffect(() => {
    setAdaptiveVisibleLimit(widthVisibleLimit);
  }, [enabledAgents, selectedId, widthVisibleLimit]);

  const clearHoverTimers = () => {
    if (hoverOpenTimer.current) {
      clearTimeout(hoverOpenTimer.current);
      hoverOpenTimer.current = null;
    }
    if (hoverCloseTimer.current) {
      clearTimeout(hoverCloseTimer.current);
      hoverCloseTimer.current = null;
    }
  };

  useEffect(() => clearHoverTimers, []);

  const handleBarMouseEnter = () => {
    clearHoverTimers();
    // Slight delay so a mouse passing through the bar doesn't flash the panel.
    hoverOpenTimer.current = setTimeout(() => setMoreVisible(true), 120);
  };

  const handleBarMouseLeave = () => {
    clearHoverTimers();
    // Grace period keeps the panel open while the mouse travels into it.
    hoverCloseTimer.current = setTimeout(() => setMoreVisible(false), 240);
  };

  useEffect(() => {
    if (!moreVisible) return;

    const handlePointerDown = (event: MouseEvent) => {
      if (barRef.current && event.target instanceof Node && !barRef.current.contains(event.target)) {
        setMoreVisible(false);
      }
    };
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setMoreVisible(false);
      }
    };

    document.addEventListener('mousedown', handlePointerDown);
    document.addEventListener('keydown', handleEscape);
    return () => {
      document.removeEventListener('mousedown', handlePointerDown);
      document.removeEventListener('keydown', handleEscape);
    };
  }, [moreVisible]);

  useEffect(() => {
    const updateAvailableWidth = () => {
      setAvailableWidth(containerRef.current?.offsetWidth || (typeof window === 'undefined' ? 800 : window.innerWidth));
    };

    updateAvailableWidth();

    const element = containerRef.current;
    if (element && typeof ResizeObserver !== 'undefined') {
      const observer = new ResizeObserver((entries) => {
        const width = entries[0]?.contentRect.width;
        if (typeof width === 'number') {
          setAvailableWidth(width);
        }
      });
      observer.observe(element);
      return () => observer.disconnect();
    }

    if (typeof window === 'undefined') {
      return;
    }

    window.addEventListener('resize', updateAvailableWidth);
    return () => window.removeEventListener('resize', updateAvailableWidth);
  }, []);

  const visibleAgents = useMemo(() => {
    if (enabledAgents.length <= visibleLimit || !selectedId) {
      return enabledAgents.slice(0, visibleLimit);
    }

    const selectedIndex = enabledAgents.findIndex((agent) => agent.id === selectedId);
    if (selectedIndex < 0 || selectedIndex < visibleLimit) {
      return enabledAgents.slice(0, visibleLimit);
    }

    return [...enabledAgents.slice(0, visibleLimit - 1), enabledAgents[selectedIndex]];
  }, [enabledAgents, selectedId, visibleLimit]);

  useLayoutEffect(() => {
    if (visibleLimit <= 1 || !hasTruncatedAgentLabels(containerRef.current)) {
      return;
    }

    setAdaptiveVisibleLimit((currentLimit) => Math.max(1, Math.min(currentLimit, visibleLimit) - 1));
  }, [visibleAgents, visibleLimit]);

  const hasOverflow = enabledAgents.length > visibleAgents.length;
  const overflowAgents = useMemo(() => {
    const visibleIds = new Set(visibleAgents.map((agent) => agent.id));
    return enabledAgents.filter((agent) => !visibleIds.has(agent.id));
  }, [enabledAgents, visibleAgents]);
  const overflowColumns = widthVisibleLimit;
  // Search only earns its row when the unfiltered list is long enough to scan.
  const showOverflowSearch = Math.ceil(overflowAgents.length / overflowColumns) > 5;
  const managedAgentRuntimeCatalog = useManagedAgentRuntimeCatalog();
  // Same haystack as the Agent settings search: joining the runtime catalog row
  // adds backend/command/binary fields, so e.g. "ag" finds Antigravity (`agy`).
  const agentSearchTextById = useMemo(() => {
    const map = new Map<string, string>();
    for (const agent of managedAgentRuntimeCatalog) {
      map.set(agent.id, managedAgentSearchText(agent, localeKey));
    }
    return map;
  }, [localeKey, managedAgentRuntimeCatalog]);
  const filteredOverflowAgents = useMemo(() => {
    const query = showOverflowSearch ? search.trim().toLowerCase() : '';
    if (!query) return overflowAgents;
    return overflowAgents.filter((agent) =>
      agentMatchesSearch(
        agent,
        localeKey,
        query,
        agent.agent_id ? agentSearchTextById.get(agent.agent_id) : undefined
      )
    );
  }, [agentSearchTextById, localeKey, overflowAgents, search, showOverflowSearch]);

  if (enabledAgents.length === 0) return null;

  const renderAgentPill = (agent: Agent, testId: string, fullWidth = false) => {
    const avatar = resolveAgentAvatar(agent.avatar);
    const isSelected = selectedId === agent.id;
    const label = agent.name_i18n?.[localeKey] || agent.name;

    return (
      <Button
        key={agent.id}
        data-testid={testId}
        data-agent-id={agent.id}
        data-agent-backend={agentRuntimeKey(agent)}
        data-agent-selected={isSelected ? 'true' : 'false'}
        type='text'
        className={`!inline-flex !min-w-0 !h-auto !items-center !gap-6px !rounded-999px !border-none !px-12px !py-8px !text-13px transition-all ${
          fullWidth ? '!w-full !justify-start' : ''
        } ${
          isSelected
            ? 'font-600 text-t-primary shadow-sm'
            : `text-t-secondary opacity-75 hover:opacity-100 ${styles.agentSelectorInactive}`
        }`}
        style={isSelected ? { background: 'var(--bg-base, #fff)' } : { background: 'transparent' }}
        onClick={() => {
          onSelectAgent(agent.id);
          setMoreVisible(false);
        }}
      >
        <span className='inline-flex h-20px w-20px items-center justify-center overflow-hidden rounded-999px bg-fill-2'>
          {avatar.kind === 'image' ? (
            <ThemedLogo src={avatar.value} alt='' className='object-contain' style={{ width: 20, height: 20 }} />
          ) : avatar.kind === 'emoji' ? (
            <span className={styles.agentCardEmoji}>{avatar.value}</span>
          ) : (
            <Robot theme='outline' size={14} />
          )}
        </span>
        <span data-agent-label='true' className='min-w-0 max-w-180px truncate whitespace-nowrap'>
          {label}
        </span>
      </Button>
    );
  };

  const overflowPanel = (
    <div
      data-testid='agent-overflow-panel'
      data-overflow-columns={overflowColumns}
      className={`absolute start-0 top-[calc(100%+8px)] z-100 w-full rounded-12px border border-border-2 p-8px shadow-lg ${styles.agentOverflowPanel}`}
      style={{ background: 'var(--bg-base, #fff)' }}
    >
      {showOverflowSearch ? (
        <div className='mb-8px'>
          <AionSearchInput
            className='w-full'
            value={search}
            onChange={setSearch}
            placeholder={t('team.create.searchPlaceholder', { defaultValue: 'Search' })}
          />
        </div>
      ) : null}
      <div
        className='grid max-h-260px gap-6px overflow-y-auto'
        style={{ gridTemplateColumns: `repeat(${overflowColumns}, minmax(0, 1fr))` }}
      >
        {filteredOverflowAgents.map((agent) => (
          <div key={agent.id} className='min-w-0'>
            {renderAgentPill(agent, `agent-overflow-${agent.id}`, true)}
          </div>
        ))}
      </div>
    </div>
  );

  return (
    <div ref={containerRef} className='mt-18px mb-16px w-full'>
      <div className='flex w-full justify-center'>
        <div
          ref={barRef}
          className='relative inline-flex max-w-full items-center rounded-999px px-6px py-6px'
          style={{ background: 'var(--color-guid-agent-bar, var(--aou-2))' }}
          onMouseEnter={hasOverflow ? handleBarMouseEnter : undefined}
          onMouseLeave={hasOverflow ? handleBarMouseLeave : undefined}
        >
          <div className='flex min-w-0 max-w-full items-center gap-6px'>
            {visibleAgents.map((agent) => renderAgentPill(agent, `preset-pill-${agent.id}`))}
            {hasOverflow ? (
              <Button
                data-testid='agent-more-btn'
                type='text'
                className={`!ms-6px !inline-flex !h-34px !shrink-0 !items-center !gap-4px !rounded-999px !border-none !px-12px !py-8px !text-13px !text-t-secondary opacity-75 transition-opacity hover:opacity-100 ${styles.agentSelectorInactive}`}
                onClick={() => setMoreVisible((visible) => !visible)}
              >
                <span>{t('common.more', { defaultValue: 'More' })}</span>
                <Down theme='outline' size={14} />
              </Button>
            ) : null}
          </div>
          {hasOverflow && moreVisible ? overflowPanel : null}
        </div>
      </div>
    </div>
  );
};

export default AgentSelectionArea;
