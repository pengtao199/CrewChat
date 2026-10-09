/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { firstModelId } from '@/common/types/provider/providerApi';
import type { IChannelPairingRequest, IChannelPluginStatus, IChannelUser } from '@/common/types/channel/channel';
import { agents, channel } from '@/common/adapter/ipcBridge';
import { isAionrsAgent, type Agent } from '@/common/types/agent/agentTypes';
import { resolveLocaleKey } from '@/common/utils';
import { resolveAgentName } from '@/renderer/utils/model/agentDisplay';
import { formatDateTime } from '@/renderer/services/i18n/format';
import GoogleModelSelector from '@/renderer/pages/conversation/platforms/gemini/GoogleModelSelector';
import type { GoogleModelSelection } from '@/renderer/pages/conversation/platforms/gemini/useGoogleModelSelection';
import { Button, Dropdown, Empty, Input, Menu, Message, Spin, Tooltip } from '@arco-design/web-react';
import { CheckOne, CloseOne, Copy, Delete, Down, Refresh } from '@icon-park/react';
import React, { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  buildChannelAgentBinding,
  getDefaultChannelAgent,
  resolveChannelAgentSelection,
} from './agentBinding';

/**
 * Preference row component
 */
const PreferenceRow: React.FC<{
  label: string;
  description?: React.ReactNode;
  extra?: React.ReactNode;
  children: React.ReactNode;
}> = ({ label, description, extra, children }) => (
  <div className='flex items-center justify-between gap-24px py-12px'>
    <div className='flex-1'>
      <div className='flex items-center gap-8px'>
        <span className='text-14px text-t-primary'>{label}</span>
        {extra}
      </div>
      {description && <div className='text-12px text-t-tertiary mt-2px'>{description}</div>}
    </div>
    <div className='flex items-center'>{children}</div>
  </div>
);

/**
 * Section header component
 */
const SectionHeader: React.FC<{ title: string; action?: React.ReactNode }> = ({ title, action }) => (
  <div className='flex items-center justify-between mb-12px'>
    <h3 className='text-14px font-500 text-t-primary m-0'>{title}</h3>
    {action}
  </div>
);

interface SlackConfigFormProps {
  pluginStatus: IChannelPluginStatus | null;
  modelSelection: GoogleModelSelection;
  onStatusChange: (status: IChannelPluginStatus | null) => void;
  onTokenChange?: (token: string) => void;
}

