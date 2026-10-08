/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * Agent composer — page-private UI. States the product's model: an agent is a
 * composition of brain (model endpoint + model), body (CLI), skills, MCP
 * servers, memory and rules. The CLI inventory lives on its own page because
 * "which binaries are installed" is a machine fact, not part of composing a
 * worker.
 */

import { Button, Input, Message, Modal, Select, Switch } from '@arco-design/web-react';
import { Delete, FolderClose, Plus, Refresh } from '@icon-park/react';
import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useLocation, useNavigate } from 'react-router-dom';

import SettingsCardList from '@/renderer/components/base/SettingsCardList';
import SettingsCardRow from '@/renderer/components/base/SettingsCardRow';
import EmptyState from '@/renderer/components/base/EmptyState';
import DataLocationNote from '@/renderer/components/settings/DataLocationNote';
import StatusBadge from '@/renderer/components/base/StatusBadge';
import SettingsPageHeader from '@/renderer/pages/settings/components/SettingsPageHeader';

import { fetchAgents, fetchOptions, fetchModels, removeAgent, saveAgent } from './api';
import type { AgentSpec, ComposerOptions } from './types';

const EMPTY: AgentSpec = {
  id: '',
  name: '',
  emoji: '🤖',
  brain: '',
  body: 'llm',
  role: '',
  goal: '',
  backstory: '',
  skills: [],
  mcps: [],
  memory: false,
  max_iter: 24,
  reasoning: false,
};

const Field: React.FC<{ label: string; hint?: string; children: React.ReactNode }> = ({
  label,
  hint,
  children,
}) => (
  <div className='flex flex-col gap-6px'>
    <div className='flex items-baseline gap-8px'>
      <span className='text-13px font-medium text-t-primary'>{label}</span>
      {hint ? <span className='text-11px text-t-tertiary'>{hint}</span> : null}
    </div>
    {children}
  </div>
);

const Section: React.FC<{ title: string; children: React.ReactNode }> = ({ title, children }) => (
  <div className='flex flex-col gap-12px rounded-12px border border-border-2 bg-2 p-14px'>
    <span className='text-13px font-medium text-t-primary'>{title}</span>
    {children}
  </div>
);

type ComposerNavState = {
  /** 从 Agents 名册页跳来时选中的 Agent */
  selectAgentId?: string;
  /** 从名册页点「新建」跳来 */
  createAgent?: boolean;
};

