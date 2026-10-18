"use client"

import { useState } from "react"
import { ChevronDown, Loader2, RefreshCw } from "lucide-react"
import { useTranslations } from "next-intl"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import type { CcrGatewayStatus, CcrHealth } from "@/lib/ccr"

interface GatewayCardProps {
  health: CcrHealth | null
  gateway: CcrGatewayStatus | null
  busy: string
  onRefresh: () => void
  onAction: (action: "start" | "stop" | "restart") => void
}

export function GatewayCard({
  health,
  gateway,
  busy,
  onRefresh,
  onAction,
}: GatewayCardProps) {
  const t = useTranslations("CcrModelSettings")
  const [open, setOpen] = useState(true)

  const state = gateway?.state ?? "unknown"
  const running = state === "running"
  const stateLabel = running
    ? t("gatewayRunning")
    : state === "stopped"
      ? t("gatewayStopped")
      : t("gatewayError")

  return (
    <section className="mt-4 px-3 md:px-4">
      <button
        type="button"
        onClick={() => setOpen((prev) => !prev)}
        className="flex w-full items-center justify-between gap-2 text-left"
      >
        <h2 className="text-xs font-medium text-muted-foreground">
          {t("statusTitle")}
        </h2>
        <span className="flex items-center gap-2">
          <Badge
            variant={running ? "secondary" : "destructive"}
            className="text-3xs px-1.5 py-0"
          >
            {stateLabel}
          </Badge>
          <ChevronDown
            className={`h-3.5 w-3.5 text-muted-foreground transition-transform ${
              open ? "rotate-180" : ""
            }`}
          />
        </span>
      </button>
      {open && (
        <div className="mt-2 space-y-2 rounded-md border px-3 py-2.5">
          <div className="grid gap-x-6 gap-y-1.5 text-xs sm:grid-cols-2">
            <span className="text-muted-foreground">
              {t("connectionLabel")}:
              <span className="ml-1.5 font-mono text-foreground">
                {health?.url ?? "-"}
              </span>
            </span>
            <span className="text-muted-foreground">
              {t("gatewayEndpointLabel")}:
              <span className="ml-1.5 font-mono text-foreground">
                {gateway?.coreEndpoint ?? gateway?.endpoint ?? "-"}
              </span>
            </span>
            <span className="text-muted-foreground">
              {t("tokenLabel")}:
              <span className="ml-1.5 text-foreground">
                {health?.tokenConfigured
                  ? t("tokenConfigured")
                  : t("tokenMissing")}
              </span>
              {/* [crewchat] Say WHERE the token came from: "discovered from CCR"
                  is what makes a double-clicked desktop app work, and it is
                  also the answer to "why does it work without env vars". */}
              {health?.tokenSource ? (
                <span className="ml-1.5 text-muted-foreground/70">
                  （
                  {health.tokenSource === "env"
                    ? t("tokenSourceEnv")
                    : t("tokenSourceDiscovered")}
                  ）
                </span>
              ) : null}
            </span>
            <span className="text-muted-foreground">
              {t("pidLabel")}:
              <span className="ml-1.5 font-mono text-foreground">
                {gateway?.pid ?? "-"}
              </span>
            </span>
            <span className="text-muted-foreground sm:col-span-2">
              {t("startedAtLabel")}:
              <span className="ml-1.5 text-foreground">
                {gateway?.lastStartedAt
                  ? new Date(gateway.lastStartedAt).toLocaleString()
                  : "-"}
              </span>
            </span>
          </div>
          {gateway?.lastError && (
            <p className="text-2xs text-red-400">
              {t("lastError")}: {gateway.lastError}
            </p>
          )}
          <div className="flex flex-wrap gap-1.5">
            <Button
              size="sm"
              variant="outline"
              className="h-7 text-xs"
              onClick={onRefresh}
              disabled={busy !== ""}
            >
              {busy === "reload" ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              ) : (
                <RefreshCw className="mr-1 h-3.5 w-3.5" />
              )}
              {t("refresh")}
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 text-xs"
              onClick={() => onAction("start")}
              disabled={busy !== "" || running}
            >
              {busy === "start" && (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              )}
              {t("start")}
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 text-xs"
              onClick={() => onAction("stop")}
              disabled={busy !== "" || !running}
            >
              {busy === "stop" && (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              )}
              {t("stop")}
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 text-xs"
              onClick={() => onAction("restart")}
              disabled={busy !== ""}
            >
              {busy === "restart" && (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              )}
              {t("restart")}
            </Button>
          </div>
        </div>
      )}
    </section>
  )
}
