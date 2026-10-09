import { ipcBridge } from '@/common';
import { httpRequest } from '@/common/adapter/httpBridge';
import { agentRuntimeKey, type AgentAgent } from '@/common/types/agent/agentTypes';
import type { CreateProviderRequest, ModelTag } from '@/common/types/provider/providerApi';

import type { ConfigKey } from './configKeys';
import type { ILegacyConfigStorageRefer, IMcpServer } from './storage';
import { BUILTIN_IMAGE_GEN_ID, BUILTIN_IMAGE_GEN_LEGACY_NAMES, BUILTIN_IMAGE_GEN_NAME } from './storage';

export type ConfigFile = {
  get<K extends keyof ILegacyConfigStorageRefer>(key: K): Promise<ILegacyConfigStorageRefer[K]>;
  set<K extends keyof ILegacyConfigStorageRefer>(key: K, value: ILegacyConfigStorageRefer[K]): Promise<unknown>;
};

const LEGACY_MCP_CONFIG_KEY = 'mcp.config' as const;
const LEGACY_CHANNEL_KEYS = [
  'agent.telegram.defaultModel',
  'agent.telegram.agent',
  'agent.lark.defaultModel',
  'agent.lark.agent',
  'agent.dingtalk.defaultModel',
  'agent.dingtalk.agent',
  'agent.weixin.defaultModel',
  'agent.weixin.agent',
  'agent.wecom.defaultModel',
  'agent.wecom.agent',
] as const;

const LEGACY_CHANNEL_PLATFORMS = ['telegram', 'lark', 'dingtalk', 'weixin', 'wecom'] as const;

type LegacyChannelConfigKey = (typeof LEGACY_CHANNEL_KEYS)[number];
type LegacyChannelPlatform = (typeof LEGACY_CHANNEL_PLATFORMS)[number];
type LegacyBusinessConfigKey =
  | 'google.config'
  | 'acp.promptTimeout'
  | 'acp.agentIdleTimeout'
  | 'mcp.config'
  | 'tools.imageGenerationModel'
  | 'tools.speechToText';
type LegacyConfigKey = ConfigKey | LegacyBusinessConfigKey | LegacyChannelConfigKey;

type LegacyMcpConfigFile = ConfigFile & {
  get(key: typeof LEGACY_MCP_CONFIG_KEY): Promise<unknown>;
  set(key: typeof LEGACY_MCP_CONFIG_KEY, value: unknown): Promise<unknown>;
};

type LegacyChannelConfigFile = ConfigFile & {
  get(key: LegacyConfigKey): Promise<unknown>;
};

type ChannelAgentCandidate = {
  id: string;
  source: string;
  agent_id: string;
  agent?: AgentAgent;
};

const ALL_LEGACY_KEYS: LegacyConfigKey[] = [
  'acp.promptTimeout',
  'acp.agentIdleTimeout',
  'language',
  'ui.zoomFactor',
  'ui.fontSize.chat',
  'ui.fontSize.markdown',
  'ui.fontSize.code',
  'webui.desktop.enabled',
  'webui.desktop.allowRemote',
  'webui.desktop.port',
  'tools.imageGenerationModel',
  'tools.speechToText',
  'workspace.pasteConfirm',
  'upload.saveToWorkspace',
  'skillsMarket.enabled',
  'pet.enabled',
  'pet.size',
  'pet.dnd',
  'pet.confirmEnabled',
  'system.closeToTray',
  'system.notificationEnabled',
  'system.cronNotificationEnabled',
  'system.keepAwake',
];

export async function migrateConfigStorage(configFile: ConfigFile): Promise<void> {
  const legacyConfigFile = configFile as LegacyChannelConfigFile;
  const entries: Record<string, unknown> = {};

  const legacyEntries = await Promise.all(
    ALL_LEGACY_KEYS.map(async (key) => {
      try {
        const value = LEGACY_CHANNEL_KEYS.includes(key as LegacyChannelConfigKey)
          ? await legacyConfigFile.get(key as LegacyChannelConfigKey)
          : await legacyConfigFile.get(key as LegacyConfigKey);
        return [key, value] as const;
      } catch {
        return [key, undefined] as const;
      }
    })
  );

  for (const [key, value] of legacyEntries) {
    if (value !== undefined && value !== null) {
      entries[key] = value;
    }
  }

  if (Object.keys(entries).length === 0) {
    console.info('[Migration] configStorage migration skipped — no legacy keys found');
  } else {
    // Merge strategy: only write keys that don't already exist in the backend DB.
    // This prevents overwriting user's runtime changes on repeated migrations.
    const existing = await fetchExistingClientKeys();
    const newEntries: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(entries)) {
      if (!(key in existing)) {
        newEntries[key] = value;
      }
    }

    if (Object.keys(newEntries).length > 0) {
      await setBackendClientPreferences(newEntries);
      console.info(
        '[Migration] configStorage migration completed, migrated %d/%d keys (skipped %d existing)',
        Object.keys(newEntries).length,
        Object.keys(entries).length,
        Object.keys(entries).length - Object.keys(newEntries).length
      );
    } else {
      console.info(
        '[Migration] configStorage migration skipped — all %d keys already exist in backend',
        Object.keys(entries).length
      );
    }
  }

  await migrateLegacyChannelSettings(legacyConfigFile);
}

