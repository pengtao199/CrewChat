import type { IMessageText, TMessage } from '@/common/chat/chatLib';
import { stripThinkTags } from '@/renderer/utils/chat/thinkTagFilter';

const isCopyableAgentText = (message: TMessage): message is IMessageText => {
  return message.type === 'text' && message.position === 'left' && !message.hidden;
};

const stripSkillSuggestPreserveWhitespace = (content: string): string => {
  return content.replace(/\[SKILL_SUGGEST\][\s\S]*?\[\/SKILL_SUGGEST\]/gi, '').replace(/\n{3,}/g, '\n\n');
};

const sanitizeAgentText = (content: string): string => {
  return stripSkillSuggestPreserveWhitespace(stripThinkTags(content));
};

export const getLastAgentText = (messageList: TMessage[], loading: boolean): string | null => {
  if (loading) {
    return null;
  }

  for (let index = messageList.length - 1; index >= 0; index -= 1) {
    const message = messageList[index];
    if (!isCopyableAgentText(message)) {
      continue;
    }

    const sanitizedContent = sanitizeAgentText(message.content.content);
    if (sanitizedContent.trim()) {
      return sanitizedContent;
    }
  }

  return null;
};
