"use client"

/**
 * Inline header for a delegated child sub-session under the parent's
 * `delegate_to_agent` ToolCallBlock. Renders as a self-contained card —
 * never falls through the generic tool-call shell — so users see "Agent
 * delegating: task" instead of "mcp__codeg-delegate__delegate_to_agent: codex".
 *
 * The card is a status + navigation affordance, and — since Phase 4.1 — can
 * host the child's live stream INLINE. `查看会话` opens the side drawer, which
 * is still the full-width way in; the inline expand is for watching a member's
 * progress without leaving the thread you are reading. Both mount the same
 * `LiveTranscriptView`, so they are mutually exclusive by construction: opening
 * either retires the other.
 *
 * All agent-type / task / status / child-id resolution lives in
 * `useDelegationCardModel` (shared with the top-right `SubAgentOverlay`), so the
 * card and the overlay never disagree about a sub-agent; the row itself is
 * `DelegationCardRow`, shared with `ResumedDelegationCard`.
 */

import { useCallback, useMemo, useState } from "react"

import type { ToolCallState } from "@/lib/adapters/ai-elements-adapter"
import { ChevronDown, ChevronUp } from "lucide-react"
import { useTranslations } from "next-intl"
import { LiveTranscriptView } from "@/components/message/live-transcript-view"
import { DelegationCardRow } from "@/components/message/delegation-card-row"
import { useChildUsage } from "@/hooks/use-child-usage"
import { SubAgentSessionDialog } from "@/components/message/sub-agent-session-dialog"
import { useSessionViewerHost } from "@/components/message/session-viewer-host"
import { useDelegationCardModel } from "@/hooks/use-delegation-card-model"

interface Props {
  parentToolUseId: string
  /** Raw JSON arguments the LLM sent to `delegate_to_agent`. Used to
   *  surface the task and agent_type before the broker's
   *  DelegationStarted event lands (or when binding never arrives — e.g.
   *  the wider session was reloaded with an inline child still around). */
  input?: string | null
  output?: string | null
  errorText?: string | null
  state?: ToolCallState
  /**
   * ACP extensibility metadata on this tool call. Read as a tertiary
   * fallback after the live `DelegationContext` binding when the parent UI
   * re-mounted on a page refresh and the live `delegation_started` event was
   * already consumed (lost): the snapshot's
   * `ToolCallState.meta["codeg.delegation"]` carries enough to re-bind the
   * card to the child conversation.
   */
  meta?: Record<string, unknown> | null
}

export function DelegatedSubThread({
  parentToolUseId,
  input,
  output,
  errorText,
  state,
  meta,
}: Props) {
  // Preferred: hand the viewer to the transcript-level host, which outlives
  // this card's virtual row. `null` means this card is rendering outside a
  // `MessageListView` (and so outside any virtualizer) — then it owns the
  // drawer itself, as it always did.
  const viewerHost = useSessionViewerHost()
  const [dialogOpen, setDialogOpen] = useState(false)
  // [crewchat] Phase 4.1: inline member stream. Off by default so the card keeps
  // the slim one-row shape it has always had.
  const [streamExpanded, setStreamExpanded] = useState(false)
  // Memoized so the open-drawer callback below has a stable dep: the host
  // re-derives its model from this source long after this card may unmount.
  const source = useMemo(
    () => ({
      parentToolUseId,
      input,
      output,
      errorText,
      state,
      meta,
    }),
    [parentToolUseId, input, output, errorText, state, meta]
  )
  const {
    agentType,
    task,
    taskId,
    status,
    errorCode,
    childConversationId,
    childConnectionId,
    hasModel,
  } = useDelegationCardModel(source)
  const t = useTranslations("Folder.chat.delegation")

  // [crewchat] Phase 3.2: per-member usage, fetched once the child's turn is
  // terminal (the only moment the numbers are final).
  const usage = useChildUsage(
    childConversationId,
    status === "ok" || status === "err"
  )

  // [crewchat] Phase 4.1: one live bridge per child conversation, never two.
  // The inline stream and the drawer both mount `LiveTranscriptView`, which
  // mirrors the child connection into the runtime session for
  // `childConversationId` and drops that session on unmount. Showing the same
  // child both ways at once would race those bridges and blank one view the
  // moment the other unmounted. So opening either one retires the other.
  const retireOtherView = useCallback(() => {
    viewerHost?.close?.()
    setDialogOpen(false)
  }, [viewerHost])

  // `source` is memoized above (the host re-derives its own model from it, so
  // the request must carry the full source, not just the id).
  const openSession = useCallback(() => {
    retireOtherView()
    setStreamExpanded(false)
    if (viewerHost) {
      viewerHost.open({ kind: "delegation", source })
    } else {
      setDialogOpen(true)
    }
  }, [retireOtherView, source, viewerHost])

  const toggleStream = useCallback(() => {
    if (streamExpanded) {
      setStreamExpanded(false)
      return
    }
    retireOtherView()
    setStreamExpanded(true)
  }, [retireOtherView, streamExpanded])

  const streamLabel = streamExpanded ? t("collapseDetail") : t("expandDetail")
  const streamToggle =
    childConversationId != null ? (
      <button
        type="button"
        onClick={toggleStream}
        aria-expanded={streamExpanded}
        className="shrink-0 flex items-center gap-1.5 px-3 border-l border-border text-xs font-medium text-foreground/80 hover:bg-muted/60 hover:text-foreground transition-colors"
        title={streamLabel}
        aria-label={streamLabel}
        data-testid="delegation-stream-toggle"
      >
        {streamExpanded ? (
          <ChevronUp className="h-3.5 w-3.5" />
        ) : (
          <ChevronDown className="h-3.5 w-3.5" />
        )}
        <span className="hidden @[24rem]/delegcard:inline">{streamLabel}</span>
      </button>
    ) : null

  // A snapshot replay with an empty/unparseable input AND no live binding has
  // no useful card to draw — fall through to the standard renderer instead of
  // an "unknown sub-agent" stub. Placed AFTER all hooks so hook order is stable.
  if (!hasModel) {
    return null
  }

  return (
    <div
      data-testid="delegated-sub-thread"
      className="@container/delegcard rounded-lg border border-border bg-card ws-msg-card"
    >
      <DelegationCardRow
        agentType={agentType}
        taskId={taskId}
        status={status}
        errorCode={errorCode}
        usage={usage}
        task={task}
        trailingAction={streamToggle}
        onOpenSession={childConversationId != null ? openSession : undefined}
      />
      {/* A bounded box, not a flex-1 fill: this expands inside a message row,
          where there is no parent column height to fill. */}
      {streamExpanded && childConversationId != null && (
        <div
          className="h-80 border-t border-border"
          data-testid="delegation-inline-stream"
        >
          <LiveTranscriptView
            conversationId={childConversationId}
            connectionId={childConnectionId}
            agentType={agentType}
            kickoffText={task}
          />
        </div>
      )}
      {viewerHost == null && childConversationId != null && (
        <SubAgentSessionDialog
          open={dialogOpen}
          onOpenChange={setDialogOpen}
          childConversationId={childConversationId}
          childConnectionId={childConnectionId}
          agentType={agentType}
          kickoffTask={task}
        />
      )}
    </div>
  )
}