export async function migrateLegacyMcpConfigToDb(configFile: ConfigFile): Promise<void> {
  const legacyConfigFile = configFile as LegacyMcpConfigFile;
  const backendPrefs = await fetchExistingClientKeys();
  const backendLegacy = backendPrefs[LEGACY_MCP_CONFIG_KEY];
  const fileLegacy = await legacyConfigFile.get(LEGACY_MCP_CONFIG_KEY).catch((): undefined => undefined);
  const legacyServers = Array.isArray(backendLegacy) ? backendLegacy : Array.isArray(fileLegacy) ? fileLegacy : [];

  if (legacyServers.length === 0) {
    console.info('[Migration] legacy MCP migration skipped — no legacy servers found');
    return;
  }

  const existing = await ipcBridge.mcpService.listServers.invoke();
  const existingNames = new Set((existing ?? []).map((server) => server.name));
  const importableServers = legacyServers.filter(isImportableMcpServer).map(normalizeLegacyMcpServer);
  const missing = importableServers.filter((server) => !existingNames.has(server.name));

  console.info(
    '[Migration] legacy MCP migration found %d servers, importing %d missing, skipping %d existing',
    legacyServers.length,
    missing.length,
    legacyServers.length - missing.length
  );

  if (missing.length > 0) {
    await ipcBridge.mcpService.batchImportServers.invoke({ servers: missing });
  }

  await setBackendClientPreferences({ [LEGACY_MCP_CONFIG_KEY]: null });
  await legacyConfigFile.set(LEGACY_MCP_CONFIG_KEY, []);
}

function isImportableMcpServer(
  server: unknown
): server is Partial<IMcpServer> & Pick<IMcpServer, 'name' | 'transport'> {
  if (!server || typeof server !== 'object') return false;
  const candidate = server as Partial<IMcpServer>;
  return typeof candidate.name === 'string' && candidate.name.length > 0 && Boolean(candidate.transport);
}

function normalizeLegacyMcpServer(
  server: Partial<IMcpServer> & Pick<IMcpServer, 'name' | 'transport'>
): Partial<IMcpServer> & Pick<IMcpServer, 'name' | 'transport'> {
  const isLegacyImageGen =
    server.builtin === true &&
    (server.id === BUILTIN_IMAGE_GEN_ID ||
      server.name === BUILTIN_IMAGE_GEN_NAME ||
      BUILTIN_IMAGE_GEN_LEGACY_NAMES.includes(server.name as (typeof BUILTIN_IMAGE_GEN_LEGACY_NAMES)[number]));

  if (!isLegacyImageGen) return server;

  return {
    ...server,
    name: BUILTIN_IMAGE_GEN_NAME,
    builtin: true,
  };
}

async function migrateLegacyChannelSettings(configFile: LegacyChannelConfigFile): Promise<void> {
  const agents: ChannelAgentCandidate[] = await ipcBridge.agents.list
    .invoke()
    .catch((): ChannelAgentCandidate[] => []);
  if (!Array.isArray(agents) || agents.length === 0) {
    console.info('[Migration] channel settings migration skipped — no agents available');
    return;
  }

  for (const platform of LEGACY_CHANNEL_PLATFORMS) {
    const agentKey = `agent.${platform}.agent` as const;
    const defaultModelKey = `agent.${platform}.defaultModel` as const;

    const [legacyAgent, legacyDefaultModel, currentSettings] = await Promise.all([
      configFile.get(agentKey).catch((): undefined => undefined),
      configFile.get(defaultModelKey).catch((): undefined => undefined),
      ipcBridge.channel.getPlatformSettings.invoke({ platform }).catch((): null => null),
    ]);

    const nextAgentId =
      currentSettings?.agent?.agent_id ?? resolveLegacyChannelAgentId(legacyAgent, agents);

    let changed = false;

    if (!currentSettings?.agent?.agent_id && nextAgentId) {
      await ipcBridge.channel.setAgentSetting.invoke({
        platform,
        agent: { agent_id: nextAgentId },
      });
      changed = true;
    }

    const nextDefaultModel =
      currentSettings?.default_model ?? normalizeLegacyChannelDefaultModelSetting(legacyDefaultModel);

    if (!currentSettings?.default_model && nextDefaultModel) {
      await ipcBridge.channel.setDefaultModelSetting.invoke({
        platform,
        default_model: nextDefaultModel,
      });
      changed = true;
    }

    if (changed) {
      await ipcBridge.channel.syncChannelSettings.invoke({ platform });
    }
  }
}

