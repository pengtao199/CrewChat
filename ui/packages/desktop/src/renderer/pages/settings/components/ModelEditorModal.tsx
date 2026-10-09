/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import type { IDiscoveredModel, IModelView, IProvider, ModelTag } from '@/common/types/provider/providerApi';
import { Button, Empty, Input, InputNumber, Message, Select, Tag, Tooltip } from '@arco-design/web-react';
import AionModal from '@/renderer/components/base/AionModal';
import { Check, Loading, Refresh } from '@icon-park/react';
import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

/** Tag vocabulary mirroring the backend's `MODEL_TAGS`. Order drives display order. */
const TAGS: ModelTag[] = ['text', 'vision', 'reasoning', 'tool_use', 'long_context', 'fast', 'free'];

/** 标签 → Arco Tag 颜色。只用语义色板，不自造色值。 */
const TAG_COLOR: Record<ModelTag, string> = {
  text: 'gray',
  vision: 'purple',
  reasoning: 'orange',
  tool_use: 'blue',
  long_context: 'arcoblue',
  fast: 'cyan',
  free: 'green',
};

export const tagLabel = (tag: ModelTag, t: (k: string) => string): string => t(`settings.models.${tag}`);
export const tagColor = (tag: ModelTag): string => TAG_COLOR[tag] ?? 'gray';

/** 紧凑展示：上下文长度 + 每百万 token 成本。 */
export const ModelMeta: React.FC<{ model: IModelView }> = ({ model }) => {
  const { t } = useTranslation();
  const parts: React.ReactNode[] = [];

  if (model.context_limit) {
    parts.push(
      <span key='ctx' className='text-12px text-t-secondary tabular-nums'>
        {Math.round(model.context_limit / 1000)}K
      </span>
    );
  }
  if (model.price?.input !== undefined || model.price?.output !== undefined) {
    parts.push(
      <span key='price' className='text-12px text-t-secondary tabular-nums'>
        {`${model.price.input ?? '?'} / ${model.price.output ?? '?'} ${t('settings.models.pricePerMillion')}`}
      </span>
    );
  }
  if (!parts.length) return null;
  return <>{parts.map((p, i) => (i === 0 ? p : <span key={`sep${i}`} className='text-t-tertiary px-4px'>·</span>))}</>;
};

export type ModelEditorTarget = { provider: IProvider; model?: IModelView };

interface ModelFormState {
  id: string;
  displayName: string;
  tags: ModelTag[];
  contextLimit: number | undefined;
}

const EMPTY_FORM: ModelFormState = { id: '', displayName: '', tags: [], contextLimit: undefined };

/**
 * 添加 / 编辑一个模型。
 *
 * 主路径是**手动填写**（服务商接口的模型标识没有可靠的自动发现方式）。
 * 「从端点获取」只是辅助：不确定 id 时拉一次列表来挑选，避免去翻文档。
 */
