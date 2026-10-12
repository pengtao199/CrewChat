export function parseTomlStringLiteral(raw: string): string | null {
  const text = raw.trim()
  if (!text) return null

  if (text.startsWith('"')) {
    let escaped = false
    for (let i = 1; i < text.length; i += 1) {
      const ch = text[i]
      if (escaped) {
        escaped = false
        continue
      }
      if (ch === "\\") {
        escaped = true
        continue
      }
      if (ch === '"') {
        const literal = text.slice(0, i + 1)
        try {
          return JSON.parse(literal) as string
        } catch {
          return literal.slice(1, -1)
        }
      }
    }
    return null
  }

  if (text.startsWith("'")) {
    const end = text.indexOf("'", 1)
    if (end <= 0) return null
    return text.slice(1, end)
  }

  return null
}

export function parseTomlStringAssignment(
  rawLine: string
): { key: string; value: string } | null {
  const key = parseTomlAssignmentKey(rawLine)
  if (!key) return null
  const line = rawLine.trim()
  const equalsIndex = line.indexOf("=")
  const valueText = line.slice(equalsIndex + 1)
  const value = parseTomlStringLiteral(valueText)
  if (value === null) return null
  return { key, value: value.trim() }
}

export function parseTomlAssignmentKey(rawLine: string): string | null {
  const line = rawLine.trim()
  if (!line || line.startsWith("#")) return null
  const equalsIndex = line.indexOf("=")
  if (equalsIndex <= 0) return null
  const key = line.slice(0, equalsIndex).trim()
  if (!/^[A-Za-z0-9_.-]+$/.test(key)) return null
  return key
}