function normalizeLegacyChannelDefaultModelSetting(value: unknown):
  | {
      id: string;
      use_model: string;
    }
  | undefined {
  if (!value || typeof value !== 'object') return undefined;
  const candidate = value as Record<string, unknown>;
  return typeof candidate.id === 'string' && typeof candidate.use_model === 'string'
    ? {
        id: candidate.id,
        use_model: candidate.use_model,
      }
    : undefined;
}

function resolveLegacyChannelAgentId(saved: unknown, agents: ChannelAgentCandidate[]): string | undefined {
  if (!saved) return undefined;

  if (typeof saved === 'string') {
    return findAgentIdByBackend(saved, agents);
  }

  if (typeof saved !== 'object') return undefined;

  const record = saved as Record<string, unknown>;
  const explicitAgentId =
    (typeof record.agent_id === 'string' ? record.agent_id : undefined) ||
    (typeof record.custom_agent_id === 'string' ? record.custom_agent_id : undefined);

  if (explicitAgentId && agents.some((agent) => agent.id === explicitAgentId)) {
    return explicitAgentId;
  }

  const backend =
    (typeof record.backend === 'string' ? record.backend : undefined) ||
    (typeof record.agent_type === 'string' ? record.agent_type : undefined);

  return findAgentIdByBackend(backend, agents);
}

function findAgentIdByBackend(
  backend: string | undefined,
  agents: ChannelAgentCandidate[]
): string | undefined {
  if (!backend) return undefined;

  return (
    agents.find((agent) => agent.source === 'generated' && agentRuntimeKey(agent) === backend)
      ?.id || agents.find((agent) => agentRuntimeKey(agent) === backend)?.id
  );
}

// ---------------------------------------------------------------------------
// Provider migration — reads legacy `model.config` from local config file
// and writes each entry to the backend via `POST /api/providers`.
// ---------------------------------------------------------------------------

type LegacyModelHealth = Record<
  string,
  {
    status: 'unknown' | 'healthy' | 'unhealthy';
    lastCheck?: number;
    latency?: number;
    error?: string;
  }
>;

type LegacyBedrockConfig = {
  authMethod: 'accessKey' | 'profile';
  region: string;
  accessKeyId?: string;
  secretAccessKey?: string;
  profile?: string;
};

type LegacyProvider = {
  id: string;
  platform: string;
  name: string;
  baseUrl: string;
  apiKey: string;
  model: string[];
  enabled?: boolean;
  /**
   * 以下字段在新的 provider 模型里已不存在，迁移时按下面的规则翻译或丢弃：
   * - capabilities/contextLimit → 每个模型的 tags / context_limit
   * - modelEnabled → 每个模型的 enabled
   * - modelHealth / modelProtocols / bedrockConfig → 后端不存储，直接丢弃
   */
  capabilities?: string[];
  contextLimit?: number;
  modelProtocols?: Record<string, string>;
  modelEnabled?: Record<string, boolean>;
  modelHealth?: LegacyModelHealth;
  bedrockConfig?: LegacyBedrockConfig;
};

/** 旧的 `capabilities` 是能力词表，新模型里对应的是模型标签。 */
const LEGACY_CAPABILITY_TO_TAG: Record<string, ModelTag> = {
  vision: 'vision',
  image: 'vision',
  'image-input': 'vision',
  reasoning: 'reasoning',
  thinking: 'reasoning',
  tool_use: 'tool_use',
  tools: 'tool_use',
  long_context: 'long_context',
  fast: 'fast',
  free: 'free',
  text: 'text',
};

/**
 * Local config file key that records "the legacy → backend provider migration
 * has already completed once on this machine". Once set, {@link migrateProviders}
 * is a no-op for the remaining lifetime of this install — even if the user
 * later deletes a provider through the UI (the deletion goes to the backend
 * DB; the legacy `model.config` on disk is left intact for downgrade safety
 * and must NOT be replayed). See ELECTRON-1KT.
 */
const PROVIDERS_MIGRATION_FLAG = 'migration.providersMigrated_v1' as const;

