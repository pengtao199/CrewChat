/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import type { TChatConversation } from '@/common/config/storage';
import { ipcBridge } from '@/common';
import { agentRuntimeKey, type Agent } from '@/common/types/agent/agentTypes';
import { resolveLocaleKey } from '@/common/utils';
import type { AgentLogoMap } from '@/renderer/utils/model/agentLogo';
import { resolveAgentLogo, useAgentLogos } from '@/renderer/utils/model/agentLogo';
import { isLikelyLocalFilePath, resolveAgentAvatar } from '@/renderer/utils/model/agentAvatar';
import useSWR from 'swr';
export interface PresetAgentInfo {
  name: string;
  logo: string;
  isEmoji: boolean;
  isFallback?: boolean;
  backend?: string;
  agentId?: string;
}

/**
 * 从 conversation extra 中解析预设助手 ID
 * Resolve preset agent ID from conversation extra
 *
 * 处理向后兼容：
 * - preset_agent_id: 新格式 'builtin-xxx'
 * - custom_agent_id: ACP 会话的旧格式
 * - enabled_skills: Gemini Cowork 会话的旧格式
 */
/**
 * Resolve the explicit agent identity stored on a conversation.
 * Legacy `custom_agent_id` is excluded here because older ACP rows used it as
 * a runtime row id, not an agent id.
 */
export function resolveAgentConfigId(conversation: TChatConversation): string | null {
  const extra = conversation.extra as {
    agent_id?: unknown;
    preset_agent_id?: unknown;
  };
  const agent_id = typeof extra?.agent_id === 'string' ? extra.agent_id.trim() : '';
  const preset_agent_id = typeof extra?.preset_agent_id === 'string' ? extra.preset_agent_id.trim() : '';
  return agent_id || preset_agent_id || null;
}

function collectExplicitAgentIdentityCandidates(conversation: TChatConversation): string[] {
  const extra = conversation.extra as {
    agent_id?: unknown;
    preset_agent_id?: unknown;
  };
  const agent_id = typeof extra?.agent_id === 'string' ? extra.agent_id.trim() : '';
  const preset_agent_id = typeof extra?.preset_agent_id === 'string' ? extra.preset_agent_id.trim() : '';
  return [agent_id, preset_agent_id].filter(
    (value, index, values) => Boolean(value) && values.indexOf(value) === index
  );
}

function collectLegacyAgentIdentityCandidates(conversation: TChatConversation): string[] {
  const extra = conversation.extra as {
    custom_agent_id?: unknown;
    enabled_skills?: unknown;
  };
  const custom_agent_id = typeof extra?.custom_agent_id === 'string' ? extra.custom_agent_id.trim() : '';
  return [custom_agent_id].filter(Boolean);
}

function normalizeAgentIdentityCandidate(value: string): string {
  return value.replace('builtin-', '');
}

function findAgentByIdentityCandidates(
  agents: Agent[] | null | undefined,
  candidates: string[]
): Agent | undefined {
  if (!agents?.length || !candidates.length) return undefined;

  for (const rawCandidate of candidates) {
    const candidate = normalizeAgentIdentityCandidate(rawCandidate);
    const match = agents.find((agent) => {
      const ids = new Set([agent.id, `builtin-${agent.id}`, `ext-${agent.id}`]);
      return ids.has(rawCandidate) || ids.has(candidate);
    });
    if (match) return match;
  }

  return undefined;
}

function hasExplicitAgentIdentity(conversation: TChatConversation): boolean {
  const extra = conversation.extra as {
    agent_id?: unknown;
    preset_agent_id?: unknown;
  };
  const agent_id = typeof extra?.agent_id === 'string' ? extra.agent_id.trim() : '';
  const preset_agent_id = typeof extra?.preset_agent_id === 'string' ? extra.preset_agent_id.trim() : '';
  return Boolean(agent_id || preset_agent_id);
}

function resolveLegacyRuntimeRowId(conversation: TChatConversation): string | null {
  const extra = conversation.extra as {
    agent_id?: unknown;
    custom_agent_id?: unknown;
  };
  const agent_id = typeof extra?.agent_id === 'string' ? extra.agent_id.trim() : '';
  const custom_agent_id = typeof extra?.custom_agent_id === 'string' ? extra.custom_agent_id.trim() : '';
  return agent_id || custom_agent_id || null;
}

