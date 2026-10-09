/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import type { MessageUsage } from '@/common/chat/chatLib';
import React from 'react';

/**
 * Per-reply provenance: which model answered, what it cost in tokens, and how
 * long it took. The backend records this per agent (see `usage_events`), so each
 * bubble in a group turn carries its own numbers rather than a turn total.
 */
const MessageUsageFooter: React.FC<{ usage: MessageUsage }> = ({ usage }) => {
  const parts: string[] = [];

  if (usage.model) parts.push(usage.model);

  const tokens = (usage.input_tokens || 0) + (usage.output_tokens || 0);
  if (tokens > 0) parts.push(`${tokens.toLocaleString()} tokens`);

  if (typeof usage.cost_usd === 'number' && usage.cost_usd > 0) {
    parts.push(`$${usage.cost_usd < 0.01 ? usage.cost_usd.toFixed(4) : usage.cost_usd.toFixed(2)}`);
  }

  if (typeof usage.duration_ms === 'number' && usage.duration_ms > 0) {
    const seconds = usage.duration_ms / 1000;
    parts.push(seconds < 60 ? `${seconds.toFixed(1)}s` : `${Math.round(seconds / 60)}min`);
  }

  if (parts.length === 0) return null;

  return (
    <div
      className='mt-4px flex flex-wrap items-center gap-6px text-12px text-t-secondary select-none'
      data-testid='message-usage-footer'
    >
      {parts.map((part) => (
        <span key={part}>{part}</span>
      ))}
    </div>
  );
};

export default MessageUsageFooter;
