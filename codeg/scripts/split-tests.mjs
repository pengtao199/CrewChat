#!/usr/bin/env node
// [crewchat] Mechanical extraction of inline test modules from Rust sources.
//
// What it does — and what it refuses to do:
//   - Finds module-shaped blocks `#[cfg(test)] mod <name> { … }` at the TOP
//     LEVEL of a .rs file. Item-level attrs (`#[cfg(test)] fn …` — a test
//     helper living in source) and already-external declarations
//     (`#[cfg(test)] mod <name>;`) are reported but never touched.
//   - extract moves each module's BODY (the items between the braces, bytes
//     untouched) into a sibling file `…/<name>.rs` (for a `foo.rs` parent,
//     into the `foo/` dir the compiler already looks in) and replaces the
//     block in place with `#[cfg(test)] mod <name>;` — the declaration keeps
//     the name and the cfg, so the wrapper text itself must NOT travel into
//     the file (that would nest the path: `foo::<name>::<name>`). The module
//     path is identical before and after (`foo::<name>`), so name resolusion,
//     `use super::*` chains and verify.mjs's per-module cargo filters are
//     unchanged — only the file location moves.
//   - No reordering, no reformatting, no rewriting. Anything unexpected
//     (unbalanced braces, existing target file, `include_str!` inside the
//     block, uncommitted changes in the source file) aborts instead of
//     guessing. Default is a dry run; `--apply` performs the writes.
//
// Usage (from `codeg/`):
//   node scripts/split-tests.mjs selftest
//   node scripts/split-tests.mjs scan
//   node scripts/split-tests.mjs extract <file…>            # plan only
//   node scripts/split-tests.mjs extract <file…> --apply    # perform
//
// After applying, register the move in NOTICE.md's 结构拆分登记 and run the
// printed verify.mjs command (module paths — and therefore its cargo
// filters — are unchanged by this move).

import { execFileSync } from "node:child_process"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

const CODEG = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..")
const SRC = path.join(CODEG, "src-tauri", "src")
const REPO = path.resolve(CODEG, "..")

const CFG_TEST = "#[cfg(test)]"

// ── Rust scanner (pure: source text in, block list out) ────────────────
//
// The scanner tracks only the states that can hide braces from the parser:
// comments (nested), " / r#" / b" strings, char/lifetime quotes. Everything
// else is a one-character advance, which keeps procedure braces exact.

function skipLineComment(src, i) {
  const nl = src.indexOf("\n", i)
  return nl === -1 ? src.length : nl + 1
}

function skipBlockComment(src, i) {
  let depth = 0
  let j = i
  while (j < src.length) {
    if (src.startsWith("/*", j)) {
      depth++
      j += 2
    } else if (src.startsWith("*/", j)) {
      depth--
      j += 2
      if (depth === 0) return j
    } else {
      j++
    }
  }
  return src.length // unbalanced; caller sees no progress and aborts
}

function skipString(src, quoteIdx) {
  let j = quoteIdx + 1
  while (j < src.length) {
    const c = src[j]
    if (c === "\\") {
      j += 2
    } else if (c === '"') {
      return j + 1
    } else {
      j++
    }
  }
  return src.length
}

function skipRawString(src, rIdx) {
  let hashes = 0
  let j = rIdx + 1
  while (src[j] === "#") {
    hashes++
    j++
  }
  if (src[j] !== '"') return rIdx + 1 // not a raw string after all
  j++ // past the opening quote
  const closers = '"' + "#".repeat(hashes)
  const end = src.indexOf(closers, j)
  return end === -1 ? src.length : end + closers.length
}

function skipCharOrLifetime(src, quoteIdx) {
  let j = quoteIdx + 1
  if (src[j] === "\\") {
    j++
    if (src[j] === "{") return src.indexOf("}", j) + 1
    if (src[j] === "u") {
      const close = src.indexOf("}", j)
      if (close === -1) return src.length
      j = close + 1
    } else {
      j += 2 // \n, \t, \\, \' … one escaped char
    }
    if (src[j] === "'") return j + 1
    return j
  }
  if (src[j] !== "\\" && src[j + 1] === "'") return j + 2 // 'x'
  j += 1 // lifetime: skip the identifier
  while (j < src.length && /[A-Za-z0-9_]/.test(src[j])) j++
  return j
}

