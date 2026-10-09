/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * Sidebar workspace switcher: pick / create the workspace (a folder) that new
 * sessions run in. "会话工作区" = no workspace (a per-session sandbox).
 */

import { ipcBridge } from '@/common';
import { Dropdown, Menu, Message } from '@arco-design/web-react';
import { FolderClose, Plus } from '@icon-park/react';
import classNames from 'classnames';
import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  getCurrentWorkspace,
  setCurrentWorkspace,
  useWorkspaces,
} from '@/renderer/hooks/workspace/useWorkspaces';

const WorkspaceSwitcher: React.FC<{ collapsed?: boolean }> = ({ collapsed = false }) => {
  const { t } = useTranslation();
  const { workspaces, create } = useWorkspaces();
  const [current, setCurrent] = useState(getCurrentWorkspace());

  const pick = (ws: { id: string; name: string; path: string } | null) => {
    setCurrentWorkspace(ws);
    setCurrent(ws);
  };

  const createWorkspace = async () => {
    try {
      const paths = await ipcBridge.dialog.showOpen.invoke({ properties: ['openDirectory', 'createDirectory'] });
      const path = paths?.[0];
      if (!path) return;
      const name = path.split('/').filter(Boolean).pop() || path;
      const ws = await create(name, path);
      pick({ id: ws.id, name: ws.name, path: ws.path });
      Message.success(t('workspaces.created', { name: ws.name }));
    } catch (error) {
      Message.error((error as Error).message);
    }
  };

  const menu = (
    <Menu
      onClickMenuItem={(key) => {
        if (key === 'none') return pick(null);
        if (key === 'new') return void createWorkspace();
        const ws = workspaces.find((item) => item.id === key);
        if (ws) pick({ id: ws.id, name: ws.name, path: ws.path });
      }}
    >
      <Menu.Item key='none'>{t('workspaces.none')}</Menu.Item>
      {workspaces.map((ws) => (
        <Menu.Item key={ws.id}>{ws.name}</Menu.Item>
      ))}
      <Menu.Item key='new'>
        <span className='inline-flex items-center gap-6px'>
          <Plus size='12' /> {t('workspaces.new')}
        </span>
      </Menu.Item>
    </Menu>
  );

  return (
    <Dropdown droplist={menu} trigger='click' position='bl'>
      <div
        className={classNames(
          'flex items-center gap-8px px-10px h-32px mx-6px rd-8px cursor-pointer text-t-secondary hover:bg-fill-2 hover:text-t-primary transition-colors',
          collapsed && 'justify-center mx-4px px-0'
        )}
        title={current?.path}
      >
        <FolderClose theme='outline' size='14' className='shrink-0' />
        {!collapsed && (
          <span className='truncate text-13px'>{current?.name ?? t('workspaces.none')}</span>
        )}
      </div>
    </Dropdown>
  );
};

export default WorkspaceSwitcher;
