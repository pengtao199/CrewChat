/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import type { IModelView, IProvider, ModelTag } from '@/common/types/provider/providerApi';
import { modelIds } from '@/common/types/provider/providerApi';
import { Button, Dropdown, Input, Menu, Message, Popconfirm, Switch, Tag, Tooltip } from '@arco-design/web-react';
import { Delete, Down, Edit, More, Plus } from '@icon-park/react';
import React, { useCallback, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import AionScrollArea from '@/renderer/components/base/AionScrollArea';
import EmptyState from '@/renderer/components/base/EmptyState';
import SettingsCardList from '@/renderer/components/base/SettingsCardList';
import SettingsCardRow from '@/renderer/components/base/SettingsCardRow';
import StatusBadge from '@/renderer/components/base/StatusBadge';
import DataLocationNote from '@/renderer/components/settings/DataLocationNote';
import SettingsPageHeader from '@/renderer/pages/settings/components/SettingsPageHeader';
import ModelEditorModal, { ModelMeta, tagColor, tagLabel } from '@/renderer/pages/settings/components/ModelEditorModal';
import type { ModelEditorTarget } from '@/renderer/pages/settings/components/ModelEditorModal';
import AddPlatformModal from '@/renderer/pages/settings/components/AddPlatformModal';
import EditModeModal from '@/renderer/pages/settings/components/EditModeModal';
import { useProvidersQuery } from '@/renderer/hooks/agent/useModelProviderList';
import { consumePendingDeepLink } from '@/renderer/hooks/system/useDeepLink';
import { useSettingsViewMode } from '../settingsViewContext';

/**
 * Settings → 模型.
 *
 * Provider 是「大脑池」条目，模型挂在它下面。模型 id 由服务商接口决定，没有可靠的
 * 自动发现方式，所以**手动添加是主路径**；「从端点获取」只是不确定 id 时的辅助。
 * 标签由用户打，视觉能力与价格来自 cc-switch（见 `core/model_catalog.py`）。
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
  const [modelEditor, setModelEditor] = useState<ModelEditorTarget | null>(null);
  const [addPlatformCtrl, addPlatformModal] = AddPlatformModal.useModal({
    onSubmit: (platform: IProvider) => {
      // 弹窗只组装，持久化在这里做（新增走 create，已存在走 update）。
      void (async () => {
        try {
          if (providers.some((p) => p.id === platform.id)) {
            await ipcBridge.mode.updateProvider.invoke({ id: platform.id, ...platform });
          } else {
            await ipcBridge.mode.createProvider.invoke(platform);
          }
          refresh();
        } catch (err) {
          message.error(err instanceof Error ? err.message : String(err));
        }
      })();
    },
  });
  const [editProviderCtrl, editProviderModal] = EditModeModal.useModal({
    onChange: () => refresh(),
  });

  // 深链（如从文档页跳转）会自动打开「添加服务商」。
  const pendingDeepLink = consumePendingDeepLink();

  const providers = data ?? [];

  const refresh = useCallback(() => {
    void mutate();
  }, [mutate]);

  const onToggleModel = useCallback(
    async (provider: IProvider, model: IModelView, enabled: boolean) => {
      try {
        await ipcBridge.mode.upsertModel.invoke({
          provider_id: provider.id,
          id: model.id,
          display_name: model.display_name,
          tags: model.tags,
          context_limit: model.context_limit ?? null,
          enabled,
          protocol: model.protocol ?? null,
        });
        refresh();
      } catch (err) {
        message.error(err instanceof Error ? err.message : String(err));
      }
    },
    [refresh, message]
  );

  const onToggleProvider = useCallback(
    async (provider: IProvider, enabled: boolean) => {
      // 供应商开关 = 该供应商下所有模型一起启停。
      try {
        await Promise.all(
          provider.models.map((m) =>
            ipcBridge.mode.upsertModel.invoke({
              provider_id: provider.id,
              id: m.id,
              display_name: m.display_name,
              tags: m.tags,
              context_limit: m.context_limit ?? null,
              enabled,
              protocol: m.protocol ?? null,
            })
          )
        );
        refresh();
      } catch (err) {
        message.error(err instanceof Error ? err.message : String(err));
      }
    },
    [refresh, message]
  );

  const onDeleteModel = useCallback(
    async (provider: IProvider, model: IModelView) => {
      try {
        await ipcBridge.mode.deleteModel.invoke({ provider_id: provider.id, model_id: model.id });
        message.success(t('settings.models.deleted'));
        refresh();
      } catch (err) {
        message.error(err instanceof Error ? err.message : String(err));
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

  // 页面上出现过的标签，供筛选条使用（而不是硬编码全部标签）。
  const availableTags = useMemo(() => {
    const set = new Set<ModelTag>();
    providers.forEach((p) => p.models.forEach((m) => m.tags.forEach((tag) => set.add(tag))));
    return Array.from(set);
  }, [providers]);

  const anyProvider = providers.some((p) => p.models.length > 0);

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

      {/* 搜索与标签筛选 */}
      {anyProvider && (
        <div className='flex flex-wrap items-center gap-8px px-16px pb-10px'>
          <Input
            className='max-w-260px'
            size='small'
            allowClear
            value={search}
            placeholder={t('settings.models.searchPlaceholder')}
            onChange={setSearch}
          />
          <div className='flex flex-wrap items-center gap-6px'>
            <Tag
              size='small'
              color={tagFilter === 'all' ? 'arcoblue' : 'gray'}
              className='cursor-pointer'
              onClick={() => setTagFilter('all')}
            >
              {t('settings.models.filterAll')}
            </Tag>
            {availableTags.map((tag) => (
              <Tag
                key={tag}
                size='small'
                color={tagFilter === tag ? 'arcoblue' : tagColor(tag)}
                className='cursor-pointer'
                onClick={() => setTagFilter(tagFilter === tag ? 'all' : tag)}
              >
                {tagLabel(tag, t)}
              </Tag>
            ))}
          </div>
        </div>
      )}

      <AionScrollArea className='flex-1 min-h-0' disableOverflow={isPageMode}>
        <div className='px-16px pb-16px'>
          {providers.length === 0 ? (
            <EmptyState>
              <div>{t('settings.models.noProviders')}</div>
              <div className='mt-4px text-12px opacity-70'>{t('settings.models.noProvidersHint')}</div>
            </EmptyState>
          ) : (
            <SettingsCardList className='flex flex-col gap-10px'>
              {providers.map((provider) => {
                const expanded = collapsed[provider.id] ?? true;
                const shown = filterModels(provider);
                const enabledCount = provider.models.filter((m) => m.enabled).length;

                return (
                  <div key={provider.id} className='rounded-12px border border-solid border-border-1 bg-base'>
                    {/* 供应商头：模型数与 Key 数常显 */}
                    <div className='flex items-center justify-between gap-12px px-14px py-10px'>
                      {/* 折叠区只包住箭头+名称，不要 flex-1 —— 否则整行会被拉成一条色块 */}
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
                      </div>

                      <div className='flex flex-shrink-0 items-center gap-10px'>
                        <span className='hidden items-center gap-8px text-12px text-t-secondary md:flex'>
                          <span>
                            {t('settings.models.modelCount')} ({provider.model_count})
                          </span>
                          <span className='text-t-tertiary'>|</span>
                          <span>
                            {t('settings.models.keyCount')} ({provider.key_count})
                          </span>
                        </span>
                        <span className='text-12px text-t-secondary md:hidden'>
                          {provider.model_count} / {provider.key_count}
                        </span>

                        {!provider.has_key && (
                          <StatusBadge tone='warning' label={t('settings.models.noKey')} />
                        )}

                        <Tooltip
                          content={
                            <span>
                              {t('settings.models.keyCount')} ({provider.key_count})
                              <br />
                              {provider.base_url}
                            </span>
                          }
                        >
                          <Switch
                            size='small'
                            checked={enabledCount > 0}
                            onChange={(checked) => onToggleProvider(provider, checked)}
                          />
                        </Tooltip>

                        <Button
                          size='mini'
                          className='!w-28px !h-28px !min-w-28px'
                          icon={<Plus size='14' />}
                          onClick={() => setModelEditor({ provider })}
                        />
                        <Button
                          size='mini'
                          className='!w-28px !h-28px !min-w-28px'
                          icon={<Edit size='14' />}
                          onClick={() => editProviderCtrl.open({ data: provider })}
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
                      <div className='border-t border-solid border-border-1 px-8px pb-8px'>
                        {shown.length === 0 ? (
                          <div className='py-16px text-center text-12px text-t-tertiary'>
                            {provider.models.length === 0
                              ? t('settings.models.noModels')
                              : t('settings.models.filterAll')}
                          </div>
                        ) : (
                          <SettingsCardList className='flex flex-col gap-0'>
                            {shown.map((model) => (
                              <SettingsCardRow
                                key={model.id}
                                title={
                                  <span className='flex min-w-0 items-center gap-6px'>
                                    <span className='truncate'>{model.label}</span>
                                    {/* 自动标签用浅色，视觉上与手工标签区分 */}
                                    {model.tags.map((tag) => (
                                      <Tag
                                        key={tag}
                                        size='small'
                                        color={tagColor(tag)}
                                        className='shrink-0'
                                      >
                                        {tagLabel(tag, t)}
                                      </Tag>
                                    ))}
                                  </span>
                                }
                                subtitle={<ModelMeta model={model} />}
                                badges={
                                  !model.enabled ? (
                                    <StatusBadge tone='neutral' label={t('settings.models.disabled')} />
                                  ) : undefined
                                }
                                actions={
                                  <>
                                    <Switch
                                      size='small'
                                      checked={model.enabled}
                                      onChange={(checked) => onToggleModel(provider, model, checked)}
                                    />
                                    <Dropdown
                                      droplist={
                                        <Menu>
                                          <Menu.Item
                                            key='edit'
                                            onClick={() => setModelEditor({ provider, model })}
                                          >
                                            {t('settings.models.editModel')}
                                          </Menu.Item>
                                          <Menu.Item key='copy' onClick={() => void navigator.clipboard?.writeText(model.id)}>
                                            {t('common.copy')}
                                          </Menu.Item>
                                          <Menu.Item
                                            key='delete'
                                            className='!text-t-error'
                                            onClick={() => onDeleteModel(provider, model)}
                                          >
                                            {t('common.delete')}
                                          </Menu.Item>
                                        </Menu>
                                      }
                                    >
                                      <Button size='mini' className='!w-28px !h-28px !min-w-28px' icon={<More size='14' />} />
                                    </Dropdown>
                                  </>
                                }
                              />
                            ))}
                          </SettingsCardList>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}
            </SettingsCardList>
          )}

          <DataLocationNote path='.data/providers.jsonc' shared />
        </div>
      </AionScrollArea>

      <ModelEditorModal
        target={modelEditor}
        onClose={() => setModelEditor(null)}
        onSaved={refresh}
      />

      {addPlatformModal}
      {editProviderModal}
    </div>
  );
};

export default ModelModalContent;