import React from 'react';
import UsageModalContent from '@/renderer/components/settings/SettingsModal/contents/UsageModalContent';
import SettingsPageWrapper from '../components/SettingsPageWrapper';

const UsageSettings: React.FC = () => (
  <SettingsPageWrapper contentClassName='max-w-1100px'>
    <UsageModalContent />
  </SettingsPageWrapper>
);

export default UsageSettings;
