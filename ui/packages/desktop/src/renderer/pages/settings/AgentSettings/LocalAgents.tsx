/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * [crewchat] 引擎页：本机执行引擎的管理（扫描 / 状态 / 安装 / 更新 / 网关通道）。
 *
 * 数据来自 `/api/crewchat/engines`（cc-switch 工具层扫描 + 我们执行器注册表），
 * 网关通道来自 `/api/crewchat/gateway`，端点来自 `/api/providers`。
 * 页面只做展示与请求，规则见 AGENTS/ui.md（共享骨架、Arco、i18n）。
 */

import { parseError } from '@/common/utils';
import ThemedLogo from '@/renderer/components/agent/ThemedLogo';
import EmptyState from '@/renderer/components/base/EmptyState';
import SettingsCardList from '@/renderer/components/base/SettingsCardList';
import SettingsCardRow from '@/renderer/components/base/SettingsCardRow';
import StatusBadge, { type StatusTone } from '@/renderer/components/base/StatusBadge';
import DataLocationNote from '@/renderer/components/settings/DataLocationNote';
import { resolveAgentAvatar, useAgentLogos } from '@/renderer/utils/model/agentLogo';
import { Avatar, Button, Message, Select, Switch, Tag, Tooltip } from '@arco-design/web-react';
import { LinkCloud, Refresh, Terminal } from '@icon-park/react';
import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import SettingsPageHeader from '../components/SettingsPageHeader';
import {
  fetchEngines,
  fetchGateway,
  fetchProviders,
  installEngines,
  setGatewayMode,
  updateEngines,
  type EngineView,
  type GatewayStatus,
  type ProviderOption,
} from './enginesApi';

// 这些 CLI 是我们可以经网关反代的"身体"，其余引擎只做扫描/安装/更新。
const GATEWAY_APPS = new Set(['codex', 'claude', 'opencode']);

type EngineStatusKey = 'ready' | 'notInstalled' | 'notRunnable' | 'conflict';

const engineStatus = (engine: EngineView): { tone: StatusTone; key: EngineStatusKey } => {
  if (!engine.installed) return { tone: 'blocked', key: 'notInstalled' };
  if (!engine.runnable) return { tone: 'blocked', key: 'notRunnable' };
  if (engine.conflict) return { tone: 'warning', key: 'conflict' };
  return { tone: 'ready', key: 'ready' };
};

