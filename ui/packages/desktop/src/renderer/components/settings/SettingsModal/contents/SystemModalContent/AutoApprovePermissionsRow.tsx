/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * [crewchat] 唯一的权限开关：「自动批准所有权限请求」。
 * 真相源在后端 `.data/permissions.jsonc`，本组件只读写 `/api/crewchat/permissions`。
 */

import { withApiVersion } from '@/common/adapter/apiPath';
import { Message, Switch } from '@arco-design/web-react';
import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import PreferenceRow from './PreferenceRow';

type PermissionState = { auto_approve: boolean };

const AutoApprovePermissionsRow: React.FC = () => {
  const { t } = useTranslation();
  const [enabled, setEnabled] = useState(true);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    fetch(withApiVersion('/api/crewchat/permissions'), { credentials: 'include' })
      .then((response) => response.json() as Promise<PermissionState>)
      .then((state) => {
        if (!cancelled) setEnabled(state.auto_approve);
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const handleChange = async (checked: boolean) => {
    const previous = enabled;
    setEnabled(checked);
    try {
      const response = await fetch(withApiVersion('/api/crewchat/permissions'), {
        method: 'PUT',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ auto_approve: checked }),
      });
      if (!response.ok) throw new Error(`${response.status}`);
      const state = (await response.json()) as PermissionState;
      setEnabled(state.auto_approve);
    } catch {
      setEnabled(previous);
      Message.error(t('settings.autoApprovePermissionsSaveFailed'));
    }
  };

  return (
    <PreferenceRow
      label={t('settings.autoApprovePermissions')}
      description={`${t('settings.autoApprovePermissionsDesc')} (${t('settings.autoApprovePermissionsSavedTo')}: .data/permissions.jsonc)`}
    >
      <Switch checked={enabled} disabled={loading} onChange={handleChange} />
    </PreferenceRow>
  );
};

export default AutoApprovePermissionsRow;