// One scanner step: returns the next index and a brace delta (-1/0/+1).
function advance(src, i) {
  const ch = src[i]
  const rawQuote = (j) => {
    let k = j
    while (src[k] === "#") k++
    return src[k] === '"' ? k : -1
  }
  if (ch === "/" && src[i + 1] === "/")
    return { next: skipLineComment(src, i), brace: 0 }
  if (ch === "/" && src[i + 1] === "*")
    return { next: skipBlockComment(src, i), brace: 0 }
  if (ch === '"') return { next: skipString(src, i), brace: 0 }
  if (ch === "'") return { next: skipCharOrLifetime(src, i), brace: 0 }
  if (ch === "r" && rawQuote(i + 1) !== -1)
    return { next: skipRawString(src, i), brace: 0 }
  if (ch === "b") {
    if (src[i + 1] === '"') return { next: skipString(src, i + 1), brace: 0 }
    if (src[i + 1] === "'")
      return { next: skipCharOrLifetime(src, i + 1), brace: 0 }
    if (src[i + 1] === "r" && rawQuote(i + 2) !== -1)
      return { next: skipRawString(src, i + 1), brace: 0 }
  }
  if (ch === "{") return { next: i + 1, brace: 1 }
  if (ch === "}") return { next: i + 1, brace: -1 }
  return { next: i + 1, brace: 0 }
}

function matchBrace(src, openIdx) {
  let depth = 1
  let i = openIdx + 1
  while (i < src.length) {
    const { next, brace } = advance(src, i)
    if (next <= i) return -1 // unbalanced or unparseable: abort, don't guess
    depth += brace
    if (depth === 0) return next
    i = next
  }
  return -1
}

function skipWsAndComments(src, i) {
  while (i < src.length) {
    const c = src[i]
    if (c === " " || c === "\t" || c === "\n" || c === "\r") {
      i++
    } else if (c === "/" && src[i + 1] === "/") {
      i = skipLineComment(src, i)
    } else if (c === "/" && src[i + 1] === "*") {
      i = skipBlockComment(src, i)
    } else {
      break
    }
  }
  return i
}

function parseCfgTestModule(src, attrStart) {
  let i = attrStart + CFG_TEST.length
  i = skipWsAndComments(src, i)
  if (!src.startsWith("mod", i)) return { kind: "item", start: attrStart }
  if (/[A-Za-z0-9_]/.test(src[i + 3])) return { kind: "item", start: attrStart }
  i += 3
  i = skipWsAndComments(src, i)
  const m = /^[A-Za-z_][A-Za-z0-9_]*/.exec(src.slice(i, i + 200))
  if (!m) return { kind: "item", start: attrStart }
  const name = m[0]
  i += name.length
  i = skipWsAndComments(src, i)
  if (src[i] === ";")
    return { kind: "declaration", name, start: attrStart, end: i + 1 }
  if (src[i] !== "{") return { kind: "item", start: attrStart }
  const open = i
  const end = matchBrace(src, open)
  if (end === -1) return { kind: "unbalanced", name, start: attrStart }
  return { kind: "module", name, start: attrStart, end, open }
}

// Every `#[cfg(test)]` at the TOP LEVEL of the file, classified.
// kind: "module" (extractable), "declaration" (already external), "item"
// (attr on a plain item — a test helper that belongs in source), or
// "unbalanced" (the file itself failed to scan clean).
export function findTestModules(src) {
  const found = []
  let depth = 0
  let i = 0
  while (i < src.length) {
    if (depth === 0 && src.startsWith(CFG_TEST, i)) {
      const parsed = parseCfgTestModule(src, i)
      if (parsed.kind === "module") {
        found.push(parsed)
        i = parsed.end
        continue
      }
      if (parsed.kind === "declaration" || parsed.kind === "unbalanced") {
        found.push(parsed)
        i = parsed.end ?? parsed.start + CFG_TEST.length
        continue
      }
      // "item": the attr sits on a plain item (a test helper that belongs
      // in source). Report it, then keep scanning from the same index.
      found.push(parsed)
    }
    const { next, brace } = advance(src, i)
    depth += brace
    i = next
  }
  return found
}

