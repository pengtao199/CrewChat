#!/usr/bin/env node
// [crewchat] Scoped verification: run only the checks the *changed* files need.
//
// Why this exists: `cargo test` compiles the whole crate and `vitest run` walks
// all 600+ frontend test files, so "just run the tests" is several minutes even
// when one module moved. The project rule (AGENTS/verification.md) asks for the
// *minimal* check per change type — this script is that rule, executable.
//
// Usage (from `codeg/`):
//   node scripts/verify.mjs changed            # derive scope from `git diff HEAD`
//   node scripts/verify.mjs changed --base origin/main
//   node scripts/verify.mjs changed --dry-run  # print the plan, run nothing
//   node scripts/verify.mjs rust ccr::         # explicit: scoped cargo test
//   node scripts/verify.mjs web src/lib/ccr.ts # explicit: scoped vitest
//   node scripts/verify.mjs types              # tsc --noEmit
//   node scripts/verify.mjs lint src/lib/ccr.ts
//
// One caveat worth knowing: `cargo test --lib` still *compiles* the whole crate
// (one crate, one test binary) — the saving is in what runs, plus one build
// instead of a separate `cargo check --all-targets`. Keep a round's Rust edits
// together so the incremental rebuild happens once.
// Exit code is non-zero if any planned check failed. The plan is printed first
// so the transcript is the evidence (commands + filters + result).

import { spawnSync } from "node:child_process"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

const CODEG = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..")
const REPO = path.resolve(CODEG, "..")
const SRC_TAURI = path.join(CODEG, "src-tauri")
const BIN = (name) => path.join(CODEG, "node_modules", ".bin", name)

/** @type {{label: string, cmd: string, args: string[], cwd: string}[]} */
const plan = []

function add(label, cmd, args, cwd) {
  plan.push({ label, cmd, args, cwd })
}

function run(cmd, args, cwd) {
  const result = spawnSync(cmd, args, { cwd, encoding: "utf8" })
  if (result.stdout) process.stdout.write(result.stdout)
  if (result.stderr) process.stderr.write(result.stderr)
  return result.status === 0
}

function git(args) {
  const result = spawnSync("git", args, { cwd: REPO, encoding: "utf8" })
  return result.status === 0 ? result.stdout.split("\n").filter(Boolean) : []
}

// ─── Change classification ────────────────────────────────────────────

function changedFiles(base) {
  return [
    ...new Set([
      ...git(["diff", "--name-only", base]),
      ...git(["diff", "--name-only", "--cached"]),
      ...git(["ls-files", "--others", "--exclude-standard"]),
    ]),
  ]
}

/** Rust module path a source file's tests live under (`acp/x/mod.rs` → `acp::x`). */
function rustModulePath(rel) {
  const stem = rel.replace(/\.rs$/, "")
  const parts = stem.split("/")
  if (parts[parts.length - 1] === "mod") parts.pop()
  return parts.join("::")
}

/**
 * The libtest filters that reach this file's tests — and nothing else.
 *
 * Tests are inline here (`#[cfg(test)] mod tests`), so the *test module name*
 * is the narrow filter; using the file's own module path instead would pull in
 * every sibling (changing `web/mod.rs` would run all of `web::*`). A file with
 * no inline tests but a sibling `tests.rs` is covered by its module path. A
 * file with neither has no tests to run: it is wiring, and compiling it is the
 * whole check.
 */
