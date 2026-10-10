import React from 'react';
import { HashRouter, Navigate, Route, Routes, useLocation } from 'react-router-dom';
import DocumentTitle from '@renderer/components/layout/DocumentTitle';
import { useCrossSessionRateLimitNotice } from '@/renderer/hooks/system/useCrossSessionRateLimitNotice';
import { useAuth } from '@renderer/hooks/context/AuthContext';
import { TEAM_MODE_ENABLED } from '@/common/config/constants';
import Conversation from '@renderer/pages/conversation';
import Guid from '@renderer/pages/guid';
import EngineSettings from '@renderer/pages/settings/EngineSettings';
// [crewchat] 独立于上游的 Agent 编排页
import AgentComposer from '@renderer/pages/settings/AgentComposer';
import AgentRepairPage from '@renderer/pages/settings/EngineSettings/AgentRepairPage';
import SkillsSettings from '@renderer/pages/settings/SkillsSettings/SkillsHubSettings';
import SkillDetailPage from '@renderer/pages/settings/SkillsSettings/SkillDetailPage';
import ToolsSettings from '@renderer/pages/settings/ToolsSettings';
// [crewchat] 用量统计页（只统计经本 app 发起的 CLI 身体消耗）
import UsageSettings from '@renderer/pages/settings/UsageSettings';
import AppearanceSettings from '@renderer/pages/settings/AppearanceSettings';
import ModeSettings from '@renderer/pages/settings/ModeSettings';
import SystemSettings from '@renderer/pages/settings/SystemSettings';
import WebuiSettings from '@renderer/pages/settings/WebuiSettings';
import PetSettings from '@renderer/pages/settings/PetSettings';
import ArchivedSettings from '@renderer/pages/settings/ArchivedSettings';
import ExtensionSettingsPage from '@renderer/pages/settings/ExtensionSettingsPage';
import ComponentsShowcase from '@renderer/pages/TestShowcase';
import ScheduledTasksPage from '@renderer/pages/cron/ScheduledTasksPage';
import TaskDetailPage from '@renderer/pages/cron/ScheduledTasksPage/TaskDetailPage';
import TeamIndex from '@renderer/pages/team';

const withRouteFallback = (Component: React.ComponentType) => <Component />;

/**
 * Legacy `/settings/capabilities?tab=tools` deep links now map to the standalone
 * Tools page; everything else (skills tab or no tab) lands on the Skills page.
 */
const CapabilitiesRedirect: React.FC = () => {
  const { search } = useLocation();
  const tab = new URLSearchParams(search).get('tab');
  return <Navigate to={tab === 'tools' ? '/settings/tools' : '/settings/skills'} replace />;
};

const ProtectedLayout: React.FC<{ layout: React.ReactElement }> = ({ layout }) => {
  const { user } = useAuth();
  // Mounted once for every route: the loop warning has to reach the user even
  // when they are looking at a THIRD conversation, which is the whole reason it
  // is a broadcast rather than an in-conversation banner.
  useCrossSessionRateLimitNotice(user?.id);
  return React.cloneElement(layout);
};

const PanelRoute: React.FC<{ layout: React.ReactElement }> = ({ layout }) => {
  return (
    <HashRouter>
      <DocumentTitle />
      <Routes>
        <Route element={<ProtectedLayout layout={layout} />}>
          <Route index element={<Navigate to='/guid' replace />} />
          <Route path='/guid' element={withRouteFallback(Guid)} />
          <Route path='/conversation/:id' element={withRouteFallback(Conversation)} />
          <Route
            path='/team/:id'
            element={TEAM_MODE_ENABLED ? withRouteFallback(TeamIndex) : <Navigate to='/guid' replace />}
          />
          <Route path='/settings/model' element={withRouteFallback(ModeSettings)} />
          {/* Agents live only under Settings now; old entries redirect there. */}
          <Route path='/agents' element={<Navigate to='/settings/agents' replace />} />
          {/* [crewchat] Agent 编排页 */}
          <Route path='/settings/agents' element={withRouteFallback(AgentComposer)} />
          {/* 引擎（CLI 身体） */}
          <Route path='/settings/engines' element={withRouteFallback(EngineSettings)} />
          <Route path='/settings/engines/:id/repair' element={withRouteFallback(AgentRepairPage)} />
          {/* Skills and Tools are top-level settings entries. */}
          <Route path='/settings/skills' element={withRouteFallback(SkillsSettings)} />
          <Route path='/settings/skills/import-history' element={withRouteFallback(SkillsSettings)} />
          <Route path='/settings/skills/detail/:skillName' element={withRouteFallback(SkillDetailPage)} />
          <Route path='/settings/tools' element={withRouteFallback(ToolsSettings)} />
          <Route path='/settings/usage' element={withRouteFallback(UsageSettings)} />
          {/* Legacy routes — the previous combined "Capabilities" page is now two pages. */}
          <Route path='/settings/capabilities' element={<CapabilitiesRedirect />} />
          <Route
            path='/settings/capabilities/skills/import-history'
            element={<Navigate to='/settings/skills/import-history' replace />}
          />
          <Route path='/settings/skills-hub' element={<Navigate to='/settings/skills' replace />} />
          <Route path='/settings/appearance' element={withRouteFallback(AppearanceSettings)} />
          <Route path='/settings/display' element={<Navigate to='/settings/appearance' replace />} />
          <Route path='/settings/webui' element={withRouteFallback(WebuiSettings)} />
          <Route path='/settings/pet' element={withRouteFallback(PetSettings)} />
          <Route path='/settings/archived' element={withRouteFallback(ArchivedSettings)} />
          <Route path='/settings/system' element={withRouteFallback(SystemSettings)} />
          <Route path='/settings/about' element={withRouteFallback(SystemSettings)} />
          <Route path='/settings/ext/:tabId' element={withRouteFallback(ExtensionSettingsPage)} />
          <Route path='/settings' element={<Navigate to='/settings/agents' replace />} />
          <Route path='/test/components' element={withRouteFallback(ComponentsShowcase)} />
          <Route path='/scheduled' element={withRouteFallback(ScheduledTasksPage)} />
          <Route path='/scheduled/:job_id' element={withRouteFallback(TaskDetailPage)} />
        </Route>
        <Route path='*' element={<Navigate to='/guid' replace />} />
      </Routes>
    </HashRouter>
  );
};

export default PanelRoute;
