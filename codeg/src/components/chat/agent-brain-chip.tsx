"use client"

import { Lock } from "lucide-react"
import { buttonVariants } from "@/components/ui/button"
import { SelectorTooltip } from "@/components/chat/selector-tooltip"
import { cn } from "@/lib/utils"
import type { SessionConfigOptionInfo } from "@/lib/types"

interface AgentBrainChipProps {
  option: SessionConfigOptionInfo
  /** The option's current value, already resolved to a display label. */
  label: string
  /** Localized sentence explaining that this is fixed by the Agent. */
  readOnlyHint: string
}

/**
 * A read-only stand-in for the composer's config-option chip: same slot, same
 * ghost-chip metrics, but it opens nothing.
 *
 * WHY it exists: the model and the reasoning level are the Agent's "brain" —
 * bound on the Agents page. For a roster-bound session the composer cannot
 * meaningfully re-point the model, so showing the value is right and offering a
 * dropdown for it is a lie. The live value stays visible (the user can see what
 * is actually running); the padlock plus the tooltip replace the dropdown.
 *
 * A `span`, not a `Button`: nothing here is actionable. A `disabled` button would
 * be the tempting shortcut, but `disabled:pointer-events-none` swallows the
 * hover that `SelectorTooltip` needs — the one place the "why is this locked"
 * explanation lives — leaving only a native `title`, and it would still announce
 * itself to assistive tech as a control. The un-hovered `ghost` variant keeps the
 * chip visually identical to its editable siblings without promising a click.
 *
 * The metrics come straight from `buttonVariants` on purpose: the brain chip sits
 * in a row with the real selectors, so any hand-rolled height/padding would drift
 * from them the moment the design system changes.
 */
export function AgentBrainChip({
  option,
  label,
  readOnlyHint,
}: AgentBrainChipProps) {
  // A boolean brain option has no value worth echoing — fall back to its name.
  const text = label || option.name

  return (
    <SelectorTooltip label={option.name} description={readOnlyHint}>
      <span
        aria-label={`${option.name}: ${text}. ${readOnlyHint}`}
        className={cn(
          buttonVariants({ variant: "ghost", size: "xs" }),
          "cursor-default text-muted-foreground hover:bg-transparent"
        )}
      >
        <Lock className="size-3 shrink-0 opacity-70" aria-hidden="true" />
        <span className="max-w-[10rem] truncate">{text}</span>
      </span>
    </SelectorTooltip>
  )
}
