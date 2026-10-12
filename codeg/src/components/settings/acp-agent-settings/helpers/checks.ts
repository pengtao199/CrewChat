import type {
  AcpAgentInfo,
  AdapterInfo,
  AgentLatestRelease,
  CheckStatus,
  ModelProviderInfo,
} from "@/lib/types"

import { acpText } from "./context"
import {
  compareVersion,
  hasComparableVersion,
  latestReleaseOffer,
} from "./version"
import type {
  AgentCheckState,
  RunningActionKind,
  UiCheckItem,
  UiFixAction,
} from "./types"

/**
 * Fix kinds that run a package operation. Only one of these may run at a time
 * across ALL agents, so while any of them is busy anywhere, every button whose
 * kind is listed here is disabled — and dimmed, so the lockout is visible on
 * agents other than the busy one.
 */
export const PACKAGE_ACTION_FIX_KINDS: ReadonlyArray<UiFixAction["kind"]> = [
  "download_binary",
  "upgrade_binary",
  "install_npx",
  "upgrade_npx",
  "uninstall_binary",
  "uninstall_npx",
  "redownload_binary",
  "install_opencode_plugins",
  "custom_install",
  "upgrade_latest",
  "install_uv",
]

export function summarizeChecks(
  checks: UiCheckItem[]
): CheckStatus | "unchecked" {
  if (checks.length === 0) return "unchecked"
  if (checks.some((check) => check.status === "fail")) return "fail"
  if (checks.some((check) => check.status === "warn")) return "warn"
  return "pass"
}

/**
 * The explainer card for agents whose codeg entry is a third-party ACP
 * *adapter* rather than the vendor's own CLI — Claude Code and Codex.
 *
 * Ten of the twelve built-ins install the vendor CLI itself, so a user's
 * existing global install is simply detected. These two are the exception:
 * neither `claude` nor `codex` speaks ACP, so codeg installs `claude-agent-acp`
 * / `codex-acp` instead, and the launch gate looks for THAT command. Without
 * this card the user only sees "Not installed" next to an agent they demonstrably
 * have — by far the most-reported confusion.
 *
 * Returns `null` for every non-adapter agent (backend decides, via
 * `PreflightResult.adapter`), and while preflight hasn't resolved yet.
 *
 * Deliberately carries NO install action: the Version Status card directly below
 * already has one, and two install buttons on adjacent cards only breeds doubt
 * about which is the right one.
 */
export function buildAcpAdapterCheck(
  adapter: AdapterInfo | null | undefined
): UiCheckItem | null {
  if (!adapter) return null

  const values = {
    nativeLabel: adapter.native_label,
    nativeCmd: adapter.native_cmd,
    nativePath: adapter.native_path ?? "",
    adapterPackage: adapter.adapter_package,
    adapterCmd: adapter.adapter_cmd,
    configDir: adapter.shared_config_dir,
  }

  // Installed → `pass`, so renderCheck collapses it: the relationship stays
  // documented for anyone who wonders later, without nagging a working setup.
  const installed = adapter.adapter_installed
  const sawNative = Boolean(adapter.native_path)
  // The English fallbacks mirror the four i18n messages one-for-one (they are
  // what renders if no translator is mounted), so each state keeps the detail
  // that state is about — above all, the path we found the vendor CLI at.
  const split = `Codeg drives agents over ACP and the ${adapter.native_label} does not speak ACP, so Codeg needs a separate adapter package, ${adapter.adapter_package}.`
  const coexist = `It ships its own runtime, never modifies or replaces your ${adapter.native_cmd} command, and reads the same ${adapter.shared_config_dir} — your existing sign-in and settings carry over.`
  const [key, fallback] = installed
    ? sawNative
      ? [
          "adapter.readyWithNative",
          `Adapter ${adapter.adapter_cmd} is installed — that is what Codeg launches, not your own ${adapter.native_cmd} at ${adapter.native_path}. They are separate packages that coexist, and both read ${adapter.shared_config_dir}.`,
        ]
      : [
          "adapter.ready",
          `Adapter ${adapter.adapter_cmd} is installed — that is what Codeg launches. It ships its own runtime, so the ${adapter.native_label} is not required.`,
        ]
    : sawNative
      ? [
          "adapter.missingWithNative",
          `Found your own ${adapter.native_label} at ${adapter.native_path}. ${split} ${coexist} Install it below.`,
        ]
      : [
          "adapter.missing",
          `${split} It ships its own runtime, so the ${adapter.native_cmd} CLI is not required first; if you do have it, the two coexist and share the same ${adapter.shared_config_dir} sign-in and settings. Install it below.`,
        ]

  return {
    check_id: "acp_adapter",
    label: acpText("adapter.label", "ACP adapter"),
    status: installed ? "pass" : "warn",
    message: acpText(key, fallback, values),
    fixes: [
      {
        label: acpText("adapter.learnMore", "Learn more"),
        kind: "open_url",
        payload: adapter.docs_url,
      },
    ],
  }
}

