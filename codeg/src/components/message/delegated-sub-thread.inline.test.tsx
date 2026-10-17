/**
 * [crewchat] Phase 4.1 — the delegation card's INLINE member stream.
 *
 * Watching a member's progress used to require opening the drawer, which
 * covers the thread you are reading. The card can now host the same
 * `LiveTranscriptView` inline. Two invariants matter:
 *
 *  1. It is opt-in and closed by default — the slim one-row card is unchanged
 *     for everyone who never clicks it, including the drawer path.
 *  2. Inline and drawer never run at once. Both mount `LiveTranscriptView`,
 *     which owns the runtime session for the child conversation, so having
 *     two would race their bridges and blank one on the other's unmount.
 */
import { fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { DelegatedSubThread } from "./delegated-sub-thread"
import enMessages from "@/i18n/messages/en.json"
import type { DelegationBinding } from "@/contexts/delegation-context"

vi.mock("@/hooks/use-delegated-sub-session", () => ({
  useDelegatedSubSession: vi.fn(),
}))

let mockUsage: unknown = null
vi.mock("@/hooks/use-child-usage", () => ({
  useChildUsage: () => mockUsage,
}))

let mockChildConnection: unknown = undefined
vi.mock("@/contexts/acp-connections-context", async () => {
  const actual = await vi.importActual<
    typeof import("@/contexts/acp-connections-context")
  >("@/contexts/acp-connections-context")
  return {
    ...actual,
    useConnectionStore: () => ({
      subscribeKey: () => () => {},
      getConnection: () => mockChildConnection,
      getConnectPending: () => undefined,
      getActiveKey: () => null,
      subscribeActiveKey: () => () => {},
    }),
  }
})

// The inline stream is the drawer's `LiveTranscriptView` verbatim. Stub it to a
// sentinel reporting what it was pointed at — this file is about WHEN it mounts,
// not about what a transcript renders.
const inlineTranscripts: Array<{
  conversationId: number
  connectionId: string | null
  kickoffText?: string | null
}> = []
vi.mock("@/components/message/live-transcript-view", () => ({
  LiveTranscriptView: ({
    conversationId,
    connectionId,
    kickoffText,
  }: {
    conversationId: number
    connectionId: string | null
    kickoffText?: string | null
  }) => {
    inlineTranscripts.push({ conversationId, connectionId, kickoffText })
    return (
      <div
        data-testid="live-transcript-view"
        data-conversation-id={conversationId}
        data-connection-id={connectionId ?? ""}
      />
    )
  },
}))

vi.mock("@/components/message/sub-agent-session-dialog", () => ({
  SubAgentSessionDialog: ({
    open,
    childConversationId,
  }: {
    open: boolean
    childConversationId: number
  }) =>
    open ? (
      <div
        data-testid="sub-agent-session-dialog"
        data-conversation-id={childConversationId}
      />
    ) : null,
}))

// The host: records the requests it receives so we can assert the inline stream
// and the drawer retire each other.
const host = {
  opened: [] as Array<Record<string, unknown>>,
  closes: 0,
}
vi.mock("@/components/message/session-viewer-host", () => ({
  useSessionViewerHost: () => ({
    open: (request: Record<string, unknown>) => host.opened.push(request),
    close: () => {
      host.closes += 1
    },
  }),
}))

const { useDelegatedSubSession } =
  await import("@/hooks/use-delegated-sub-session")
const mockedHook = vi.mocked(useDelegatedSubSession)

function renderWithIntl(ui: React.ReactElement) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
}

function bindRunningChild(overrides: Partial<DelegationBinding> = {}) {
  mockedHook.mockReturnValue({
    binding: {
      parentConnectionId: "p1",
      parentToolUseId: "pt-1",
      childConnectionId: "c1",
      childConversationId: 99,
      agentType: "codex",
      status: "running",
      task: "run the audit",
      taskId: "task-1",
      ...overrides,
    },
    detail: null,
    loading: false,
    error: null,
  })
}

