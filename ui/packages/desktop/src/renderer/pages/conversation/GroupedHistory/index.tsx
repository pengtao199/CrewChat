/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import type { TChatConversation } from '@/common/config/storage';
import AionModal from '@/renderer/components/base/AionModal';
import { useLayoutContext } from '@/renderer/hooks/context/LayoutContext';
import { useWorkspaces } from '@/renderer/hooks/workspace/useWorkspaces';
import { useCronJobsMap } from '@/renderer/pages/cron';
import { getActivityTime } from '@/renderer/utils/chat/timeline';
import { restrictToVerticalAxis } from '@/renderer/utils/ui/dndModifiers';
import { DndContext, closestCenter } from '@dnd-kit/core';
import { SortableContext, verticalListSortingStrategy } from '@dnd-kit/sortable';
import { Button, Dropdown, Empty, Input, Menu, Message, Modal, Tooltip } from '@arco-design/web-react';
import { FolderClose, ListCheckbox, MoreOne, Plus, Right } from '@icon-park/react';
import classNames from 'classnames';
import React, { useCallback, useEffect, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate, useParams } from 'react-router-dom';

import WorkspaceCollapse from '../components/WorkspaceCollapse';
import ConversationRow from './ConversationRow';
import SortableConversationRow from './SortableConversationRow';
import { useBatchSelection } from './hooks/useBatchSelection';
import { useConversationActions } from './hooks/useConversationActions';
import { useConversations } from './hooks/useConversations';
import { useDragAndDrop } from './hooks/useDragAndDrop';
import type { ConversationRowProps, WorkspaceGroupedHistoryProps } from './types';

