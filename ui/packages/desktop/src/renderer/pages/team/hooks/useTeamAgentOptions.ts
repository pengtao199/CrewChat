import { useMemo } from 'react';
import { resolveLocaleKey } from '@/common/utils';
import { useConversationAgents } from '@renderer/pages/conversation/hooks/useConversationAgents';
import {
  agentToOption,
  filterTeamSupportedAgents,
  type TeamAgentOption,
} from '../components/agentSelectUtils';

export function useTeamAgentOptions(locale = 'en-US'): {
  agents: TeamAgentOption[];
  loading: boolean;
  error: unknown;
  filterByQuery: (query: string) => TeamAgentOption[];
} {
  const result = useConversationAgents() as {
    presetAgents?: Parameters<typeof agentToOption>[0][];
    loading?: boolean;
    isLoading?: boolean;
    error?: unknown;
  };
  const localeKey = resolveLocaleKey(locale);
  const agents = useMemo(
    () =>
      filterTeamSupportedAgents(
        (result.presetAgents ?? []).map((agent) => agentToOption(agent, localeKey))
      ),
    [result.presetAgents, localeKey]
  );
  const filterByQuery = useMemo(
    () => (query: string) => {
      const q = query.trim().toLowerCase();
      if (!q) return agents;
      return agents.filter((agent) => agent.name.toLowerCase().includes(q));
    },
    [agents]
  );

  return {
    agents,
    loading: Boolean(result.loading ?? result.isLoading),
    error: result.error,
    filterByQuery,
  };
}
