/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import React from 'react';
import EngineModalContent from '@/renderer/components/settings/SettingsModal/contents/EngineModalContent';
import SettingsPageWrapper from '../components/SettingsPageWrapper';

const EngineSettings: React.FC = () => {
  return (
    <SettingsPageWrapper>
      <EngineModalContent />
    </SettingsPageWrapper>
  );
};

export default EngineSettings;
