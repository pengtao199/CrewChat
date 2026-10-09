import { resolveExtensionAssetUrl } from '@/renderer/utils/platform';
import { isBackendRelativeAssetPath, isLikelyLocalFilePath } from '@/renderer/utils/model/agentAvatar';
import type { AgentListItem, AvailableBackend } from './types';
import type { ManagedAgent } from '@/renderer/utils/model/agentTypes';

export type AgentListFilter = 'all' | 'enabled' | 'disabled' | 'builtin' | 'user';

/**
 * Source tag shown next to an agent in the settings list.
 *
 * - `builtin` → "Built-in" tag
 * - `user` → "Custom" tag
 * - `generated` (agent-generated) → "CLI" tag, matching the product terminology.
 */
export type AgentSourceTag = 'builtin' | 'custom' | 'cli' | null;

export const resolveAgentSourceTag = (source: string): AgentSourceTag => {
  if (source === 'builtin') return 'builtin';
  if (source === 'generated') return 'cli';
  return 'custom';
};

/**
 * Check if a string is an emoji (simple check for common emoji patterns).
 */
export const isEmoji = (str: string): boolean => {
  if (!str) return false;
  const emojiRegex = /^(?:\p{Emoji_Presentation}|\p{Emoji}️)(?:‍(?:\p{Emoji_Presentation}|\p{Emoji}️))*$/u;
  return emojiRegex.test(str);
};

/**
 * Resolve an avatar string to an image src URL, or undefined if it is not an image.
 */
export const resolveAvatarImageSrc = (avatar: string | undefined): string | undefined => {
  const value = avatar?.trim();
  if (!value) return undefined;

  if (isLikelyLocalFilePath(value)) return undefined;
  if (value.startsWith('/') && !isBackendRelativeAssetPath(value)) return undefined;

  const resolved = resolveExtensionAssetUrl(value) || value;
  const isImage = /\.(svg|png|jpe?g|webp|gif)$/i.test(resolved) || /^(https?:|file:\/\/|data:|\/)/i.test(resolved);
  return isImage ? resolved : undefined;
};

/**
 * Sort agents by sortOrder. The backend already returns sorted lists; this
 * is a deterministic fallback for local reorder operations.
 */
export const sortAgents = (list: AgentListItem[]): AgentListItem[] =>
  [...list].toSorted((a, b) => a.sort_order - b.sort_order);

/**
 * Reorder agents by moving `activeId` to the position of `overId`.
 */
export const reorderAgentList = (
  agents: AgentListItem[],
  activeId: string,
  overId: string
): AgentListItem[] => {
  const activeIndex = agents.findIndex((agent) => agent.id === activeId);
  const overIndex = agents.findIndex((agent) => agent.id === overId);
  if (activeIndex < 0 || overIndex < 0 || activeIndex === overIndex) {
    return agents;
  }

  const nextAgents = [...agents];
  const [movedAgent] = nextAgents.splice(activeIndex, 1);
  nextAgents.splice(overIndex, 0, movedAgent);
  return nextAgents;
};

/**
 * Apply search and management filter to agent list.
 */
export const filterAgents = (
  agents: AgentListItem[],
  query: string,
  filter: AgentListFilter,
  localeKey: string
): AgentListItem[] => {
  const normalizedQuery = query.trim().toLowerCase();

  return agents.filter((agent) => {
    if (normalizedQuery) {
      const searchableText = [
        agent.name_i18n?.[localeKey] || agent.name,
        agent.description_i18n?.[localeKey] || agent.description || '',
      ]
        .join(' ')
        .toLowerCase();

      if (!searchableText.includes(normalizedQuery)) return false;
    }

    switch (filter) {
      case 'enabled':
        return agent.enabled !== false;
      case 'disabled':
        return agent.enabled === false;
      case 'builtin':
        return agent.source === 'builtin';
      case 'user':
        return agent.source === 'user';
      case 'all':
      default:
        return true;
    }
  });
};

/**
 * Split agents into enabled and disabled groups while preserving order.
 */
export const groupAgentsByEnabled = (agents: AgentListItem[]) => ({
  enabledAgents: agents.filter((agent) => agent.enabled !== false),
  disabledAgents: agents.filter((agent) => agent.enabled === false),
});

export type AgentEnabledFilter = 'all' | 'enabled' | 'disabled';

