/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * Agent composer — the page that states our model: an agent is a composition of
 * brain (model endpoint + model), body (CLI), skills, MCP servers, memory and
 * rules. The CLI inventory lives on its own page because "which binaries are
 * installed" is a machine fact, not part of composing a worker.
 */

import { Button, Input, Message, Modal, Select, Switch, Tag, Tooltip } from '@arco-design/web-react';
import { Delete, Plus, Refresh } from '@icon-park/react';
import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import AionScrollArea from '@/renderer/components/base/AionScrollArea';

type AgentSpec = {
  id: string;
  name: string;
  emoji: string;
  brain: string;
  body: string;
  role: string;
  goal: string;
  backstory: string;
  skills: string[];
  mcps: string[];
  memory: boolean;
  max_iter: number;
  reasoning: boolean;
};

type Options = {
  providers: Array<{ id: string; name: string; model: string; models: string[]; has_key: boolean }>;
  bodies: Array<{ id: string; label: string; available: boolean; has_builtin_brain: boolean }>;
  skills: string[];
  mcps: string[];
  plugins: string[];
};

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

async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    credentials: 'include',
    headers: { 'Content-Type': 'application/json' },
    ...init,
  });
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return (await response.json()) as T;
}

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

const AgentComposerContent: React.FC = () => {
  const { t } = useTranslation();
  const [agents, setAgents] = useState<AgentSpec[]>([]);
  const [options, setOptions] = useState<Options | null>(null);
  const [draft, setDraft] = useState<AgentSpec>(EMPTY);
  const [selected, setSelected] = useState<string>('');
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [list, opts] = await Promise.all([
        api<{ agents: AgentSpec[] }>('/api/crewchat/agents'),
        api<Options>('/api/crewchat/agents/options'),
      ]);
      setAgents(list.agents);
      setOptions(opts);
      if (list.agents.length && !selected) {
        setSelected(list.agents[0].id);
        setDraft(normalize(list.agents[0], opts));
      }
    } catch (error) {
      Message.error(`加载失败：${(error as Error).message}`);
    } finally {
      setLoading(false);
    }
  }, [selected]);

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const providerModels = useMemo(() => {
    const providerId = draft.brain.split('/')[0];
    const provider = options?.providers.find((p) => p.id === providerId);
    return provider?.models?.length ? provider.models : provider?.model ? [provider.model] : [];
  }, [draft.brain, options]);

  const update = (patch: Partial<AgentSpec>) => {
    setDraft((prev) => ({ ...prev, ...patch }));
    setDirty(true);
  };

  /** brain 只有 provider 时补上池子里的默认模型，否则模型下拉是空的。 */
  const normalize = (spec: AgentSpec, options: Options | null): AgentSpec => {
    const [providerId, model] = spec.brain.split('/');
    if (!providerId || model) return spec;
    const provider = options?.providers.find((p) => p.id === providerId);
    return provider ? { ...spec, brain: `${providerId}/${provider.model}` } : spec;
  };

  const pickAgent = (spec: AgentSpec) => {
    if (dirty && !window.confirm('有未保存的修改，确定切换？')) return;
    const normalized = normalize(spec, options);
    setSelected(spec.id);
    setDraft(normalized);
    setDirty(false);
  };

  const newAgent = () => {
    const id = window.prompt('新 Agent 的 id（英文，用作文件名）', 'my-agent');
    if (!id) return;
    setSelected(id);
    setDraft({ ...EMPTY, id, name: id });
    setDirty(true);
  };

  const save = async () => {
    if (!draft.id) {
      Message.error('缺少 id');
      return;
    }
    if (!draft.body) {
      Message.error('请选择身体');
      return;
    }
    if (draft.body === 'llm' && !draft.brain) {
      Message.error('纯模型身体必须选择大脑');
      return;
    }
    setSaving(true);
    try {
      const saved = await api<AgentSpec>(`/api/crewchat/agents/${encodeURIComponent(draft.id)}`, {
        method: 'PUT',
        body: JSON.stringify(draft),
      });
      setDraft(saved);
      setDirty(false);
      setAgents((prev) => {
        const next = prev.filter((a) => a.id !== saved.id);
        return [...next, saved].sort((a, b) => a.id.localeCompare(b.id));
      });
      Message.success('已保存到 agents/' + saved.id + '.jsonc');
    } catch (error) {
      Message.error(`保存失败：${(error as Error).message}`);
    } finally {
      setSaving(false);
    }
  };

  const remove = () => {
    if (!selected) return;
    Modal.confirm({
      title: `删除 ${draft.name || selected}？`,
      content: `会删除 agents/${selected}.jsonc，不可撤销。`,
      okButtonProps: { status: 'danger' },
      onOk: async () => {
        await api(`/api/crewchat/agents/${encodeURIComponent(selected)}`, { method: 'DELETE' });
        const rest = agents.filter((a) => a.id !== selected);
        setAgents(rest);
        setSelected(rest[0]?.id ?? '');
        setDraft(rest[0] ?? EMPTY);
        setDirty(false);
        Message.success('已删除');
      },
    });
  };

  const bodyInfo = options?.bodies.find((b) => b.id === draft.body);
  const composedPrompt = [draft.role, draft.goal, draft.backstory].filter(Boolean).join('\n');

  return (
    <AionScrollArea className='flex-1 min-h-0 pb-16px scrollbar-hide'>
      <div className='flex items-start gap-16px'>
        {/* 左：Agent 列表 */}
        <div className='w-260px shrink-0 flex flex-col gap-8px'>
          <div className='flex items-center justify-between'>
            <span className='text-14px font-semibold'>Agent</span>
            <Button size='mini' type='text' icon={<Plus />} onClick={newAgent}>
              新建
            </Button>
          </div>
          <div className='text-11px text-t-tertiary leading-relaxed'>
            一个 Agent = 大脑 + 身体 + 技能 + MCP + 记忆 + 规则。可跨项目复用。
          </div>
          {loading ? (
            <div className='text-12px text-t-tertiary py-12px'>加载中…</div>
          ) : (
            agents.map((agent) => (
              <button
                key={agent.id}
                type='button'
                onClick={() => pickAgent(agent)}
                className={`text-left px-10px py-8px rd-8px border transition-colors ${
                  selected === agent.id
                    ? 'border-primary-6 bg-primary-1'
                    : 'border-transparent hover:bg-fill-1'
                }`}
              >
                <div className='flex items-center gap-6px'>
                  <span>{agent.emoji}</span>
                  <span className='text-13px font-medium truncate'>{agent.name || agent.id}</span>
                </div>
                <div className='text-11px text-t-tertiary mt-2px truncate'>
                  {agent.body} · {agent.brain || '未配大脑'}
                </div>
              </button>
            ))
          )}
          {!loading && !agents.length ? (
            <div className='text-12px text-t-tertiary py-8px'>还没有 Agent，点「新建」开始。</div>
          ) : null}
        </div>

        {/* 右：编排表单 */}
        <div className='flex-1 min-w-0 flex flex-col gap-16px'>
          <div className='flex items-center justify-between'>
            <div className='flex items-center gap-8px'>
              <span className='text-14px font-semibold'>
                {draft.id ? `编辑：${draft.name || draft.id}` : '新建 Agent'}
              </span>
              {dirty ? <Tag color='orange'>未保存</Tag> : null}
            </div>
            <div className='flex items-center gap-8px'>
              <Button size='small' icon={<Refresh />} onClick={() => void load()}>
                刷新
              </Button>
              <Button size='small' status='danger' icon={<Delete />} disabled={!selected} onClick={remove}>
                删除
              </Button>
              <Button size='small' type='primary' loading={saving} disabled={!dirty} onClick={() => void save()}>
                保存
              </Button>
            </div>
          </div>

          <Field label='标识'>
            <div className='flex gap-8px'>
              <Input
                style={{ width: 96 }}
                value={draft.emoji}
                onChange={(value) => update({ emoji: value })}
                placeholder='🧭'
              />
              <Input
                value={draft.name}
                onChange={(value) => update({ name: value })}
                placeholder='名称，例如：架构顾问'
              />
              <Input
                style={{ width: 180 }}
                value={draft.id}
                disabled={Boolean(selected)}
                onChange={(value) => update({ id: value })}
                placeholder='id（文件名）'
              />
            </div>
          </Field>

          <div className='h-1px bg-[var(--color-border-1)]' />

          <Field label='大脑' hint='模型端点 + 具体模型；经网关驱动任意身体'>
            <div className='flex gap-8px'>
              <Select
                style={{ width: 220 }}
                value={draft.brain.split('/')[0] || undefined}
                placeholder='选择模型端点'
                onChange={(providerId) => {
                  const provider = options?.providers.find((p) => p.id === providerId);
                  update({ brain: provider ? `${providerId}/${provider.model}` : providerId });
                }}
              >
                {(options?.providers ?? []).map((p) => (
                  <Select.Option key={p.id} value={p.id}>
                    <div className='flex items-center gap-6px'>
                      <span>{p.name}</span>
                      {!p.has_key ? <Tag size='small' color='red'>缺 key</Tag> : null}
                    </div>
                  </Select.Option>
                ))}
              </Select>
              <Select
                style={{ flex: 1 }}
                value={draft.brain.split('/')[1] || undefined}
                placeholder='选择模型'
                onChange={(model) => update({ brain: `${draft.brain.split('/')[0]}/${model}` })}
                allowCreate
                showSearch
              >
                {providerModels.map((m) => (
                  <Select.Option key={m} value={m}>
                    {m}
                  </Select.Option>
                ))}
              </Select>
            </div>
          </Field>

          <Field label='身体' hint='执行者；CLI 自带工具链与沙箱'>
            <div className='flex items-center gap-8px'>
              <Select
                style={{ width: 220 }}
                value={draft.body}
                onChange={(body) => update({ body })}
              >
                {(options?.bodies ?? []).map((b) => (
                  <Select.Option key={b.id} value={b.id}>
                    <div className='flex items-center gap-6px'>
                      <span>{b.label}</span>
                      {!b.available ? <Tag size='small' color='red'>未安装</Tag> : null}
                    </div>
                  </Select.Option>
                ))}
              </Select>
              {bodyInfo ? (
                bodyInfo.available ? (
                  <span className='text-12px text-success-6'>● 就绪</span>
                ) : (
                  <span className='text-12px text-danger-6'>● 未安装</span>
                )
              ) : null}
              {bodyInfo ? (
                <span className='text-11px text-t-tertiary'>
                  {bodyInfo.has_builtin_brain ? '自带大脑，可被上方大脑覆盖' : '无内置大脑，必须配大脑'}
                </span>
              ) : null}
            </div>
          </Field>

          <Field label='技能' hint={`来自 ~/.agents/skills（${options?.skills.length ?? 0} 个可选）`}>
            <Select
              mode='multiple'
              value={draft.skills}
              onChange={(skills) => update({ skills })}
              placeholder='选择技能'
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
          </Field>

          <Field label='MCP' hint='来自 ~/.agents/mcp.json，按名字引用'>
            <Select
              mode='multiple'
              value={draft.mcps}
              onChange={(mcps) => update({ mcps })}
              placeholder='选择 MCP 服务'
              allowClear
              maxTagCount={6}
            >
              {(options?.mcps ?? []).map((m) => (
                <Select.Option key={m} value={m}>
                  {m}
                </Select.Option>
              ))}
            </Select>
          </Field>

          <Field label='记忆' hint='开启后该 Agent 跨会话记住自己的产出'>
            <div className='flex items-center gap-8px'>
              <Switch checked={draft.memory} onChange={(memory) => update({ memory })} />
              <span className='text-12px text-t-tertiary'>
                {draft.memory ? '已开启' : '关闭（每次会话从零开始）'}
              </span>
            </div>
          </Field>

          <div className='h-1px bg-[var(--color-border-1)]' />

          <Field label='角色' hint='它是谁'>
            <Input.TextArea
              value={draft.role}
              onChange={(value) => update({ role: value })}
              autoSize={{ minRows: 1, maxRows: 3 }}
              placeholder='例如：资深软件工程师'
            />
          </Field>
          <Field label='目标' hint='要达成什么'>
            <Input.TextArea
              value={draft.goal}
              onChange={(value) => update({ goal: value })}
              autoSize={{ minRows: 1, maxRows: 3 }}
              placeholder='例如：交付可以直接运行的代码'
            />
          </Field>
          <Field label='背景' hint='行为偏好'>
            <Input.TextArea
              value={draft.backstory}
              onChange={(value) => update({ backstory: value })}
              autoSize={{ minRows: 2, maxRows: 5 }}
              placeholder='例如：先读代码再动手，改动范围最小化'
            />
          </Field>

          <Field label='规则预览' hint='实际发给身体的提示词前缀'>
            <Tooltip content='角色 + 目标 + 背景，按此顺序拼接'>
              <pre className='m-0 px-12px py-10px rd-8px bg-fill-1 text-12px whitespace-pre-wrap break-words max-h-200px overflow-auto'>
                {composedPrompt || '（还没填规则）'}
              </pre>
            </Tooltip>
          </Field>

          <div className='text-11px text-t-tertiary'>
            保存后写入 <code>agents/{draft.id || '…'}.jsonc</code>
            {t('settings.agentComposerHint', { defaultValue: '' })}
          </div>
        </div>
      </div>
    </AionScrollArea>
  );
};

export default AgentComposerContent;
