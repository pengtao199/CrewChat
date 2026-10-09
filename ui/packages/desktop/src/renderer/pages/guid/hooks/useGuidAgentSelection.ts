/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { agentRuntimeKey, isAionrsAgent, type Agent } from '@/common/types/agent/agentTypes';
import { configService } from '@/common/config/configService';
import type { AcpModelInfo } from '../types';
import type { AgentModeOption } from '@/renderer/utils/model/agentTypes';
import {
  buildAgentRuntimeModeState,
  buildAgentRuntimeModelInfo,
  buildAgentRuntimeSlashCommands,
  buildAgentRuntimeThoughtLevelOption,
  type AgentRuntimeCatalog,
  type AgentRuntimeDerivedOption,
} from '@/renderer/utils/model/agentRuntimeCatalog';
import type { SlashCommandItem } from '@/common/chat/slash/types';
import { useManagedAgentRuntimeCatalog } from '@/renderer/hooks/agent/useManagedAgents';
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useCustomAgentsLoader } from './useCustomAgentsLoader';

export {
  buildAgentRuntimeModeState,
  buildAgentRuntimeModelInfo,
  buildAgentRuntimeSlashCommands,
  type AgentRuntimeCatalog,
};

export type GuidAgentSelectionResult = {
  selectedAgentId: string | null;
  setSelectedAgentId: (agentId: string) => void;
  defaultAgentId: string | null;
  selectedAgent: Agent | undefined;
  selectedAgentBackend: string;
  selectedAgentAvailable: boolean;
  agents: Agent[];
  selectedMode: string;
  setSelectedMode: (mode: React.SetStateAction<string>, options?: { persistPreference?: boolean }) => void;
  selectedAcpModel: string | null;
  setSelectedAcpModel: (model: React.SetStateAction<string | null>, options?: { persistPreference?: boolean }) => void;
  currentAcpCachedModelInfo: AcpModelInfo | null;
  currentAgentAvailableCommands: SlashCommandItem[];
  currentAgentModeOptions: AgentModeOption[];
  currentThoughtLevelOption: AgentRuntimeDerivedOption | null;
  selectedThoughtLevelValue: string;
  setSelectedThoughtLevelValue: (
    value: React.SetStateAction<string>,
    options?: { persistPreference?: boolean }
  ) => void;
};

export function resolveInitialAgentModel(models: string[]): string | null {
  if (models.length > 0) {
    return models[0];
  }

  return null;
}

export function buildAgentModelInfo(models: string[]): AcpModelInfo | null {
  if (models.length > 0) {
    return {
      current_model_id: models[0],
      current_model_label: models[0],
      available_models: models.map((model) => ({ id: model, label: model })),
    } satisfies AcpModelInfo;
  }

  return null;
}

export function resolveAgentSelectionKey(
  savedKey: string | undefined,
  agents: Agent[]
): string | undefined {
  if (!savedKey) return undefined;

  if (savedKey.startsWith('custom:')) {
    const agentId = savedKey.slice(7);
    return agents.some((agent) => agent.id === agentId) ? agentId : undefined;
  }

  if (agents.some((agent) => agent.id === savedKey)) {
    return savedKey;
  }

  return undefined;
}

function readPersistedGuidAgentSelectionKey(agents: Agent[]): string | undefined {
  const savedKey = configService.get('guid.lastAgentId');
  const enabledAgents = agents.filter((agent) => agent.enabled !== false);
  return resolveAgentSelectionKey(savedKey, enabledAgents);
}

function persistGuidAgentSelectionKey(agentId: string): void {
  void configService.set('guid.lastAgentId', agentId).catch((error) => {
    console.error('[Guid] Failed to persist selected agent:', error);
  });
}

export function pickDefaultAgentSelectionKey(agents: Agent[]): string | null {
  const enabledAgents = agents.filter((agent) => agent.enabled !== false);
  const preferred =
    enabledAgents.find((agent) => agent.source === 'generated' && isAionrsAgent(agent)) ??
    enabledAgents.find((agent) => isAionrsAgent(agent)) ??
    enabledAgents[0];
  return preferred?.id ?? null;
}

type UseGuidAgentSelectionOptions = {
  resetAgent?: boolean;
  preselectAgentId?: string;
  locationKey?: string;
};

