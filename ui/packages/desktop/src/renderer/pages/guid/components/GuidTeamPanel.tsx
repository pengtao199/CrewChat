import { Button } from '@arco-design/web-react';
import type { ManagerStatus } from '@/common/adapter/ipcBridge';
import StatusBadge from '@/renderer/components/base/StatusBadge';
import React from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';

type GuidTeamPanelProps = {
  manager: ManagerStatus | null;
  managerLoading: boolean;
  onRestoreManager: () => void;
};

const GuidTeamPanel: React.FC<GuidTeamPanelProps> = ({ manager, managerLoading, onRestoreManager }) => {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const missing = !manager?.exists;
  const needsRepair = missing || !manager.ready;
  if (!needsRepair) return null;

  return (
    <div className='mb-8px flex items-center justify-center gap-8px'>
      <StatusBadge
        tone={missing ? 'blocked' : 'warning'}
        label={missing ? t('conversation.group.managerMissing') : t('conversation.group.managerUnconfigured')}
      />
      {missing ? (
        <Button size='mini' type='outline' loading={managerLoading} onClick={onRestoreManager}>
          {t('conversation.group.restore')}
        </Button>
      ) : (
        <Button size='mini' type='outline' onClick={() => navigate('/settings/agents')}>
          {t('conversation.group.configure')}
        </Button>
      )}
    </div>
  );
};

export default GuidTeamPanel;
