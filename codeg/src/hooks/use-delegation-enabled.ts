"use client"

// [crewchat] Is multi-agent delegation on?
//
// A conversation's team only MEANS something when it is: the admin's team frame
// (Phase 1.3) is injected only for delegation-enabled connections, so with the
// switch off the leader cannot see its members — and cannot delegate to them,
// because the tool is not injected either. Meanwhile the roster page happily let
// the user pick members. That mismatch is invisible in the transcript: the
// leader just answers "I can't see any other agent".
//
// This hook is how the conversation can say so, in the one place the user looks.

import { useEffect, useState } from "react"

import { getDelegationSettings } from "@/lib/api"

/**
 * Shared read, memoized for the process the way the roster is: this is read by
 * the header strip of every open team conversation, and it changes only when
 * the user flips the switch. A FAILED read is not cached — a transient error
 * must not pin the warning for the rest of the session.
 */
let cached: Promise<boolean> | null = null

export function delegationEnabledCached(): Promise<boolean> {
  if (!cached) {
    cached = getDelegationSettings()
      .then((settings) => settings.enabled)
      .catch((error: unknown) => {
        cached = null
        throw error
      })
  }
  return cached
}

/** Drop the memoized read (test seam, and after a settings change). */
export function resetDelegationEnabledCache(): void {
  cached = null
}

/**
 * `true` / `false` once read; `null` while unknown or when the read failed (a
 * failed read must not render a warning that may be wrong).
 *
 * Refreshes on window focus, so flipping the switch in the settings overlay and
 * coming back to the conversation updates the strip without a reload.
 */
export function useDelegationEnabled(): boolean | null {
  const [enabled, setEnabled] = useState<boolean | null>(null)

  useEffect(() => {
    let cancelled = false
    const read = (fresh: boolean) => {
      if (fresh) resetDelegationEnabledCache()
      delegationEnabledCached()
        .then((value) => {
          if (!cancelled) setEnabled(value)
        })
        .catch(() => {
          if (!cancelled) setEnabled(null)
        })
    }
    read(false)
    const onFocus = () => read(true)
    window.addEventListener("focus", onFocus)
    return () => {
      cancelled = true
      window.removeEventListener("focus", onFocus)
    }
  }, [])

  return enabled
}