describe("DelegatedSubThread inline member stream (Phase 4.1)", () => {
  beforeEach(() => {
    mockChildConnection = undefined
    mockUsage = null
    inlineTranscripts.length = 0
    host.opened.length = 0
    host.closes = 0
    mockedHook.mockReturnValue({
      binding: undefined,
      detail: null,
      loading: false,
      error: null,
    })
  })

  it("keeps the card a single slim row until the stream is asked for", () => {
    bindRunningChild()
    renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
    // The toggle is there (there is a child to show)…
    const toggle = screen.getByTestId("delegation-stream-toggle")
    expect(toggle).toHaveAttribute("aria-expanded", "false")
    // …but no transcript yet, and the drawer stays closed.
    expect(screen.queryByTestId("delegation-inline-stream")).toBeNull()
    expect(screen.queryByTestId("live-transcript-view")).toBeNull()
    expect(screen.queryByTestId("sub-agent-session-dialog")).toBeNull()
  })

  it("expands the child's live stream inline, pointed at the child conversation", () => {
    bindRunningChild()
    renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
    fireEvent.click(screen.getByTestId("delegation-stream-toggle"))

    expect(screen.getByTestId("delegation-inline-stream")).toBeInTheDocument()
    const transcript = screen.getByTestId("live-transcript-view")
    expect(transcript).toHaveAttribute("data-conversation-id", "99")
    // The child connection, so the live bridge mirrors the member and not the
    // parent — the kickoff task covers the lag before the CLI's JSONL lands.
    expect(transcript).toHaveAttribute("data-connection-id", "c1")
    expect(inlineTranscripts).toEqual([
      { conversationId: 99, connectionId: "c1", kickoffText: "run the audit" },
    ])
    expect(screen.getByTestId("delegation-stream-toggle")).toHaveAttribute(
      "aria-expanded",
      "true"
    )
  })

  it("collapses again on a second click", () => {
    bindRunningChild()
    renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
    fireEvent.click(screen.getByTestId("delegation-stream-toggle"))
    expect(screen.getByTestId("live-transcript-view")).toBeInTheDocument()
    fireEvent.click(screen.getByTestId("delegation-stream-toggle"))
    expect(screen.queryByTestId("live-transcript-view")).toBeNull()
    expect(screen.queryByTestId("delegation-inline-stream")).toBeNull()
    expect(screen.getByTestId("delegation-stream-toggle")).toHaveAttribute(
      "aria-expanded",
      "false"
    )
  })

  it("offers no inline toggle when the child conversation is unknown", () => {
    // Starting state: the binding has not landed, so there is no child yet.
    mockedHook.mockReturnValue({
      binding: undefined,
      detail: null,
      loading: false,
      error: null,
    })
    renderWithIntl(
      <DelegatedSubThread
        parentToolUseId="pt-1"
        input={JSON.stringify({ agent_type: "codex", task: "set things up" })}
      />
    )
    expect(screen.queryByTestId("delegation-stream-toggle")).toBeNull()
    expect(screen.queryByRole("button")).toBeNull()
  })

  // The mutual exclusion. Two `LiveTranscriptView`s on one child would race the
  // runtime session both own, so each way in retires the other.
  it("closes the drawer when the inline stream opens", () => {
    bindRunningChild()
    renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
    host.closes = 0
    fireEvent.click(screen.getByTestId("delegation-stream-toggle"))
    expect(host.closes).toBe(1)
  })

  it("collapses the inline stream when the drawer is opened instead", () => {
    bindRunningChild()
    renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
    fireEvent.click(screen.getByTestId("delegation-stream-toggle"))
    expect(screen.getByTestId("live-transcript-view")).toBeInTheDocument()

    fireEvent.click(screen.getByRole("button", { name: "Open conversation" }))
    // Drawer asked for, inline view gone — one live bridge per child.
    expect(host.opened).toEqual([
      {
        kind: "delegation",
        source: { parentToolUseId: "pt-1" },
      },
    ])
    expect(screen.queryByTestId("live-transcript-view")).toBeNull()
    expect(screen.queryByTestId("delegation-inline-stream")).toBeNull()
    expect(screen.getByTestId("delegation-stream-toggle")).toHaveAttribute(
      "aria-expanded",
      "false"
    )
  })

  it("never mounts the bridge twice at once", () => {
    // The real hazard: two `LiveTranscriptView`s on one child conversation
    // would race the runtime session both own. Every toggle must leave at
    // most one mounted — collapse unmounts (and so releases the session)
    // before the next expand can mount again.
    bindRunningChild()
    renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
    const toggle = screen.getByTestId("delegation-stream-toggle")
    fireEvent.click(toggle)
    expect(screen.queryAllByTestId("live-transcript-view")).toHaveLength(1)
    fireEvent.click(toggle)
    fireEvent.click(toggle)
    expect(screen.queryAllByTestId("live-transcript-view")).toHaveLength(1)
  })

  it("leaves the card's own row — status, task, usage — intact above the stream", () => {
    bindRunningChild()
    mockUsage = {
      inputTokens: 1200,
      outputTokens: 400,
      totalTokens: 1600,
      durationMs: 8200,
    }
    renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
    fireEvent.click(screen.getByTestId("delegation-stream-toggle"))

    // The row is still there, unchanged, and the drawer affordance survives.
    expect(screen.getByText("run the audit")).toBeInTheDocument()
    expect(screen.getByText("running")).toBeInTheDocument()
    expect(screen.getByTestId("delegation-usage")).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: "Open conversation" })
    ).toBeInTheDocument()
    // The stream renders BELOW it, not instead of it.
    const card = screen.getByTestId("delegated-sub-thread")
    const row = card.querySelector("[data-testid='delegation-usage']")
    const stream = screen.getByTestId("delegation-inline-stream")
    expect(row?.contains(stream)).toBe(false)
  })
})