/**
 * Which provider an agent should land on when it returns to "model_provider"
 * auth mode with no binding in the draft.
 *
 * In order: `lastPick`, the user's own last pick for this agent in this panel;
 * `savedBinding`, the provider the agent is bound to on disk; then the head of
 * the list. Each candidate counts only while it is still listed (not deleted,
 * not moved to another agent), so a stale pick falls through to a saved
 * binding that is still good rather than straight to the head. Going straight
 * to `available[0]` rebound the agent to its OLDEST provider (the list arrives
 * ordered by row id) whenever the auth-mode dropdown round-tripped through
 * another mode, and the rebind copies that provider's model names into the
 * draft, the env text and the config text. The head stays the fallback for a
 * first-time pick.
 */
export function providerToRebindTo(
  available: readonly ModelProviderInfo[],
  lastPick: number | null | undefined,
  savedBinding: number | null | undefined
): ModelProviderInfo | null {
  const listed = (id: number | null | undefined) =>
    id == null ? undefined : available.find((provider) => provider.id === id)
  return listed(lastPick) ?? listed(savedBinding) ?? available[0] ?? null
}

// `uvReady` reports whether the uv runtime (uvx) is installed — only meaningful
// for uvx agents (custom Python-package agents; built-in Hermes moved to the
// npm bridge). Derived from the uv preflight check by the caller. uvx agents
// need uv installed before their package can be prepared, so when uv isn't
// ready every managed install/upgrade action is surfaced disabled and the
// user is pointed at the separate "Install uv" preflight action.
//
// `latestRelease` is the unreviewed latest release looked up when the user
// opened this agent, if one was found (see `latestReleaseOffer`).
export function buildVersionCheck(
  agent: AcpAgentInfo,
  uvReady: boolean = true,
  latestRelease: AgentLatestRelease | null = null
): UiCheckItem | null {
  if (
    agent.distribution_type !== "binary" &&
    agent.distribution_type !== "npx" &&
    agent.distribution_type !== "uvx"
  )
    return null

  // The version codeg has reviewed and pinned for this agent.
  const recommendedVersion = agent.registry_version ?? "unknown"
  const localVersion =
    agent.installed_version ?? acpText("version.notInstalled", "Not installed")
  // A manually written definition has no registry behind it — its stored
  // version is whatever the user typed — so "Recommended:" would be comparing
  // against noise. Every message shows the local side alone.
  const manualSource = agent.custom_source === "manual"
  const versionText = manualSource
    ? acpText("version.localOnly", "Local: {localVersion}", { localVersion })
    : acpText(
        "version.recommendedLocal",
        "Recommended: {recommendedVersion} · Local: {localVersion}",
        { recommendedVersion, localVersion }
      )
  const installAction: RunningActionKind =
    agent.distribution_type === "binary" ? "download_binary" : "install_npx"
  const upgradeAction: RunningActionKind =
    agent.distribution_type === "binary" ? "upgrade_binary" : "upgrade_npx"
  const uninstallAction: RunningActionKind =
    agent.distribution_type === "binary" ? "uninstall_binary" : "uninstall_npx"

  // uvx agents need the uv runtime before any managed install/upgrade can
  // run. Surface a single blocked state pointing at the separate "Install
  // uv" preflight action below, with the agent-install action shown disabled.
  // This covers both the fresh case (available=false) and the rare system-CLI
  // case (available=true via the agent's own PATH CLI, but uvx still missing).
  // Uninstall stays available even without uv — it only clears the prepared
  // marker — so a prepared package can still be removed when uv is gone.
  if (agent.distribution_type === "uvx" && !uvReady) {
    const blockedFixes: UiFixAction[] = [
      {
        label: acpText("actions.install", "Install"),
        kind: installAction,
        payload: agent.agent_type,
        disabled: true,
      },
    ]
    if (agent.installed_version) {
      blockedFixes.push({
        label: acpText("actions.uninstall", "Uninstall"),
        kind: uninstallAction,
        payload: agent.agent_type,
      })
    }
    return {
      check_id: "version_status",
      label: acpText("version.statusLabel", "Version Status"),
      status: "warn",
      message: acpText(
        "version.uvxNotReady",
        "{versionText}. The uv runtime isn't installed — install it from the uv check below to use this agent.",
        { versionText }
      ),
      fixes: blockedFixes,
    }
  }

  // Only binary agents can be genuinely platform-unsupported (no binary for
  // this platform). uvx runs everywhere — a uvx agent that reaches here (uv
  // treated as ready, i.e. preflight unknown) falls through to an actionable
  // install rather than a dead-end "unsupported" message.
  if (!agent.available && agent.distribution_type !== "uvx") {
    return {
      check_id: "version_status",
      label: acpText("version.statusLabel", "Version Status"),
      status: "fail",
      message: acpText(
        "version.platformUnsupported",
        "{versionText}. Current platform does not support this agent.",
        { versionText }
      ),
      fixes: [],
    }
  }

  // Custom-version install is offered in every installable state (and stays
  // available after a version is installed, so users can switch versions).
  //
  // The backend decides, because the condition is a property of the download
  // URL rather than of the distribution kind: a binary agent's custom install
  // substitutes the requested version into the pinned URL, which only yields a
  // different archive when the pinned version appears in it. Antigravity's URLs
  // carry a Google build id, so inferring support from
  // `binary && registry_version` — as this did — offered an install that
  // downloaded the same bytes and cached them under the number the user typed.
  const supportsCustomInstall = agent.supports_custom_version
  const customInstallFix: UiFixAction = {
    label: acpText("actions.customInstall", "Custom install"),
    kind: "custom_install",
    payload: agent.agent_type,
  }
  // Offered on the card's bottom row, which it shares with Custom install:
  // both install a version other than the recommended one, through the same
  // path. The offer never changes a branch's status, which still judges the
  // installed version against the recommended one.
  const latestVersion = latestReleaseOffer(agent, latestRelease)
  const footerFixes: UiFixAction[] = []
  if (latestVersion) {
    footerFixes.push({
      label: acpText(
        "actions.upgradeToLatest",
        "Upgrade to unreviewed latest {version}",
        { version: latestVersion }
      ),
      kind: "upgrade_latest",
      payload: latestVersion,
    })
  }
  if (supportsCustomInstall) footerFixes.push(customInstallFix)

  if (!agent.installed_version) {
    return {
      check_id: "version_status",
      label: acpText("version.statusLabel", "Version Status"),
      status: "fail",
      message: acpText(
        "version.clickInstall",
        "{versionText}. Click Install on the right.",
        { versionText }
      ),
      fixes: [
        {
          label: acpText("actions.install", "Install"),
          kind: installAction,
          payload: agent.agent_type,
        },
      ],
      footerFixes,
    }
  }

  // Manual definitions stop here: installed is the whole story, and the
  // registry-comparison branches below would only manufacture "upgrade
  // available" noise against a user-typed version.
  if (manualSource) {
    return {
      check_id: "version_status",
      label: acpText("version.statusLabel", "Version Status"),
      status: "pass",
      message: acpText("version.localInstalled", "{versionText}. Installed.", {
        versionText,
      }),
      fixes: [
        {
          label: acpText("actions.uninstall", "Uninstall"),
          kind: uninstallAction,
          payload: agent.agent_type,
        },
      ],
      footerFixes,
    }
  }

  if (
    agent.registry_version &&
    hasComparableVersion(agent.registry_version) &&
    !hasComparableVersion(agent.installed_version)
  ) {
    return {
      check_id: "version_status",
      label: acpText("version.statusLabel", "Version Status"),
      status: "warn",
      message: acpText(
        "version.localUnrecognized",
        "{versionText}. Local version is not comparable; try upgrade to overwrite install.",
        { versionText }
      ),
      fixes: [
        {
          label: acpText("actions.upgrade", "Upgrade"),
          kind: upgradeAction,
          payload: agent.agent_type,
        },
        {
          label: acpText("actions.uninstall", "Uninstall"),
          kind: uninstallAction,
          payload: agent.agent_type,
        },
      ],
      footerFixes,
    }
  }

  if (
    hasComparableVersion(agent.registry_version) &&
    hasComparableVersion(agent.installed_version) &&
    compareVersion(agent.installed_version, agent.registry_version) < 0
  ) {
    return {
      check_id: "version_status",
      label: acpText("version.statusLabel", "Version Status"),
      status: "warn",
      message: acpText(
        "version.upgradeAvailable",
        "{versionText}. Upgrade available.",
        { versionText }
      ),
      fixes: [
        {
          label: acpText("actions.upgrade", "Upgrade"),
          kind: upgradeAction,
          payload: agent.agent_type,
        },
        {
          label: acpText("actions.uninstall", "Uninstall"),
          kind: uninstallAction,
          payload: agent.agent_type,
        },
      ],
      footerFixes,
    }
  }

  if (!agent.registry_version) {
    return {
      check_id: "version_status",
      label: acpText("version.statusLabel", "Version Status"),
      status: "warn",
      message: acpText(
        "version.recommendedUnavailable",
        "{versionText}. No recommended version is available.",
        { versionText }
      ),
      fixes: [
        {
          label: acpText("actions.uninstall", "Uninstall"),
          kind: uninstallAction,
          payload: agent.agent_type,
        },
      ],
      footerFixes,
    }
  }

  // At or past the recommended version. "Already latest" would contradict an
  // offer of a newer release beside it, so name the newer release instead.
  return {
    check_id: "version_status",
    label: acpText("version.statusLabel", "Version Status"),
    status: "pass",
    message: latestVersion
      ? acpText(
          "version.unreviewedAvailable",
          "{versionText}. A newer, unreviewed release is available.",
          { versionText }
        )
      : acpText("version.latest", "{versionText}. Already latest.", {
          versionText,
        }),
    fixes: [
      {
        label: acpText("actions.uninstall", "Uninstall"),
        kind: uninstallAction,
        payload: agent.agent_type,
      },
    ],
    footerFixes,
  }
}