/** Apply the enabled/disabled dropdown filter used by the "My Agents" tab. */
export const filterByEnabled = (
  agents: AgentListItem[],
  filter: AgentEnabledFilter
): AgentListItem[] => {
  switch (filter) {
    case 'enabled':
      return agents.filter((agent) => agent.enabled !== false);
    case 'disabled':
      return agents.filter((agent) => agent.enabled === false);
    default:
      return agents;
  }
};

const byAgentSortOrder = (a: AgentListItem, b: AgentListItem) => a.sort_order - b.sort_order;

/// Agent types the agent editor can drive.
///
/// This is a whitelist, so every new `AgentType` is invisible here until it is
/// added — which is how Antigravity ended up missing from the Agent dropdown
/// AND showing its raw id (`a9f3c21e`) instead of its name in the editor. Both
/// symptoms are this one line: an agent filtered out here never reaches
/// `availableBackends`, and `Select` with no matching Option falls back to
/// rendering the bare value.
///
/// `antigravity` is a first-class agent type rather than an `acp` backend
/// because agy is a direct-CLI integration (one process per turn), the same
/// reason it has its own variant in `AgentType`.
///
/// Excluded on purpose: `gemini` and `codex` are legacy read-only variants kept
/// so historical rows stay readable, and `remote` / `nanobot` /
/// `openclaw-gateway` are not editor-driven.
const ASSISTANT_EDITOR_AGENT_TYPES = new Set(['acp', 'aionrs', 'antigravity']);

const isAgentEditorAgent = (agent: ManagedAgent): boolean => ASSISTANT_EDITOR_AGENT_TYPES.has(agent.agent_type);

/**
 * Split the user's own agents into the two "My Agents" groups, each
 * sorted by sort_order. Bare CLI agents come first (fixed), then
 * user-created. Official (builtin) agents live in the other tab and are
 * excluded here.
 */
export const groupMyAgents = (agents: AgentListItem[]) => {
  return {
    // 'generated' == a bare CLI agent auto-created from a local CLI tool.
    cliAgents: agents.filter((a) => a.source === 'generated').toSorted(byAgentSortOrder),
    createdAgents: agents.filter((a) => a.source === 'user').toSorted(byAgentSortOrder),
  };
};

/**
 * Narrow the editor's agent list by a search query.
 *
 * Matches the id and runtime key as well as the display name: a user who knows
 * an agent as "codex" or "antigravity" should find it without knowing what the
 * row is labelled.
 *
 * Lives here rather than inline in the component so the matching rule is
 * testable on its own — driving an Arco popup to assert which rows survive is
 * both slower and less precise.
 */
export const filterAgentEditorBackends = (backends: AvailableBackend[], query: string): AvailableBackend[] => {
  const keyword = query.trim().toLowerCase();
  if (!keyword) return backends;
  return backends.filter((option) =>
    [option.name, option.id, option.runtimeKey].some((field) => field?.toLowerCase().includes(keyword))
  );
};

export const buildAgentEditorBackends = (
  agents: ManagedAgent[],
  localeKey: string,
  currentAgentId?: string
): AvailableBackend[] => {
  const backendMap = new Map<string, AvailableBackend>();

  for (const agent of agents) {
    if (!isAgentEditorAgent(agent)) {
      continue;
    }

    const agentId = agent.id?.trim() || '';
    const status = agent.status;
    const isCurrent = Boolean(currentAgentId && agentId === currentAgentId);
    const isSelectable = agent.enabled !== false && (status === 'online' || status === 'unchecked');
    if (!agentId || backendMap.has(agentId) || (!isSelectable && !isCurrent)) {
      continue;
    }

    const runtimeKey = (agent.backend || agent.agent_type || '').trim();
    if (!runtimeKey) {
      continue;
    }

    backendMap.set(agentId, {
      id: agentId,
      name: agent.name_i18n?.[localeKey] || agent.name,
      runtimeKey,
      isExtension: agent.isExtension,
      // Prefer the agent's own avatar/icon; the dropdown falls back to the logo
      // catalog (keyed by runtimeKey) when this is empty.
      icon: agent.avatar || agent.icon,
      customAgentId: agent.custom_agent_id,
      modelOptions: [],
    });
  }

  return [...backendMap.values()];
};
