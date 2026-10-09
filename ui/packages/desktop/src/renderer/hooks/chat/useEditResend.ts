/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import { useCallback, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Message as Toast } from '@arco-design/web-react';

type UseEditResendOptions = {
  conversationId: string;
  /** Called after a successful edit so the caller can re-run the turn. */
  onResend?: (content: string) => void | Promise<void>;
};

/**
 * Edit a sent message and ask again with the new wording.
 *
 * The backend edits the row in place rather than appending a corrected copy, so
 * history shows what was actually asked — an appended duplicate would leave two
 * different questions with one answer between them.
 */
export const useEditResend = ({ conversationId, onResend }: UseEditResendOptions) => {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);

  const submit = useCallback(
    async (next: string) => {
      const content = next.trim();
      if (!content || saving) {
        setEditing(false);
        return;
      }
      setSaving(true);
      try {
        setEditing(false);
        await onResend?.(content);
      } finally {
        setSaving(false);
      }
    },
    [onResend, saving]
  );

  const edit = useCallback(
    async (messageId: string, content: string) => {
      setSaving(true);
      try {
        await ipcBridge.conversation.updateMessage.invoke({
          conversation_id: conversationId,
          message_id: messageId,
          content,
        });
        await submit(content);
      } catch (error) {
        Toast.error(
          t('messages.edit.failed', {
            defaultValue: 'Edit failed: {{error}}',
            error: error instanceof Error ? error.message : String(error),
          })
        );
      } finally {
        setSaving(false);
      }
    },
    [conversationId, submit, t]
  );

  return { edit, editing, setEditing, saving };
};

export default useEditResend;
