"use client"

import { useCallback, useEffect, useState } from "react"
import { Loader2, RefreshCw, Server } from "lucide-react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  ccrSupervisorStart,
  ccrSupervisorState,
  type CcrDaemonState,
  type CcrDaemonStatus,
} from "@/lib/ccr"
import { errorText } from "./helpers"

/**
 * 5.1 CCR 托管 — the state of CCR's *management process*.
 *
 * The neighbouring `GatewayCard` shows the proxy inside CCR (start/stop it via
 * CCR's own RPC). This card is the host: whether the management process is up,
 * who started it, and whether Codeg can start it again. It never stops CCR —
 * stopping is the user's decision — so the only action is 探活 / 拉起.
 */
export function CcrDaemonCard() {
  const t = useTranslations("CcrModelSettings")

  const [status, setStatus] = useState<CcrDaemonStatus | null>(null)
  const [busy, setBusy] = useState("")

  const probe = useCallback(async () => {
    try {
      setStatus(await ccrSupervisorState())
    } catch (err: unknown) {
      toast.error(errorText(err), { description: t("daemonTitle") })
    }
  }, [t])

  useEffect(() => {
    probe().catch(console.error)
  }, [probe])

  const start = useCallback(async () => {
    setBusy("start")
    try {
      setStatus(await ccrSupervisorStart())
    } catch (err: unknown) {
      toast.error(errorText(err), { description: t("daemonStartFailed") })
      await probe()
    } finally {
      setBusy("")
    }
  }, [probe, t])

  const state: CcrDaemonState | "unknown" = status?.state ?? "unknown"
  const running = state === "running"
  const stateLabel = {
    running: t("daemonRunning"),
    starting: t("daemonStarting"),
    stopped: t("daemonStopped"),
    failed: t("daemonFailed"),
    disabled: t("daemonDisabled"),
    unknown: t("daemonStopped"),
  }[state]

  return (
    <section className="mt-4 px-3 md:px-4">
      <div className="flex items-center justify-between gap-2">
        <h2 className="text-xs font-medium text-muted-foreground">
          {t("daemonTitle")}
        </h2>
        <span className="flex items-center gap-2">
          <Badge
            variant={running ? "secondary" : "destructive"}
            className="text-3xs px-1.5 py-0"
          >
            {stateLabel}
          </Badge>
          {running && status && (
            <span className="text-2xs text-muted-foreground">
              {status.managed ? t("daemonManaged") : t("daemonAdopted")}
            </span>
          )}
        </span>
      </div>
      <div className="mt-2 space-y-2 rounded-md border px-3 py-2.5">
        <div className="grid gap-x-6 gap-y-1.5 text-xs sm:grid-cols-2">
          <span className="text-muted-foreground">
            {t("connectionLabel")}:
            <span className="ml-1.5 font-mono text-foreground">
              {status?.webUrl ?? "-"}
            </span>
          </span>
          <span className="text-muted-foreground">
            {t("pidLabel")}:
            <span className="ml-1.5 font-mono text-foreground">
              {status?.pid ?? "-"}
            </span>
          </span>
          {status?.bin && (
            <span className="text-muted-foreground sm:col-span-2">
              CLI:
              <span className="ml-1.5 font-mono text-foreground">
                {status.bin}
              </span>
            </span>
          )}
        </div>
        {!running && status?.blockedReason && (
          <p className="text-2xs text-muted-foreground">
            <Server className="mr-1 inline h-3 w-3" />
            {t("daemonBlocked")}: {status.blockedReason}
          </p>
        )}
        {status?.lastError && (
          <p className="text-2xs text-red-400">
            {t("lastError")}: {status.lastError}
          </p>
        )}
        <p className="text-2xs text-muted-foreground">{t("daemonHint")}</p>
        <div className="flex flex-wrap gap-1.5">
          {!running && !status?.blockedReason && (
            <Button
              size="sm"
              variant="outline"
              className="h-7 text-xs"
              onClick={() => start().catch(console.error)}
              disabled={busy !== ""}
            >
              {busy === "start" ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              ) : (
                <Server className="mr-1 h-3.5 w-3.5" />
              )}
              {status?.state === "failed" ? t("daemonRetry") : t("daemonStart")}
            </Button>
          )}
          <Button
            size="sm"
            variant="outline"
            className="h-7 text-xs"
            onClick={() => {
              setBusy("probe")
              probe().finally(() => setBusy(""))
            }}
            disabled={busy !== ""}
          >
            {busy === "probe" ? (
              <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
            ) : (
              <RefreshCw className="mr-1 h-3.5 w-3.5" />
            )}
            {t("refresh")}
          </Button>
        </div>
      </div>
    </section>
  )
}