export function parseTomlBooleanAssignment(
  rawLine: string
): { key: string; value: boolean } | null {
  const key = parseTomlAssignmentKey(rawLine)
  if (!key) return null
  const line = rawLine.trim()
  const equalsIndex = line.indexOf("=")
  const valueText = line.slice(equalsIndex + 1).trim()
  const boolMatch = valueText.match(/^(true|false)(?:\s+#.*)?$/)
  if (!boolMatch) return null
  return { key, value: boolMatch[1] === "true" }
}

export function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
}

/**
 * Whether a line opens a new TOML table — `[table]` or `[[array]]` — allowing
 * the trailing comment TOML permits after a header. Matching `[x]` exactly
 * (what these helpers used to do) makes `[features] # flags` invisible, which
 * is not a cosmetic miss: the scanner then keeps treating the lines below it as
 * root-table keys, so an upsert appends a SECOND `[features]` and the file
 * stops parsing.
 */
export function isTomlSectionHeader(rawLine: string): boolean {
  const line = rawLine.trim()
  return line.startsWith("[") && /^\[.*\]\s*(?:#.*)?$/.test(line)
}

/**
 * The table name from a `[table]` header, or null for anything else —
 * including `[[array]]`, which is not a plain table. Surrounding whitespace is
 * insignificant in TOML (`[ features ]` names `features`), so it is trimmed.
 */
export function tomlSectionHeaderName(rawLine: string): string | null {
  const match = rawLine.trim().match(/^\[([^[\]]*)\]\s*(?:#.*)?$/)
  return match ? match[1].trim() : null
}

export function findTomlRootEndIndex(lines: string[]): number {
  for (let i = 0; i < lines.length; i += 1) {
    if (isTomlSectionHeader(lines[i])) return i
  }
  return lines.length
}

export function findTomlRootAssignmentIndex(
  lines: string[],
  key: string
): number {
  const rootEnd = findTomlRootEndIndex(lines)
  for (let i = 0; i < rootEnd; i += 1) {
    const assignmentKey = parseTomlAssignmentKey(lines[i])
    if (assignmentKey === key) return i
  }
  return -1
}

export function preferredTomlRootInsertionIndex(
  lines: string[],
  key: string
): number {
  if (key === "model") {
    const providerIndex = findTomlRootAssignmentIndex(lines, "model_provider")
    return providerIndex >= 0 ? providerIndex : 0
  }
  let insertAt = findTomlRootEndIndex(lines)
  while (insertAt > 0 && lines[insertAt - 1].trim() === "") {
    insertAt -= 1
  }
  return insertAt
}

export function updateTomlRootStringKey(
  configTomlText: string,
  key: string,
  value: string
): string {
  const lineText = `${key} = ${JSON.stringify(value)}`
  const lines = configTomlText.split(/\r?\n/)
  const assignmentIndex = findTomlRootAssignmentIndex(lines, key)

  const nextValue = value.trim()
  if (!nextValue) {
    if (assignmentIndex >= 0) {
      lines.splice(assignmentIndex, 1)
    }
    return lines.join("\n").trim()
  }

  const insertAt = preferredTomlRootInsertionIndex(lines, key)
  if (assignmentIndex >= 0) {
    lines[assignmentIndex] = lineText
  } else {
    lines.splice(Math.max(0, insertAt), 0, lineText)
  }
  return lines.join("\n").trim()
}

export function updateTomlRootBooleanKey(
  configTomlText: string,
  key: string,
  value: boolean
): string {
  const lineText = `${key} = ${value ? "true" : "false"}`
  const lines = configTomlText.split(/\r?\n/)
  const assignmentIndex = findTomlRootAssignmentIndex(lines, key)
  if (assignmentIndex >= 0) {
    lines[assignmentIndex] = lineText
  } else {
    lines.splice(0, 0, lineText)
  }
  return lines.join("\n").trim()
}

export function findTomlSectionRange(
  lines: string[],
  sectionName: string
): { start: number; end: number } | null {
  let sectionStart = -1
  let sectionEnd = lines.length
  for (let i = 0; i < lines.length; i += 1) {
    if (sectionStart < 0) {
      if (tomlSectionHeaderName(lines[i]) === sectionName) {
        sectionStart = i
      }
      continue
    }
    if (isTomlSectionHeader(lines[i])) {
      sectionEnd = i
      break
    }
  }
  if (sectionStart < 0) return null
  return { start: sectionStart, end: sectionEnd }
}

export function removeTomlSection(
  configTomlText: string,
  sectionName: string
): string {
  const lines = configTomlText.split(/\r?\n/)
  const range = findTomlSectionRange(lines, sectionName)
  if (!range) return configTomlText
  // Remove blank line before section header if present
  const removeStart =
    range.start > 0 && lines[range.start - 1].trim() === ""
      ? range.start - 1
      : range.start
  lines.splice(removeStart, range.end - removeStart)
  return lines.join("\n").trim()
}

/**
 * Drop any ROOT-level `<section>.<key> = …` line — the dotted spelling of the
 * very key this upsert is about to write into `[section]`.
 *
 * TOML treats `features.skills = true` and `[features]` + `skills = true` as
 * the same key, but they cannot coexist: the dotted form defines the `features`
 * table implicitly, so a later `[features]` header is a hard
 * "trying to redefine an already defined table" parse error. Leaving the dotted
 * line in place therefore breaks both directions — turning the switch OFF would
 * not remove the value the reader still sees (the control snaps back), and
 * turning it ON would emit a config.toml the backend refuses to persist.
 *
 * Only lines above the FIRST section header are considered: inside
 * `[model_providers.codeg]`, `features.skills` means
 * `model_providers.codeg.features.skills`, an unrelated key we must not touch.
 */
export function stripRootDottedKey(
  lines: string[],
  sectionName: string,
  key: string
): void {
  const dotted = `${sectionName}.${key}`
  for (let i = 0; i < lines.length; i += 1) {
    if (isTomlSectionHeader(lines[i])) return
    if (parseTomlAssignmentKey(lines[i]) === dotted) {
      lines.splice(i, 1)
      i -= 1
    }
  }
}

/**
 * Index of the last ROOT-level `<section>.…` dotted assignment, or -1.
 *
 * A surviving sibling means the root table already defines `[section]`
 * implicitly, so emitting a `[section]` header would be a redefinition. Writing
 * the new key in the same dotted spelling keeps the document valid and leaves
 * the sibling — someone else's setting — exactly where the user put it.
 */
export function lastRootDottedSiblingIndex(
  lines: string[],
  sectionName: string
): number {
  const prefix = `${sectionName}.`
  let last = -1
  for (let i = 0; i < lines.length; i += 1) {
    if (isTomlSectionHeader(lines[i])) break
    if (parseTomlAssignmentKey(lines[i])?.startsWith(prefix)) last = i
  }
  return last
}

export function upsertTomlSectionBooleanKey(
  configTomlText: string,
  sectionName: string,
  key: string,
  value: boolean | null
): string {
  const lines = configTomlText.split(/\r?\n/)
  stripRootDottedKey(lines, sectionName, key)
  const section = findTomlSectionRange(lines, sectionName)

  if (section) {
    let assignmentIndex = -1
    for (let i = section.start + 1; i < section.end; i += 1) {
      const assignmentKey = parseTomlAssignmentKey(lines[i])
      if (assignmentKey === key) {
        assignmentIndex = i
        break
      }
    }

    if (value === null) {
      if (assignmentIndex >= 0) {
        lines.splice(assignmentIndex, 1)
      }
      const refreshedSection = findTomlSectionRange(lines, sectionName)
      if (refreshedSection) {
        const hasEntries = lines
          .slice(refreshedSection.start + 1, refreshedSection.end)
          .some((rawLine) => {
            const line = rawLine.trim()
            return line !== "" && !line.startsWith("#")
          })
        if (!hasEntries) {
          const before = lines.slice(0, refreshedSection.start)
          const after = lines.slice(refreshedSection.end)
          while (before.length > 0 && before[before.length - 1].trim() === "") {
            before.pop()
          }
          while (after.length > 0 && after[0].trim() === "") {
            after.shift()
          }
          const merged =
            before.length > 0 && after.length > 0
              ? [...before, "", ...after]
              : [...before, ...after]
          return merged.join("\n").trim()
        }
      }
      return lines.join("\n").trim()
    }

    const lineText = `${key} = ${value ? "true" : "false"}`
    if (assignmentIndex >= 0) {
      lines[assignmentIndex] = lineText
    } else {
      let insertAt = section.end
      for (let i = section.end - 1; i > section.start; i -= 1) {
        if (lines[i].trim() !== "") {
          insertAt = i + 1
          break
        }
      }
      lines.splice(insertAt, 0, lineText)
    }
    return lines.join("\n").trim()
  }

  // No `[section]` to edit. Removing is still not a no-op: a root-level dotted
  // spelling of this key may have just been stripped above, and that IS the
  // value the reader was showing.
  if (value === null) {
    return lines.join("\n").trim()
  }

  const lineText = `${key} = ${value ? "true" : "false"}`

  // The root table may already define this section through a dotted sibling we
  // must not touch (`features.skills = true` while we write
  // `features.default_mode_request_user_input`). Join it in its own spelling
  // rather than opening a header that would redefine the table.
  const sibling = lastRootDottedSiblingIndex(lines, sectionName)
  if (sibling >= 0) {
    lines.splice(sibling + 1, 0, `${sectionName}.${lineText}`)
    return lines.join("\n").trim()
  }

  const insertAt = findTomlRootEndIndex(lines)
  const prefixBlank =
    insertAt > 0 && lines[insertAt - 1].trim() !== "" ? [""] : []
  const suffixBlank =
    insertAt < lines.length && lines[insertAt].trim() !== "" ? [""] : []
  lines.splice(
    insertAt,
    0,
    ...prefixBlank,
    `[${sectionName}]`,
    lineText,
    ...suffixBlank
  )
  return lines.join("\n").trim()
}
