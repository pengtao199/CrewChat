import type { TChatConversation } from '@/common/config/storage';
import type { PresetAgentInfo } from '@/renderer/hooks/agent/usePresetAgentInfo';
import { resolveAgentAvatar } from '@/renderer/utils/model/agentAvatar';
import { resolveAgentLogo } from '@/renderer/utils/model/agentLogo';
import type { AgentLogoMap } from '@/renderer/utils/model/agentLogo';

/**
 * Resolve the effective runtime backend for a conversation.
 *
 * New agent-led flows should pass the agent backend explicitly when it
 * is known. Legacy conversations may still fall back to `extra.backend`.
 */
export function resolveConversationBackend(
  conversation: TChatConversation | undefined,
  presetAgentBackend?: string
): string | undefined {
  const explicitAgentBackend = presetAgentBackend?.trim();
  if (explicitAgentBackend) {
    return explicitAgentBackend;
  }

  if (!conversation) return undefined;

  const conversationAgentBackend = conversation.agent?.backend?.trim();
  if (conversationAgentBackend) {
    return conversationAgentBackend;
  }

  if (conversation.type === 'acp') {
    return conversation.extra?.backend;
  }

  if (conversation.type === 'openclaw-gateway') {
    return conversation.extra?.backend || 'openclaw-gateway';
  }

  if (conversation.type === 'remote') {
    return 'remote';
  }

  return conversation.type;
}

export type ConversationLeadingMark =
  | {
      kind: 'emoji';
      value: string;
      label: string;
    }
  | {
      kind: 'image';
      value: string;
      label: string;
    }
  | {
      kind: 'fallback';
      label: string;
    }
  | {
      kind: 'agent_fallback';
      label: string;
    };

export function resolveConversationLeadingMark(
  conversation: TChatConversation,
  agentInfo: PresetAgentInfo | undefined,
  logos: AgentLogoMap
): ConversationLeadingMark {
  if (agentInfo) {
    if (agentInfo.isFallback) {
      return {
        kind: 'agent_fallback',
        label: agentInfo.name,
      };
    }

    return agentInfo.isEmoji
      ? {
          kind: 'emoji',
          value: agentInfo.logo,
          label: agentInfo.name,
        }
      : {
          kind: 'image',
          value: agentInfo.logo,
          label: agentInfo.name,
        };
  }

  if (conversation.agent) {
    const agentLabel = conversation.agent.name.trim() || conversation.agent.id;
    const agentAvatar = resolveAgentAvatar(conversation.agent.avatar);
    if (agentAvatar.kind === 'emoji') {
      return {
        kind: 'emoji',
        value: agentAvatar.value,
        label: agentLabel,
      };
    }
    if (agentAvatar.kind === 'image') {
      return {
        kind: 'image',
        value: agentAvatar.value,
        label: agentLabel,
      };
    }

    return {
      kind: 'agent_fallback',
      label: agentLabel,
    };
  }

  const backendKey = resolveConversationBackend(conversation)?.trim() || 'agent';
  const logo = resolveAgentLogo(logos, { backend: backendKey });
  if (logo) {
    return {
      kind: 'image',
      value: logo,
      label: backendKey,
    };
  }

  return {
    kind: 'fallback',
    label: backendKey,
  };
}
