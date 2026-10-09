/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * [crewchat] 引擎页：本机执行引擎（CLI）的状态与维护。
 *
 * 只做引擎本身：装没装、能不能跑、有没有新版、多处冲突。按状态分组为「已安装 / 未安装」
 * 两个 tab；版本与更新就在各自那一行上。模型端点与网关通道不在这里（归 Agent 的大脑）。
 *
 * 探测很重（本地扫 + 联网查版本），所以用 SWR 缓存：进入页面即时显示上次结果，只有点
 * "刷新"或装机/更新后才会重扫。规则见 AGENTS/ui.md（共享骨架、Arco、i18n；状态由后端算好）。
 */

import { parseError } from '@/common/utils';
import ThemedLogo from '@/renderer/components/agent/ThemedLogo';
import EmptyState from '@/renderer/components/base/EmptyState';
import SettingsCardList from '@/renderer/components/base/SettingsCardList';
import SettingsCardRow from '@/renderer/components/base/SettingsCardRow';
import StatusBadge, { type StatusTone } from '@/renderer/components/base/StatusBadge';
import { resolveAgentAvatar, useAgentLogos } from '@/renderer/utils/model/agentLogo';
import { Avatar, Button, Message, Tag } from '@arco-design/web-react';
import { Refresh, Terminal } from '@icon-park/react';
import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import useSWR from 'swr';
import SettingsPageHeader from '../components/SettingsPageHeader';
import {
  ENGINES_SWR_KEY,
  fetchEngines,
  installEngines,
  updateEngines,
  type EngineStatus,
  type EngineView,
} from './enginesApi';

const STATUS_TONE: Record<EngineStatus, StatusTone> = {
  ready: 'ready',
  not_installed: 'blocked',
  broken: 'blocked',
  conflict: 'warning',
};

type EngineTab = 'installed' | 'not_installed';