export async function migrateProviders(configFile: ConfigFile): Promise<void> {
  // Idempotency guard: once the flag is set, never replay legacy providers.
  // Without this, deletions made by the user post-migration would be silently
  // undone on every launch as the legacy `model.config` is still on disk
  // (kept on purpose so the user can downgrade to a pre-backend Electron build).
  let alreadyMigrated = false;
  try {
    alreadyMigrated = Boolean(await configFile.get(PROVIDERS_MIGRATION_FLAG));
  } catch {
    // Flag missing or read failed — proceed as if first run; we'll set the
    // flag at the end of a successful pass.
  }
  if (alreadyMigrated) {
    console.info('[Migration] providers migration skipped — completion flag already set');
    return;
  }

  let legacyProviders: LegacyProvider[];
  try {
    legacyProviders = (await configFile.get(
      'model.config' as keyof ILegacyConfigStorageRefer
    )) as unknown as LegacyProvider[];
  } catch (err) {
    console.info('[Migration] providers migration skipped — no model.config in config file', err);
    // Nothing to migrate ever again on this machine — flag it so future launches
    // skip the read entirely and we don't risk a stray legacy file appearing later
    // (e.g. via a settings restore from backup) re-injecting deleted providers.
    await markProvidersMigrationDone(configFile);
    return;
  }

  if (!legacyProviders || !Array.isArray(legacyProviders) || legacyProviders.length === 0) {
    console.info('[Migration] providers migration skipped — model.config is empty or invalid');
    await markProvidersMigrationDone(configFile);
    return;
  }

  const existing = await ipcBridge.mode.listProviders.invoke();
  const existingIds = new Set((existing ?? []).map((p: { id: string }) => p.id));

  const newProviders = legacyProviders.filter((p) => !existingIds.has(p.id));
  if (newProviders.length === 0) {
    console.info(
      '[Migration] providers migration skipped — all %d legacy providers already exist in backend',
      legacyProviders.length
    );
    // Backend already has every legacy id — migration is effectively done.
    await markProvidersMigrationDone(configFile);
    return;
  }

  console.info(
    '[Migration] found %d new legacy providers to migrate (skipping %d existing)',
    newProviders.length,
    legacyProviders.length - newProviders.length
  );

  const requests = newProviders.map((legacy) => ({
    legacy,
    req: {
      id: legacy.id,
      name: legacy.name,
      base_url: legacy.baseUrl,
      api_key: legacy.apiKey,
      // 旧的裸字符串模型列表 → 新的模型对象：把供应商级的能力词表翻译成标签，
      // 把供应商级的 context_limit 与启停状态摊到每个模型上。
      models: legacy.model.map((id) => ({
        id,
        display_name: '',
        label: id,
        tags: (legacy.capabilities ?? []).map((c) => LEGACY_CAPABILITY_TO_TAG[c]).filter((x): x is ModelTag => Boolean(x)),
        context_limit: legacy.contextLimit ?? null,
        enabled: legacy.modelEnabled?.[id] !== false,
        protocol: null as string | null,
        capabilities: {},
        price: {},
      })),
      enabled: legacy.enabled ?? true,
      is_full_url: false,
    } satisfies CreateProviderRequest,
  }));

  const results = await Promise.allSettled(requests.map(({ req }) => ipcBridge.mode.createProvider.invoke(req)));
  let migrated = 0;
  let failed = 0;
  results.forEach((result, index) => {
    if (result.status === 'fulfilled') {
      migrated += 1;
      return;
    }
    failed += 1;
    console.warn('[Migration] failed to create provider %s:', requests[index].legacy.id, result.reason);
  });

  console.info('[Migration] providers migration completed, migrated %d/%d providers', migrated, newProviders.length);

  // Only set the completion flag on a fully clean pass. A partial failure
  // (e.g. backend returned 5xx for one provider) leaves the flag unset so the
  // next launch retries just the still-missing rows; that retry is safe
  // because the existing-by-id filter above already skips any provider the
  // backend has accepted in the meantime.
  if (failed === 0) {
    await markProvidersMigrationDone(configFile);
  }
}

async function markProvidersMigrationDone(configFile: ConfigFile): Promise<void> {
  try {
    await configFile.set(PROVIDERS_MIGRATION_FLAG, true);
  } catch (err) {
    // Failure to persist the flag is non-fatal — worst case the migration
    // re-runs next launch and the existing-by-id filter makes it a no-op.
    console.warn('[Migration] failed to persist providers migration flag', err);
  }
}

type BackendClientPreferences = Record<string, unknown>;

async function fetchExistingClientKeys(): Promise<Record<string, unknown>> {
  try {
    return (await httpRequest<Record<string, unknown>>('GET', '/api/settings/client')) || {};
  } catch {
    return {};
  }
}

async function setBackendClientPreferences(entries: BackendClientPreferences): Promise<void> {
  await httpRequest<void>('PUT', '/api/settings/client', entries);
}