function resolveLegacyRuntimeDisplayName(conversation: TChatConversation): string | null {
  const extra = conversation.extra as {
    agent_name?: unknown;
    backend?: unknown;
  };
  const agent_name = typeof extra?.agent_name === 'string' ? extra.agent_name.trim() : '';
  if (agent_name) return agent_name;

  const backend = typeof extra?.backend === 'string' ? extra.backend.trim() : '';
  if (!backend) return null;

  return backend
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map((segment) => segment.charAt(0).toUpperCase() + segment.slice(1))
    .join(' ');
}

/**
 * 规范化头像：支持 emoji / 内置 svg / 扩展资源 URL
 * Normalize avatar to either emoji text or a renderable image URL
 */
function normalizeAvatar(avatar: string | undefined): { logo: string; isEmoji: boolean; isFallback?: boolean } {
  const resolved = resolveAgentAvatar(avatar);
  if (resolved.kind === 'image') {
    return { logo: resolved.value, isEmoji: false };
  }

  if (resolved.kind === 'fallback') {
    return { logo: '', isEmoji: false, isFallback: true };
  }

  return { logo: resolved.value, isEmoji: true };
}

function normalizeAgentLabel(value: string | undefined): string {
  return (value || '')
    .normalize('NFKC')
    .replace(/[*_`>#]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
    .toLowerCase();
}

function extractLegacyPresetPayload(conversation: TChatConversation): {
  rules: string;
  enabled_skills: string[];
  hasPayload: boolean;
} {
  const extra = conversation.extra as {
    preset_context?: unknown;
    preset_rules?: unknown;
    enabled_skills?: unknown;
  };
  const preset_context = typeof extra?.preset_context === 'string' ? extra.preset_context.trim() : '';
  const preset_rules = typeof extra?.preset_rules === 'string' ? extra.preset_rules.trim() : '';
  const enabled_skills = Array.isArray(extra?.enabled_skills)
    ? extra.enabled_skills.filter((skill): skill is string => typeof skill === 'string' && skill.trim().length > 0)
    : [];

  return {
    rules: preset_context || preset_rules,
    enabled_skills,
    hasPayload: Boolean(preset_context || preset_rules || enabled_skills.length > 0),
  };
}

function extractAgentNameFromRules(rules: string): string | null {
  const trimmed = rules.trim();
  if (!trimmed) return null;

  const headingMatch = trimmed.match(/^\s*#\s+(.+?)\s*$/m);
  if (headingMatch?.[1]) return headingMatch[1].trim();

  const zhAgentMatch = trimmed.match(/你是\s+\*\*([^*]+)\*\*/);
  if (zhAgentMatch?.[1]) return zhAgentMatch[1].trim();

  const enAgentMatch = trimmed.match(/you are\s+\*\*([^*]+)\*\*/i);
  if (enAgentMatch?.[1]) return enAgentMatch[1].trim();

  return null;
}

function matchesAgentName(candidate: string | null, names: Array<string | undefined>): boolean {
  if (!candidate) return false;
  const normalizedCandidate = normalizeAgentLabel(candidate);
  if (!normalizedCandidate) return false;
  return names.some((name) => normalizeAgentLabel(name) === normalizedCandidate);
}

function hasMatchingEnabledSkills(candidateSkills: string[] | undefined, enabled_skills: string[]): boolean {
  if (!candidateSkills?.length || !enabled_skills.length) return false;
  const normalizedCandidate = [...candidateSkills].map((skill) => skill.trim()).toSorted();
  const normalizedEnabled = [...enabled_skills].map((skill) => skill.trim()).toSorted();
  if (normalizedCandidate.length !== normalizedEnabled.length) return false;
  return normalizedCandidate.every((skill, index) => skill === normalizedEnabled[index]);
}

/**
 * Build agent info from a backend-provided Agent record.
 */
function buildPresetInfoFromAgent(agent: Agent, locale: string): PresetAgentInfo {
  const localeKey = resolveLocaleKey(locale);
  const name = agent.name_i18n?.[localeKey] || agent.name_i18n?.[locale] || agent.name || agent.id;
  const avatar = typeof agent.avatar === 'string' ? agent.avatar : '';
  const normalized = normalizeAvatar(avatar);
  return {
    name,
    logo: normalized.logo,
    isEmoji: normalized.isEmoji,
    isFallback: normalized.isFallback,
    backend: agentRuntimeKey(agent) || undefined,
    agentId: agent.id,
  };
}

function buildPresetInfoFromConversationAgent(
  agent: NonNullable<TChatConversation['assistant']>
): PresetAgentInfo {
  const normalized = normalizeAvatar(agent.avatar);
  return {
    name: agent.name,
    logo: normalized.logo,
    isEmoji: normalized.isEmoji,
    isFallback: normalized.isFallback,
    backend: agent.backend,
    agentId: agent.id,
  };
}

function inferLegacyAgentInfo(
  conversation: TChatConversation,
  locale: string,
  agents?: Agent[] | null
): PresetAgentInfo | null {
  const { rules, enabled_skills } = extractLegacyPresetPayload(conversation);
  const extractedName = extractAgentNameFromRules(rules);

  const byName = agents?.find((agent) =>
    matchesAgentName(extractedName, [
      agent.id,
      agent.name,
      agent.name_i18n?.['zh-CN'],
      agent.name_i18n?.['en-US'],
    ])
  );
  if (byName) return buildPresetInfoFromAgent(byName, locale);

  const bySkills = agents?.filter((agent) =>
    hasMatchingEnabledSkills(agent.enabled_skills, enabled_skills)
  );
  if (bySkills?.length === 1) return buildPresetInfoFromAgent(bySkills[0], locale);

  return null;
}

/**
 * 获取预设助手信息的 Hook
 * Hook to get preset agent info from conversation
 *
 * @param conversation - 会话对象 / Conversation object
 * @returns 预设助手信息或 null / Preset agent info or null
 */
export function usePresetAgentInfo(conversation: TChatConversation | undefined): {
  info: PresetAgentInfo | null;
  isLoading: boolean;
} {
  const { i18n } = useTranslation();
  const logos = useAgentLogos();

  // Merged agent catalog (builtin + user) from backend
  const { data: agentsList, isLoading: isLoadingAgents } = useSWR('agents.list', () =>
    ipcBridge.agents.list.invoke().catch(() => [] as Agent[])
  );

  // Extension-contributed ACP adapters (for ext:{extensionName}:{adapterId} conversations)
  const { data: extensionAcpAdapters, isLoading: isLoadingExtAdapters } = useSWR('extensions.acpAdapters', () =>
    ipcBridge.extensions.getAcpAdapters.invoke().catch(() => [] as Record<string, unknown>[])
  );

  // Remote agent for remote conversations
  const remoteAgentId =
    conversation?.type === 'remote' ? (conversation.extra as { remoteAgentId?: string })?.remoteAgentId : undefined;
  const { data: remoteAgent, isLoading: isLoadingRemoteAgent } = useSWR(
    remoteAgentId ? `remote-agent.get.${remoteAgentId}` : null,
    () => (remoteAgentId ? ipcBridge.remoteAgent.get.invoke({ id: remoteAgentId }) : null)
  );

  return useMemo(() => {
    if (!conversation) return { info: null, isLoading: false };

    const locale = i18n.language || 'en-US';

    if (conversation.agent) {
      const snapshotAvatar =
        typeof conversation.agent.avatar === 'string' ? conversation.agent.avatar.trim() : '';
      if (snapshotAvatar && isLikelyLocalFilePath(snapshotAvatar)) {
        const snapshotCandidates = [
          conversation.agent.id,
          ...collectExplicitAgentIdentityCandidates(conversation),
          ...collectLegacyAgentIdentityCandidates(conversation),
        ].filter((value, index, values) => Boolean(value) && values.indexOf(value) === index);
        const catalogAgent = findAgentByIdentityCandidates(agentsList, snapshotCandidates);
        if (catalogAgent) {
          return { info: buildPresetInfoFromAgent(catalogAgent, locale), isLoading: false };
        }
        if (isLoadingAgents) return { info: null, isLoading: true };
      }

      return {
        info: buildPresetInfoFromConversationAgent(conversation.agent),
        isLoading: false,
      };
    }

    // Remote agent conversations short-circuit to the remote record
    if (conversation.type === 'remote' && remoteAgentId) {
      if (isLoadingRemoteAgent) return { info: null, isLoading: true };
      if (remoteAgent) {
        const normalized = normalizeAvatar(remoteAgent.avatar);
        return {
          info: {
            name: remoteAgent.name,
            logo: normalized.logo,
            isEmoji: normalized.isEmoji,
            isFallback: normalized.isFallback,
          },
          isLoading: false,
        };
      }
      return { info: null, isLoading: false };
    }

    const explicitAgentCandidates = collectExplicitAgentIdentityCandidates(conversation);
    const legacyAgentCandidates = collectLegacyAgentIdentityCandidates(conversation);
    const hasExplicitAgentId = hasExplicitAgentIdentity(conversation);
    const agentMatch = hasExplicitAgentId
      ? findAgentByIdentityCandidates(agentsList, explicitAgentCandidates)
      : findAgentByIdentityCandidates(agentsList, legacyAgentCandidates);
    const runtimeRowAgentId = resolveLegacyRuntimeRowId(conversation);
    const adapterIdentity = (hasExplicitAgentId ? explicitAgentCandidates : legacyAgentCandidates).find(
      (candidate) => candidate.startsWith('ext:')
    );

    const resolveLegacyRuntimeInfo = (): { info: PresetAgentInfo; isLoading: false } | null => {
      if (!runtimeRowAgentId) return null;
      const name = resolveLegacyRuntimeDisplayName(conversation);
      if (!name) return null;
      const legacyBackend =
        typeof (conversation.extra as { backend?: unknown })?.backend === 'string'
          ? ((conversation.extra as { backend?: string }).backend ?? '').trim()
          : '';
      const backendLogo = resolveAgentLogo(logos, { backend: legacyBackend });
      if (backendLogo) {
        return {
          info: { name, logo: backendLogo, isEmoji: false, backend: legacyBackend },
          isLoading: false,
        };
      }
      return { info: { name, logo: '', isEmoji: false, isFallback: true }, isLoading: false };
    };

    if (agentMatch) {
      return { info: buildPresetInfoFromAgent(agentMatch, locale), isLoading: false };
    }

    const inferredInfo = inferLegacyAgentInfo(conversation, locale, agentsList);
    if (inferredInfo) return { info: inferredInfo, isLoading: false };

    const { hasPayload } = extractLegacyPresetPayload(conversation);
    if (
      (hasPayload || explicitAgentCandidates.length > 0 || legacyAgentCandidates.length > 0) &&
      isLoadingAgents
    ) {
      return { info: null, isLoading: true };
    }

    if (adapterIdentity && isLoadingExtAdapters) return { info: null as PresetAgentInfo | null, isLoading: true };

    // Extension ACP adapters (custom_agent_id like ext:{extensionName}:{adapterId})
    if (adapterIdentity && extensionAcpAdapters && Array.isArray(extensionAcpAdapters)) {
      const parts = adapterIdentity.split(':');
      if (parts.length >= 3) {
        const extensionName = parts[1];
        const adapterId = parts.slice(2).join(':');
        const adapter = extensionAcpAdapters.find((a) => {
          const extName = typeof a._extensionName === 'string' ? a._extensionName : '';
          const id = typeof a.id === 'string' ? a.id : '';
          return extName === extensionName && id === adapterId;
        });
        if (adapter) {
          const name = typeof adapter.name === 'string' ? adapter.name : adapterId;
          const avatar = typeof adapter.avatar === 'string' ? adapter.avatar : '';
          const normalized = normalizeAvatar(avatar);
          return {
            info: {
              name,
              logo: normalized.logo,
              isEmoji: normalized.isEmoji,
              isFallback: normalized.isFallback,
            },
            isLoading: false,
          };
        }
      }
    }

    // Custom ACP row short-circuit: only when there is no explicit agent
    // identity. Legacy `custom_agent_id` sometimes carries a runtime row id,
    // not an agent id, so let agent-based restore win first.
    if (!hasExplicitAgentId) {
      const runtimeInfo = resolveLegacyRuntimeInfo();
      if (runtimeInfo) return runtimeInfo;
    }

    const runtimeInfo = resolveLegacyRuntimeInfo();
    if (runtimeInfo) return runtimeInfo;

    return { info: null, isLoading: false };
  }, [
    conversation,
    i18n.language,
    logos,
    agentsList,
    isLoadingAgents,
    extensionAcpAdapters,
    isLoadingExtAdapters,
    remoteAgentId,
    remoteAgent,
    isLoadingRemoteAgent,
  ]);
}
