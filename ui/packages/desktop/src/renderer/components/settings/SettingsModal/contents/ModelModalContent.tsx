/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import type { IModelView, IProvider, ModelTag } from '@/common/types/provider/providerApi';
import { Button, Input, Message, Popconfirm, Switch, Tooltip } from '@arco-design/web-react';
import { Delete, Down, Edit, Minus, More, Plus, Refresh } from '@icon-park/react';
import React, { useCallback, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import AionScrollArea from '@/renderer/components/base/AionScrollArea';
import EmptyState from '@/renderer/components/base/EmptyState';
import SettingsCardList from '@/renderer/components/base/SettingsCardList';
import SettingsCardRow from '@/renderer/components/base/SettingsCardRow';
import StatusBadge from '@/renderer/components/base/StatusBadge';
import DataLocationNote from '@/renderer/components/settings/DataLocationNote';
import SettingsPageHeader from '@/renderer/pages/settings/components/SettingsPageHeader';
import ModelDetailModal, { ModelMeta, ModelTags, tagColor, tagLabel } from '@/renderer/pages/settings/components/ModelDetailModal';
import AddPlatformModal from '@/renderer/pages/settings/components/AddPlatformModal';
import EditModeModal from '@/renderer/pages/settings/components/EditModeModal';
import { useProvidersQuery } from '@/renderer/hooks/agent/useModelProviderList';
import { useSettingsViewMode } from '../settingsViewContext';

/**
 * Settings → 模型.
 *
 * 新增供应商时后端会自动抓取该端点的**全部**模型（模型 id 无法可靠地人工枚举），
 * 标签由 cc-switch 的能力判定 + models.dev 价格目录自动推导。因此这个页面的
 * 主要职责不是"录入"，而是**在大量模型里定位与取舍**：
 * 搜索、标签筛选、批量启停。
 */
const ModelModalContent: React.FC = () => {
  const { t } = useTranslation();
  const viewMode = useSettingsViewMode();
  const isPageMode = viewMode === 'page';
  const [message, messageContext] = Message.useMessage();
  const { data, mutate } = useProvidersQuery();

  const [search, setSearch] = useState('');
  const [tagFilter, setTagFilter] = useState<ModelTag | 'all'>('all');
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const [detail, setDetail] = useState<{ provider: IProvider; model: IModelView } | null>(null);
  const [busy, setBusy] = useState('');

  const providers = data ?? [];

  const refresh = useCallback(() => {
    void mutate();
  }, [mutate]);

  const [addPlatformCtrl, addPlatformModal] = AddPlatformModal.useModal({
    onSubmit: (platform: IProvider) => {
      // 弹窗只组装，持久化在这里做（后端会自动抓取全部模型）。
      void (async () => {
        setBusy(platform.id);
        try {
          const created =
            providers.some((p) => p.id === platform.id)
              ? await ipcBridge.mode.updateProvider.invoke({ id: platform.id, ...platform })
              : await ipcBridge.mode.createProvider.invoke(platform);
          message.success(t('settings.models.fetched', { count: created.model_count ?? created.models?.length ?? 0 }));
          refresh();
        } catch (err) {
          message.error(err instanceof Error ? err.message : String(err));
        } finally {
          setBusy('');
        }
      })();
    },
  });

  const [editProviderCtrl, editProviderModal] = EditModeModal.useModal({
    onChange: () => refresh(),
  });

  /** 单个模型启停。 */
  const onToggleModel = useCallback(
    async (provider: IProvider, model: IModelView, enabled: boolean) => {
      setBusy(`${provider.id}:${model.id}`);
      try {
        await ipcBridge.mode.bulkToggleModels.invoke({ provider_id: provider.id, ids: [model.id], enabled });
        refresh();
      } catch (err) {
        message.error(err instanceof Error ? err.message : String(err));
      } finally {
        setBusy('');
      }
    },
    [refresh, message]
  );

  /** 批量启停：不传 ids 即作用于该供应商全部模型（自动抓取后可能有上百个）。 */
  const onBulkToggle = useCallback(
    async (provider: IProvider, enabled: boolean, ids?: string[]) => {
      setBusy(`${provider.id}:bulk`);
      try {
        await ipcBridge.mode.bulkToggleModels.invoke({ provider_id: provider.id, ids: ids ?? null, enabled });
        refresh();
      } catch (err) {
        message.error(err instanceof Error ? err.message : String(err));
      } finally {
        setBusy('');
      }
    },
    [refresh, message]
  );

  const onDeleteModel = useCallback(
    async (provider: IProvider, model: IModelView) => {
      setBusy(`${provider.id}:${model.id}`);
      try {
        await ipcBridge.mode.deleteModel.invoke({ provider_id: provider.id, model_id: model.id });
        message.success(t('settings.models.deleted'));
        refresh();
      } catch (err) {
        message.error(err instanceof Error ? err.message : String(err));
      } finally {
        setBusy('');
      }
    },
    [refresh, message, t]
  );

  const onDeleteProvider = useCallback(
    async (provider: IProvider) => {
      try {
        await ipcBridge.mode.deleteProvider.invoke({ id: provider.id });
        message.success(t('settings.models.deleted'));
        refresh();
      } catch (err) {
        message.error(err instanceof Error ? err.message : String(err));
      }
    },
    [refresh, message, t]
  );

  const filterModels = useCallback(
    (provider: IProvider): IModelView[] => {
      const keyword = search.trim().toLowerCase();
      return provider.models.filter((m) => {
        if (tagFilter !== 'all' && !m.tags.includes(tagFilter)) return false;
        if (!keyword) return true;
        return m.id.toLowerCase().includes(keyword) || m.display_name.toLowerCase().includes(keyword);
      });
    },
    [search, tagFilter]
  );

  /** 页面上真实出现过的标签，供筛选条使用（而不是硬编码全部标签）。 */
  const availableTags = useMemo(() => {
    const counts = new Map<ModelTag, number>();
    providers.forEach((p) => p.models.forEach((m) => m.tags.forEach((tag) => counts.set(tag, (counts.get(tag) ?? 0) + 1))));
    return Array.from(counts.entries()).sort((a, b) => b[1] - a[1]);
  }, [providers]);

  const totalModels = providers.reduce((sum, p) => sum + p.models.length, 0);
  const totalEnabled = providers.reduce((sum, p) => sum + p.models.filter((m) => m.enabled).length, 0);

  const hasFilters = search.trim().length > 0 || tagFilter !== 'all';

  return (
    <div className='flex h-full flex-col'>
      {messageContext}
      <SettingsPageHeader
        title={t('settings.models.title')}
        description={t('settings.models.description')}
        actions={
          <Button type='primary' onClick={() => addPlatformCtrl.open()}>
            {t('settings.models.add')}
          </Button>
        }
      />

      <AionScrollArea className='flex-1 min-h-0' disableOverflow={isPageMode}>
        <div className='px-16px pb-16px'>
          {providers.length === 0 ? (
            <EmptyState>
              <div>{t('settings.models.noProviders')}</div>
              <div className='mt-4px text-12px opacity-70'>{t('settings.models.noProvidersHint')}</div>
            </EmptyState>
          ) : (
            <SettingsCardList className='gap-0'>
              {/* 工具条：搜索 + 标签筛选。与列表内容同 padding，属于这个容器的一部分。 */}
              <div className='flex flex-col gap-8px pb-10px'>
                <Input
                  size='small'
                  allowClear
                  value={search}
                  placeholder={t('settings.models.searchPlaceholder')}
                  onChange={setSearch}
                />
                <div className='flex flex-wrap items-center gap-6px'>
                  <span className='text-12px text-t-secondary tabular-nums'>
                    {hasFilters
                      ? t('settings.models.filtered', { shown: filteredTotal(providers, filterModels), total: totalModels })
                      : t('settings.models.totalModels', { total: totalModels, enabled: totalEnabled })}
                  </span>
                  <span className='text-t-tertiary'>|</span>
                  <FilterChip active={tagFilter === 'all'} onClick={() => setTagFilter('all')} label={t('settings.models.filterAll')} />
                  {availableTags.map(([tag, count]) => (
                    <FilterChip
                      key={tag}
                      active={tagFilter === tag}
                      onClick={() => setTagFilter(tagFilter === tag ? 'all' : tag)}
                      label={`${tagLabel(tag, t)} ${count}`}
                      color={tagColor(tag)}
                    />
                  ))}
                </div>
              </div>

              {providers.map((provider) => {
                const expanded = collapsed[provider.id] ?? true;
                const shown = filterModels(provider);
                const enabledCount = provider.models.filter((m) => m.enabled).length;
                const providerBusy = busy.startsWith(`${provider.id}:`);

                return (
                  <div key={provider.id} className='border-t border-solid border-border-2'>
                    {/* 供应商头：模型数与 Key 数常显 */}
                    <div className='flex items-center justify-between gap-12px py-10px'>
                      <div
                        role='button'
                        tabIndex={0}
                        className='flex min-w-0 cursor-pointer items-center gap-6px rounded-6px px-6px py-2px hover:bg-fill-2'
                        onClick={() => setCollapsed((prev) => ({ ...prev, [provider.id]: !expanded }))}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter' || e.key === ' ') {
                            e.preventDefault();
                            setCollapsed((prev) => ({ ...prev, [provider.id]: !expanded }));
                          }
                        }}
                      >
                        <Down size='14' className={`shrink-0 text-t-tertiary transition-transform ${expanded ? '' : '-rotate-90'}`} />
                        <span className='truncate text-14px font-medium text-t-primary'>{provider.name}</span>
                        <span className='shrink-0 text-12px tabular-nums text-t-secondary'>
                          {enabledCount}/{provider.model_count}
                        </span>
                        <span className='hidden shrink-0 text-12px tabular-nums text-t-tertiary md:inline'>
                          {`· ${t('settings.models.keyCount')} ${provider.key_count}`}
                        </span>
                      </div>

                      <div className='flex flex-shrink-0 items-center gap-6px'>
                        {!provider.has_key && <StatusBadge tone='warning' label={t('settings.models.noKey')} />}
                        {/* 批量启停：全部开 / 全部关 */}
                        <Tooltip content={t('settings.models.enableAll')}>
                          <Button
                            size='mini'
                            className='!w-28px !h-28px !min-w-28px'
                            icon={<Refresh size='14' />}
                            onClick={() => void onBulkToggle(provider, true)}
                          />
                        </Tooltip>
                        <Tooltip content={t('settings.models.disableAll')}>
                          <Button
                            size='mini'
                            className='!w-28px !h-28px !min-w-28px'
                            disabled={enabledCount === 0 || providerBusy}
                            icon={<Minus size='14' />}
                            onClick={() => void onBulkToggle(provider, false)}
                          />
                        </Tooltip>
                        <Button
                          size='mini'
                          className='!w-28px !h-28px !min-w-28px'
                          icon={<Plus size='14' />}
                          onClick={() => editProviderCtrl.open({ data: provider })}
                        />
                        <Button
                          size='mini'
                          className='!w-28px !h-28px !min-w-28px'
                          icon={<Edit size='14' />}
                          onClick={() => setDetail({ provider, model: provider.models[0] })}
                          disabled={!provider.models.length}
                        />
                        <Popconfirm
                          title={t('settings.models.confirmDeleteProvider', { name: provider.name })}
                          onOk={() => onDeleteProvider(provider)}
                        >
                          <Button size='mini' className='!w-28px !h-28px !min-w-28px' icon={<Delete size='14' />} />
                        </Popconfirm>
                      </div>
                    </div>

                    {expanded && (
                      <div className='pb-6px'>
                        {shown.length === 0 ? (
                          <div className='py-16px text-center text-12px text-t-tertiary'>
                            {provider.models.length === 0 ? t('settings.models.noModels') : t('settings.models.noMatch')}
                          </div>
                        ) : (
                          /* 分组列表里的子项不该是卡片：用分隔线而不是圆角+边框 */
                          <div className='flex flex-col'>
                            {shown.map((model) => (
                              <SettingsCardRow
                                key={model.id}
                                flat
                                title={
                                  <span className='flex min-w-0 flex-wrap items-center gap-6px'>
                                    <span className='truncate'>{model.label}</span>
                                    <ModelTags tags={model.tags} />
                                  </span>
                                }
                                subtitle={<ModelMeta model={model} />}
                                badges={!model.enabled ? <StatusBadge tone='neutral' label={t('settings.models.disabled')} /> : undefined}
                                actions={
                                  <>
                                    <Switch
                                      size='small'
                                      loading={busy === `${provider.id}:${model.id}`}
                                      checked={model.enabled}
                                      onChange={(checked) => void onToggleModel(provider, model, checked)}
                                    />
                                    <Button
                                      size='mini'
                                      className='!w-28px !h-28px !min-w-28px'
                                      icon={<More size='14' />}
                                      onClick={() => setDetail({ provider, model })}
                                    />
                                  </>
                                }
                              />
                            ))}
                          </div>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}

              <DataLocationNote path='.data/providers.jsonc' shared />
            </SettingsCardList>
          )}
        </div>
      </AionScrollArea>

      <ModelDetailModal
        model={detail?.model ?? null}
        provider={detail?.provider}
        onClose={() => setDetail(null)}
        onSaved={refresh}
      />
      {addPlatformModal}
      {editProviderModal}
    </div>
  );
};

/** 筛选标签胶囊。 */
const FilterChip: React.FC<{ active: boolean; onClick: () => void; label: string; color?: string }> = ({
  active,
  onClick,
  label,
  color,
}) => (
  <button
    type='button'
    onClick={onClick}
    className={`cursor-pointer rounded-4px border border-solid px-6px py-1px text-12px transition-colors ${
      active ? 'border-primary-6 bg-primary-1 text-primary-6' : 'border-transparent text-t-secondary hover:bg-fill-2'
    }`}
    style={!active && color ? { color: `var(--${color}-6)` } : undefined}
  >
    {label}
  </button>
);

/** 当前筛选下可见的模型总数。 */
const filteredTotal = (providers: IProvider[], filter: (p: IProvider) => IModelView[]): number =>
  providers.reduce((sum, p) => sum + filter(p).length, 0);

export default ModelModalContent;