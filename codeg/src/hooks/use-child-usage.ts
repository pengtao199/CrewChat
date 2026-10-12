"use client"

// [crewchat] Per-member usage for a delegation card (Phase 3.2).
//
// A delegation card is a status + navigation affordance, so it fetches nothing
// by default. Usage is the one number a user compares between members, and it
// is only meaningful once the child's turn is terminal — so this hook fetches
// the child conversation's parsed stats exactly then, caching per child so
// re-rendering a transcript (or several cards for the same child) costs one
// call. The numbers come from the SAME source the session details dialog
// shows (`DbConversationDetail.session_stats`), which keeps the two surfaces
// on one measurement.
//
// Failures stay silent: an unreachable/parsed-out child simply shows no usage.

import { useEffect, useState } from "react"

import { getFolderConversation } from "@/lib/api"

export interface DelegationUsage {
  inputTokens: number
  outputTokens: number
  totalTokens: number
  durationMs: number
}

/** Cache per child conversation id; `null` = fetched and nothing to show. */
const usageCache = new Map<number, DelegationUsage | null>()
const inflight = new Map<number, Promise<DelegationUsage | null>>()

function toUsage(
  detail: Awaited<ReturnType<typeof getFolderConversation>>
): DelegationUsage | null {
  const stats = detail.session_stats
  if (!stats) return null
  const usage: DelegationUsage = {
    inputTokens: stats.total_usage?.input_tokens ?? 0,
    outputTokens: stats.total_usage?.output_tokens ?? 0,
    totalTokens: stats.total_tokens ?? 0,
    durationMs: stats.total_duration_ms ?? 0,
  }
  if (
    usage.totalTokens === 0 &&
    usage.inputTokens === 0 &&
    usage.outputTokens === 0
  ) {
    return null
  }
  return usage
}

export function useChildUsage(
  childConversationId: number | null,
  enabled: boolean
): DelegationUsage | null {
  const [fetched, setFetched] = useState<DelegationUsage | null>(null)

  useEffect(() => {
    if (!enabled || childConversationId == null) return
    // Already resolved (this mount or an earlier one): the render below reads
    // the cache, so there is nothing to do here.
    if (usageCache.has(childConversationId)) return
    let cancelled = false
    let load = inflight.get(childConversationId)
    if (!load) {
      load = getFolderConversation(childConversationId)
        .then((detail) => {
          const value = toUsage(detail)
          usageCache.set(childConversationId, value)
          return value
        })
        .catch(() => {
          usageCache.set(childConversationId, null)
          return null
        })
        .finally(() => {
          inflight.delete(childConversationId)
        })
      inflight.set(childConversationId, load)
    }
    void load.then((value) => {
      if (!cancelled) setFetched(value)
    })
    return () => {
      cancelled = true
    }
  }, [childConversationId, enabled])

  if (childConversationId == null) return null
  const cached = usageCache.get(childConversationId)
  return cached !== undefined ? cached : fetched
}

/** Test seam: drop the module cache between cases. */
export function resetChildUsageCache() {
  usageCache.clear()
  inflight.clear()
}
