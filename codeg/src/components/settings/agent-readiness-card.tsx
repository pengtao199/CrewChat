"use client"

// [crewchat] The CLI page's readiness card — its default face. Three
// readiness rows (install / enable / model) plus the CCR binding line and the
// three binding writes; everything else on the page lives behind the
// Advanced fold. Presentational on purpose: the page owns the data and the
// effects, this file owns the layout (see codeg/NOTICE.md).

import type { RefObject } from "react"
import { useTranslations } from "next-intl"
import {
  AlertCircle,
  ArrowRight,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Circle,
  Download,
  Loader2,
  Plug,
  RefreshCw,
  XCircle,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import type { CcrProfileSummary } from "@/lib/ccr"
import type { AcpAgentInfo } from "@/lib/types"

export type AgentCcrState =
  | "loading"
  | "bound"
  | "bindable"
  | "unsupported"
  | "unreachable"

export interface AgentInstallReadiness {
  installedVersion: string | null
  recommendedVersion: string | null
  hasUpdate: boolean
  quickInstallable: boolean
  installing: boolean
  uvReady: boolean
}

export interface AgentCcrBindingView {
  kind: string | null
  state: AgentCcrState
  profile: CcrProfileSummary | null
  busy: boolean
  onBind: () => void
  onReapply: () => void
  onRetry: () => void
}

export interface AgentReadinessCardProps {
  agent: AcpAgentInfo
  enabled: boolean
  install: AgentInstallReadiness
  ccr: AgentCcrBindingView
  /** Rendered while an install/upgrade for THIS agent is streaming. */
  installLogs: string[] | null
  installLogEndRef?: RefObject<HTMLDivElement | null>
  onInstall: () => void
  onUpgrade: () => void
  /** The Advanced fold's open state, so the card can own its toggle. */
  advancedOpen: boolean
  onToggleAdvanced: () => void
}

export function AgentReadinessCard({
  agent,
  enabled,
  install,
  ccr,
  installLogs,
  installLogEndRef,
  onInstall,
  onUpgrade,
  advancedOpen,
  onToggleAdvanced,
}: AgentReadinessCardProps) {
  const t = useTranslations("AcpAgentSettings")

  return (
    <div className="border-b px-4 py-3 space-y-2">
      <div className="rounded-md border bg-muted/10 p-3 space-y-2.5">
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-xs">
          <span className="inline-flex items-center gap-1.5">
            {install.installing ? (
              <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-muted-foreground" />
            ) : !install.installedVersion ? (
              <XCircle className="h-3.5 w-3.5 shrink-0 text-red-500" />
            ) : install.hasUpdate ? (
              <AlertCircle className="h-3.5 w-3.5 shrink-0 text-yellow-500" />
            ) : (
              <CheckCircle2 className="h-3.5 w-3.5 shrink-0 text-emerald-500" />
            )}
            <span className="text-muted-foreground">
              {t("readiness.installLabel")}
            </span>
            <span className="font-medium">
              {install.installing
                ? t("readiness.installing")
                : !install.installedVersion
                  ? t("readiness.notInstalled")
                  : install.hasUpdate
                    ? t("readiness.upgradable", {
                        installed: install.installedVersion,
                        recommended: install.recommendedVersion ?? "",
                      })
                    : t("readiness.installedVersion", {
                        version: install.installedVersion,
                      })}
            </span>
            {!install.installing &&
              !install.installedVersion &&
              install.quickInstallable && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="h-6 px-2 text-2xs"
                  aria-label={t("readiness.installAria", { name: agent.name })}
                  onClick={onInstall}
                >
                  <Download className="h-3 w-3" />
                  {t("actions.install")}
                </Button>
              )}
            {!install.installing &&
              install.installedVersion &&
              install.hasUpdate &&
              install.uvReady && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="h-6 px-2 text-2xs"
                  aria-label={t("readiness.upgradeAria", { name: agent.name })}
                  onClick={onUpgrade}
                >
                  <RefreshCw className="h-3 w-3" />
                  {t("actions.upgrade")}
                </Button>
              )}
          </span>

          <span className="inline-flex items-center gap-1.5">
            {enabled ? (
              <CheckCircle2 className="h-3.5 w-3.5 shrink-0 text-emerald-500" />
            ) : (
              <XCircle className="h-3.5 w-3.5 shrink-0 text-red-500" />
            )}
            <span className="text-muted-foreground">
              {t("readiness.enableLabel")}
            </span>
            <span className="font-medium">
              {enabled ? t("status.enabled") : t("status.disabled")}
            </span>
          </span>

          <span className="inline-flex items-center gap-1.5">
            {ccr.state === "loading" ? (
              <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-muted-foreground" />
            ) : ccr.state === "unreachable" ? (
              <XCircle className="h-3.5 w-3.5 shrink-0 text-red-500" />
            ) : ccr.state === "unsupported" ? (
              <Circle className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
            ) : ccr.state === "bindable" ? (
              <AlertCircle className="h-3.5 w-3.5 shrink-0 text-yellow-500" />
            ) : (
              <CheckCircle2 className="h-3.5 w-3.5 shrink-0 text-emerald-500" />
            )}
            <span className="text-muted-foreground">
              {t("readiness.modelLabel")}
            </span>
            <span className="font-medium">
              {ccr.state === "loading"
                ? "…"
                : ccr.state === "unreachable"
                  ? t("readiness.modelUnreachable")
                  : ccr.state === "unsupported"
                    ? t("readiness.modelUnsupported")
                    : ccr.state === "bindable"
                      ? t("readiness.modelBindable")
                      : t("readiness.modelBound")}
            </span>
          </span>
        </div>

        <div className="text-2xs text-muted-foreground">
          {ccr.state === "bound" && ccr.profile ? (
            <>
              {t("readiness.bindingLine", {
                kind: ccr.kind ?? "",
                model: ccr.profile.model || t("readiness.gatewayDefaultModel"),
              })}
              {ccr.profile.enabled === false
                ? ` ${t("readiness.bindingDisabledNote")}`
                : ""}
            </>
          ) : ccr.state === "bindable" ? (
            t("readiness.bindingMissing", { kind: ccr.kind ?? "" })
          ) : ccr.state === "unsupported" ? (
            t("readiness.unsupportedHint")
          ) : ccr.state === "unreachable" ? (
            <span className="text-red-400">
              {t("readiness.unreachableHint")}
            </span>
          ) : (
            t("readiness.loadingHint")
          )}
        </div>

        <div className="flex flex-wrap items-center gap-2">
          {ccr.state === "bound" && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="h-7 text-xs"
              disabled={ccr.busy}
              onClick={ccr.onReapply}
            >
              {ccr.busy ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <RefreshCw className="h-3.5 w-3.5" />
              )}
              {t("readiness.reapply")}
            </Button>
          )}
          {ccr.state === "bindable" && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="h-7 text-xs"
              disabled={ccr.busy}
              onClick={ccr.onBind}
            >
              {ccr.busy ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Plug className="h-3.5 w-3.5" />
              )}
              {t("readiness.bindAction")}
            </Button>
          )}
          {ccr.state === "unreachable" && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="h-7 text-xs"
              onClick={ccr.onRetry}
            >
              <RefreshCw className="h-3.5 w-3.5" />
              {t("readiness.retry")}
            </Button>
          )}
          <button
            type="button"
            className="ms-auto inline-flex items-center gap-1 text-xs text-primary hover:underline"
            onClick={() => {
              // [crewchat] Plain location navigation, carrying the query
              // string like the settings shell's own transitions do: the App
              // Router context this leaf component would otherwise need is
              // exactly what its unit tests don't mount.
              if (typeof window === "undefined") return
              const search = window.location.search
              window.location.assign(`/settings/agent-roles${search}`)
            }}
          >
            {t("readiness.goAgents")}
            <ArrowRight className="h-3 w-3" />
          </button>
        </div>

        {installLogs !== null && (
          <div className="rounded-md border bg-muted/50 text-muted-foreground p-3 max-h-40 overflow-y-auto font-mono text-2xs leading-relaxed">
            {installLogs.map((line, i) => (
              <div
                key={i}
                className={line.startsWith("ERROR:") ? "text-destructive" : ""}
              >
                {line}
              </div>
            ))}
            <div ref={installLogEndRef} />
          </div>
        )}
      </div>

      <button
        type="button"
        className="inline-flex items-center gap-1 rounded px-1 py-0.5 text-xs text-muted-foreground transition-colors hover:text-foreground"
        aria-expanded={advancedOpen}
        onClick={onToggleAdvanced}
      >
        {advancedOpen ? (
          <ChevronDown className="h-3.5 w-3.5" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5" />
        )}
        {t("readiness.advanced")}
      </button>
    </div>
  )
}