// Split a source into { source, files } — each extracted module becomes a
// standalone file holding the module BODY (the declaration `#[cfg(test)] mod
// <name>;` that replaces the block carries the name and the cfg; writing the
// wrapper into the file would nest the path one level deeper:
// `foo::tests::tests`). The original position keeps a bare declaration.
// All-or-nothing: throws before writing anything if an extracted block
// embeds include_str!/include_bytes! (fixture paths would need a human
// decision) or if two blocks share a name.
export function planExtraction(src, blocks) {
  const modules = blocks.filter((b) => b.kind === "module")
  if (modules.length === 0) return { source: src, files: [] }
  for (const b of modules) {
    const text = src.slice(b.start, b.end)
    const include = text.match(/include_(?:str|bytes)\s*!\s*\(/g)
    if (include) {
      throw new Error(
        `mod ${b.name} embeds include_str!/include_bytes! — fixture ` +
          `paths are relative to the file and need a human decision`
      )
    }
  }
  const names = modules.map((b) => b.name)
  if (new Set(names).size !== names.length) {
    throw new Error(`duplicate module name among blocks: ${names.join(", ")}`)
  }
  let source = src
  const files = []
  for (const b of [...modules].sort((a, z) => z.start - a.start)) {
    // Body = strictly between the braces; drop the wrapper's own leading
    // newline so the file starts on the first item (the byte-identical
    // style the earlier manual splits used, indentation untouched).
    let body = src.slice(b.open + 1, b.end - 1)
    if (body.startsWith("\n")) body = body.slice(1)
    files.push({ name: b.name, text: body.endsWith("\n") ? body : body + "\n" })
    source =
      source.slice(0, b.start) +
      `#[cfg(test)]\nmod ${b.name};\n` +
      source.slice(b.end)
  }
  return { source, files }
}

// Where `mod <name>;` declared in `file` resolves to, both 2018 styles:
// `foo.rs` → `foo/<name>.rs` (dir the compiler already looks in for child
// modules of a file module), `foo/mod.rs` → `foo/<name>.rs`.
export function childModulePath(file, name) {
  const dir = path.dirname(file)
  const base = path.basename(file)
  if (base === "mod.rs") return path.join(dir, `${name}.rs`)
  return path.join(dir, base.replace(/\.rs$/, ""), `${name}.rs`)
}

// verify.mjs's cargo filter for a file under src-tauri/src.
export function rustTestFilter(absFile) {
  const rel = path.relative(SRC, absFile).replace(/\.rs$/, "")
  return rel.split(path.sep).join("::")
}

function lineOf(src, idx) {
  return src.slice(0, idx).split("\n").length
}

// ── CLI ────────────────────────────────────────────────────────────────

function walkRs(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, entry.name)
    if (entry.isDirectory()) walkRs(p, out)
    else if (entry.name.endsWith(".rs")) out.push(p)
  }
  return out
}

function scan() {
  const rows = walkRs(SRC).map((file) => {
    const src = fs.readFileSync(file, "utf8")
    const blocks = findTestModules(src)
    return { file, blocks }
  })
  const withBlocks = rows.filter((r) => r.blocks.length > 0)
  const total = (blocks) =>
    blocks
      .filter((b) => b.kind === "module")
      .reduce((n, b) => n + b.end - b.start, 0)
  withBlocks.sort((a, z) => total(z.blocks) - total(a.blocks))
  for (const { file, blocks } of withBlocks) {
    const rel = path.relative(REPO, file)
    const lines = (b) =>
      `${b.name} @${lineOf(fs.readFileSync(file, "utf8"), b.start)} (${b.end - b.start} B)`
    const modules = blocks.filter((b) => b.kind === "module")
    const items = blocks.filter((b) => b.kind === "item").length
    const decls = blocks.filter((b) => b.kind === "declaration").length
    const bad = blocks.filter((b) => b.kind === "unbalanced")
    console.log(
      `${rel}: ${modules.length} module(s), ` + `${total(blocks)} B extractable`
    )
    for (const b of modules) console.log(`    mod ${lines(b)}`)
    if (items) console.log(`    + ${items} item-level #[cfg(test)] (skipped)`)
    if (decls) console.log(`    + ${decls} external declaration(s) (skipped)`)
    if (bad.length) console.log(`    ! UNBALANCED: ${bad.length}`)
  }
  if (withBlocks.length === 0) console.log("no inline test modules found")
}

