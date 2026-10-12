import { useCallback, useRef } from "react"
import { Badge } from "@/components/ui/badge"
import {
  Combobox,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxGroup,
  ComboboxInput,
  ComboboxItem,
  ComboboxLabel,
  ComboboxList,
} from "@/components/ui/combobox"
import {
  OpenCodeModelOptionGroup,
  formatContextWindow,
} from "@/lib/opencode-connect"
import { toErrorMessage } from "@/lib/app-error"

import {
  asObjectRecord,
  parseConfigJsonText,
  pickFirstString,
} from "./config-text"
import { acpText } from "./context"

export function parseOpenCodeAuthJsonText(authJsonText: string): {
  authObject: Record<string, unknown> | null
  error: string | null
} {
  const trimmed = authJsonText.trim()
  if (!trimmed) return { authObject: {}, error: null }
  try {
    const parsed = JSON.parse(trimmed) as unknown
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {
        authObject: null,
        error: acpText(
          "errors.openCodeAuthMustBeObject",
          "OpenCode auth.json must be a JSON object"
        ),
      }
    }
    return { authObject: parsed as Record<string, unknown>, error: null }
  } catch (err) {
    const message = toErrorMessage(err)
    return {
      authObject: null,
      error: acpText(
        "errors.openCodeAuthInvalid",
        "OpenCode auth.json format error: {message}",
        { message }
      ),
    }
  }
}

export function patchOpenCodeAuthJsonText(
  authJsonText: string,
  mutator: (authObject: Record<string, unknown>) => void
): { authJsonText: string; recoveredFromInvalid: boolean } {
  const parsed = parseOpenCodeAuthJsonText(authJsonText)
  const authObject = parsed.error
    ? {}
    : (JSON.parse(JSON.stringify(parsed.authObject ?? {})) as Record<
        string,
        unknown
      >)
  mutator(authObject)
  return {
    authJsonText:
      Object.keys(authObject).length === 0
        ? ""
        : JSON.stringify(authObject, null, 2),
    recoveredFromInvalid: Boolean(parsed.error),
  }
}

export interface OpenCodeProviderView {
  id: string
  name: string
  api: string
  npm: string
  baseUrl: string
  apiKey: string
  modelCount: number
  modelIds: string[]
  models: Record<string, OpenCodeModelView>
}

export interface OpenCodeModelView {
  id: string
  name: string
  extraFieldCount: number
}

export interface OpenCodeConfigView {
  model: string
  smallModel: string
  enabledProviders: string[]
  disabledProviders: string[]
  providerIds: string[]
  providers: Record<string, OpenCodeProviderView>
}

export const OPENCODE_PROVIDER_NPM_OPTIONS = [
  {
    value: "@ai-sdk/openai-compatible",
    label: "@ai-sdk/openai-compatible",
  },
  {
    value: "@ai-sdk/cerebras",
    label: "@ai-sdk/cerebras",
  },
  {
    value: "@ai-sdk/azure",
    label: "@ai-sdk/azure",
  },
  {
    value: "@ai-sdk/xai",
    label: "@ai-sdk/xai",
  },
  {
    value: "@ai-sdk/anthropic",
    label: "@ai-sdk/anthropic",
  },
  {
    value: "@ai-sdk/amazon-bedrock",
    label: "@ai-sdk/amazon-bedrock",
  },
  {
    value: "@ai-sdk/google",
    label: "@ai-sdk/google",
  },
  {
    value: "@ai-sdk/google-vertex",
    label: "@ai-sdk/google-vertex",
  },
  {
    value: "@ai-sdk/deepseek",
    label: "@ai-sdk/deepseek",
  },
] as const

export function buildOpenCodeModelOptions(
  config: OpenCodeConfigView | null
): OpenCodeModelOptionGroup[] {
  if (!config) return []
  const groups: OpenCodeModelOptionGroup[] = []
  for (const providerId of config.providerIds) {
    const provider = config.providers[providerId]
    if (!provider || provider.modelIds.length === 0) continue
    groups.push({
      providerId,
      label: provider.name || providerId,
      models: provider.modelIds.map((modelId) => ({
        value: `${providerId}/${modelId}`,
        label: modelId,
      })),
    })
  }
  return groups
}

export function OpenCodeModelCombobox({
  value,
  onValueChange,
  groups,
  placeholder,
}: {
  value: string
  onValueChange: (value: string) => void
  groups: OpenCodeModelOptionGroup[]
  placeholder: string
}) {
  const inputRef = useRef<HTMLInputElement>(null)

  const handleSelect = useCallback(
    (next: string | null) => {
      if (typeof next === "string" && next !== value) {
        onValueChange(next)
      }
    },
    [onValueChange, value]
  )

  const handleBlur = useCallback(() => {
    const trimmed = (inputRef.current?.value ?? "").trim()
    if (trimmed !== value) {
      onValueChange(trimmed)
    }
  }, [onValueChange, value])

  return (
    <Combobox key={value} value={value} onValueChange={handleSelect}>
      <ComboboxInput
        ref={inputRef}
        placeholder={placeholder}
        onBlur={handleBlur}
        showClear={false}
      />
      <ComboboxContent>
        <ComboboxList>
          {groups.map((group) => (
            <ComboboxGroup key={group.providerId}>
              <ComboboxLabel>{group.label}</ComboboxLabel>
              {group.models.map((model) => {
                const contextLabel =
                  typeof model.context === "number"
                    ? formatContextWindow(model.context)
                    : ""
                return (
                  <ComboboxItem key={model.value} value={model.value}>
                    <span className="truncate">{model.value}</span>
                    {(model.reasoning || contextLabel) && (
                      <span className="ml-auto flex shrink-0 items-center gap-1.5 pl-2">
                        {model.reasoning && (
                          <Badge
                            variant="outline"
                            className="px-1 text-[0.5625rem] font-normal"
                          >
                            {acpText("openCode.reasoningBadge", "reasoning")}
                          </Badge>
                        )}
                        {contextLabel && (
                          <span
                            className="text-3xs text-muted-foreground"
                            title={acpText(
                              "openCode.contextWindow",
                              "Context window"
                            )}
                          >
                            {contextLabel}
                          </span>
                        )}
                      </span>
                    )}
                  </ComboboxItem>
                )
              })}
            </ComboboxGroup>
          ))}
          <ComboboxEmpty>
            {acpText("openCode.noMatchingModels", "No matching models")}
          </ComboboxEmpty>
        </ComboboxList>
      </ComboboxContent>
    </Combobox>
  )
}

