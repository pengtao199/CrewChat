"use client"

import { ChevronRight, Loader2, Pencil, RefreshCw, Trash2 } from "lucide-react"
import { useTranslations } from "next-intl"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import type { CcrAccountSnapshot, CcrProvider } from "@/lib/ccr"
import {
  credentialKey,
  credentialName,
  credentialPriority,
  headerUsageBadge,
  keyTail,
  limitsSummary,
  meterText,
  providerBaseUrl,
  providerCredentials,
  providerModels,
} from "./helpers"

interface ProviderCardProps {
  provider: CcrProvider
  index: number
  expanded: boolean
  snapshots: CcrAccountSnapshot[] | undefined
  snapshotBusy: boolean
  modelBusy: string
  toggleBusy: string
  disabledModels: string[]
  onToggleExpand: () => void
  onEdit: () => void
  onDelete: () => void
  onToggleModel: (model: string, enable: boolean) => void
  onToggleCredential: (credentialIndex: number) => void
  onLoadSnapshots: (force?: boolean) => void
}

export function ProviderCard({
  provider,
  index,
  expanded,
  snapshots,
  snapshotBusy,
  modelBusy,
  toggleBusy,
  disabledModels,
  onToggleExpand,
  onEdit,
  onDelete,
  onToggleModel,
  onToggleCredential,
  onLoadSnapshots,
}: ProviderCardProps) {
  const t = useTranslations("CcrModelSettings")

  const credentials = providerCredentials(provider)
  const models = providerModels(provider)
  const listedModels = [...new Set([...models, ...disabledModels])].sort(
    (a, b) => a.localeCompare(b)
  )
  const snapshotList = snapshots ?? []
  const usage = headerUsageBadge(snapshotList)

  return (
    <div className="rounded-md border">
      <div className="flex items-center justify-between gap-3 px-3 py-2.5">
        <button
          type="button"
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
          onClick={onToggleExpand}
        >
          <ChevronRight
            className={`h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform ${
              expanded ? "rotate-90" : ""
            }`}
          />
          <span className="min-w-0 flex-1">
            <span className="block truncate text-sm font-medium">
              {provider.name}
            </span>
            <span className="block truncate font-mono text-xs text-muted-foreground">
              {providerBaseUrl(provider)}
            </span>
          </span>
          <span className="flex shrink-0 flex-wrap items-center gap-1">
            {provider.type && (
              <Badge variant="secondary" className="text-3xs px-1.5 py-0">
                {provider.type}
              </Badge>
            )}
            <Badge variant="outline" className="text-3xs px-1.5 py-0">
              {t("keysCount", { count: credentials.length })}
            </Badge>
            {usage && (
              <Badge
                variant="outline"
                className="max-w-40 truncate text-3xs px-1.5 py-0"
              >
                {usage}
              </Badge>
            )}
          </span>
        </button>
        <div className="flex shrink-0 gap-1">
          <Button
            size="icon"
            variant="ghost"
            className="h-7 w-7"
            onClick={onEdit}
          >
            <Pencil className="h-3.5 w-3.5" />
          </Button>
          <Button
            size="icon"
            variant="ghost"
            className="h-7 w-7 text-destructive"
            onClick={onDelete}
          >
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>

      {expanded && (
        <div className="space-y-3 border-t px-3 py-3">
          {/* 凭据概览（只读 + 启用开关） */}
          <div>
            <h3 className="mb-1.5 text-xs font-medium text-muted-foreground">
              {t("credentialsTitle")}
            </h3>
            {credentials.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                {t("credentialsEmpty")}
              </p>
            ) : (
              <div className="rounded-md border">
                {credentials.map((credential, credentialIndex) => (
                  <div
                    key={`${credential.id ?? credentialIndex}`}
                    className="flex items-center justify-between gap-3 border-b px-2.5 py-1.5 text-xs last:border-b-0"
                  >
                    <span className="flex min-w-0 items-center gap-2">
                      <span className="w-5 shrink-0 text-right font-mono text-muted-foreground">
                        {credentialPriority(credential, credentialIndex)}
                      </span>
                      <span className="truncate">
                        {credentialName(credential, credentialIndex)}
                      </span>
                      <span className="shrink-0 font-mono text-muted-foreground">
                        {keyTail(credentialKey(credential))}
                      </span>
                      {Number.isFinite(credential.weight) &&
                        Number(credential.weight) !== 1 && (
                          <Badge
                            variant="outline"
                            className="text-3xs px-1.5 py-0"
                          >
                            w{String(credential.weight)}
                          </Badge>
                        )}
                      {limitsSummary(credential.limits) && (
                        <Badge
                          variant="outline"
                          className="text-3xs px-1.5 py-0"
                        >
                          {limitsSummary(credential.limits)}
                        </Badge>
                      )}
                    </span>
                    <Switch
                      checked={credential.enabled !== false}
                      disabled={toggleBusy === `${index}:${credentialIndex}`}
                      onCheckedChange={() =>
                        onToggleCredential(credentialIndex)
                      }
                    />
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* 用量 */}
          <div>
            <div className="mb-1.5 flex items-center justify-between">
              <h3 className="text-xs font-medium text-muted-foreground">
                {t("usageTitle")}
              </h3>
              <Button
                size="icon"
                variant="ghost"
                className="h-6 w-6"
                title={t("refreshUsage")}
                disabled={snapshotBusy}
                onClick={() => onLoadSnapshots(true)}
              >
                {snapshotBusy ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <RefreshCw className="h-3.5 w-3.5" />
                )}
              </Button>
            </div>
            {snapshotList.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                {snapshotBusy ? t("testing") : t("usageEmpty")}
              </p>
            ) : (
              <div className="space-y-2">
                {snapshotList.map((snapshot, snapshotIndex) => (
                  <div
                    key={`${snapshot.credentialId ?? snapshotIndex}`}
                    className="rounded-md border px-2.5 py-2"
                  >
                    <div className="flex items-center justify-between gap-2 text-xs">
                      <span className="truncate">
                        {snapshot.credentialLabel ?? snapshot.provider}
                      </span>
                      <Badge
                        variant={
                          snapshot.status === "ok" ? "secondary" : "destructive"
                        }
                        className="text-3xs px-1.5 py-0"
                      >
                        {snapshot.status}
                      </Badge>
                    </div>
                    {(snapshot.meters ?? []).map((meter) => (
                      <div
                        key={meter.id}
                        className="mt-1 flex items-center justify-between gap-2 text-xs"
                      >
                        <span className="truncate text-muted-foreground">
                          {meter.label}
                          {meter.window ? ` (${meter.window})` : ""}
                        </span>
                        <span className="shrink-0 font-mono">
                          {meterText(meter)}
                        </span>
                      </div>
                    ))}
                    {snapshot.message && (
                      <p className="mt-1 text-2xs text-muted-foreground">
                        {snapshot.message}
                      </p>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* 模型（逐行 + 启停开关） */}
          <div>
            <h3 className="mb-1.5 text-xs font-medium text-muted-foreground">
              {t("modelsTitle")}
              {listedModels.length > 0 && (
                <span className="ml-1.5">({listedModels.length})</span>
              )}
            </h3>
            {listedModels.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                {t("modelsEmpty")}
              </p>
            ) : (
              <div className="space-y-1">
                {listedModels.map((model) => (
                  <div
                    key={model}
                    className="flex items-center justify-between gap-3 rounded-md border px-2.5 py-1.5"
                  >
                    <span className="truncate font-mono text-xs">{model}</span>
                    <Switch
                      checked={models.includes(model)}
                      disabled={modelBusy !== ""}
                      onCheckedChange={(checked) =>
                        onToggleModel(model, checked)
                      }
                    />
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  )
}
