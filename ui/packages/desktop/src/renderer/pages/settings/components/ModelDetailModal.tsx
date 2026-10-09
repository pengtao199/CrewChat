/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import type { IModelView, IProvider, ModelTag } from '@/common/types/provider/providerApi';
import { Button, Descriptions, Input, InputNumber, Message, Tag, Tooltip } from '@arco-design/web-react';
import { Loading } from '@icon-park/react';
import React, { useCallback, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import AionModal from '@/renderer/components/base/AionModal';

/** Tag vocabulary mirroring the backend's `MODEL_TAGS`. Order drives display order. */
const TAGS: ModelTag[] = ['text', 'vision', 'reasoning', 'tool_use', 'long_context', 'fast', 'free', 'embedding'];

/** 标签 → Arco Tag 颜色。只用语义色板，不自造色值。 */
const TAG_COLOR: Record<ModelTag, string> = {
  text: 'gray',
  vision: 'purple',
  reasoning: 'orange',
  tool_use: 'blue',
  long_context: 'arcoblue',
  fast: 'cyan',
  free: 'green',
  embedding: 'pink',
};

/**
 * 标签 → i18n key 的显式映射。
 * 不能用 `settings.models.${tag}` 拼：多词标签在 JSON 里是 camelCase（toolUse），
 * 而 tag id 是 snake_case（tool_use），直接拼会查不到而显示成原始 key。
 */
const TAG_I18N_KEY: Record<ModelTag, string> = {
  text: 'text',
  vision: 'vision',
  reasoning: 'reasoning',
  tool_use: 'toolUse',
  long_context: 'longContext',
  fast: 'fast',
  free: 'free',
  embedding: 'embedding',
};

export const tagLabel = (tag: ModelTag, t: (k: string) => string): string => {
  const key = `settings.models.${TAG_I18N_KEY[tag]}`;
  const value = t(key);
  // i18n 缺词条时 react-i18next 会原样返回 key；此时退回标签 id，绝不给用户看原始 key。
  return value === key ? tag : value;
};

export const tagColor = (tag: ModelTag): string => TAG_COLOR[tag] ?? 'gray';

/** 紧凑展示：上下文长度 + 每百万 token 成本。 */
export const ModelMeta: React.FC<{ model: IModelView }> = ({ model }) => {
  const { t } = useTranslation();
  const parts: React.ReactNode[] = [];

  if (model.context_limit) {
    parts.push(
      <span key='ctx' className='text-12px text-t-secondary tabular-nums'>
        {model.context_limit >= 1000 ? `${Math.round(model.context_limit / 1000)}K` : model.context_limit}
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
  return (
    <>
      {parts.map((p, i) => (
        <React.Fragment key={i}>
          {i > 0 && <span className='text-t-tertiary px-4px'>·</span>}
          {p}
        </React.Fragment>
      ))}
    </>
  );
};

/** 标签行（只读）。模型的一切属性都由后端自动推导，这里只展示。 */
export const ModelTags: React.FC<{ tags: ModelTag[] }> = ({ tags }) => {
  const { t } = useTranslation();
  if (!tags.length) return null;
  return (
    <>
      {tags.map((tag) => (
        <Tag key={tag} size='small' color={tagColor(tag)} className='shrink-0'>
          {tagLabel(tag, t)}
        </Tag>
      ))}
    </>
  );
};

/**
 * 模型详情。
 *
 * 模型 id、标签、上下文、价格全部由后端自动推导（cc-switch 能力判定 + models.dev
 * 价格目录 + 命名约定），所以这里**只允许覆盖两样东西**：显示名和上下文长度。
 * 标签不可编辑 —— 让用户手工维护标签只会和自动推导打架。
 */
const ModelDetailModal: React.FC<{
  model: IModelView | null;
  provider?: IProvider;
  onClose: () => void;
  onSaved: () => void;
}> = ({ model, provider, onClose, onSaved }) => {
  const { t } = useTranslation();
  const [message, messageContext] = Message.useMessage();
  const [saving, setSaving] = useState(false);
  const [displayName, setDisplayName] = useState(model?.display_name ?? '');
  const [contextLimit, setContextLimit] = useState<number | undefined>(model?.context_limit ?? undefined);
  const [lastModelId, setLastModelId] = useState<string | undefined>(model?.id);

  // 切换到另一个模型时重置草稿
  if (model?.id !== lastModelId) {
    setLastModelId(model?.id);
    setDisplayName(model?.display_name ?? '');
    setContextLimit(model?.context_limit ?? undefined);
  }

  const onSubmit = useCallback(async () => {
    if (!provider || !model) return;
    setSaving(true);
    try {
      await ipcBridge.mode.upsertModel.invoke({
        provider_id: provider.id,
        id: model.id,
        display_name: displayName.trim(),
        context_limit: contextLimit ?? null,
        enabled: model.enabled,
        protocol: model.protocol ?? null,
      });
      message.success(t('settings.models.saved'));
      onSaved();
      onClose();
    } catch (err) {
      message.error(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }, [provider, model, displayName, contextLimit, message, t, onSaved, onClose]);

  const allTags = useMemo(() => (model ? TAGS.filter((tag) => model.tags.includes(tag)) : []), [model]);

  return (
    <>
      {messageContext}
      <AionModal
        variant='standard'
        visible={Boolean(model)}
        onCancel={onClose}
        onOk={onSubmit}
        okText={t('common.save')}
        cancelText={t('common.cancel')}
        confirmLoading={saving}
        header={{ title: t('settings.models.modelDetail'), showClose: true }}
        size='medium'
        unmountOnExit
      >
        {model && (
          <div className='flex flex-col gap-14px py-4px'>
            {/* 只读信息：模型标识与自动推导结果 */}
            <div className='rounded-8px bg-[var(--fill-1)] p-12px'>
              <div className='text-12px text-t-tertiary'>{t('settings.models.modelId')}</div>
              {/* 不要用 break-all：模型 id 是长无空格字符串，break-all 会在任意字符
                  间断行，容器一变窄就竖着排下来。截断 + Tooltip 显示全文。 */}
              <Tooltip content={model.id} className='block'>
                <div className='mt-2px truncate font-mono text-13px text-t-primary'>{model.id}</div>
              </Tooltip>

              {model.display_name && model.display_name !== model.id && (
                <>
                  <div className='mt-10px text-12px text-t-tertiary'>{t('settings.models.sourceName')}</div>
                  <div className='mt-2px text-13px text-t-primary'>{model.display_name}</div>
                </>
              )}

              <div className='mt-10px text-12px text-t-tertiary'>{t('settings.models.tags')}</div>
              <div className='mt-4px flex flex-wrap items-center gap-4px'>
                {allTags.length ? (
                  <ModelTags tags={allTags} />
                ) : (
                  <span className='text-13px text-t-tertiary'>{t('settings.models.noTags')}</span>
                )}
              </div>
              <div className='mt-4px text-12px text-t-tertiary'>{t('settings.models.tagsReadOnlyHint')}</div>

              {(model.price?.input !== undefined || model.price?.output !== undefined) && (
                <>
                  <div className='mt-10px text-12px text-t-tertiary'>{t('settings.models.pricePerMillion')}</div>
                  <div className='mt-2px text-13px tabular-nums text-t-primary'>
                    {`${t('settings.models.input')} ${model.price.input ?? '—'} · ${t('settings.models.output')} ${model.price.output ?? '—'}`}
                  </div>
                </>
              )}
            </div>

            {/* 可覆盖的两项 */}
            <div className='flex flex-col gap-6px'>
              <span className='text-13px text-t-primary'>{t('settings.models.displayName')}</span>
              <Input
                value={displayName}
                placeholder={t('settings.models.displayNamePlaceholder')}
                onChange={setDisplayName}
              />
            </div>

            <div className='flex flex-col gap-6px'>
              <span className='flex items-center gap-4px text-13px text-t-primary'>
                {t('settings.models.contextLimit')}
                <Tooltip content={t('settings.models.contextLimitHint')}>
                  <span className='text-12px text-t-tertiary'>?</span>
                </Tooltip>
              </span>
              <InputNumber
                value={contextLimit}
                placeholder={t('settings.models.contextLimitPlaceholder')}
                min={0}
                className='w-full'
                onChange={(value) => setContextLimit(value ?? undefined)}
              />
            </div>
          </div>
        )}
      </AionModal>
    </>
  );
};

/** 供应商详情里用到的「重新拉取模型」按钮。 */
export const RefreshModelsButton: React.FC<{ provider: IProvider; onDone: () => void }> = ({
  provider,
  onDone,
}) => {
  const { t } = useTranslation();
  const [message, messageContext] = Message.useMessage();
  const [busy, setBusy] = useState(false);

  const onClick = useCallback(async () => {
    setBusy(true);
    try {
      const res = await ipcBridge.mode.fetchModelList.invoke({ provider_id: provider.id, api_key: '' });
      if (!res.success) {
        message.error(res.message || t('settings.models.probeFailed'));
        return;
      }
      message.success(t('settings.models.probeFound', { count: res.models?.length ?? 0 }));
      onDone();
    } finally {
      setBusy(false);
    }
  }, [provider, message, t, onDone]);

  return (
    <>
      {messageContext}
      <Button size='mini' loading={busy} icon={busy ? <Loading size='14' /> : undefined} onClick={() => void onClick()}>
        {t('settings.models.refreshModels')}
      </Button>
    </>
  );
};

export default ModelDetailModal;