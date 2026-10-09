/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import { useCallback, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Message as Toast } from '@arco-design/web-react';

type UseRetryMessageOptions = {
  conversationId: string;
  /** The agent that wrote the bubble; omit to retry every agent in the turn. */
  agentId?: string;
  agentName?: string;
};

/**
 * Re-ask one agent (or the whole turn) about the message being retried.
 *
 * Scoped to a single agent on purpose: replaying the whole turn would re-run
 * every speaker and repeat their answers, which is expensive and rarely what the
 * user wants when one reply looked wrong.
 */
export const useRetryMessage = ({ conversationId, agentId, agentName }: UseRetryMessageOptions) => {
  const { t } = useTranslation();
  const [retrying, setRetrying] = useState(false);

  const retry = useCallback(async () => {
    if (!conversationId || retrying) return;
    setRetrying(true);
    try {
      await ipcBridge.conversation.retryMessage.invoke({
        conversation_id: conversationId,
        ...(agentId ? { agent_id: agentId } : {}),
      });
    } catch (error) {
      Toast.error(
        t('messages.retry.failed', {
          defaultValue: 'Retry failed: {{error}}',
          error: error instanceof Error ? error.message : String(error),
        })
      );
      setRetrying(false);
      return;
    }
    // The stream takes over from here; the spinner ends when the new turn settles.
    setTimeout(() => setRetrying(false), 1500);
  }, [agentId, conversationId, retrying, t]);

  return { retry, retryLatestTurn: retry, retrying, agentName };
};

export default useRetryMessage;