const ModelEditorModal: React.FC<{
  target: ModelEditorTarget | null;
  onClose: () => void;
  onSaved: () => void;
}> = ({ target, onClose, onSaved }) => {
  const { t } = useTranslation();
  const [message, messageContext] = Message.useMessage();
  const [form, setForm] = useState<ModelFormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [fetching, setFetching] = useState(false);
  const [discovered, setDiscovered] = useState<IDiscoveredModel[]>([]);
  const [fetchError, setFetchError] = useState<string>('');

  const isEdit = Boolean(target?.model);
  const provider = target?.provider;

  useEffect(() => {
    if (!target) return;
    setDiscovered([]);
    setFetchError('');
    setForm(
      target.model
        ? {
            id: target.model.id,
            displayName: target.model.display_name,
            tags: target.model.tags,
            contextLimit: target.model.context_limit ?? undefined,
          }
        : EMPTY_FORM
    );
  }, [target]);

  const existingIds = useMemo(() => new Set((provider?.models ?? []).map((m) => m.id)), [provider]);

  const onFetch = useCallback(async () => {
    if (!provider) return;
    setFetching(true);
    setFetchError('');
    try {
      const res = await ipcBridge.mode.fetchModelList.invoke({
        provider_id: provider.id,
        api_key: '',
      });
      if (res?.success && res.models?.length) {
        setDiscovered(res.models);
      } else {
        setFetchError(res?.message || t('settings.models.fetchNoResult'));
      }
    } catch (err) {
      setFetchError(err instanceof Error ? err.message : String(err));
    } finally {
      setFetching(false);
    }
  }, [provider, t]);

  const onSubmit = useCallback(async () => {
    if (!provider) return;
    const modelId = form.id.trim();
    if (!modelId) {
      message.warning(t('settings.models.modelId'));
      return;
    }
    setSaving(true);
    try {
      await ipcBridge.mode.upsertModel.invoke({
        provider_id: provider.id,
        id: modelId,
        display_name: form.displayName.trim(),
        tags: form.tags,
        context_limit: form.contextLimit ?? null,
        enabled: target?.model?.enabled ?? true,
        protocol: target?.model?.protocol ?? null,
      });
      message.success(t('settings.models.saved'));
      onSaved();
      onClose();
    } catch (err) {
      message.error(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }, [provider, form, target, message, t, onSaved, onClose]);

  const tagOptions = useMemo(
    () =>
      TAGS.map((tag) => ({
        value: tag,
        label: (
          <Tag size='small' color={TAG_COLOR[tag]} className='mr-0'>
            {tagLabel(tag, t)}
          </Tag>
        ),
      })),
    [t]
  );

  return (
    <>
      {messageContext}
      <AionModal
        variant='standard'
        visible={Boolean(target)}
        onCancel={onClose}
        onOk={onSubmit}
        okText={t('common.save')}
        cancelText={t('common.cancel')}
        confirmLoading={saving}
        header={{ title: isEdit ? t('settings.models.editModel') : t('settings.models.addModel'), showClose: true }}
        size='medium'
        unmountOnExit
      >
        <div className='flex flex-col gap-14px py-4px'>
          <div className='flex flex-col gap-6px'>
            <span className='text-13px text-t-primary'>
              {t('settings.models.modelId')}
              <span className='text-t-error'> *</span>
            </span>
            <Input
              value={form.id}
              disabled={isEdit}
              placeholder={t('settings.models.modelIdPlaceholder')}
              onChange={(value) => setForm((f) => ({ ...f, id: value }))}
            />
            <span className='text-12px text-t-tertiary'>{t('settings.models.modelIdHint')}</span>
          </div>

          <div className='flex flex-col gap-6px'>
            <span className='text-13px text-t-primary'>{t('settings.models.displayName')}</span>
            <Input
              value={form.displayName}
              placeholder={t('settings.models.displayNamePlaceholder')}
              onChange={(value) => setForm((f) => ({ ...f, displayName: value }))}
            />
          </div>

          <div className='flex flex-col gap-6px'>
            <span className='text-13px text-t-primary'>{t('settings.models.tags')}</span>
            <Select
              mode='multiple'
              value={form.tags}
              options={tagOptions}
              placeholder={t('settings.models.tagsHint')}
              onChange={(value) => setForm((f) => ({ ...f, tags: value as ModelTag[] }))}
            />
          </div>

          <div className='flex flex-col gap-6px'>
            <span className='text-13px text-t-primary'>{t('settings.models.contextLimit')}</span>
            <InputNumber
              value={form.contextLimit}
              placeholder={t('settings.models.contextLimitPlaceholder')}
              min={0}
              className='w-full'
              onChange={(value) => setForm((f) => ({ ...f, contextLimit: value ?? undefined }))}
            />
          </div>

          {/* 辅助：不确定模型 id 时从服务端点拉一次，避免去翻服务商文档。 */}
          <div className='rounded-8px bg-[var(--fill-1)] p-10px'>
            <div className='flex items-center justify-between gap-8px'>
              <div className='min-w-0 flex-1'>
                <div className='text-13px text-t-primary'>{t('settings.models.fetchFromEndpoint')}</div>
                <div className='text-12px text-t-tertiary'>{t('settings.models.fetchHint')}</div>
              </div>
              <Button
                size='small'
                loading={fetching}
                icon={fetching ? <Loading size='14' /> : <Refresh size='14' />}
                onClick={onFetch}
              >
                {fetching ? t('settings.models.fetching') : t('settings.models.fetchFromEndpoint')}
              </Button>
            </div>

            {fetchError && <div className='mt-8px text-12px text-t-error'>{fetchError}</div>}

            {discovered.length > 0 && (
              <div className='mt-10px flex flex-wrap gap-6px max-h-160px overflow-y-auto'>
                {discovered.map((item) => {
                  const added = existingIds.has(item.id);
                  return (
                    <Tooltip
                      key={item.id}
                      content={
                        added
                          ? t('settings.models.alreadyAdded')
                          : item.price
                            ? `${t('settings.models.input')} ${item.price.input ?? '?'} / ${t('settings.models.output')} ${item.price.output ?? '?'}`
                            : item.owned_by || item.id
                      }
                    >
                      <Tag
                        size='small'
                        color={added ? 'gray' : 'arcoblue'}
                        className='cursor-pointer'
                        onClick={() => !added && setForm((f) => ({ ...f, id: item.id }))}
                      >
                        {added && <Check size='12' className='mr-2px' />}
                        {item.id}
                      </Tag>
                    </Tooltip>
                  );
                })}
              </div>
            )}

            {discovered.length === 0 && !fetching && !fetchError && (
              <Empty className='py-12px' />
            )}
          </div>
        </div>
      </AionModal>
    </>
  );
};

export default ModelEditorModal;