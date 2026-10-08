/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * Status badge with three fixed tones, so a resource reads the same on every
 * page: whether it exists, whether it can be used, and whether it is in use.
 *
 * - ready:   installed / configured and usable
 * - warning: present but not usable yet (missing key, not signed in)
 * - blocked: absent or unusable (not installed, unresolved reference)
 * - neutral: informational, no action implied (usage counts)
 */

import { Tag, Tooltip } from '@arco-design/web-react';
import React from 'react';

export type StatusTone = 'ready' | 'warning' | 'blocked' | 'neutral';

const TONE_COLOR: Record<StatusTone, string> = {
  ready: 'green',
  warning: 'orange',
  blocked: 'red',
  neutral: 'gray',
};

type StatusBadgeProps = {
  tone: StatusTone;
  label: React.ReactNode;
  tooltip?: React.ReactNode;
};

const StatusBadge: React.FC<StatusBadgeProps> = ({ tone, label, tooltip }) => {
  const tag = (
    <Tag size='small' color={TONE_COLOR[tone]}>
      {label}
    </Tag>
  );
  return tooltip ? <Tooltip content={tooltip}>{tag}</Tooltip> : tag;
};

export default StatusBadge;
