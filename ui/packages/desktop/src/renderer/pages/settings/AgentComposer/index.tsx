/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import React from 'react';
import SettingsPageWrapper from '@/renderer/pages/settings/components/SettingsPageWrapper';
import AgentComposerContent from './AgentComposerContent';

const AgentComposer: React.FC = () => {
  return (
    <SettingsPageWrapper>
      <AgentComposerContent />
    </SettingsPageWrapper>
  );
};

export default AgentComposer;