function rustTestFilters(rel, absPath) {
  const base = rustModulePath(rel)
  if (rel.endsWith("tests.rs") || rel.endsWith("test.rs"))
    return [base].filter(Boolean)
  let source = ""
  try {
    source = fs.readFileSync(absPath, "utf8")
  } catch {
    return [base].filter(Boolean)
  }
  const names = [...source.matchAll(/#\[cfg\(test\)\]\s*mod\s+(\w+)/g)].map(
    (m) => m[1]
  )
  if (names.length > 0) {
    return names.map((name) => (base ? `${base}::${name}` : name))
  }
  if (fs.existsSync(path.join(path.dirname(absPath), "tests.rs"))) {
    return [base].filter(Boolean)
  }
  return []
}

/** The sibling test file vitest would run for a source file, if it exists. */
function webTestsFor(absSource) {
  const dir = path.dirname(absSource)
  const stem = path.basename(absSource).replace(/\.(tsx?|jsx?)$/, "")
  const candidates = [
    path.join(dir, `${stem}.test.ts`),
    path.join(dir, `${stem}.test.tsx`),
    path.join(dir, `${stem}.spec.ts`),
    path.join(dir, `${stem}.spec.tsx`),
  ]
  const hits = candidates.filter((candidate) => fs.existsSync(candidate))
  if (/\.(test|spec)\.tsx?$/.test(absSource)) hits.unshift(absSource)
  return hits.map((hit) => path.relative(CODEG, hit))
}

function pythonModule(rel) {
  return rel
    .replace(/^src\//, "")
    .replace(/\.py$/, "")
    .replace(/\/__init__$/, "")
    .split("/")
    .join(".")
}

function planFor(files, root = REPO) {
  const rustFilters = new Set()
  const rustWiring = []
  let binChanged = false
  let testsChanged = false
  const webTests = new Set()
  const webChanged = []
  const pythonModules = new Set()

  for (const rel of files) {
    if (rel.startsWith("codeg/src-tauri/src/bin/")) {
      // Bin targets: their unit tests are not part of `--lib`, so the check is
      // a compile (`--bins`). Run bin tests explicitly if you need them:
      // `cargo test --features test-utils --bin codeg_server`.
      binChanged = true
      rustWiring.push(rel)
    } else if (rel.startsWith("codeg/src-tauri/src/")) {
      const inner = rel.slice("codeg/src-tauri/src/".length)
      const found = rustTestFilters(inner, path.join(root, rel))
      if (found.length > 0) for (const filter of found) rustFilters.add(filter)
      else rustWiring.push(inner)
    } else if (rel.startsWith("codeg/src-tauri/bin/")) {
      binChanged = true
      rustWiring.push(rel)
    } else if (rel.startsWith("codeg/src-tauri/tests/")) {
      testsChanged = true
      rustWiring.push(rel)
    } else if (/^codeg\/src\/i18n\/messages\/.*\.json$/.test(rel)) {
      // Locale bundles: key parity vs en.json is enforced by a test.
      webTests.add("src/i18n/messages.test.ts")
    } else if (/^codeg\/src\/.*\.(tsx?|jsx?)$/.test(rel)) {
      const abs = path.join(root, rel)
      webChanged.push(rel)
      for (const test of webTestsFor(abs)) webTests.add(test)
    } else if (/^src\/crewchat\/.*\.py$/.test(rel)) {
      pythonModules.add(pythonModule(rel))
    }
  }

  const filters = [...rustFilters].sort()
  if (filters.length > 0) {
    // One test build subsumes the lib type-check; libtest ORs multiple filters.
    add(
      `rust tests · ${filters.join(", ")}`,
      "cargo",
      ["test", "--features", "test-utils", "--lib", "--", ...filters],
      SRC_TAURI
    )
  } else if (rustWiring.length > 0 && !binChanged) {
    // Wiring only (lib.rs, router, …): nothing to run, just compile the lib.
    add(
      "rust lib compile",
      "cargo",
      ["check", "--features", "test-utils", "--lib"],
      SRC_TAURI
    )
  }
  if (binChanged) {
    add(
      "rust bins compile",
      "cargo",
      ["check", "--features", "test-utils", "--bins"],
      SRC_TAURI
    )
  }
  if (testsChanged) {
    add(
      "rust integration tests compile",
      "cargo",
      ["check", "--features", "test-utils", "--tests"],
      SRC_TAURI
    )
  }

  const tests = [...webTests].sort()
  if (tests.length > 0) {
    add(
      `web tests · ${tests.join(", ")}`,
      BIN("vitest"),
      ["run", ...tests],
      CODEG
    )
  }
  if (webChanged.length > 0) {
    add(
      `types · ${webChanged.length} file(s) changed`,
      BIN("tsc"),
      ["--noEmit"],
      CODEG
    )
  }

  if (pythonModules.size > 0) {
    for (const pyModule of [...pythonModules].sort()) {
      add(
        `python import · ${pyModule}`,
        "uv",
        ["run", "python", "-c", `import ${pyModule}`],
        REPO
      )
    }
  }

  return {
    filters,
    rustWiring,
    binChanged,
    testsChanged,
    tests,
    webChanged,
    pythonModules: [...pythonModules],
  }
}

// ─── Entry points ─────────────────────────────────────────────────────

const [mode = "changed", ...rest] = process.argv.slice(2)

if (mode === "changed") {
  const baseIndex = rest.indexOf("--base")
  const base = baseIndex === -1 ? "HEAD" : rest[baseIndex + 1]
  const files = changedFiles(base)
  if (files.length === 0) {
    console.log(`no changes vs ${base} — nothing to verify`)
    process.exit(0)
  }
  const scope = planFor(files)
  console.log(`changed (vs ${base}): ${files.length} file(s)`)
  for (const file of files) console.log(`  ${file}`)
  console.log("")
  if (plan.length === 0) {
    console.log("no code changed (docs / data only) — no checks to run")
    process.exit(0)
  }
  console.log(
    `scope: rust tests=[${scope.filters.join(", ") || "—"}] ` +
      `rust wiring=${scope.rustWiring.length} web tests=${scope.tests.length} ` +
      `web changed=${scope.webChanged.length} python=${scope.pythonModules.length}`
  )
  console.log("")
} else if (mode === "rust") {
  if (rest.length === 0) {
    console.error("usage: verify.mjs rust <test-filter> [filter...]")
    process.exit(2)
  }
  add(
    `rust tests · ${rest.join(", ")}`,
    "cargo",
    ["test", "--features", "test-utils", "--lib", "--", ...rest],
    SRC_TAURI
  )
} else if (mode === "web") {
  if (rest.length === 0) {
    console.error("usage: verify.mjs web <test-file> [file...]")
    process.exit(2)
  }
  add(`web tests · ${rest.join(", ")}`, BIN("vitest"), ["run", ...rest], CODEG)
} else if (mode === "types") {
  add("types", BIN("tsc"), ["--noEmit"], CODEG)
} else if (mode === "lint") {
  add("lint", BIN("eslint"), rest, CODEG)
} else {
  console.error(`unknown mode: ${mode}`)
  console.error(
    "modes: changed | rust <filter…> | web <file…> | types | lint [file…]"
  )
  process.exit(2)
}

// ─── Run ──────────────────────────────────────────────────────────────

const dryRun = process.argv.includes("--dry-run")
if (dryRun) {
  for (const [index, step] of plan.entries()) {
    console.log(`${index + 1}. ${step.label}`)
    console.log(
      `   $ ${step.cmd} ${step.args.join(" ")}   (cwd: ${path.relative(REPO, step.cwd) || "."})`
    )
  }
  process.exit(0)
}

let failed = 0
for (const [index, step] of plan.entries()) {
  const command = `${step.cmd} ${step.args.join(" ")}`
  console.log(`[${index + 1}/${plan.length}] ${step.label}`)
  console.log(`    $ ${command}`)
  const started = Date.now()
  const ok = run(step.cmd, step.args, step.cwd)
  const seconds = ((Date.now() - started) / 1000).toFixed(1)
  console.log(`    ${ok ? "ok" : "FAILED"} (${seconds}s)\n`)
  if (!ok) failed += 1
}

if (failed > 0) {
  console.error(`${failed} of ${plan.length} check(s) failed`)
  process.exit(1)
}
console.log(`all ${plan.length} check(s) passed`)