function gitClean(file) {
  const out = execFileSync(
    "git",
    ["-C", REPO, "status", "--porcelain", "--", file],
    { encoding: "utf8" }
  )
  return out.trim() === ""
}

function extract(files, apply, force) {
  const plans = []
  for (const file of files) {
    const abs = path.resolve(file)
    if (!abs.endsWith(".rs") || !fs.existsSync(abs)) {
      console.error(`skip: not an existing .rs file: ${file}`)
      process.exitCode = 1
      continue
    }
    if (!gitClean(abs) && !force) {
      console.error(
        `refuse: ${path.relative(REPO, abs)} has uncommitted changes — ` +
          `extract from a committed state (or pass --force)`
      )
      process.exitCode = 1
      continue
    }
    const src = fs.readFileSync(abs, "utf8")
    const blocks = findTestModules(src)
    const modules = blocks.filter((b) => b.kind === "module")
    if (modules.length === 0) {
      console.log(
        `${path.relative(REPO, abs)}: nothing module-shaped to extract`
      )
      continue
    }
    for (const b of blocks) {
      if (b.kind === "unbalanced") {
        console.error(
          `refuse: ${path.relative(REPO, abs)} — brace scan failed (near line ${lineOf(src, b.start)})`
        )
        process.exitCode = 1
      }
    }
    if (process.exitCode === 1) continue
    let plan
    try {
      plan = planExtraction(src, modules)
    } catch (e) {
      console.error(`refuse: ${path.relative(REPO, abs)} — ${e.message}`)
      process.exitCode = 1
      continue
    }
    const targets = plan.files.map((f) => childModulePath(abs, f.name))
    for (const t of targets) {
      if (fs.existsSync(t)) {
        console.error(
          `refuse: target already exists: ${path.relative(REPO, t)}`
        )
        process.exitCode = 1
      }
    }
    if (process.exitCode === 1) continue
    plans.push({ abs, src, plan, targets })
    for (const f of plan.files) {
      const t = targets[plan.files.indexOf(f)]
      console.log(
        `${path.relative(REPO, abs)}: mod ${f.name} ` +
          `@${lineOf(src, modules.find((b) => b.name === f.name).start)} ` +
          `(${f.text.split("\n").length - 1} lines) -> ${path.relative(REPO, t)}`
      )
    }
    console.log(
      `    filters: ${targets.map((t) => rustTestFilter(t)).join(", ")}`
    )
  }
  if (process.exitCode === 1 || !apply) {
    if (!apply && plans.length > 0)
      console.log("(dry run — pass --apply to write)")
    return
  }
  for (const p of plans) {
    fs.writeFileSync(p.abs, p.plan.source)
    for (let i = 0; i < p.plan.files.length; i++) {
      const t = p.targets[i]
      fs.mkdirSync(path.dirname(t), { recursive: true })
      fs.writeFileSync(t, p.plan.files[i].text)
    }
    console.log(`applied: ${path.relative(REPO, p.abs)}`)
  }
  console.log(
    "next: run the printed verify.mjs filters, then register the move " +
      "in codeg/NOTICE.md's 结构拆分登记 (上游文件 → 本仓库结构)."
  )
}

