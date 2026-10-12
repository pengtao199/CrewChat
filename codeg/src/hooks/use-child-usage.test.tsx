import { renderHook, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type { DbConversationDetail } from "@/lib/types"

const h = vi.hoisted(() => ({
  getFolderConversation: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  getFolderConversation: h.getFolderConversation,
}))

const { useChildUsage, resetChildUsageCache } =
  await import("@/hooks/use-child-usage")

function detail(
  stats: Partial<NonNullable<DbConversationDetail["session_stats"]>> | null
): DbConversationDetail {
  return {
    summary: {} as DbConversationDetail["summary"],
    turns: [],
    session_stats: stats
      ? {
          total_usage: null,
          total_tokens: 0,
          total_duration_ms: 0,
          ...stats,
        }
      : null,
  } as DbConversationDetail
}

beforeEach(() => {
  h.getFolderConversation.mockReset()
  resetChildUsageCache()
})

describe("useChildUsage", () => {
  it("does not fetch while the child is still running", () => {
    renderHook(() => useChildUsage(7, false))
    expect(h.getFolderConversation).not.toHaveBeenCalled()
  })

  it("maps the child's parsed session stats onto tokens and duration", async () => {
    h.getFolderConversation.mockResolvedValue(
      detail({
        total_usage: {
          input_tokens: 1200,
          output_tokens: 400,
          cache_creation_input_tokens: 0,
          cache_read_input_tokens: 0,
        },
        total_tokens: 1600,
        total_duration_ms: 8200,
      })
    )
    const { result } = renderHook(() => useChildUsage(7, true))
    await waitFor(() => expect(result.current).not.toBeNull())
    expect(h.getFolderConversation).toHaveBeenCalledWith(7)
    expect(result.current).toEqual({
      inputTokens: 1200,
      outputTokens: 400,
      totalTokens: 1600,
      durationMs: 8200,
    })
  })

  it("caches per child so a re-render costs one call", async () => {
    h.getFolderConversation.mockResolvedValue(
      detail({ total_usage: null, total_tokens: 42, total_duration_ms: 10 })
    )
    const first = renderHook(() => useChildUsage(9, true))
    await waitFor(() => expect(first.result.current).not.toBeNull())
    first.unmount()
    const second = renderHook(() => useChildUsage(9, true))
    await waitFor(() => expect(second.result.current).not.toBeNull())
    expect(h.getFolderConversation).toHaveBeenCalledTimes(1)
  })

  it("shows nothing when the stats carry no tokens or the fetch fails", async () => {
    h.getFolderConversation.mockResolvedValue(detail({ total_tokens: 0 }))
    const empty = renderHook(() => useChildUsage(11, true))
    await waitFor(() =>
      expect(h.getFolderConversation).toHaveBeenCalledTimes(1)
    )
    expect(empty.result.current).toBeNull()

    h.getFolderConversation.mockRejectedValue(new Error("gone"))
    const failed = renderHook(() => useChildUsage(12, true))
    await waitFor(() =>
      expect(h.getFolderConversation).toHaveBeenCalledTimes(2)
    )
    expect(failed.result.current).toBeNull()
  })

  it("ignores a missing child id", () => {
    renderHook(() => useChildUsage(null, true))
    expect(h.getFolderConversation).not.toHaveBeenCalled()
  })
})
