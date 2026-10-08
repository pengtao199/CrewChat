/**
 * AssistantSettings — Settings page for managing assistants.
 *
 * Editing permissions by assistant type:
 *
 * | Field          | Builtin | Custom |
 * |----------------|---------|--------|
 * | Save button    |  yes    |  yes   |
 * | Name           |  no     |  yes   |
 * | Description    |  no     |  yes   |
 * | Avatar         |  no     |  yes   |
 * | Main Agent     |  yes    |  yes   |
 * | Prompt editing |  no     |  yes   |
 * | Delete         |  no     |  yes   |
 *
 * Builtin assistants only allow Main Agent plus default model / permission
 * overrides. The full-page editor still renders builtin skills and prompts as
 * read-only so users can inspect what's bundled.
 */
import { Message } from '@arco-design/web-react';
import { useAssistantEditor, useAssistantList } from '@/renderer/hooks/assistant';
import AssistantHomeTabs from './home/AssistantHomeTabs';
import DeleteAssistantModal from './DeleteAssistantModal';
import SkillConfirmModals from './SkillConfirmModals';
import type { AssistantListItem } from './types';
import React, { useCallback, useEffect, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { useLocation, useNavigate } from 'react-router-dom';

type AssistantNavigationState = {
  openAssistantId?: string;
  openAssistantEditor?: boolean;
};
const OPEN_ASSISTANT_EDITOR_INTENT_KEY = 'guid.openAssistantEditorIntent';

const AssistantSettings: React.FC = () => {
  const [message, messageContext] = Message.useMessage({ maxCount: 10 });
  const { t } = useTranslation();
  const location = useLocation();
  const navigate = useNavigate();
  const navigationState = (location.state as AssistantNavigationState | null) ?? null;

  // Keep the current management surface when returning from the editor. The
  // unified Enabled tab is the default entry point for assistant ordering.
  const [homeTab, setHomeTab] = React.useState<'enabled' | 'mine' | 'official'>('enabled');

  // "Chat" on an assistant → open a new conversation with it preselected.
  const handleStartChat = useCallback(
    (assistant: AssistantListItem) => {
      navigate('/guid', { state: { selectedAssistantId: assistant.id } });
    },
    [navigate]
  );

  // [crewchat] 编辑不在此页进行：名册页只负责启停与排序，编辑统一在设置 → Agent。
  const openComposer = useCallback(
    (assistantId?: string) => {
      navigate('/settings/agents', { state: assistantId ? { selectAgentId: assistantId } : { createAgent: true } });
    },
    [navigate]
  );

  // Compose hooks
  const {
    assistants,
    activeAssistantId,
    setActiveAssistantId,
    activeAssistant,
    loadAssistants,
    reorderEnabledAssistants,
    assistantOrder,
    setAssistantOrder,
    localeKey,
  } = useAssistantList();
  const editor = useAssistantEditor({
    localeKey,
    activeAssistant,
    setActiveAssistantId,
    loadAssistants,
    assistants,
    assistantOrder,
    setAssistantOrder,
    message,
  });
  const hasConsumedNavigationIntentRef = useRef(false);

  useEffect(() => {
    if (hasConsumedNavigationIntentRef.current) return;
    const openAssistantFromRoute =
      navigationState?.openAssistantEditor && navigationState.openAssistantId ? navigationState.openAssistantId : null;

    let openAssistantFromSession: string | null = null;
    try {
      const rawIntent = sessionStorage.getItem(OPEN_ASSISTANT_EDITOR_INTENT_KEY);
      if (rawIntent) {
        const parsedIntent = JSON.parse(rawIntent) as { assistantId?: string; openAssistantEditor?: boolean };
        if (parsedIntent.openAssistantEditor && parsedIntent.assistantId) {
          openAssistantFromSession = parsedIntent.assistantId;
        }
      }
    } catch (error) {
      console.error('[AssistantManagement] Failed to parse assistant open intent:', error);
    }

    const targetAssistantId = openAssistantFromRoute ?? openAssistantFromSession;
    if (!targetAssistantId) return;
    if (assistants.length === 0) return;

    const targetAssistant = assistants.find((assistant) => assistant.id === targetAssistantId);
    if (!targetAssistant) return;

    hasConsumedNavigationIntentRef.current = true;
    try {
      sessionStorage.removeItem(OPEN_ASSISTANT_EDITOR_INTENT_KEY);
    } catch (error) {
      console.error('[AssistantManagement] Failed to clear assistant open intent:', error);
    }
    // [crewchat] 编辑意图改为跳转到设置 → Agent
    openComposer(targetAssistant.id);
  }, [assistants, navigationState, openComposer]);

  return (
    <div className='h-full w-full overflow-hidden bg-bg-0'>
      <div className='flex flex-col h-full w-full'>
        {messageContext}
        <div className='flex-1 min-h-0'>
          {/* [crewchat] 原地编辑已移除，名册页只做启停/排序/进对话 */}
          {(
            <AssistantHomeTabs
              assistants={assistants}
              assistantOrder={assistantOrder}
              localeKey={localeKey}
              initialTab={homeTab}
              onTabChange={setHomeTab}
              onOpenDetail={(assistant) => openComposer(assistant.id)}
              onOpenSettings={(assistant) => openComposer(assistant.id)}
              onDuplicate={(assistant) => {
                // A duplicate becomes a new user assistant, so return to My
                // Assistants after saving — not the Official tab it came from.
                setHomeTab('mine');
                void editor.handleDuplicate(assistant);
              }}
              onDelete={(assistant) => editor.handleDeleteRequest(assistant)}
              onCreate={() => {
                setHomeTab('mine');
                openComposer();
              }}
              onToggleEnabled={(assistant, checked) => void editor.handleToggleEnabled(assistant, checked)}
              onReorderEnabled={async (activeId, overId) => {
                try {
                  await reorderEnabledAssistants(activeId, overId);
                } catch {
                  message.error(t('common.failed', { defaultValue: 'Failed' }));
                }
              }}
              onStartChat={handleStartChat}
            />
          )}

          <DeleteAssistantModal
            visible={editor.deleteConfirmVisible}
            onCancel={() => editor.setDeleteConfirmVisible(false)}
            onConfirm={editor.handleDeleteConfirm}
            activeAssistant={activeAssistant}
          />

          <SkillConfirmModals
            deletePendingSkillName={editor.deletePendingSkillName}
            setDeletePendingSkillName={editor.setDeletePendingSkillName}
            pendingSkills={editor.pendingSkills}
            setPendingSkills={editor.setPendingSkills}
            deleteCustomSkillName={editor.deleteCustomSkillName}
            setDeleteCustomSkillName={editor.setDeleteCustomSkillName}
            customSkills={editor.customSkills}
            setCustomSkills={editor.setCustomSkills}
            selectedSkills={editor.selectedSkills}
            setSelectedSkills={editor.setSelectedSkills}
            message={message}
          />
        </div>
      </div>
    </div>
  );
};

export default AssistantSettings;