function selftest() {
  let ok = 0
  let total = 0
  const check = (label, fn) => {
    total++
    try {
      fn()
      ok++
    } catch (e) {
      console.error(`FAIL ${label}: ${e.message}`)
    }
  }
  const eq = (a, b) => {
    const la = JSON.stringify(a)
    const lb = JSON.stringify(b)
    if (la !== lb) throw new Error(`\n  got: ${la}\n  want: ${lb}`)
  }
  check("tail module extracts byte-exact", () => {
    const src = [
      "fn a() {}",
      "",
      "#[cfg(test)]",
      "mod tests {",
      '    const S: &str = "}{"; // brace in string',
      '    static R: &str = r##"{ raw "# }"##;',
      "    // comment { brace",
      "    /* nested { { } } */",
      "    const C: char = '{';",
      '    const L: &\'static str = "x";',
      "    #[test]",
      "    fn t() { assert!(true); }",
      "}",
      "",
    ].join("\n")
    const blocks = findTestModules(src)
    if (blocks.length !== 1 || blocks[0].name !== "tests")
      throw new Error(`expected 1 module named tests`)
    const plan = planExtraction(src, blocks)
    eq(plan.source, "fn a() {}\n\n#[cfg(test)]\nmod tests;\n\n")
    // File holds the BODY only (src lines 4..11: the items between the
    // braces) — the wrapper stays behind as the bare declaration.
    eq(plan.files[0].text, src.split("\n").slice(4, 12).join("\n") + "\n")
  })
  check("mid-file module + item cfg + declaration", () => {
    const src = [
      "fn a() {}",
      "#[cfg(test)]",
      "fn helper() -> u8 { 1 }",
      "fn b(x: u8) -> u8 { x + 1 }",
      "#[cfg(test)]",
      "mod tests;",
      "#[cfg(test)]",
      "mod stats {",
      "    use super::*;",
      "    #[test]",
      "    fn t() { assert_eq!(helper(), b(0)); }",
      "}",
      "fn z() {}",
    ].join("\n")
    const blocks = findTestModules(src)
    eq(
      blocks.map((b) => `${b.kind}:${b.name ?? "-"}`),
      ["item:-", "declaration:tests", "module:stats"]
    )
    const plan = planExtraction(src, blocks)
    eq(
      plan.source,
      [
        "fn a() {}",
        "#[cfg(test)]",
        "fn helper() -> u8 { 1 }",
        "fn b(x: u8) -> u8 { x + 1 }",
        "#[cfg(test)]",
        "mod tests;",
        "#[cfg(test)]",
        "mod stats;",
        "",
        "fn z() {}",
      ].join("\n")
    )
  })
  check("nested mod inside the test block stays inside", () => {
    const src = [
      "fn a() {}",
      "#[cfg(test)]",
      "mod tests {",
      "    mod inner { fn i() {} }",
      "    #[test]",
      "    fn t() { inner::i(); }",
      "}",
    ].join("\n")
    const blocks = findTestModules(src)
    eq(blocks.length, 1)
    eq(blocks[0].name, "tests")
    const plan = planExtraction(src, blocks)
    // Body = src lines 3..5; the inner `mod inner` stays nested in the body.
    eq(plan.files[0].text, src.split("\n").slice(3, 6).join("\n") + "\n")
  })
  check("unbalanced brace refuses", () => {
    const src = [
      "fn a() {}",
      "#[cfg(test)]",
      "mod broken {",
      "  fn t() { }",
    ].join("\n")
    const blocks = findTestModules(src)
    if (!blocks.some((b) => b.kind === "unbalanced"))
      throw new Error("expected unbalanced flag")
  })
  check("include_str! refuses", () => {
    const src = [
      "fn a() {}",
      "#[cfg(test)]",
      "mod tests {",
      '    const F: &str = include_str!("../fixtures/x.json");',
      "}",
    ].join("\n")
    try {
      planExtraction(src, findTestModules(src))
      throw new Error("should have thrown")
    } catch (e) {
      if (!e.message.includes("include_str")) throw e
    }
  })
  check("childModulePath for both 2018 styles", () => {
    eq(
      childModulePath("/x/parsers/codex.rs", "tests"),
      "/x/parsers/codex/tests.rs"
    )
    eq(
      childModulePath("/x/commands/acp/mod.rs", "diagnostics_tests"),
      "/x/commands/acp/diagnostics_tests.rs"
    )
  })
  console.log(
    process.exitCode === 1 ? "selftest failed" : `selftest ${ok}/${total} ok`
  )
  if (ok !== total) process.exitCode = 1
}

// CLI entry — only when this file itself is the entry point, so the pure
// helpers can be imported (probes, future tooling) without side effects.
const isEntry =
  process.argv[1] &&
  import.meta.url === new URL(`file://${path.resolve(process.argv[1])}`).href

if (isEntry) {
  const [mode, ...rest] = process.argv.slice(2)
  if (mode === "selftest") {
    selftest()
  } else if (mode === "scan") {
    scan()
  } else if (mode === "extract") {
    const files = rest.filter((a) => !a.startsWith("--"))
    if (files.length === 0) {
      console.error(
        "usage: split-tests.mjs extract <file…> [--apply] [--force]"
      )
      process.exit(2)
    }
    extract(files, rest.includes("--apply"), rest.includes("--force"))
  } else {
    console.error(
      "modes: selftest | scan | extract <file…> [--apply] [--force]"
    )
    process.exit(2)
  }
}