export function buildOpenCodeNpmOptions(currentValue: string): string[] {
  const next = new Set<string>(
    OPENCODE_PROVIDER_NPM_OPTIONS.map((v) => v.value)
  )
  const current = currentValue.trim()
  if (current) next.add(current)
  return Array.from(next)
}

export function extractOpenCodeConfigValues(
  configText: string,
  authJsonText: string
): OpenCodeConfigView {
  const parseResult = parseConfigJsonText(configText)
  const config = parseResult.error ? {} : parseResult.config
  const authParsed = parseOpenCodeAuthJsonText(authJsonText)
  const authObject = authParsed.authObject ?? {}
  const providerRoot = asObjectRecord(config.provider) ?? {}
  const providerIds = Object.keys(providerRoot)
  const providers: Record<string, OpenCodeProviderView> = {}
  const knownModelKeys = new Set(["id", "name"])

  for (const providerId of providerIds) {
    const rawProvider = asObjectRecord(providerRoot[providerId]) ?? {}
    const options = asObjectRecord(rawProvider.options) ?? {}
    const models = asObjectRecord(rawProvider.models) ?? {}
    const modelIds = Object.keys(models)
    const providerModels: Record<string, OpenCodeModelView> = {}
    for (const modelId of modelIds) {
      const rawModel = asObjectRecord(models[modelId]) ?? {}
      providerModels[modelId] = {
        // OpenCode uses `provider.models.<model_id>` as the true model id.
        id: modelId,
        name:
          pickFirstString(rawModel, ["name"]) ??
          pickFirstString(rawModel, ["id"]) ??
          "",
        extraFieldCount: Object.keys(rawModel).filter(
          (key) => !knownModelKeys.has(key)
        ).length,
      }
    }
    const authEntry = asObjectRecord(authObject[providerId]) ?? {}
    const authKey = pickFirstString(authEntry, ["key"]) ?? ""
    providers[providerId] = {
      id: providerId,
      name: pickFirstString(rawProvider, ["name"]) ?? "",
      api: pickFirstString(rawProvider, ["api"]) ?? "",
      npm: pickFirstString(rawProvider, ["npm"]) ?? "",
      baseUrl: pickFirstString(options, ["baseURL", "baseUrl"]) ?? "",
      apiKey: pickFirstString(options, ["apiKey", "api_key"]) ?? authKey,
      modelCount: modelIds.length,
      modelIds,
      models: providerModels,
    }
  }

  return {
    model: pickFirstString(config, ["model"]) ?? "",
    smallModel:
      pickFirstString(config, ["small_model", "smallModel", "small-model"]) ??
      "",
    enabledProviders: Array.isArray(config.enabled_providers)
      ? config.enabled_providers
          .filter((item): item is string => typeof item === "string")
          .map((item) => item.trim())
          .filter(Boolean)
      : [],
    disabledProviders: Array.isArray(config.disabled_providers)
      ? config.disabled_providers
          .filter((item): item is string => typeof item === "string")
          .map((item) => item.trim())
          .filter(Boolean)
      : [],
    providerIds,
    providers,
  }
}

export function patchOpenCodeConfigText(
  configText: string,
  mutator: (config: Record<string, unknown>) => void
): {
  configText: string
  recoveredFromInvalid: boolean
} {
  const parseResult = parseConfigJsonText(configText)
  const config = parseResult.error
    ? {}
    : (JSON.parse(JSON.stringify(parseResult.config)) as Record<
        string,
        unknown
      >)
  mutator(config)
  return {
    configText:
      Object.keys(config).length === 0 ? "" : JSON.stringify(config, null, 2),
    recoveredFromInvalid: Boolean(parseResult.error),
  }
}

// Fill in `provider.<id>.npm` with the first option for any providers that
// lack it, so the displayed Select value matches what gets persisted to disk.
export function ensureOpenCodeProviderNpm(configText: string): string {
  if (!configText.trim()) return configText
  const parseResult = parseConfigJsonText(configText)
  if (parseResult.error) return configText
  const config = parseResult.config
  const providerRoot = asObjectRecord(config.provider)
  if (!providerRoot) return configText
  let mutated = false
  for (const providerId of Object.keys(providerRoot)) {
    const provider = asObjectRecord(providerRoot[providerId])
    if (!provider) continue
    const currentNpm =
      typeof provider.npm === "string" ? provider.npm.trim() : ""
    if (!currentNpm) {
      provider.npm = OPENCODE_PROVIDER_NPM_OPTIONS[0].value
      mutated = true
    }
  }
  if (!mutated) return configText
  return JSON.stringify(config, null, 2)
}
