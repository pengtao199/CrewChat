import { describe, expect, it } from 'vitest';
import { buildMessageIndex, composeMessageWithIndex } from './hooks';
import type { IMessageText, IMessageThinking, IMessageToolCall, TMessage } from '@/common/chat/chatLib';

const textFrame = (msg_id: string, content: string, replace = true): IMessageText => ({
  id: `t-${content}`,
  msg_id,
  conversation_id: 'conv',
  type: 'text',
  position: 'left',
  status: 'work',
  content: { content, ...(replace ? { replace: true } : {}) },
});

const thinkingFrame = (msg_id: string, content: string): IMessageThinking => ({
  id: `th-${content}`,
  msg_id,
  conversation_id: 'conv',
  type: 'thinking',
  position: 'left',
  content: { content, status: 'thinking', replace: true },
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

  it('replaces (not appends) an accumulated thinking stream', () => {
    // The backend streams the whole accumulated thought each frame with
    // replace:true. Appending stacked every prefix, which is the
    // "推理内容重复很多" bug.
    let list: TMessage[] = [];
    let index = buildMessageIndex(list);

    list = composeMessageWithIndex(thinkingFrame('m1', 'step one'), list, index);
    list = composeMessageWithIndex(thinkingFrame('m1', 'step one step two'), list, index);

    expect(list).toHaveLength(1);
    expect(list[0].type).toBe('thinking');
    expect((list[0] as IMessageThinking).content.content).toBe('step one step two');
  });

  it('keeps one thinking row when a tool card lands between thought frames', () => {
    let list: TMessage[] = [];
    let index = buildMessageIndex(list);

    list = composeMessageWithIndex(thinkingFrame('m1', 'step one'), list, index);
    list = composeMessageWithIndex(toolFrame('m1', 'call_1'), list, index);
    list = composeMessageWithIndex(thinkingFrame('m1', 'step one step two'), list, index);

    expect(list.filter((m) => m.type === 'thinking')).toHaveLength(1);
    expect((list[0] as IMessageThinking).content.content).toBe('step one step two');
  });
});