export function getAgentChecks(
  agent: AcpAgentInfo,
  current?: AgentCheckState,
  latestRelease?: AgentLatestRelease | null
): UiCheckItem[] {
  // For uvx agents, only treat uv as not-ready when the preflight result is
  // present AND its uv check isn't passing. With no result yet (or an errored
  // preflight) stay optimistic — otherwise we'd block the version-status
  // install while the "Install uv" button (which lives in that same preflight
  // result) is absent, a dead end. When the result IS present, the button is
  // present alongside it, so blocking is always paired with an actionable fix.
  const uvCheck = current?.result?.checks?.find(
    (check) => check.check_id === "uv_available"
  )
  const uvReady =
    agent.distribution_type !== "uvx" || !uvCheck || uvCheck.status === "pass"
  const versionCheck = buildVersionCheck(agent, uvReady, latestRelease ?? null)
  const remoteChecks: UiCheckItem[] = (current?.result?.checks ?? []).map(
    (check) => ({
      ...check,
      fixes: [...check.fixes],
    })
  )
  // The adapter explainer goes FIRST: it answers "why does this say not
  // installed when I have the CLI?" before the Version Status card below it
  // offers the Install that fixes it.
  const adapterCheck = buildAcpAdapterCheck(current?.result?.adapter)
  return [adapterCheck, versionCheck, ...remoteChecks].filter(
    (check): check is UiCheckItem => check != null
  )
}
