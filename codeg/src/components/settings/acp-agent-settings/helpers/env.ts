/**
 * Per-agent `env_json` knob deciding WHICH SIDE of the ACP connection reads
 * files and runs commands (`HostToolsPolicy`, Rust side). codeg advertises
 * `fs.readTextFile` / `terminal` by default, and an agent that sees them stops
 * using its own backends and delegates — so the work happens in CODEG's
 * process, outside any OS sandbox the agent applies to itself. Set to
 * {@link HOST_TOOLS_AGENT} and codeg advertises neither, so the agent does its
 * own I/O and its own sandbox covers it again (#436). Absent ⇒ codeg hosts.
 */
export const HOST_TOOLS_ENV = "CODEG_ACP_HOST_TOOLS"

export const HOST_TOOLS_AGENT = "agent"

export const HOST_TOOLS_DEFAULT = "default"

export function envMapToText(env: Record<string, string>): string {
  return Object.entries(env)
    .map(([key, value]) => `${key}=${value}`)
    .join("\n")
}

export function parseEnvText(envText: string): Record<string, string> {
  const map: Record<string, string> = {}
  for (const rawLine of envText.split(/\r?\n/)) {
    const line = rawLine.trim()
    if (!line || line.startsWith("#")) continue
    const idx = line.indexOf("=")
    if (idx <= 0) continue
    const key = line.slice(0, idx).trim()
    const value = line.slice(idx + 1).trim()
    if (!key) continue
    map[key] = value
  }
  return map
}

/**
 * Set (or, for an empty value, delete) exactly the given keys in a raw env
 * draft, leaving every other LINE byte-identical.
 *
 * Textual on purpose. The obvious implementation — parse to a map, patch,
 * serialize — rewrites the whole textarea, and the parser only understands
 * `KEY=VALUE`: a comment, a blank line, and a half-typed `NEW_PROXY` all
 * vanish. These patches run on refresh and on save completion, so that would
 * silently delete what the user is still typing in the raw editor next to the
 * structured panel that triggered the save.
 *
 * A key appearing on several lines collapses to one (its patched value), which
 * matches how `parseEnvText` reads the draft afterwards.
 */
export function patchEnvText(
  envText: string,
  patch: Record<string, string | undefined>
): string {
  // `key in patch` would also answer yes for `constructor`, `toString` and the
  // rest of Object.prototype — all of them legal env var names — and then read
  // a function where a string was expected. Own properties only.
  const owns = (key: string) => Object.prototype.hasOwnProperty.call(patch, key)
  const pending = new Set(
    Object.keys(patch).filter((key) => (patch[key]?.trim() ?? "") !== "")
  )
  const lines = envText === "" ? [] : envText.split(/\r?\n/)
  const kept: string[] = []
  for (const rawLine of lines) {
    const line = rawLine.trim()
    const idx = line.startsWith("#") ? -1 : line.indexOf("=")
    const key = idx > 0 ? line.slice(0, idx).trim() : ""
    if (!key || !owns(key)) {
      kept.push(rawLine)
      continue
    }
    const value = patch[key]?.trim() ?? ""
    // Empty ⇒ the key is being removed; a duplicate line for a key already
    // emitted goes too, so the result reads back as the value just written.
    if (!value || !pending.delete(key)) continue
    kept.push(`${key}=${value}`)
  }
  if (pending.size > 0) {
    // A key with no line yet goes after the last real one, not after the blank
    // line the user may be about to type into.
    let end = kept.length
    while (end > 0 && kept[end - 1].trim() === "") end -= 1
    const tail = kept.splice(end)
    for (const key of pending) kept.push(`${key}=${patch[key]?.trim() ?? ""}`)
    kept.push(...tail)
  }
  return kept.join("\n")
}

/**
 * Whether this agent's env draft hands the ACP fs/terminal channels back to the
 * agent — see {@link HOST_TOOLS_ENV}. Anything other than the exact sentinel
 * (including a hand-typed `default`) reads as off, matching the Rust resolver,
 * which fails OPEN on an unrecognized value rather than silently withholding.
 *
 * Reads the per-agent layer ONLY. When the key is absent and an operator has
 * exported `CODEG_ACP_HOST_TOOLS=agent` in codeg's own environment, the switch
 * renders off while the next connection actually withholds the channels — the
 * display understates how restricted the agent is. Showing that inherited state
 * would need the backend to report its resolved process-env value; until then
 * the error is in the safe direction, and {@link setHostToolsAgentMode} makes
 * the per-agent value authoritative the moment the user touches the switch.
 */
export function hostToolsAgentModeEnabled(envText: string): boolean {
  return parseEnvText(envText)[HOST_TOOLS_ENV] === HOST_TOOLS_AGENT
}

/**
 * Flip the knob in an env draft, always writing an EXPLICIT value — including
 * `default` for off, rather than deleting the key.
 *
 * Deleting would be tidier but wrong: the backend resolves this knob as
 * `env_json` first, then codeg's own process env. An operator who exported
 * `CODEG_ACP_HOST_TOOLS=agent` process-wide makes "absent" mean `agent`, so a
 * toggle that cleared the key on OFF could not turn the mode off at all — the
 * switch would read false while the next connection still withheld the
 * channels. Writing the value the user actually chose makes the per-agent
 * setting authoritative in both directions.
 */
export function setHostToolsAgentMode(
  envText: string,
  enabled: boolean
): string {
  return patchEnvText(envText, {
    [HOST_TOOLS_ENV]: enabled ? HOST_TOOLS_AGENT : HOST_TOOLS_DEFAULT,
  })
}
