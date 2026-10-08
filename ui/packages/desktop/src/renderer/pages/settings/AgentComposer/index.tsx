/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import React from 'react';
import AgentComposerContent from '@/renderer/components/settings/SettingsModal/contents/AgentComposerContent';
import SettingsPageWrapper from '../components/SettingsPageWrapper';

const AgentComposer: React.FC = () => {
  return (
    <SettingsPageWrapper>
      <AgentComposerContent />
    </SettingsPageWrapper>
  );
};

export default AgentComposer;
