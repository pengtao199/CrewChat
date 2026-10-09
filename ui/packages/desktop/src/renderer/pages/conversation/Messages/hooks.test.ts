import { describe, expect, it } from 'vitest';
import { buildMessageIndex, composeMessageWithIndex } from './hooks';
import type { IMessageText, IMessageToolCall, TMessage } from '@/common/chat/chatLib';

const textFrame = (msg_id: string, content: string, replace = true): IMessageText => ({
  id: `t-${content}`,
  msg_id,
  conversation_id: 'conv',
  type: 'text',
  position: 'left',
  status: 'work',
  content: { content, ...(replace ? { replace: true } : {}) },
});

const toolFrame = (msg_id: string, call_id: string, status = 'running'): IMessageToolCall => ({
  id: `tool-${call_id}-${status}`,
  msg_id,
  conversation_id: 'conv',
  type: 'tool_call',
  content: { call_id, name: 'Bash', status: status as never, args: {} },
});

describe('composeMessageWithIndex', () => {
  it('rewrites a text row in place even when a tool card landed in between', () => {
    // Stream order for one agent reply interleaves: text frame → tool cards →
    // text frame (with the full accumulated text, replace). The second text frame
    // used to append a duplicate bubble because the tool card was in the way —
    // exactly the "现在来优化配置：" repeating-on-screen bug.
    let list: TMessage[] = [];
    let index = buildMessageIndex(list);

    list = composeMessageWithIndex(textFrame('m1', 'hello'), list, index);
    list = composeMessageWithIndex(toolFrame('m1', 'call_1'), list, index);
    list = composeMessageWithIndex(textFrame('m1', 'hello world'), list, index);

    expect(list).toHaveLength(2); // one text row + one tool card
    expect(list[0].type).toBe('text');
    expect((list[0] as IMessageText).content.content).toBe('hello world');
    expect(list[1].type).toBe('tool_call');
  });

  it('does not let a tool frame own the shared msg_id key', () => {
    let list: TMessage[] = [];
    let index = buildMessageIndex(list);

    list = composeMessageWithIndex(textFrame('m1', 'hello'), list, index);
    const textRowIdx = list.length - 1;
    expect(index.msgIdIndex.get('m1')).toBe(textRowIdx);

    list = composeMessageWithIndex(toolFrame('m1', 'call_1'), list, index);
    // The tool card must not steal the text bubble's slot in msgIdIndex.
    expect(index.msgIdIndex.get('m1')).toBe(textRowIdx);
    expect(index.call_idIndex.get('call_1')).toBe(list.length - 1);
  });
});
