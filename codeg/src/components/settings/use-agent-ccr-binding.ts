"use client"

// [crewchat] CCR binding state for the CLI page's readiness card: the profile
// list every card reads from, plus the card's three writes — bind a missing
// profile, re-apply all profiles, retry a failed read. Kept out of
// `acp-agent-settings.tsx` so the vendored page file carries only the
// integration (see codeg/NOTICE.md).

import { useCallback, useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"

import { toErrorMessage } from "@/lib/app-error"
import {
  buildCcrProfileForKind,
  ccrApplyProfile,
  ccrGetConfig,
  ccrGetProfiles,
  ccrSaveConfig,
  type CcrProfileSummary,
} from "@/lib/ccr"

export interface AgentCcrBinding {
  /** `null` until the first read answers (or fails). */
  profiles: CcrProfileSummary[] | null
  /** The read failure, kept as state — the card renders it, never throws. */
  error: string | null
  /** True while a bind / re-apply write is in flight. */
  busy: boolean
  refresh: () => Promise<void>
  bind: (kind: string) => Promise<void>
  reapply: () => Promise<void>
}

export function useAgentCcrBinding(): AgentCcrBinding {
  const t = useTranslations("AcpAgentSettings")
  const [profiles, setProfiles] = useState<CcrProfileSummary[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  // Read once per mount; failures become state, not throws — the card renders
  // them as "CCR unreachable" with a retry.
  const refresh = useCallback(async () => {
    try {
      const next = await ccrGetProfiles()
      setProfiles(next)
      setError(null)
    } catch (err) {
      setProfiles(null)
      setError(toErrorMessage(err))
    }
  }, [])

  useEffect(() => {
    refresh().catch(() => {})
  }, [refresh])

  // The bind write: when the CLI's CCR agent kind has no profile yet, append
  // one (model left empty — the gateway default applies) and re-apply, so the
  // CLI gets its CCR counterpart the moment the user asks for it. An existing
  // profile — created here or in CCR's own UI — is never touched.
  const bind = useCallback(
    async (kind: string) => {
      setBusy(true)
      try {
        const config = await ccrGetConfig()
        // A bind is a read-modify-write of the WHOLE config, so a read that
        // came back empty must abort instead of persisting the emptiness.
        if (!Array.isArray(config.Providers)) {
          throw new Error("CCR config is unavailable")
        }
        const existing = Array.isArray(config.profile?.profiles)
          ? config.profile.profiles
          : []
        if (!existing.some((profile) => profile.agent === kind)) {
          const profile = buildCcrProfileForKind(kind, existing)
          await ccrSaveConfig({
            ...config,
            profile: {
              ...config.profile,
              profiles: [...existing, profile],
            },
          })
        }
        await ccrApplyProfile()
        await refresh()
        toast.success(t("readiness.bindSuccess", { kind }))
      } catch (err) {
        const message = toErrorMessage(err)
        toast.error(t("readiness.bindFailed"), { description: message })
      } finally {
        setBusy(false)
      }
    },
    [refresh, t]
  )

  // "Re-apply": rewrite every bound CLI's own config from CCR — the repair
  // action for a CLI whose touched config drifted.
  const reapply = useCallback(async () => {
    setBusy(true)
    try {
      await ccrApplyProfile()
      await refresh()
      toast.success(t("readiness.applySuccess"))
    } catch (err) {
      const message = toErrorMessage(err)
      toast.error(t("readiness.applyFailed"), { description: message })
    } finally {
      setBusy(false)
    }
  }, [refresh, t])

  return { profiles, error, busy, refresh, bind, reapply }
}