const LocalAgents: React.FC = () => {
  const { t } = useTranslation();
  const logos = useAgentLogos();
  const [engines, setEngines] = useState<EngineView[]>([]);
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const [providers, setProviders] = useState<ProviderOption[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [providerByEngine, setProviderByEngine] = useState<Record<string, string>>({});

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [nextEngines, nextGateway, nextProviders] = await Promise.all([
        fetchEngines(),
        fetchGateway(),
        fetchProviders(),
      ]);
      setEngines(nextEngines);
      setGateway(nextGateway);
      setProviders(nextProviders);
    } catch (error) {
      Message.error(parseError(error));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const providerOptions = useMemo(
    () => providers.map((p) => ({ label: p.name, value: p.id })),
    [providers]
  );

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

  const toggleGateway = useCallback(
    async (engine: EngineView, enabled: boolean) => {
      const providerId = providerByEngine[engine.id] || providers[0]?.id;
      setBusy(`gw-${engine.id}`);
      try {
        const result = await setGatewayMode(engine.id, enabled, providerId);
        setGateway((prev) => (prev ? { ...prev, channels: result.channels } : prev));
      } catch (error) {
        Message.error(parseError(error));
      } finally {
        setBusy(null);
      }
    },
    [providerByEngine, providers]
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
    return engine.kind === 'api' ? <LinkCloud theme='outline' size='18' /> : <Terminal theme='outline' size='18' />;
  };

  const subtitleFor = (engine: EngineView) => {
    if (!engine.installed) return t('settings.engines.notInstalledHint');
    return [engine.source, engine.version, engine.path].filter((part) => !!part).join('  ·  ');
  };

  const gatewayReady = gateway?.available ?? false;

  return (
    <div data-testid='engines-page' className='flex flex-col gap-16px'>
      <SettingsPageHeader
        data-testid='engines-header'
        title={t('settings.engines.title', { defaultValue: '引擎' })}
        description={t('settings.engines.description')}
        actions={
          <Button
            size='small'
            type='outline'
            loading={loading}
            onClick={() => void refresh()}
            icon={<Refresh theme='outline' size='14' />}
          >
            {t('settings.engines.refresh', { defaultValue: '刷新' })}
          </Button>
        }
      />

      {loading && engines.length === 0 ? (
        <div className='text-12px text-t-tertiary'>{t('settings.engines.scanning', { defaultValue: '正在扫描…' })}</div>
      ) : (
        <SettingsCardList>
          {engines.map((engine) => {
            const status = engineStatus(engine);
            const showGateway = gatewayReady && engine.is_body && GATEWAY_APPS.has(engine.id);
            const channelOn = gateway?.channels?.[engine.id] ?? false;
            const refCount = engine.agent_ids.length;
            return (
              <SettingsCardRow
                key={engine.id}
                testId={`engine-row-${engine.id}`}
                icon={avatarFor(engine)}
                title={
                  <span className='flex min-w-0 items-center gap-8px'>
                    <span className='truncate'>{engine.name}</span>
                    <StatusBadge
                      tone={status.tone}
                      label={t(`settings.engines.status.${status.key}`)}
                      tooltip={
                        engine.conflict
                          ? t('settings.engines.conflictHint', { count: engine.installs.length })
                          : undefined
                      }
                    />
                    <Tag size='small' bordered={false} className='!shrink-0 !rounded-8px !bg-fill-2 !text-11px !text-t-secondary'>
                      {t(`settings.engineKind.${engine.kind}`, { defaultValue: engine.kind })}
                    </Tag>
                    {refCount > 0 ? (
                      <Tag size='small' bordered={false} className='!shrink-0 !rounded-8px !bg-fill-2 !text-11px !text-t-secondary'>
                        {t('settings.engines.refCount', { count: refCount })}
                      </Tag>
                    ) : null}
                  </span>
                }
                subtitle={subtitleFor(engine)}
                actions={
                  <>
                    {showGateway ? (
                      <>
                        <Tooltip content={t('settings.engines.viaGateway')}>
                          <span className='flex items-center gap-6px text-11px text-t-secondary'>
                            <Switch
                              size='small'
                              checked={channelOn}
                              loading={busy === `gw-${engine.id}`}
                              onChange={(checked) => void toggleGateway(engine, checked)}
                            />
                            {t('settings.engines.viaGateway')}
                          </span>
                        </Tooltip>
                        <Select
                          size='small'
                          style={{ width: 150 }}
                          value={providerByEngine[engine.id] || providers[0]?.id}
                          options={providerOptions}
                          placeholder={t('settings.engines.provider')}
                          onChange={(value) =>
                            setProviderByEngine((prev) => ({ ...prev, [engine.id]: String(value) }))
                          }
                        />
                      </>
                    ) : null}
                    {!engine.installed ? (
                      <Button
                        size='small'
                        type='primary'
                        loading={busy === engine.id}
                        onClick={() => void runLifecycle(engine, 'install')}
                      >
                        {t('settings.engines.install', { defaultValue: '安装' })}
                      </Button>
                    ) : engine.update_command ? (
                      <Button
                        size='small'
                        type='outline'
                        loading={busy === engine.id}
                        onClick={() => void runLifecycle(engine, 'update')}
                      >
                        {t('settings.engines.update', { defaultValue: '更新' })}
                      </Button>
                    ) : null}
                  </>
                }
              />
            );
          })}
          {engines.length === 0 ? <EmptyState>{t('settings.engines.empty')}</EmptyState> : null}
        </SettingsCardList>
      )}

      <DataLocationNote path={t('settings.engines.dataPath')} />
    </div>
  );
};

export default LocalAgents;