const SlackConfigForm: React.FC<SlackConfigFormProps> = ({
  pluginStatus,
  modelSelection,
  onStatusChange,
  onTokenChange,
}) => {
  const { t, i18n } = useTranslation();
  const localeKey = resolveLocaleKey(i18n?.language ?? 'en-US');

  const [botToken, setBotToken] = useState('');
  const [appToken, setAppToken] = useState('');
  const [testLoading, setTestLoading] = useState(false);
  const [tokenTested, setTokenTested] = useState(false);
  const [testedBotUsername, setTestedBotUsername] = useState<string | null>(null);
  const [pairingLoading, setPairingLoading] = useState(false);
  const [usersLoading, setUsersLoading] = useState(false);
  const [pendingPairings, setPendingPairings] = useState<IChannelPairingRequest[]>([]);
  const [authorizedUsers, setAuthorizedUsers] = useState<IChannelUser[]>([]);

  // Agent selection (used for Slack conversations)
  const [availableAgents, setAvailableAgents] = useState<Agent[]>([]);
  const [selectedAgent, setSelectedAgent] = useState<Agent | null>(null);
  const [hasBrokenSavedAgent, setHasBrokenSavedAgent] = useState(false);

  // Load pending pairings
  const loadPendingPairings = useCallback(async () => {
    setPairingLoading(true);
    try {
      const pairings = await channel.getPendingPairings.invoke();
      if (pairings) {
        setPendingPairings(pairings.filter((p) => p.platformType === 'slack'));
      }
    } catch (error) {
      console.error('[ChannelSettings] Failed to load pending pairings:', error);
    } finally {
      setPairingLoading(false);
    }
  }, []);

  // Load authorized users
  const loadAuthorizedUsers = useCallback(async () => {
    setUsersLoading(true);
    try {
      const users = await channel.getAuthorizedUsers.invoke();
      if (users) {
        setAuthorizedUsers(users.filter((u) => u.platformType === 'slack'));
      }
    } catch (error) {
      console.error('[ChannelSettings] Failed to load authorized users:', error);
    } finally {
      setUsersLoading(false);
    }
  }, []);

  // Initial load
  useEffect(() => {
    void loadPendingPairings();
    void loadAuthorizedUsers();
  }, [loadPendingPairings, loadAuthorizedUsers]);

  // Load available agents + saved selection
  useEffect(() => {
    const loadAgentsAndSelection = async () => {
      try {
        const [agentList, saved] = await Promise.all([
          agents.list.invoke(),
          channel.getPlatformSettings.invoke({ platform: 'slack' }),
        ]);

        setAvailableAgents(agentList);

        const selection = resolveChannelAgentSelection(saved.agent ?? undefined, agentList);
        const nextAgent =
          agentList.find((agent) => agent.id === selection.agentId) ||
          (!selection.hasBrokenSavedAgent ? getDefaultChannelAgent(agentList) : undefined) ||
          null;

        setHasBrokenSavedAgent(selection.hasBrokenSavedAgent);
        setSelectedAgent(nextAgent);
      } catch (error) {
        console.error('[SlackConfig] Failed to load agents:', error);
      }
    };

    void loadAgentsAndSelection();
  }, []);

  const persistSelectedAgent = async (agent: Agent) => {
    try {
      await channel.setAgentSetting.invoke({
        platform: 'slack',
        agent: buildChannelAgentBinding(agent),
      });
      Message.success(t('settings.agent.agentSwitched', 'Agent switched successfully'));
    } catch (error) {
      console.error('[SlackConfig] Failed to save agent:', error);
      Message.error(t('common.saveFailed', 'Failed to save'));
    }
  };

  // Listen for pairing requests
  useEffect(() => {
    const unsubscribe = channel.pairingRequested.on((request) => {
      if (request.platformType !== 'slack') return;
      setPendingPairings((prev) => {
        const exists = prev.some((p) => p.code === request.code);
        if (exists) return prev;
        return [request, ...prev];
      });
    });
    return () => unsubscribe();
  }, []);

  // Listen for user authorization
  useEffect(() => {
    const unsubscribe = channel.userAuthorized.on((user) => {
      setAuthorizedUsers((prev) => {
        const exists = prev.some((u) => u.id === user.id);
        if (exists) return prev;
        return [user, ...prev];
      });
      setPendingPairings((prev) => prev.filter((p) => p.platformUserId !== user.platformUserId));
    });
    return () => unsubscribe();
  }, []);

  // Test Slack connection (only validates the Bot Token via auth.test)
  const handleTestConnection = async () => {
    const trimmedBotToken = botToken.trim();
    const trimmedAppToken = appToken.trim();

    if (!trimmedBotToken) {
      Message.warning(t('settings.agent.slackBotTokenRequired', 'Please enter a Bot Token (xoxb-)'));
      return;
    }
    if (!trimmedAppToken) {
      Message.warning(t('settings.agent.slackAppTokenRequired', 'Please enter an App-Level Token (xapp-)'));
      return;
    }
    // Soft prefix validation (hard validation happens on the backend)
    if (!trimmedBotToken.startsWith('xoxb-')) {
      Message.warning(t('settings.agent.slackBotTokenPrefix', 'Bot Token should start with "xoxb-"'));
      return;
    }
    if (!trimmedAppToken.startsWith('xapp-')) {
      Message.warning(t('settings.agent.slackAppTokenPrefix', 'App-Level Token should start with "xapp-"'));
      return;
    }

    setTestLoading(true);
    setTokenTested(false);
    setTestedBotUsername(null);
    try {
      // testPlugin returns { success, bot_username?, error? } directly.
      // Slack test only validates the Bot Token (auth.test); the App-Level
      // Token is only needed when starting the Socket Mode connection.
      const result = await channel.testPlugin.invoke({
        plugin_id: 'slack',
        token: trimmedBotToken,
      });

      if (result.success) {
        setTokenTested(true);
        setTestedBotUsername(result.bot_username || null);
        Message.success(
          t('settings.agent.connectionSuccess', {
            defaultValue: 'Connected! Bot: @{{username}}',
            username: result.bot_username || 'unknown',
          })
        );

        // Auto-enable bot after successful test
        await handleAutoEnable();
      } else {
        setTokenTested(false);
        Message.error(result.error || t('settings.agent.connectionFailed', 'Connection failed'));
      }
    } catch (error: any) {
      setTokenTested(false);
      Message.error(error.message || t('settings.agent.connectionFailed', 'Connection failed'));
    } finally {
      setTestLoading(false);
    }
  };

  // Auto-enable plugin after successful test (submits both tokens)
  const handleAutoEnable = async () => {
    try {
      // enablePlugin returns void; success if no throw
      await channel.enablePlugin.invoke({
        plugin_id: 'slack',
        config: {
          credentials: {
            token: botToken.trim(),
            app_token: appToken.trim(),
          },
        },
      });

      Message.success(t('settings.agent.slackPluginEnabled', 'Slack bot enabled'));
      const plugins = await channel.getPluginStatus.invoke();
      if (plugins) {
        const slackPlugin = plugins.find((p) => p.type === 'slack');
        onStatusChange(slackPlugin || null);
      }
    } catch (error: unknown) {
      console.error('[ChannelSettings] Auto-enable failed:', error);
    }
  };

  // Reset token tested state when the bot token changes
  const handleBotTokenChange = (value: string) => {
    setBotToken(value);
    setTokenTested(false);
    setTestedBotUsername(null);
    onTokenChange?.(value);
  };

  // Reset token tested state when the app token changes
  const handleAppTokenChange = (value: string) => {
    setAppToken(value);
    setTokenTested(false);
    setTestedBotUsername(null);
  };

  // Approve pairing
  const handleApprovePairing = async (code: string) => {
    try {
      await channel.approvePairing.invoke({ code });
      Message.success(t('settings.agent.pairingApproved', 'Pairing approved'));
      await loadPendingPairings();
      await loadAuthorizedUsers();
    } catch (error: unknown) {
      Message.error(error instanceof Error ? error.message : String(error));
    }
  };

  // Reject pairing
  const handleRejectPairing = async (code: string) => {
    try {
      await channel.rejectPairing.invoke({ code });
      Message.info(t('settings.agent.pairingRejected', 'Pairing rejected'));
      await loadPendingPairings();
    } catch (error: unknown) {
      Message.error(error instanceof Error ? error.message : String(error));
    }
  };

  // Revoke user
  const handleRevokeUser = async (user_id: string) => {
    try {
      await channel.revokeUser.invoke({ user_id });
      Message.success(t('settings.agent.userRevoked', 'User access revoked'));
      await loadAuthorizedUsers();
    } catch (error: unknown) {
      Message.error(error instanceof Error ? error.message : String(error));
    }
  };

  // Copy to clipboard
  const copyToClipboard = (text: string) => {
    void navigator.clipboard.writeText(text);
    Message.success(t('common.copySuccess', 'Copied'));
  };

  // Format timestamp
  const formatTime = (timestamp: number) => {
    return formatDateTime(timestamp, i18n.language);
  };

  // Calculate remaining time
  const getRemainingMinutes = (expiresAt: number) => {
    return Math.max(0, Math.ceil((expiresAt - Date.now()) / 1000 / 60));
  };

  const showModelSelector = isAionrsAgent(selectedAgent);
  const agentOptions = availableAgents;
  const selectedAgentName = selectedAgent
    ? resolveAgentName(selectedAgent, localeKey, selectedAgent.name)
    : t('settings.agent.name', 'Agent');

  // Tokens are locked once there are authorized users (mirrors Telegram).
  const tokensLocked = authorizedUsers.length > 0;

  return (
    <div className='flex flex-col gap-24px'>
      {/* Bot Token (xoxb-) */}
      <PreferenceRow
        label={t('settings.agent.slackBotToken', 'Bot Token')}
        description={t(
          'settings.agent.slackBotTokenDesc',
          'Create a Slack app, install it to your workspace, then copy the Bot User OAuth Token (xoxb-).'
        )}
      >
        {tokensLocked ? (
          <Tooltip
            content={t(
              'settings.agent.tokenLocked',
              'Please close the Channel and delete all authorized users before modifying the configuration'
            )}
          >
            <span>
              <Input.Password
                value={botToken}
                onChange={handleBotTokenChange}
                placeholder={tokensLocked || pluginStatus?.hasToken ? '••••••••••••••••' : 'xoxb-...'}
                style={{ width: 260 }}
                visibilityToggle
                disabled={tokensLocked}
              />
            </span>
          </Tooltip>
        ) : (
          <Input.Password
            value={botToken}
            onChange={handleBotTokenChange}
            placeholder={tokensLocked || pluginStatus?.hasToken ? '••••••••••••••••' : 'xoxb-...'}
            style={{ width: 260 }}
            visibilityToggle
            disabled={tokensLocked}
          />
        )}
      </PreferenceRow>

      {/* App-Level Token (xapp-) */}
      <PreferenceRow
        label={t('settings.agent.slackAppToken', 'App-Level Token')}
        description={t(
          'settings.agent.slackAppTokenDesc',
          'Enable Socket Mode and generate an App-Level Token with the connections:write scope (xapp-).'
        )}
      >
        <div className='flex items-center gap-8px'>
          {tokensLocked ? (
            <Tooltip
              content={t(
                'settings.agent.tokenLocked',
                'Please close the Channel and delete all authorized users before modifying the configuration'
              )}
            >
              <span>
                <Input.Password
                  value={appToken}
                  onChange={handleAppTokenChange}
                  placeholder={tokensLocked || pluginStatus?.hasToken ? '••••••••••••••••' : 'xapp-...'}
                  style={{ width: 260 }}
                  visibilityToggle
                  disabled={tokensLocked}
                />
              </span>
            </Tooltip>
          ) : (
            <Input.Password
              value={appToken}
              onChange={handleAppTokenChange}
              placeholder={tokensLocked || pluginStatus?.hasToken ? '••••••••••••••••' : 'xapp-...'}
              style={{ width: 260 }}
              visibilityToggle
              disabled={tokensLocked}
            />
          )}
          {tokensLocked ? (
            <Tooltip
              content={t(
                'settings.agent.tokenLocked',
                'Please close the Channel and delete all authorized users before modifying the configuration'
              )}
            >
              <span>
                <Button type='outline' loading={testLoading} onClick={handleTestConnection} disabled={tokensLocked}>
                  {t('settings.agent.testConnection', 'Test')}
                </Button>
              </span>
            </Tooltip>
          ) : (
            <Button type='outline' loading={testLoading} onClick={handleTestConnection} disabled={tokensLocked}>
              {t('settings.agent.testConnection', 'Test')}
            </Button>
          )}
        </div>
      </PreferenceRow>

      {/* Agent Selection */}
      <div className='flex flex-col gap-8px'>
        <PreferenceRow
          label={t('settings.agent.name', 'Agent')}
          description={
            <div className='flex flex-col gap-4px'>
              <span>{t('settings.agent.agentDescSlack', 'Used for Slack conversations')}</span>
              {hasBrokenSavedAgent && (
                <span className='text-orange-6'>
                  {t(
                    'conversation.agentError.codes.TEAM_ASSISTANT_NOT_FOUND.title',
                    'The selected agent is no longer available'
                  )}
                </span>
              )}
            </div>
          }
        >
          <Dropdown
            trigger='click'
            position='br'
            droplist={
              <Menu selectedKeys={selectedAgent ? [selectedAgent.id] : []}>
                {agentOptions.map((agent) => {
                  const agentName = resolveAgentName(agent, localeKey, agent.name);
                  return (
                    <Menu.Item
                      key={agent.id}
                      onClick={() => {
                        if (agent.id === selectedAgent?.id) return;

                        setHasBrokenSavedAgent(false);
                        setSelectedAgent(agent);
                        void persistSelectedAgent(agent);

                        if (isAionrsAgent(agent)) {
                          const providers = modelSelection.providers;
                          const savedProviderExists =
                            modelSelection.current_model?.id &&
                            providers.some((p) => p.id === modelSelection.current_model?.id);
                          if (!savedProviderExists && providers.length > 0) {
                            const firstProvider = providers[0];
                            if (firstProvider.id && firstModelId(firstProvider)) {
                              void modelSelection.handleSelectModel(firstProvider, firstModelId(firstProvider));
                            }
                          }
                        }
                      }}
                    >
                      {agentName}
                    </Menu.Item>
                  );
                })}
              </Menu>
            }
          >
            <Button type='secondary' className='min-w-160px flex items-center justify-between gap-8px'>
              <span className='truncate'>{selectedAgentName}</span>
              <Down theme='outline' size={14} />
            </Button>
          </Dropdown>
        </PreferenceRow>
      </div>

      {/* Default Model Selection */}
      <PreferenceRow
        label={t('settings.agent.defaultModel', 'Default Model')}
        description={t('settings.agent.defaultModelDescSlack', 'Model used for Slack conversations')}
      >
        <GoogleModelSelector
          selection={showModelSelector ? modelSelection : undefined}
          disabled={!showModelSelector}
          label={
            !showModelSelector
              ? t('settings.agent.autoFollowCliModel', 'Automatically follow the model when CLI is running')
              : undefined
          }
          variant='settings'
        />
      </PreferenceRow>

      {/* Next Steps Guide - show when bot is enabled and no authorized users yet */}
      {pluginStatus?.enabled && pluginStatus?.connected && authorizedUsers.length === 0 && (
        <div className='bg-blue-50 dark:bg-blue-900/20 rd-12px p-16px border border-blue-200 dark:border-blue-800'>
          <SectionHeader title={t('settings.agent.nextSteps', 'Next Steps')} />
          <div className='text-14px text-t-secondary space-y-8px'>
            <p className='m-0'>
              <strong>1.</strong>{' '}
              {t('settings.agent.slackStep1', 'Open Slack and mention your bot in a channel, or send it a DM')}
              {pluginStatus.botUsername && (
                <span className='ms-4px'>
                  <code className='bg-fill-2 px-6px py-2px rd-4px'>@{pluginStatus.botUsername}</code>
                </span>
              )}
            </p>
            <p className='m-0'>
              <strong>2.</strong>{' '}
              {t('settings.agent.slackStep2', 'Send any message or @mention to initiate pairing')}
            </p>
            <p className='m-0'>
              <strong>3.</strong>{' '}
              {t(
                'settings.agent.slackStep3',
                'A pairing request will appear below. Click "Approve" to authorize the user.'
              )}
            </p>
            <p className='m-0'>
              <strong>4.</strong>{' '}
              {t('settings.agent.slackStep4', 'Once approved, you can start chatting with the agent in Slack!')}
            </p>
          </div>
        </div>
      )}

      {/* Pending Pairings - show when bot is enabled and no authorized users yet */}
      {pluginStatus?.enabled && authorizedUsers.length === 0 && (
        <div className='bg-fill-1 rd-12px pt-16px pe-16px pb-16px ps-0'>
          <SectionHeader
            title={t('settings.agent.pendingPairings', 'Pending Pairing Requests')}
            action={
              <Button
                size='mini'
                type='text'
                icon={<Refresh size={14} />}
                loading={pairingLoading}
                onClick={loadPendingPairings}
              >
                {t('common.refresh', 'Refresh')}
              </Button>
            }
          />

          {pairingLoading ? (
            <div className='flex justify-center py-24px'>
              <Spin />
            </div>
          ) : pendingPairings.length === 0 ? (
            <Empty description={t('settings.agent.noPendingPairings', 'No pending pairing requests')} />
          ) : (
            <div className='flex flex-col gap-12px'>
              {pendingPairings.map((pairing) => (
                <div key={pairing.code} className='flex items-center justify-between bg-fill-2 rd-8px p-12px'>
                  <div className='flex-1'>
                    <div className='flex items-center gap-8px'>
                      <span className='text-14px font-500 text-t-primary'>
                        {pairing.display_name || t('settings.agent.unknownUser', 'Unknown User')}
                      </span>
                      <Tooltip content={t('settings.agent.copyCode', 'Copy pairing code')}>
                        <Button
                          type='text'
                          size='mini'
                          icon={<Copy size={14} />}
                          onClick={() => copyToClipboard(pairing.code)}
                        />
                      </Tooltip>
                    </div>
                    <div className='text-12px text-t-tertiary mt-4px'>
                      {t('settings.agent.pairingCode', 'Code')}:{' '}
                      <code className='bg-fill-3 px-4px rd-2px'>{pairing.code}</code>
                      <span className='mx-8px'>|</span>
                      {t('settings.agent.expiresInMinutes', { count: getRemainingMinutes(pairing.expiresAt) })}
                    </div>
                  </div>
                  <div className='flex items-center gap-8px'>
                    <Button
                      type='primary'
                      size='small'
                      icon={<CheckOne size={14} />}
                      onClick={() => handleApprovePairing(pairing.code)}
                    >
                      {t('settings.agent.approve', 'Approve')}
                    </Button>
                    <Button
                      type='secondary'
                      size='small'
                      status='danger'
                      icon={<CloseOne size={14} />}
                      onClick={() => handleRejectPairing(pairing.code)}
                    >
                      {t('settings.agent.reject', 'Reject')}
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Authorized Users - show when there are authorized users */}
      {authorizedUsers.length > 0 && (
        <div className='bg-fill-1 rd-12px pt-16px pe-16px pb-16px ps-0'>
          <SectionHeader
            title={t('settings.agent.authorizedUsers', 'Authorized Users')}
            action={
              <Button
                size='mini'
                type='text'
                icon={<Refresh size={14} />}
                loading={usersLoading}
                onClick={loadAuthorizedUsers}
              >
                {t('common.refresh', 'Refresh')}
              </Button>
            }
          />

          {usersLoading ? (
            <div className='flex justify-center py-24px'>
              <Spin />
            </div>
          ) : authorizedUsers.length === 0 ? (
            <Empty description={t('settings.agent.noAuthorizedUsers', 'No authorized users yet')} />
          ) : (
            <div className='flex flex-col gap-12px'>
              {authorizedUsers.map((user) => (
                <div key={user.id} className='flex items-center justify-between bg-fill-2 rd-8px p-12px'>
                  <div className='flex-1'>
                    <div className='text-14px font-500 text-t-primary'>
                      {user.display_name || t('settings.agent.unknownUser', 'Unknown User')}
                    </div>
                    <div className='text-12px text-t-tertiary mt-4px'>
                      {t('settings.agent.platform', 'Platform')}: {user.platformType}
                      <span className='mx-8px'>|</span>
                      {t('settings.agent.authorizedAt', 'Authorized')}: {formatTime(user.authorizedAt)}
                    </div>
                  </div>
                  <Tooltip content={t('settings.agent.revokeAccess', 'Revoke access')}>
                    <Button
                      type='text'
                      status='danger'
                      size='small'
                      icon={<Delete size={16} />}
                      onClick={() => handleRevokeUser(user.id)}
                    />
                  </Tooltip>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
};

export default SlackConfigForm;
