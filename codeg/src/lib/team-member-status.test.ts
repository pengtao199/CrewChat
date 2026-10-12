import { describe, expect, it } from "vitest"

import {
  memberTaskPreview,
  resolveMemberPresence,
  runningChildConnectionIds,
  type MemberBindingSignal,
  type MemberChildSignal,
} from "@/lib/team-member-status"

function child(
  id: number,
  status: MemberChildSignal["status"]
): MemberChildSignal {
  return { id, status }
}

function binding(
  overrides: Partial<MemberBindingSignal> & { childConversationId: number }
): MemberBindingSignal {
  return {
    childConnectionId: `conn-${overrides.childConversationId}`,
    status: "running",
    task: null,
    ...overrides,
  }
}

describe("resolveMemberPresence", () => {
  it("is idle before anything was ever delegated", () => {
    expect(resolveMemberPresence({ children: [], bindings: [] })).toBe("idle")
  })

  it("is running while a child row is in flight", () => {
    expect(
      resolveMemberPresence({
        children: [child(9, "in_progress")],
        bindings: [binding({ childConversationId: 9 })],
      })
    ).toBe("running")
  })

  it("is queued while a live binding's child row is not visible yet", () => {
    // The spawn/claim window: `delegation_started` arrived, the children
    // refetch hasn't — there is no queued ROW, the binding is the evidence.
    expect(
      resolveMemberPresence({
        children: [child(4, "completed")],
        bindings: [binding({ childConversationId: 9 })],
      })
    ).toBe("queued")
  })

  it("is done for a newest completed child even without a live binding", () => {
    // Refresh mid-after-completion: bindings are gone, only the DB row is.
    expect(
      resolveMemberPresence({ children: [child(9, "completed")], bindings: [] })
    ).toBe("done")
    expect(
      resolveMemberPresence({
        children: [child(9, "pending_review")],
        bindings: [],
      })
    ).toBe("done")
  })

  it("is done for a completed binding with no child row at all", () => {
    expect(
      resolveMemberPresence({
        children: [],
        bindings: [binding({ childConversationId: 9, status: "ok" })],
      })
    ).toBe("done")
  })

  it("is cancelled for a cancelled child whose binding carries no failure", () => {
    expect(
      resolveMemberPresence({
        children: [child(9, "cancelled")],
        bindings: [
          binding({
            childConversationId: 9,
            status: "err",
            errorCode: "canceled",
          }),
        ],
      })
    ).toBe("cancelled")
  })

  it("reads a non-cancel failure code as failed, not cancelled", () => {
    expect(
      resolveMemberPresence({
        children: [child(9, "cancelled")],
        bindings: [
          binding({
            childConversationId: 9,
            status: "err",
            errorCode: "spawn_failed",
          }),
        ],
      })
    ).toBe("failed")
  })

  it("is failed for an errored binding with no child row", () => {
    expect(
      resolveMemberPresence({
        children: [],
        bindings: [binding({ childConversationId: 9, status: "err" })],
      })
    ).toBe("failed")
  })

  it("keeps a NEWER running child ahead of an older terminal one", () => {
    expect(
      resolveMemberPresence({
        children: [child(10, "in_progress"), child(9, "completed")],
        bindings: [binding({ childConversationId: 10 })],
      })
    ).toBe("running")
  })

  it("keeps a newer terminal state ahead of an older ok binding", () => {
    // The member's newest task was cancelled; an older completed binding must
    // not paint it "done".
    expect(
      resolveMemberPresence({
        children: [child(10, "cancelled")],
        bindings: [binding({ childConversationId: 9, status: "ok" })],
      })
    ).toBe("cancelled")
  })
})

describe("runningChildConnectionIds", () => {
  it("returns one connection per in-flight child the bindings point at", () => {
    expect(
      runningChildConnectionIds({
        children: [
          child(10, "in_progress"),
          child(9, "in_progress"),
          child(8, "completed"),
        ],
        bindings: [
          binding({ childConversationId: 10 }),
          binding({ childConversationId: 9 }),
          binding({ childConversationId: 8, status: "ok" }),
        ],
      })
    ).toEqual(["conn-10", "conn-9"])
  })

  it("is empty for a queued member (no session to watch yet)", () => {
    expect(
      runningChildConnectionIds({
        children: [],
        bindings: [binding({ childConversationId: 10 })],
      })
    ).toEqual([])
  })
})

describe("memberTaskPreview", () => {
  it("prefers the running child's task", () => {
    expect(
      memberTaskPreview({
        children: [child(10, "in_progress"), child(9, "completed")],
        bindings: [
          binding({ childConversationId: 10, task: "build the page" }),
          binding({ childConversationId: 9, status: "ok", task: "old task" }),
        ],
      })
    ).toBe("build the page")
  })

  it("falls back to the newest child's task after completion", () => {
    expect(
      memberTaskPreview({
        children: [child(10, "completed")],
        bindings: [
          binding({ childConversationId: 10, status: "ok", task: "ship it" }),
        ],
      })
    ).toBe("ship it")
  })

  it("is null without any task text", () => {
    expect(memberTaskPreview({ children: [], bindings: [] })).toBeNull()
  })
})
