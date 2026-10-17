import type { SessionConfigOptionInfo } from "@/lib/types"
import { isModelConfigOption, modelListGroups } from "@/lib/model-config-groups"

// An Agent's "brain" is the pair the user must not retune mid-conversation: the
// model it thinks with, and the effort level it thinks at. Both reach the
// composer as ordinary ACP config options, indistinguishable in shape from the
// ones that stay adjustable (DeepSeek's file-permission sandbox, Codex's
// approval preset), so they have to be recognised by IDENTITY rather than by
// position or label.
//
// Identity means the ids the agents codeg ships actually publish, plus ACP's own
// `thought_level` category — the one signal a future relabel cannot break. ACP
// reserves no id for the reasoning knob, so this is a convention in the same
// spirit as `MODEL_CONFIG_OPTION_ID`, not a guarantee.
const REASONING_CONFIG_OPTION_IDS = new Set([
  "reasoning",
  "reasoning_effort",
  "reasoning-effort",
  "effort",
])

export function isReasoningConfigOption(
  option: SessionConfigOptionInfo
): boolean {
  return (
    REASONING_CONFIG_OPTION_IDS.has(option.id) ||
    option.category === "thought_level"
  )
}

// Model OR reasoning — everything the composer shows but does not let the user
// change. See `splitAgentConfigOptions` for why.
export function isBrainConfigOption(option: SessionConfigOptionInfo): boolean {
  return isModelConfigOption(option) || isReasoningConfigOption(option)
}

export interface AgentConfigOptionSplit {
  /** Read-only: the model and the reasoning level. */
  brain: SessionConfigOptionInfo[]
  /** Everything the user may still switch, in the agent's own order. */
  switchable: SessionConfigOptionInfo[]
}

// Partition an agent's config options into the brain (read-only) and the rest
// (adjustable), preserving the agent's own order inside each half so the
// composer's chip row reads the way the agent published it.
//
// This is a DISPLAY split only: the backend still owns the option list, and
// nothing here stops a `setConfigOption` call. It exists because the model is
// bound on the Agents page and a roster-bound session cannot meaningfully
// re-point itself at another model — the gateway either rejects the change or
// silently ignores it — so the composer must stop offering the switch instead of
// showing a control that cannot work.
export function splitAgentConfigOptions(
  options: SessionConfigOptionInfo[]
): AgentConfigOptionSplit {
  const brain: SessionConfigOptionInfo[] = []
  const switchable: SessionConfigOptionInfo[] = []
  for (const option of options) {
    if (isBrainConfigOption(option)) brain.push(option)
    else switchable.push(option)
  }
  return { brain, switchable }
}

// The label to show for a brain option: its CURRENT value, resolved through the
// same grouping the pickers use, so the chip reads `Opus 4.1` rather than
// `anthropic/claude-opus-4-1`. Boolean options carry no value worth echoing —
// callers render the option's own name for those instead.
export function brainOptionCurrentLabel(
  option: SessionConfigOptionInfo
): string {
  if (option.kind.type !== "select") return ""
  const rendered = modelListGroups(option).flatMap((group) => group.options)
  const current = rendered.find(
    (item) => item.value === option.kind.current_value
  )
  return current?.name ?? option.kind.current_value
}
