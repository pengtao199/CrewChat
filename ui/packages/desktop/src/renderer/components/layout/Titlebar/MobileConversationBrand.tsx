/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import { AgentLogoIcon } from '@/renderer/components/agent/AgentBadge';
import { usePresetAgentInfo } from '@/renderer/hooks/agent/usePresetAgentInfo';
import { resolveConversationBackend } from '@/renderer/pages/conversation/utils/conversationAgentIdentity';
import React from 'react';
import useSWR from 'swr';

type MobileConversationBrandProps = {
  conversation_id: string;
  fallbackTitle: string;
};

const MobileConversationBrand: React.FC<MobileConversationBrandProps> = ({ conversation_id, fallbackTitle }) => {
  const { data: conversation } = useSWR(
    conversation_id ? `mobile-titlebar.conversation.${conversation_id}` : null,
    () => ipcBridge.conversation.get.invoke({ id: conversation_id })
  );
  const { info: presetAgent } = usePresetAgentInfo(conversation || undefined);
  const backend = resolveConversationBackend(conversation, presetAgent?.backend);

  const showLogo = Boolean(backend || presetAgent);
  const title = conversation?.name || fallbackTitle;

  return (
    <span className='app-titlebar__brand-mobile'>
      {showLogo && (
        <AgentLogoIcon
          backend={backend}
          agent_name={title}
          agentLogo={presetAgent?.logo}
          agentLogoIsEmoji={presetAgent?.isEmoji}
          agentLogoIsFallback={presetAgent?.isFallback}
        />
      )}
      <span className='app-titlebar__brand-text'>{title}</span>
    </span>
  );
};

export default MobileConversationBrand;
