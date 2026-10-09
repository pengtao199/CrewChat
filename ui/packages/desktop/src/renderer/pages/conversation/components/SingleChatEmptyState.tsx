/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import React from 'react';
import { useTranslation } from 'react-i18next';
import useSWR from 'swr';
import type { TChatConversation } from '@/common/config/storage';
import { getConversationOrNull } from '@/renderer/pages/conversation/utils/conversationCache';
import { resolveConversationBackend } from '@/renderer/pages/conversation/utils/conversationAgentIdentity';
import { resolveAgentAvatar, useAgentLogos } from '@renderer/utils/model/agentLogo';
import { usePresetAgentInfo } from '@renderer/hooks/agent/usePresetAgentInfo';
import { Robot } from '@icon-park/react';

type Props = {
  conversation_id: string;
  agent_name?: string;
  agent_backend?: string;
  icon?: string;
};

/**
 * Resolve the agent display name for an empty single-chat window. Unlike
 * team conversations, a single-chat `conversation.name` is the chat title (a
 * summary of the first message), not the agent identity — so it is never
 * used as a fallback here.
 */
const resolveAgentName = (
  conversation: TChatConversation,
  presetName: string | null,
  explicitAgentName: string | undefined,
  fallback: string
): string => {
  if (presetName) return presetName;
  const trimmedExplicitName = explicitAgentName?.trim();
  if (trimmedExplicitName) return trimmedExplicitName;
  const extraAgentName = (conversation.extra as { agent_name?: string } | undefined)?.agent_name;
  if (extraAgentName && extraAgentName.trim()) return extraAgentName.trim();
  return fallback;
};

/**
 * Empty state shown in a single-chat window that has no messages yet (for
 * example a freshly created or cloned conversation). Renders the agent
 * identity plus a short greeting so the pre-built window is not blank.
 */
const SingleChatEmptyState: React.FC<Props> = ({ conversation_id, agent_name, agent_backend, icon }) => {
  const { t } = useTranslation();
  const logos = useAgentLogos();

  // Reuse an SWR-cached read of the conversation record; the send box and chat
  // surface hit the same backend get, so this dedupes rather than refetches.
  const { data: conversation } = useSWR(conversation_id ? ['single-conversation', conversation_id] : null, () =>
    getConversationOrNull(conversation_id)
  );
  const { info: presetInfo } = usePresetAgentInfo(conversation ?? undefined);

  if (!conversation) return null;

  const agentBackend = resolveConversationBackend(conversation, agent_backend || presetInfo?.backend) || 'acp';
  const agentName = resolveAgentName(
    conversation,
    presetInfo?.name ?? null,
    agent_name,
    t('common.aiAgent', { defaultValue: 'AI Agent' })
  );
  const agentAvatar = resolveAgentAvatar(logos, { icon, backend: agentBackend });

  const renderAvatar = () => {
    if (presetInfo) {
      if (presetInfo.isFallback) {
        return (
          <span className='w-48px h-48px rounded-8px flex items-center justify-center bg-fill-2'>
            <Robot theme='outline' size={24} />
          </span>
        );
      }
      if (presetInfo.isEmoji) {
        return (
          <span className='w-48px h-48px rounded-8px flex items-center justify-center text-32px leading-none bg-fill-2'>
            {presetInfo.logo}
          </span>
        );
      }
      return (
        <img
          src={presetInfo.logo}
          alt={presetInfo.name}
          className='w-48px h-48px object-contain rounded-8px opacity-90'
        />
      );
    }
    if (agentAvatar.kind === 'image') {
      return (
        <img
          src={agentAvatar.value}
          alt={agentName}
          className='w-48px h-48px object-contain rounded-8px opacity-80'
        />
      );
    }
    if (agentAvatar.kind === 'emoji') {
      return (
        <span className='w-48px h-48px rounded-8px flex items-center justify-center text-32px leading-none bg-fill-2'>
          {agentAvatar.value}
        </span>
      );
    }
    return (
      <div className='w-48px h-48px rounded-full bg-fill-3 flex items-center justify-center text-20px font-medium text-t-secondary'>
        <Robot theme='outline' size={24} />
      </div>
    );
  };

  return (
    <div
      data-testid='single-chat-empty-state'
      className='flex flex-col items-center gap-20px px-24px text-center max-w-360px'
    >
      {renderAvatar()}
      <div className='flex flex-col gap-6px'>
        <span className='text-16px font-semibold text-t-primary'>{agentName}</span>
        <span data-testid='single-chat-empty-state-greeting' className='text-13px text-t-secondary'>
          {t('conversation.emptyState.greeting')}
        </span>
      </div>
    </div>
  );
};

export default SingleChatEmptyState;
