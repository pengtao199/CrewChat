import type { AcpAgentInfo, AgentLatestRelease } from "@/lib/types"

export function compareVersion(a: string, b: string): number {
  const toParts = (value: string): number[] => {
    const normalized = value.trim().replace(/^[^\d]*/, "")
    return normalized.split(".").map((part) => Number.parseInt(part, 10) || 0)
  }
  const left = toParts(a)
  const right = toParts(b)
  const len = Math.max(left.length, right.length)
  for (let i = 0; i < len; i += 1) {
    const lv = left[i] ?? 0
    const rv = right[i] ?? 0
    if (lv !== rv) return lv > rv ? 1 : -1
  }
  return 0
}

export function hasComparableVersion(
  value: string | null | undefined
): value is string {
  return Boolean(value && /\d/.test(value) && value.includes("."))
}

// Mirror of the backend `sanitize_custom_version`: a custom install version
// tolerates a leading `v`, must start with a digit, must be dotted (e.g.
// `1.2.3`), and may only contain `[0-9A-Za-z.-+]` (semver pre-release/build +
// calendar versions). Rejects npm dist-tags like `latest`, bare majors like
// `2`, and anything with spaces / `@`.
export function isValidCustomVersion(value: string): boolean {
  const normalized = value.trim().replace(/^[vV]/, "")
  return /^[0-9][0-9A-Za-z.\-+]*$/.test(normalized) && normalized.includes(".")
}

// SemVer 2.0.0; group 4 is the prerelease.
export const SEMVER_PATTERN =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+[0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*)?$/

// Digit strings compared as numbers, without Number's precision limits.
export function compareDigits(a: string, b: string): number {
  const left = a.replace(/^0+(?=\d)/, "")
  const right = b.replace(/^0+(?=\d)/, "")
  if (left.length !== right.length) return left.length > right.length ? 1 : -1
  return left === right ? 0 : left > right ? 1 : -1
}

export function comparePrerelease(
  a: string | undefined,
  b: string | undefined
): number {
  if (a === b) return 0
  // A release outranks its own prereleases.
  if (a === undefined) return 1
  if (b === undefined) return -1
  const left = a.split(".")
  const right = b.split(".")
  for (let i = 0; i < Math.max(left.length, right.length); i += 1) {
    const l = left[i]
    const r = right[i]
    if (l === undefined) return -1
    if (r === undefined) return 1
    const lNumeric = /^\d+$/.test(l)
    const rNumeric = /^\d+$/.test(r)
    if (lNumeric && rNumeric) {
      const order = compareDigits(l, r)
      if (order !== 0) return order
    } else if (lNumeric !== rNumeric) {
      return lNumeric ? -1 : 1
    } else if (l !== r) {
      return l > r ? 1 : -1
    }
  }
  return 0
}

/**
 * Which of two release versions is newer: semver precedence when both parse
 * as semver (so `0.85.0-rc.1` sorts below `0.85.0`), otherwise the leading
 * digits of each dot-separated segment, compared in order. Equal digits
 * throughout is equal even when a suffix differs, so Cursor's same-day builds
 * (`2026.09.26-dd393fe`) tie. Same rules as the backend's `is_strictly_newer`,
 * which decides what the unreviewed-latest offer carries. `compareVersion`
 * ignores prerelease suffixes, which is why this exists.
 *
 * Orders versions; it does not tell whether two strings name the same build.
 */
export function compareReleaseVersion(a: string, b: string): number {
  const left = a.trim().replace(/^[vV]+/, "")
  const right = b.trim().replace(/^[vV]+/, "")
  const l = SEMVER_PATTERN.exec(left)
  const r = SEMVER_PATTERN.exec(right)
  if (l && r) {
    for (let i = 1; i <= 3; i += 1) {
      const order = compareDigits(l[i], r[i])
      if (order !== 0) return order
    }
    return comparePrerelease(l[4], r[4])
  }
  const segments = (value: string) =>
    value.split(".").map((segment) => /^\d*/.exec(segment)?.[0] || "0")
  const ls = segments(left)
  const rs = segments(right)
  for (let i = 0; i < Math.max(ls.length, rs.length); i += 1) {
    const order = compareDigits(ls[i] ?? "0", rs[i] ?? "0")
    if (order !== 0) return order
  }
  return 0
}

/**
 * Whether Version Status can offer this agent's unreviewed latest release.
 * Custom install has to be able to fetch it (an npx package or a binary whose
 * URL carries its version, never uvx), and there has to be a recommended
 * version to be newer than, which a manually written definition does not
 * have. Also decides whether the settings page asks the backend at all.
 */
export function canCheckLatestRelease(agent: AcpAgentInfo): boolean {
  return (
    (agent.distribution_type === "npx" ||
      agent.distribution_type === "binary") &&
    agent.supports_custom_version &&
    agent.custom_source !== "manual" &&
    agent.available &&
    Boolean(agent.registry_version)
  )
}

/**
 * The unreviewed latest version to offer, or `null`. The backend already
 * vouches that `release` is newer than the recommended version and that Custom
 * install accepts it; this only drops the offer once the installed version
 * reaches it. An installed version that cannot be compared keeps the offer,
 * and so does an agent that is not installed.
 */
export function latestReleaseOffer(
  agent: AcpAgentInfo,
  release: AgentLatestRelease | null | undefined
): string | null {
  if (!release || !canCheckLatestRelease(agent)) return null
  const installed = agent.installed_version
  if (
    installed &&
    hasComparableVersion(installed) &&
    compareReleaseVersion(installed, release.version) >= 0
  ) {
    return null
  }
  return release.version
}