const WorkspaceGroupedHistory: React.FC<WorkspaceGroupedHistoryProps> = ({
  onSessionClick,
  collapsed = false,
  tooltipEnabled = false,
  batchMode = false,
  onBatchModeChange,
  onNewSession,
  afterPinnedContent,
}) => {
  const { id } = useParams();
  const { t } = useTranslation();
  const navigate = useNavigate();
  const layout = useLayoutContext();
  const isMobile = layout?.isMobile ?? false;
  const { getJobStatus, markAsRead, setActiveConversation } = useCronJobsMap();
  const { workspaces, create: createWorkspace } = useWorkspaces();

  const {
    conversations,
    isConversationGenerating,
    isConversationWaitingConfirmation,
    hasCompletionUnread,
    isManualUnread,
    markManualUnread,
    clearManualUnread,
    expandedWorkspaces,
    pinnedConversations,
    timelineSections,
    handleToggleWorkspace,
    collapsedSections,
    toggleSection,
  } = useConversations();

  const SectionLabel = useCallback(
    ({ sectionKey, label, trailing }: { sectionKey: string; label: string; trailing?: React.ReactNode }) => {
      const isCollapsed = collapsedSections.has(sectionKey);
      return (
        <div
          className='group/label sider-section-label flex items-center px-12px h-28px select-none sticky top-0 z-10 mt-8px cursor-pointer'
          onClick={() => toggleSection(sectionKey)}
        >
          <span className='text-14px text-t-tertiary sider-section-title group-hover/label:text-t-primary transition-colors font-[500] leading-none'>
            {label}
          </span>
          <span className='ms-2px flex items-center justify-center opacity-0 group-hover/label:opacity-100 transition-opacity text-t-tertiary shrink-0'>
            <Right
              theme='outline'
              size={12}
              className={classNames('transition-transform duration-150', { 'rotate-90': !isCollapsed })}
            />
          </span>
          {trailing && (
            <div className='ms-auto' onClick={(e) => e.stopPropagation()}>
              {trailing}
            </div>
          )}
        </div>
      );
    },
    [collapsedSections, toggleSection]
  );

  // Sync active conversation ref when route changes (for URL navigation)
  // This doesn't trigger state update, avoiding double render
  useEffect(() => {
    if (id) {
      setActiveConversation(id);
    }
  }, [id, setActiveConversation]);

  const {
    selectedConversationIds,
    setSelectedConversationIds,
    selectedCount,
    allSelected,
    toggleSelectedConversation,
    handleToggleSelectAll,
  } = useBatchSelection(batchMode, conversations);

  const {
    renameModalVisible,
    renameModalName,
    setRenameModalName,
    renameLoading,
    dropdownVisibleId,
    handleConversationClick,
    handleArchive,
    handleBatchArchive,
    handleEditStart,
    handleRenameConfirm,
    handleRenameCancel,
    handleTogglePin,
    handleMenuVisibleChange,
    handleOpenMenu,
    handleToggleManualUnread,
    handleCreateCronTask,
    handleArchiveProject,
    archiveProjectTarget,
    archiveProjectLoading,
    handleArchiveProjectCancel,
    handleArchiveProjectConfirm,
  } = useConversationActions({
    batchMode,
    onSessionClick,
    onBatchModeChange,
    selectedConversationIds,
    setSelectedConversationIds,
    toggleSelectedConversation,
    markAsRead,
    markManualUnread,
    clearManualUnread,
    isManualUnread,
  });

  const { sensors, handleDragEnd, isDragEnabled } = useDragAndDrop({
    pinnedConversations,
    batchMode,
    collapsed,
  });

  // Fork-lineage badge support: resolve a parent conversation's display name
  // from the already-loaded sidebar list (no extra fetch; unresolved = the
  // parent was deleted or not loaded → the badge falls back to a generic tip).
  const conversationNameById = useMemo(() => {
    const map = new Map<string, string>();
    for (const conversation of conversations) {
      map.set(conversation.id, conversation.name);
    }
    return map;
  }, [conversations]);
  const resolveConversationName = useCallback(
    (conversationId: string) => conversationNameById.get(conversationId),
    [conversationNameById]
  );

  const getConversationRowProps = useCallback(
    (conversation: TChatConversation): ConversationRowProps => ({
      conversation,
      isGenerating: isConversationGenerating(conversation.id),
      isWaitingConfirmation: isConversationWaitingConfirmation(conversation.id),
      hasUnread: hasCompletionUnread(conversation.id) || isManualUnread(conversation.id),
      isManualUnread: isManualUnread(conversation.id),
      collapsed,
      tooltipEnabled,
      batchMode,
      checked: selectedConversationIds.has(conversation.id),
      selected: id === conversation.id,
      menuVisible: dropdownVisibleId !== null && dropdownVisibleId === conversation.id,
      onToggleChecked: toggleSelectedConversation,
      onConversationClick: handleConversationClick,
      onOpenMenu: handleOpenMenu,
      onMenuVisibleChange: handleMenuVisibleChange,
      onEditStart: handleEditStart,
      onCreateCronTask: handleCreateCronTask,
      onArchive: handleArchive,
      onTogglePin: handleTogglePin,
      onToggleManualUnread: handleToggleManualUnread,
      getJobStatus,
      resolveConversationName,
    }),
    [
      collapsed,
      tooltipEnabled,
      batchMode,
      isConversationGenerating,
      isConversationWaitingConfirmation,
      hasCompletionUnread,
      isManualUnread,
      selectedConversationIds,
      id,
      dropdownVisibleId,
      toggleSelectedConversation,
      handleConversationClick,
      handleOpenMenu,
      handleMenuVisibleChange,
      handleEditStart,
      handleCreateCronTask,
      handleArchive,
      handleTogglePin,
      handleToggleManualUnread,
      getJobStatus,
      resolveConversationName,
    ]
  );

  const renderConversation = (conversation: TChatConversation, dimIcon = false) => {
    const rowProps = getConversationRowProps(conversation);
    return <ConversationRow key={conversation.id} {...rowProps} dimIcon={dimIcon} />;
  };

  // Collect all sortable IDs for the pinned section
  const pinnedIds = useMemo(() => pinnedConversations.map((c) => c.id), [pinnedConversations]);

  // Projects are explicit (from /api/crewchat/projects), not inferred from folders,
  // so an empty project still shows and can start its first session. A project's
  // sessions are those bound by project_id (falling back to matching workspace path).
  const projectGroups = useMemo(() => {
    return workspaces.map((workspace) => {
      const members = conversations.filter(
        (conversation) =>
          conversation.project_id === workspace.id ||
          (!!workspace.path && (conversation.extra as { workspace?: string } | undefined)?.workspace === workspace.path)
      );
      return {
        projectId: workspace.id,
        workspace: workspace.path,
        displayName: workspace.name,
        conversations: [...members].toSorted((a, b) => getActivityTime(b) - getActivityTime(a)),
      };
    });
  }, [workspaces, conversations]);

  const addProject = useCallback(async () => {
    try {
      const paths = await ipcBridge.dialog.showOpen.invoke({ properties: ['openDirectory', 'createDirectory'] });
      const path = paths?.[0];
      if (!path) return;
      const name = path.split('/').filter(Boolean).pop() || path;
      await createWorkspace(name, path);
    } catch (error) {
      Message.error((error as Error).message);
    }
  }, [createWorkspace]);

  const projectsSection = (
    <div className='min-w-0'>
      <SectionLabel
        sectionKey='projects'
        label={t('conversation.history.projectsSection')}
        trailing={
          <Tooltip content={t('conversation.history.addProject')} position='top'>
            <span
              role='button'
              tabIndex={0}
              aria-label={t('conversation.history.addProject')}
              className='flex-center cursor-pointer transition-colors text-t-secondary hover:text-t-primary size-20px rd-4px sider-action-btn'
              onClick={(e) => {
                e.stopPropagation();
                void addProject();
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  e.stopPropagation();
                  void addProject();
                }
              }}
            >
              <Plus theme='outline' size='14' fill='currentColor' className='block leading-none' />
            </span>
          </Tooltip>
        }
      />
      {!collapsedSections.has('projects') &&
        (projectGroups.length > 0
          ? projectGroups.map((group) => {
              const projectMenu = (
                <Menu
                  onClickMenuItem={(key) => {
                    if (key === 'archive') {
                      handleArchiveProject(group.displayName, group.conversations);
                    }
                  }}
                >
                  <Menu.Item key='archive'>
                    <span className='flex items-center gap-8px'>
                      <FolderClose theme='outline' size='14' />
                      {t('conversation.history.archiveProject')}
                    </span>
                  </Menu.Item>
                </Menu>
              );
              return (
                <div key={group.projectId} className='min-w-0'>
                  <WorkspaceCollapse
                    expanded={expandedWorkspaces.includes(group.workspace)}
                    onToggle={() => handleToggleWorkspace(group.workspace)}
                    siderCollapsed={collapsed}
                    stickyHeader
                    stickyTop={28}
                    header={
                      <span className='text-14px font-[500] truncate flex-1 text-t-primary min-w-0'>
                        {group.displayName}
                      </span>
                    }
                    trailing={
                      <span className='flex items-center gap-6px'>
                        <Tooltip content={t('conversation.history.newConversationInProject')} position='top'>
                          <span
                            role='button'
                            tabIndex={0}
                            aria-label={t('conversation.history.newConversationInProject')}
                            className={classNames(
                              'flex-center cursor-pointer transition-colors text-t-secondary hover:text-t-primary size-20px rd-4px sider-action-btn',
                              isMobile ? 'flex' : 'hidden group-hover:flex'
                            )}
                            onClick={(e) => {
                              e.stopPropagation();
                              void navigate('/guid', { state: { workspace: group.workspace } });
                            }}
                            onKeyDown={(e) => {
                              if (e.key === 'Enter' || e.key === ' ') {
                                e.preventDefault();
                                e.stopPropagation();
                                void navigate('/guid', { state: { workspace: group.workspace } });
                              }
                            }}
                          >
                            <Plus theme='outline' size='14' fill='currentColor' className='block leading-none' />
                          </span>
                        </Tooltip>
                        <Dropdown
                          droplist={projectMenu}
                          trigger='click'
                          position='br'
                          getPopupContainer={() => document.body}
                          unmountOnExit={false}
                        >
                          <span
                            aria-label='Project actions'
                            className={classNames(
                              'flex-center cursor-pointer transition-colors text-t-secondary hover:text-t-primary size-20px rd-4px sider-action-btn',
                              isMobile ? 'flex' : 'hidden group-hover:flex'
                            )}
                            onClick={(e) => e.stopPropagation()}
                          >
                            <MoreOne theme='outline' size='14' fill='currentColor' className='block leading-none' />
                          </span>
                        </Dropdown>
                      </span>
                    }
                  >
                    <div className={classNames('flex flex-col min-w-0', { 'mt-1px': !collapsed })}>
                      {group.conversations.map((conversation) => renderConversation(conversation, true))}
                    </div>
                  </WorkspaceCollapse>
                </div>
              );
            })
          : !collapsed && (
              <div
                role='button'
                tabIndex={0}
                aria-label={t('conversation.history.addProject')}
                className='flex items-center gap-8px px-12px h-34px cursor-pointer text-t-secondary hover:bg-fill-2 hover:text-t-primary transition-colors'
                onClick={() => void addProject()}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    void addProject();
                  }
                }}
              >
                <Plus theme='outline' size='14' className='shrink-0' />
                <span className='text-13px'>{t('conversation.history.addProject')}</span>
              </div>
            ))}
    </div>
  );

  // Conversations section: keep timeline grouping (today/yesterday/...) but only show non-workspace conversations.
  const conversationOnlySections = useMemo(
    () =>
      timelineSections
        .map((section) => ({
          ...section,
          items: section.items.filter((item) => item.type === 'conversation' && item.conversation),
        }))
        .filter((section) => section.items.length > 0),
    [timelineSections]
  );

  const hasAnyConversation = timelineSections.length > 0 || pinnedConversations.length > 0 || projectGroups.length > 0;

  return (
    <>
      <Modal
        title={t('conversation.history.renameTitle')}
        visible={renameModalVisible}
        onOk={handleRenameConfirm}
        onCancel={handleRenameCancel}
        okText={t('conversation.history.saveName')}
        cancelText={t('conversation.history.cancelEdit')}
        confirmLoading={renameLoading}
        okButtonProps={{ disabled: !renameModalName.trim() }}
        style={{ borderRadius: '12px' }}
        alignCenter
        getPopupContainer={() => document.body}
      >
        <Input
          autoFocus
          value={renameModalName}
          onChange={setRenameModalName}
          onPressEnter={handleRenameConfirm}
          placeholder={t('conversation.history.renamePlaceholder')}
          allowClear
        />
      </Modal>

      {batchMode && !collapsed && (
        <div className='px-12px pb-8px pt-2px sticky top-0 z-20 bg-[var(--bg-2)]'>
          <div className='rd-8px bg-fill-1 p-10px flex flex-col gap-8px border border-solid border-[rgba(var(--primary-6),0.08)]'>
            <div className='text-12px leading-18px text-t-secondary'>
              {t('conversation.history.selectedCount', { count: selectedCount })}
            </div>
            <div className='grid grid-cols-2 gap-6px'>
              <Button
                className='!w-full !justify-center !min-w-0 !h-30px !px-8px !text-12px whitespace-nowrap'
                size='mini'
                type='secondary'
                onClick={handleToggleSelectAll}
              >
                {allSelected ? t('common.cancel') : t('conversation.history.selectAll')}
              </Button>
              <Button
                className='!w-full !justify-center !min-w-0 !h-30px !px-8px !text-12px whitespace-nowrap'
                size='mini'
                type='primary'
                onClick={handleBatchArchive}
              >
                {t('conversation.history.batchArchive')}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* 归档项目确认弹窗 — 使用项目自家 AionModal + 圆角线框按钮（归档为非危险态，用主色） */}
      <AionModal
        visible={archiveProjectTarget !== null}
        style={{ width: '400px' }}
        header={{
          title: t('conversation.history.archiveProjectTitle'),
          showClose: true,
          style: { borderBottom: 'none' },
        }}
        onCancel={handleArchiveProjectCancel}
        footer={
          <div className='flex justify-end gap-12px pt-16px'>
            <button
              type='button'
              className='px-24px py-8px rounded-20px text-14px font-medium transition-all'
              style={{
                border: '1px solid var(--color-border-2)',
                backgroundColor: 'var(--color-fill-2)',
                color: 'var(--color-text-1)',
                cursor: archiveProjectLoading ? 'not-allowed' : 'pointer',
                opacity: archiveProjectLoading ? 0.55 : 1,
              }}
              onMouseEnter={(event) => {
                if (!archiveProjectLoading) event.currentTarget.style.backgroundColor = 'var(--color-fill-3)';
              }}
              onMouseLeave={(event) => {
                if (!archiveProjectLoading) event.currentTarget.style.backgroundColor = 'var(--color-fill-2)';
              }}
              onClick={handleArchiveProjectCancel}
              disabled={archiveProjectLoading}
            >
              {t('common.cancel')}
            </button>
            <button
              type='button'
              className='px-24px py-8px rounded-20px text-14px font-medium transition-all'
              style={{
                border: '1px solid rgb(var(--primary-6))',
                backgroundColor: 'transparent',
                color: 'rgb(var(--primary-6))',
                cursor: archiveProjectLoading ? 'not-allowed' : 'pointer',
                opacity: archiveProjectLoading ? 0.55 : 1,
              }}
              onMouseEnter={(event) => {
                if (!archiveProjectLoading) {
                  event.currentTarget.style.backgroundColor = 'rgba(var(--primary-6), 0.08)';
                }
              }}
              onMouseLeave={(event) => {
                if (!archiveProjectLoading) event.currentTarget.style.backgroundColor = 'transparent';
              }}
              onClick={() => void handleArchiveProjectConfirm()}
              disabled={archiveProjectLoading}
            >
              {archiveProjectLoading ? t('conversation.history.archiving') : t('conversation.history.archiveProject')}
            </button>
          </div>
        }
      >
        <div className='text-14px leading-22px text-t-secondary'>
          {t('conversation.history.archiveProjectConfirm', {
            name: archiveProjectTarget?.name ?? '',
            count: archiveProjectTarget?.conversations.length ?? 0,
          })}
        </div>
      </AionModal>

      <div>
        {/* L1: Pinned section */}
        <DndContext
          sensors={sensors}
          collisionDetection={closestCenter}
          modifiers={[restrictToVerticalAxis]}
          onDragEnd={handleDragEnd}
        >
          {pinnedConversations.length > 0 && (
            <div className='min-w-0'>
              {!collapsed && <SectionLabel sectionKey='pinned' label={t('conversation.history.pinnedSection')} />}
              {!collapsedSections.has('pinned') && (
                <SortableContext items={pinnedIds} strategy={verticalListSortingStrategy}>
                  <div className='min-w-0'>
                    {pinnedConversations.map((conversation) => {
                      const props = getConversationRowProps(conversation);
                      return isDragEnabled ? (
                        <SortableConversationRow key={conversation.id} {...props} />
                      ) : (
                        <ConversationRow key={conversation.id} {...props} />
                      );
                    })}
                  </div>
                </SortableContext>
              )}
            </div>
          )}
        </DndContext>

        {/* Slot 由父级（Sider）填入：例如 Team / CronJob sections，位于「置顶」之后、「项目」之前 */}
        {afterPinnedContent}

        {/* L1: Projects — explicit projects (may be empty), peer to sessions */}
        {projectsSection}

        {/* L1: Sessions — always present; holds sessions with no project */}
        <div className='min-w-0'>
          {!collapsed && (
            <SectionLabel
              sectionKey='conversations'
              label={t('conversation.history.conversationsSection')}
              trailing={
                <span className='flex items-center gap-6px'>
                  {onBatchModeChange && (
                    <Tooltip
                      content={batchMode ? t('conversation.history.batchModeExit') : t('conversation.history.batchManage')}
                      position='top'
                    >
                      <span
                        role='button'
                        tabIndex={0}
                        aria-label={t('conversation.history.batchManage')}
                        className={classNames(
                          'flex-center cursor-pointer transition-colors size-20px rd-4px sider-action-btn',
                          batchMode
                            ? 'text-primary bg-[rgba(var(--primary-6),0.12)]'
                            : 'text-t-secondary hover:text-t-primary'
                        )}
                        onClick={(e) => {
                          e.stopPropagation();
                          onBatchModeChange(!batchMode);
                        }}
                      >
                        <ListCheckbox theme='outline' size='14' className='block leading-none' />
                      </span>
                    </Tooltip>
                  )}
                  {onNewSession && (
                    <Tooltip content={t('conversation.welcome.newConversation')} position='top'>
                      <span
                        role='button'
                        tabIndex={0}
                        aria-label={t('conversation.welcome.newConversation')}
                        className='flex-center cursor-pointer transition-colors text-t-secondary hover:text-t-primary size-20px rd-4px sider-action-btn'
                        onClick={(e) => {
                          e.stopPropagation();
                          onNewSession();
                        }}
                      >
                        <Plus theme='outline' size='14' fill='currentColor' className='block leading-none' />
                      </span>
                    </Tooltip>
                  )}
                </span>
              }
            />
          )}
          {!collapsedSections.has('conversations') &&
            (conversationOnlySections.length > 0
              ? conversationOnlySections.map((section) => (
                  <div key={section.timeline} className='min-w-0'>
                    {!collapsed && conversationOnlySections.length > 1 && (
                      <div className='flex items-center px-16px h-24px select-none'>
                        <span className='text-12px text-t-secondary font-[500] leading-none'>{section.timeline}</span>
                      </div>
                    )}
                    {section.items.map((item) =>
                      item.type === 'conversation' && item.conversation ? renderConversation(item.conversation) : null
                    )}
                  </div>
                ))
              : !collapsed && (
                  <div className='px-12px py-6px text-13px text-t-tertiary'>
                    {t('conversation.history.noHistory')}
                  </div>
                ))}
        </div>

        {!hasAnyConversation && (
          <div className='py-24px flex-center'>
            <Empty description={t('conversation.history.noHistory')} />
          </div>
        )}
      </div>
    </>
  );
};

export default WorkspaceGroupedHistory;