export const useGuidAgentSelection = ({
  resetAgent,
  preselectAgentId,
  locationKey,
}: UseGuidAgentSelectionOptions): GuidAgentSelectionResult => {
  const [selectedAgentIdState, _setSelectedAgentId] = useState<string | null>(null);
  const [selectedMode, _setSelectedMode] = useState<string>('default');
  const [selectedAcpModel, _setSelectedAcpModel] = useState<string | null>(null);
  const [selectedThoughtLevelValue, _setSelectedThoughtLevelValue] = useState<string>('');
  const { agents } = useCustomAgentsLoader();
  const managedAgentRuntimeCatalog = useManagedAgentRuntimeCatalog();

  const setSelectedMode = useCallback(
    (mode: React.SetStateAction<string>, _options?: { persistPreference?: boolean }) => {
      _setSelectedMode((prev) => {
        const nextMode = typeof mode === 'function' ? mode(prev) : mode;
        return nextMode;
      });
    },
    []
  );

  const setSelectedAcpModel = useCallback(
    (modelId: React.SetStateAction<string | null>, _options?: { persistPreference?: boolean }) => {
      _setSelectedAcpModel((prev) => {
        const nextModelId = typeof modelId === 'function' ? modelId(prev) : modelId;
        return nextModelId;
      });
    },
    []
  );

  const setSelectedThoughtLevelValue = useCallback(
    (value: React.SetStateAction<string>, _options?: { persistPreference?: boolean }) => {
      _setSelectedThoughtLevelValue((prev) => {
        const nextValue = typeof value === 'function' ? value(prev) : value;
        return nextValue;
      });
    },
    []
  );

  const setSelectedAgentId = useCallback(
    (agentId: string) => {
      const normalizedId = resolveAgentSelectionKey(agentId, agents) ?? agentId;
      _setSelectedAgentId(normalizedId);
      persistGuidAgentSelectionKey(normalizedId);
    },
    [agents]
  );

  const resetHandledRef = useRef(false);
  const prevLocationKeyRef = useRef(locationKey);
  if (locationKey !== prevLocationKeyRef.current) {
    prevLocationKeyRef.current = locationKey;
    resetHandledRef.current = false;
  }

  useLayoutEffect(() => {
    if (agents.length === 0) return;
    if (resetHandledRef.current) return;

    if (preselectAgentId) {
      const resolvedPreselect = resolveAgentSelectionKey(preselectAgentId, agents);
      if (resolvedPreselect) {
        resetHandledRef.current = true;
        _setSelectedAgentId(resolvedPreselect);
        return;
      }
    }

    if (resetAgent) {
      resetHandledRef.current = true;
      const fallbackId =
        readPersistedGuidAgentSelectionKey(agents) ?? pickDefaultAgentSelectionKey(agents);
      _setSelectedAgentId(fallbackId);
    }
  }, [agents, preselectAgentId, resetAgent]);

  useEffect(() => {
    if (agents.length === 0) return;
    if (resetAgent) return;
    if (preselectAgentId && resolveAgentSelectionKey(preselectAgentId, agents)) return;
    if (!selectedAgentIdState || !agents.some((agent) => agent.id === selectedAgentIdState)) {
      _setSelectedAgentId(
        readPersistedGuidAgentSelectionKey(agents) ?? pickDefaultAgentSelectionKey(agents)
      );
    }
  }, [agents, preselectAgentId, resetAgent, selectedAgentIdState]);

  const selectedAgent = useMemo(
    () =>
      selectedAgentIdState ? agents.find((agent) => agent.id === selectedAgentIdState) : undefined,
    [agents, selectedAgentIdState]
  );
  const selectedAgentId = selectedAgent?.id ?? null;
  const selectedAgentBackend = agentRuntimeKey(selectedAgent);
  const selectedAgentModels = selectedAgent?.models ?? [];
  const selectedManagedAgentRuntimeCatalog = useMemo(
    () =>
      selectedAgent?.agent_id
        ? managedAgentRuntimeCatalog.find((agent) => agent.id === selectedAgent.agent_id)
        : undefined,
    [managedAgentRuntimeCatalog, selectedAgent?.agent_id]
  );
  const selectedAgentRuntimeModelInfo = useMemo(
    () => buildAgentRuntimeModelInfo(selectedManagedAgentRuntimeCatalog),
    [selectedManagedAgentRuntimeCatalog]
  );
  const currentAgentAvailableCommands = useMemo(
    () => buildAgentRuntimeSlashCommands(selectedManagedAgentRuntimeCatalog),
    [selectedManagedAgentRuntimeCatalog]
  );
  const selectedAgentRuntimeModeState = useMemo(
    () => buildAgentRuntimeModeState(selectedManagedAgentRuntimeCatalog),
    [selectedManagedAgentRuntimeCatalog]
  );
  const selectedAgentRuntimeThoughtLevelOption = useMemo(
    () => buildAgentRuntimeThoughtLevelOption(selectedManagedAgentRuntimeCatalog),
    [selectedManagedAgentRuntimeCatalog]
  );
  const currentThoughtLevelOption = useMemo<AgentRuntimeDerivedOption | null>(() => {
    if (!selectedAgentRuntimeThoughtLevelOption) return null;
    return {
      ...selectedAgentRuntimeThoughtLevelOption,
      currentValue: selectedThoughtLevelValue || selectedAgentRuntimeThoughtLevelOption.currentValue,
    };
  }, [selectedAgentRuntimeThoughtLevelOption, selectedThoughtLevelValue]);
  const currentAgentModeOptions = selectedAgentRuntimeModeState.options;

  const selectedAgentAvailable = useMemo(() => {
    return selectedAgent?.agent_status === 'online';
  }, [selectedAgent]);

  const modelSelectionScopeRef = useRef<string | null>(null);
  useEffect(() => {
    // A CLI agent's runtime catalog must NOT seed a selection. `current_model_id` there is
    // whatever the LAST session of this agent wrote back — not this user's intent — and
    // `available_models[0]` is the agent's own "Default" row, which for claude is a REAL
    // choice (it pins the account default, overriding the user's ANTHROPIC_MODEL). Seeding
    // either one turned "I have not picked a model" into a silent pick, so a fresh
    // conversation could never start on the model `claude` itself would have used.
    //
    // `null` is the meaningful value: `useGuidSend` omits the model override entirely, and
    // the agent then resolves it from the user's own config, exactly as its CLI does. The
    // picker renders this as no checkmark + the generic "default model" label, and once a
    // session exists the backend reports the model it actually resolved to.
    //
    // Agents backed by an API provider have no runtime catalog; they keep seeding from
    // their own `models` list, which is a real per-agent configuration.
    const fallbackModelId = selectedAgentRuntimeModelInfo
      ? null
      : selectedAgentModels.length > 0
        ? resolveInitialAgentModel(selectedAgentModels)
        : null;
    const availableModelIds = new Set(
      selectedAgentRuntimeModelInfo?.available_models.map((model) => model.id) ?? selectedAgentModels
    );
    const selectionScope = selectedAgentId ?? '';

    _setSelectedAcpModel((previousModelId) => {
      const scopeChanged = modelSelectionScopeRef.current !== selectionScope;
      modelSelectionScopeRef.current = selectionScope;

      if (
        !scopeChanged &&
        previousModelId &&
        (availableModelIds.size === 0 || availableModelIds.has(previousModelId))
      ) {
        return previousModelId;
      }

      return fallbackModelId;
    });
  }, [selectedAgentId, selectedAgentModels, selectedAgentRuntimeModelInfo]);

  useEffect(() => {
    const fallbackMode =
      selectedAgentRuntimeModeState.currentMode || selectedAgentRuntimeModeState.options[0]?.value || 'default';
    _setSelectedMode(fallbackMode);
  }, [selectedAgentRuntimeModeState]);

  const thoughtLevelSelectionScopeRef = useRef<string | null>(null);
  useEffect(() => {
    const optionValues = new Set(selectedAgentRuntimeThoughtLevelOption?.options.map((option) => option.value) ?? []);
    const fallbackThoughtLevel =
      selectedAgentRuntimeThoughtLevelOption?.currentValue ||
      selectedAgentRuntimeThoughtLevelOption?.options[0]?.value ||
      '';
    const selectionScope = selectedAgentId ?? '';

    _setSelectedThoughtLevelValue((previousValue) => {
      const scopeChanged = thoughtLevelSelectionScopeRef.current !== selectionScope;
      thoughtLevelSelectionScopeRef.current = selectionScope;

      if (!selectedAgentRuntimeThoughtLevelOption) {
        return '';
      }

      if (!scopeChanged && previousValue && optionValues.has(previousValue)) {
        return previousValue;
      }

      return fallbackThoughtLevel;
    });
  }, [selectedAgentRuntimeThoughtLevelOption, selectedAgentId]);

  const currentAcpCachedModelInfo = useMemo(() => {
    if (selectedAgentRuntimeModelInfo) {
      return selectedAgentRuntimeModelInfo;
    }

    return buildAgentModelInfo(selectedAgentModels);
  }, [selectedAgentModels, selectedAgentRuntimeModelInfo]);

  const defaultAgentId = useMemo(() => pickDefaultAgentSelectionKey(agents), [agents]);

  return {
    selectedAgentId,
    setSelectedAgentId,
    defaultAgentId,
    selectedAgent,
    selectedAgentBackend,
    selectedAgentAvailable,
    agents,
    selectedMode,
    setSelectedMode,
    selectedAcpModel,
    setSelectedAcpModel,
    currentAcpCachedModelInfo,
    currentAgentAvailableCommands,
    currentAgentModeOptions,
    currentThoughtLevelOption,
    selectedThoughtLevelValue,
    setSelectedThoughtLevelValue,
  };
};