const AgentComposerContent: React.FC = () => {
  const { t } = useTranslation();
  const location = useLocation();
  const navigate = useNavigate();
  const [agents, setAgents] = useState<AgentSpec[]>([]);
  const [options, setOptions] = useState<ComposerOptions | null>(null);
  const [draft, setDraft] = useState<AgentSpec>(EMPTY);
  const [selected, setSelected] = useState('');
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [loading, setLoading] = useState(true);
  const [models, setModels] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);

  /** brain 只写了端点时补上默认模型，否则模型下拉是空的。 */
  const normalize = useCallback((spec: AgentSpec, opts: ComposerOptions | null): AgentSpec => {
    const [providerId, model] = spec.brain.split('/');
    if (!providerId || model) return spec;
    const provider = opts?.providers.find((p) => p.id === providerId);
    return provider ? { ...spec, brain: `${providerId}/${provider.model}` } : spec;
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [list, opts] = await Promise.all([fetchAgents(), fetchOptions()]);
      setAgents(list.agents);
      setOptions(opts);
      const intent = (location.state as ComposerNavState | null) ?? null;
      if (intent?.createAgent) {
        setSelected('');
        setDraft({ ...EMPTY, id: '', name: '' });
        setDirty(false);
        navigate(location.pathname, { replace: true, state: null });
      } else if (intent?.selectAgentId) {
        const target = list.agents.find((a) => a.id === intent.selectAgentId);
        if (target) {
          setSelected(target.id);
          setDraft(normalize(target, opts));
          setDirty(false);
        }
        navigate(location.pathname, { replace: true, state: null });
      } else if (list.agents.length && !selected) {
        setSelected(list.agents[0].id);
        setDraft(normalize(list.agents[0], opts));
      }
    } catch (error) {
      Message.error(t('settings.agentComposer.loadFailed', { message: (error as Error).message }));
    } finally {
      setLoading(false);
    }
  }, [normalize, selected, t, location.state, location.pathname, navigate]);

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const providerId = draft.brain.split('/')[0] ?? '';
  const provider = useMemo(
    () => options?.providers.find((p) => p.id === providerId) ?? null,
    [options, providerId]
  );

  /** 模型列表来自端点本身，不让用户手输。 */
  const loadModels = useCallback(
    async (id: string) => {
      if (!id) {
        setModels([]);
        return;
      }
      setModelsLoading(true);
      try {
        const result = await fetchModels(id);
        setModels(result.models);
        if (!result.success) {
          Message.warning(t('settings.agentComposer.loadModelsFailed', { message: result.message ?? '' }));
        }
      } catch (error) {
        setModels([]);
        Message.warning(t('settings.agentComposer.loadModelsFailed', { message: (error as Error).message }));
      } finally {
        setModelsLoading(false);
      }
    },
    [t]
  );

  useEffect(() => {
    void loadModels(providerId);
  }, [providerId, loadModels]);

  const update = (patch: Partial<AgentSpec>) => {
    setDraft((prev) => ({ ...prev, ...patch }));
    setDirty(true);
  };

  const pickAgent = (spec: AgentSpec) => {
    if (dirty && !window.confirm(t('settings.agentComposer.unsavedSwitch'))) return;
    setSelected(spec.id);
    setDraft(normalize(spec, options));
    setDirty(false);
  };

  const create = () => {
    const id = window.prompt(t('settings.agentComposer.idPrompt'), 'my-agent');
    if (!id) return;
    setSelected(id);
    setDraft({ ...EMPTY, id, name: id });
    setDirty(true);
  };

  const save = async () => {
    if (!draft.id) return void Message.error(t('settings.agentComposer.idRequired'));
    if (!draft.body) return void Message.error(t('settings.agentComposer.bodyRequired'));
    if (draft.body === 'llm' && !draft.brain) {
      return void Message.error(t('settings.agentComposer.brainRequired'));
    }
    setSaving(true);
    try {
      const saved = await saveAgent(draft.id, draft);
      setDraft(saved);
      setDirty(false);
      setAgents((prev) => [...prev.filter((a) => a.id !== saved.id), saved].sort((a, b) => a.id.localeCompare(b.id)));
      Message.success(t('settings.agentComposer.saved', { id: saved.id }));
    } catch (error) {
      Message.error(t('settings.agentComposer.saveFailed', { message: (error as Error).message }));
    } finally {
      setSaving(false);
    }
  };

  const confirmDelete = () => {
    if (!selected) return;
    Modal.confirm({
      title: t('settings.agentComposer.deleteTitle', { name: draft.name || selected }),
      content: t('settings.agentComposer.deleteBody', { id: selected }),
      okButtonProps: { status: 'danger' },
      onOk: async () => {
        await removeAgent(selected);
        const rest = agents.filter((a) => a.id !== selected);
        setAgents(rest);
        setSelected(rest[0]?.id ?? '');
        setDraft(rest[0] ?? EMPTY);
        setDirty(false);
      },
    });
  };

  const bodyInfo = options?.bodies.find((b) => b.id === draft.body);
  const composedPrompt = [draft.role, draft.goal, draft.backstory].filter(Boolean).join('\n');
  const c = 'settings.agentComposer';

  const bodyStatus = !bodyInfo ? null : bodyInfo.available ? (
    <StatusBadge tone='ready' label={t(`${c}.statusReady`)} />
  ) : (
    <StatusBadge tone='blocked' label={t(`${c}.statusMissing`)} />
  );

  return (
    <div className='flex flex-col gap-16px'>
      <SettingsPageHeader
        title={t(`${c}.title`)}
        description={t(`${c}.description`)}
        actions={
          <>
            <Button size='small' icon={<Refresh />} onClick={() => void load()}>
              {t(`${c}.refresh`)}
            </Button>
            <Button size='small' type='outline' icon={<Plus />} onClick={create}>
              {t(`${c}.new`)}
            </Button>
            <Button size='small' status='danger' icon={<Delete />} disabled={!selected} onClick={confirmDelete}>
              {t(`${c}.delete`)}
            </Button>
            <Button size='small' type='primary' loading={saving} disabled={!dirty} onClick={() => void save()}>
              {t(`${c}.save`)}
            </Button>
          </>
        }
      />

      <div className='flex items-start gap-16px'>
        <div className='flex w-260px shrink-0 flex-col gap-8px'>
          <SettingsCardList>
            {loading ? (
              <EmptyState>…</EmptyState>
            ) : agents.length ? (
              agents.map((agent) => (
                <SettingsCardRow
                  key={agent.id}
                  testId={`agent-composer-row-${agent.id}`}
                  icon={agent.emoji}
                  title={agent.name || agent.id}
                  subtitle={`${agent.body} · ${agent.brain || '—'}`}
                  selected={selected === agent.id}
                  onClick={() => pickAgent(agent)}
                />
              ))
            ) : (
              <EmptyState>{t(`${c}.empty`)}</EmptyState>
            )}
          </SettingsCardList>
        </div>

        <div className='flex min-w-0 flex-1 flex-col gap-16px'>
          <div className='flex items-center justify-between'>
            <span className='text-14px font-semibold text-t-primary'>
              {draft.id ? t(`${c}.edit`, { name: draft.name || draft.id }) : t(`${c}.create`)}
            </span>
            {dirty ? <StatusBadge tone='warning' label={t(`${c}.unsaved`)} /> : null}
          </div>

          <Section title={t(`${c}.identity`)}>
            <div className='flex gap-8px'>
              <Input
                className='w-80px'
                value={draft.emoji}
                onChange={(value) => update({ emoji: value })}
                placeholder='🧭'
              />
              <Input
                value={draft.name}
                onChange={(value) => update({ name: value })}
                placeholder={t(`${c}.namePlaceholder`)}
              />
              <Input
                className='w-180px'
                value={draft.id}
                disabled={Boolean(selected)}
                onChange={(value) => update({ id: value })}
                placeholder={t(`${c}.idLabel`)}
              />
            </div>
          </Section>

          <Section title={t(`${c}.brain`)}>
            <div className='flex flex-col gap-8px'>
              <span className='text-11px text-t-tertiary'>{t(`${c}.brainHint`)}</span>
              <div className='flex gap-8px'>
                <Select
                  className='w-220px'
                  value={providerId || undefined}
                  placeholder={t(`${c}.pickEndpoint`)}
                  onChange={(id) => {
                    const picked = options?.providers.find((p) => p.id === id);
                    update({ brain: picked ? `${id}/${picked.model}` : id });
                  }}
                >
                  {(options?.providers ?? []).map((p) => (
                    // 受约束选择：缺 key 的端点可见但不可选
                    <Select.Option key={p.id} value={p.id} disabled={!p.has_key}>
                      <span className='inline-flex items-center gap-6px'>
                        {p.name}
                        {!p.has_key ? <StatusBadge tone='blocked' label={t(`${c}.noKey`)} /> : null}
                      </span>
                    </Select.Option>
                  ))}
                </Select>
                <Select
                  className='flex-1'
                  value={draft.brain.split('/')[1] || undefined}
                  placeholder={t(`${c}.pickModel`)}
                  loading={modelsLoading}
                  showSearch
                  onDropdownVisibleChange={(open) => {
                    if (open && !models.length) void loadModels(providerId);
                  }}
                  onChange={(model) => update({ brain: `${providerId}/${model}` })}
                >
                  {models.map((m) => (
                    <Select.Option key={m} value={m}>
                      {m}
                    </Select.Option>
                  ))}
                  {!models.length && !modelsLoading ? (
                    <Select.Option value='' disabled>
                      {t(`${c}.noModels`)}
                    </Select.Option>
                  ) : null}
                </Select>
              </div>
              {provider ? <span className='text-11px text-t-tertiary'>{provider.base_url}</span> : null}
            </div>
          </Section>

          <Section title={t(`${c}.body`)}>
            <div className='flex flex-col gap-8px'>
              <span className='text-11px text-t-tertiary'>{t(`${c}.bodyHint`)}</span>
              <div className='flex items-center gap-8px'>
                <Select className='w-220px' value={draft.body} onChange={(body) => update({ body })}>
                  {(options?.bodies ?? []).map((b) => (
                    // 受约束选择：不可用的身体可见但不可选，选中也跑不起来
                    <Select.Option key={b.id} value={b.id} disabled={!b.available}>
                      <span className='inline-flex items-center gap-6px'>
                        {b.label}
                        {!b.available ? <StatusBadge tone='blocked' label={t(`${c}.statusMissing`)} /> : null}
                      </span>
                    </Select.Option>
                  ))}
                </Select>
                {bodyStatus}
                <span className='text-11px text-t-tertiary'>
                  {bodyInfo?.has_builtin_brain ? t(`${c}.builtinBrain`) : t(`${c}.needsBrain`)}
                </span>
              </div>
            </div>
          </Section>

          <Section title={t(`${c}.skills`)}>
            <div className='flex flex-col gap-8px'>
              <span className='text-11px text-t-tertiary'>
                {t(`${c}.skillsHint`, { count: options?.skills.length ?? 0 })}
              </span>
              <Select
                mode='multiple'
                value={draft.skills}
                onChange={(skills) => update({ skills })}
                showSearch
                allowClear
                maxTagCount={6}
              >
                {(options?.skills ?? []).map((s) => (
                  <Select.Option key={s} value={s}>
                    {s}
                  </Select.Option>
                ))}
              </Select>
            </div>
          </Section>

          <Section title={t(`${c}.mcps`)}>
            <div className='flex flex-col gap-8px'>
              <span className='text-11px text-t-tertiary'>{t(`${c}.mcpsHint`)}</span>
              <Select
                mode='multiple'
                value={draft.mcps}
                onChange={(mcps) => update({ mcps })}
                allowClear
                maxTagCount={6}
              >
                {(options?.mcps ?? []).map((m) => (
                  <Select.Option key={m} value={m}>
                    {m}
                  </Select.Option>
                ))}
              </Select>
            </div>
          </Section>

          <Section title={t(`${c}.memory`)}>
            <div className='flex items-center gap-8px'>
              <Switch checked={draft.memory} onChange={(memory) => update({ memory })} />
              <span className='text-12px text-t-tertiary'>
                {draft.memory ? t(`${c}.memoryOn`) : t(`${c}.memoryOff`)}
              </span>
            </div>
            <span className='text-11px text-t-tertiary'>{t(`${c}.memoryHint`)}</span>
          </Section>

          <Section title={t(`${c}.rulesPreview`)}>
            <Field label={t(`${c}.role`)} hint={t(`${c}.roleHint`)}>
              <Input.TextArea
                value={draft.role}
                onChange={(value) => update({ role: value })}
                autoSize={{ minRows: 1, maxRows: 3 }}
              />
            </Field>
            <Field label={t(`${c}.goal`)} hint={t(`${c}.goalHint`)}>
              <Input.TextArea
                value={draft.goal}
                onChange={(value) => update({ goal: value })}
                autoSize={{ minRows: 1, maxRows: 3 }}
              />
            </Field>
            <Field label={t(`${c}.backstory`)} hint={t(`${c}.backstoryHint`)}>
              <Input.TextArea
                value={draft.backstory}
                onChange={(value) => update({ backstory: value })}
                autoSize={{ minRows: 2, maxRows: 5 }}
              />
            </Field>
            <div className='flex flex-col gap-6px'>
              <span className='text-11px text-t-tertiary'>{t(`${c}.rulesPreviewHint`)}</span>
              <Input.TextArea
                value={composedPrompt || t(`${c}.rulesEmpty`)}
                readOnly
                autoSize={{ minRows: 2, maxRows: 6 }}
              />
            </div>
          </Section>

          <DataLocationNote path={`config/agents/${draft.id || '…'}.jsonc`} />
        </div>
      </div>
    </div>
  );
};

export default AgentComposerContent;
