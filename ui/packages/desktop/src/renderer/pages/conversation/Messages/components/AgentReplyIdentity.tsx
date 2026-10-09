/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import React from 'react';
import { Robot } from '@icon-park/react';
import ThemedLogo from '@/renderer/components/agent/ThemedLogo';
import { useConversationAgents } from '@/renderer/pages/conversation/hooks/useConversationAgents';
import { resolveAgentAvatar, useAgentLogos } from '@/renderer/utils/model/agentLogo';

type Props = {
  /** Display name the backend stamped on the reply. */
  name: string;
  /** Our agent-catalog id, when the reply carries one. */
  agentId?: string;
  /** Engine that answered (claude/codex/llm/...), used as the avatar fallback. */
  engine?: string;
  /** Teammate replies keep their per-speaker colour; a 1:1 reply uses the default. */
  color?: string;
  size?: number;
};

/**
 * Who is speaking, above a reply bubble.
 *
 * Resolution order: the agent's own configured avatar (looked up by id), then
 * the engine's logo, then a neutral glyph. The engine fallback matters because a
 * reply can outlive the agent that produced it — if that agent is renamed or
 * removed, the thread must still identify the speaker.
 */
const AgentReplyIdentity: React.FC<Props> = ({ name, agentId, engine, color, size = 20 }) => {
  const { presetAgents } = useConversationAgents();
  const logos = useAgentLogos();

  const agent = agentId ? presetAgents.find((item) => item.id === agentId || item.agent_id === agentId) : undefined;
  const avatar = resolveAgentAvatar(logos, {
    icon: agent?.avatar,
    backend: engine,
  });

  return (
    <div className='flex items-center gap-6px mb-4px' data-testid='agent-reply-identity'>
      <ThemedLogo
        src={avatar.kind === 'fallback' ? null : avatar.value}
        alt=''
        className='rd-full shrink-0'
        style={{ width: `${size}px`, height: `${size}px` }}
        fallback={
          <span
            className='rounded-full flex items-center justify-center leading-none bg-fill-2'
            style={{ width: `${size}px`, height: `${size}px` }}
          >
            <Robot theme='outline' size={Math.round(size * 0.6)} />
          </span>
        }
      />
      <span className='text-12px' style={{ color: color || 'var(--text-secondary)' }}>
        {name}
      </span>
    </div>
  );
};

export default AgentReplyIdentity;