const LocalAgents: React.FC = () => {
  const { t } = useTranslation();
  const logos = useAgentLogos();
  const [busy, setBusy] = useState<string | null>(null);
  const [tab, setTab] = useState<EngineTab>('installed');

  // 缓存优先：同一会话内再次进入不重扫；只有显式 refresh 走 ?refresh=true。
  const { data, isLoading, mutate } = useSWR<EngineView[]>(ENGINES_SWR_KEY, () => fetchEngines(), {
    revalidateOnFocus: false,
    revalidateOnReconnect: false,
    revalidateIfStale: false,
    dedupingInterval: 5 * 60_000,
  });
  const engines = data ?? [];

  const refresh = useCallback(() => mutate(fetchEngines(true), { revalidate: false }), [mutate]);

  const installed = useMemo(() => engines.filter((e) => e.installed), [engines]);
  const notInstalled = useMemo(() => engines.filter((e) => !e.installed), [engines]);
  const shown = tab === 'installed' ? installed : notInstalled;

  // 版本探测在后台跑（首次较慢、且被缓存 6 小时）。有结果前短暂轮询把它带出来，最多 8 次。
  const versionsPending = engines.some((e) => e.installed && !e.latest_version);
  const polls = useRef(0);
  useEffect(() => {
    if (!versionsPending) {
      polls.current = 0;
      return;
    }
    const id = setInterval(() => {
      polls.current += 1;
      void mutate();
      if (polls.current >= 8) clearInterval(id);
    }, 5000);
    return () => clearInterval(id);
  }, [versionsPending, mutate]);

  const runLifecycle = useCallback(
    async (engine: EngineView, action: 'install' | 'update') => {
      setBusy(engine.id);
      try {
        await (action === 'install' ? installEngines([engine.id]) : updateEngines([engine.id]));
        Message.success(t(`settings.engines.${action}Done`, { name: engine.name }));
        await refresh();
      } catch (error) {
        Message.error(parseError(error));
      } finally {
        setBusy(null);
      }
    },
    [refresh, t]
  );

  const avatarFor = (engine: EngineView) => {
    const avatar = resolveAgentAvatar(logos, { backend: engine.id });
    if (avatar.kind === 'image') {
      return (
        <Avatar size={28} shape='square' style={{ backgroundColor: 'transparent' }}>
          <ThemedLogo
            src={avatar.value}
            alt={engine.name}
            style={{ width: 28, height: 28, objectFit: 'contain', color: 'var(--text-primary)' }}
          />
        </Avatar>
      );
    }
    if (avatar.kind === 'emoji') return <span className='text-18px leading-none'>{avatar.value}</span>;
    return <Terminal theme='outline' size='18' />;
  };

  const subtitleFor = (engine: EngineView) => {
    if (!engine.installed) return t('settings.engines.notInstalledHint');
    const head = [engine.source, engine.version].filter((part) => !!part).join('  ·  ');
    const fresh =
      engine.has_update && engine.latest_version ? `  ${t('settings.engines.newVersion', { version: engine.latest_version })}` : '';
    const tail = engine.path ? `  ·  ${engine.path}` : '';
    return `${head}${fresh}${tail}`;
  };

  const statusTip = (engine: EngineView) => {
    if (engine.status === 'broken') return engine.error || t('settings.engines.brokenHint');
    if (engine.status === 'conflict') return t('settings.engines.conflictHint', { count: engine.installs.length });
    return undefined;
  };

  const actionsFor = (engine: EngineView) => {
    if (engine.status === 'not_installed') {
      return (
        <Button size='small' type='primary' loading={busy === engine.id} onClick={() => void runLifecycle(engine, 'install')}>
          {t('settings.engines.install')}
        </Button>
      );
    }
    if (engine.status === 'broken' || engine.status === 'conflict') {
      return (
        <Button size='small' type='outline' loading={busy === engine.id} onClick={() => void runLifecycle(engine, 'update')}>
          {t('settings.engines.repair')}
        </Button>
      );
    }
    if (engine.status === 'ready' && engine.has_update) {
      return (
        <Button size='small' type='outline' loading={busy === engine.id} onClick={() => void runLifecycle(engine, 'update')}>
          {t('settings.engines.update')}
        </Button>
      );
    }
    return null;
  };

  return (
    <div data-testid='engines-page' className='flex flex-col gap-16px'>
      <SettingsPageHeader
        data-testid='engines-header'
        title={t('settings.engines.title')}
        description={t('settings.engines.description')}
        actions={
          <Button
            size='small'
            type='outline'
            loading={isLoading}
            onClick={() => void refresh()}
            icon={<Refresh theme='outline' size='14' />}
          >
            {t('settings.engines.refresh')}
          </Button>
        }
        tabs={[
          { key: 'installed', label: t('settings.engines.tabInstalled'), count: installed.length },
          { key: 'not_installed', label: t('settings.engines.tabNotInstalled'), count: notInstalled.length },
        ]}
        activeTab={tab}
        onTabChange={(key) => setTab(key as EngineTab)}
      />

      {isLoading && engines.length === 0 ? (
        <div className='text-12px text-t-tertiary'>{t('settings.engines.scanning')}</div>
      ) : (
        <SettingsCardList>
          {shown.map((engine) => {
            const refCount = engine.agent_ids.length;
            return (
              <SettingsCardRow
                key={engine.id}
                testId={`engine-row-${engine.id}`}
                icon={avatarFor(engine)}
                title={
                  <span className='flex min-w-0 items-center gap-8px'>
                    <span className='truncate'>{engine.name}</span>
                    <StatusBadge tone={STATUS_TONE[engine.status]} label={t(`settings.engines.status.${engine.status}`)} tooltip={statusTip(engine)} />
                    {refCount > 0 ? (
                      <Tag size='small' bordered={false} className='!shrink-0 !rounded-8px !bg-fill-2 !text-11px !text-t-secondary'>
                        {t('settings.engines.refCount', { count: refCount })}
                      </Tag>
                    ) : null}
                  </span>
                }
                subtitle={subtitleFor(engine)}
                actions={actionsFor(engine)}
              />
            );
          })}
          {shown.length === 0 ? (
            <EmptyState>{tab === 'installed' ? t('settings.engines.emptyInstalled') : t('settings.engines.emptyNotInstalled')}</EmptyState>
          ) : null}
        </SettingsCardList>
      )}
    </div>
  );
};

export default LocalAgents;
