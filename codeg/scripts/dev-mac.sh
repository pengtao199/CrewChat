#!/usr/bin/env bash
#
# [crewchat] One-command desktop DEV loop (hot reload) for this machine.
#
#   bash scripts/dev-mac.sh
#
# What you get, and what you do NOT have to do:
#   - frontend (TSX / CSS / i18n): Next dev server + HMR — changes appear in the
#     running window in about a second. No build, no restart, state preserved.
#   - Rust (`src-tauri/src/**`): Tauri watches the crate, rebuilds INCREMENTALLY
#     and restarts the window by itself (the first build is the slow one).
#   - you never run `next build` / `tauri build` for development. Those are for
#     handing someone an installable app (`scripts/package-mac.sh`).
#
# Why a script instead of `pnpm tauri dev`: the system pnpm is 9.x and refuses
# this repo's pnpm-workspace.yaml, so Tauri's `beforeDevCommand`
# (`pnpm tauri:before-dev`) cannot run. The script starts Next itself and tells
# Tauri to skip that hook.
#
# Data: desktop dev uses `codeg-dev.db` inside the effective data dir (see
# `db/mod.rs`: `debug + tauri-runtime`). To see your real roster in dev, copy it
# once — the script prints the exact command when it notices the file is missing.
#
# Set `CODEG_DATA_DIR` before running to point dev at a data dir (otherwise it
# uses the platform default `~/Library/Application Support/app.codeg`).
set -euo pipefail

cd "$(dirname "$0")/.."

TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"

# 1. Sidecars: release binaries the window needs (delegation + computer use). A
#    0-byte placeholder reads as "not found", so check for size, not existence.
for bin in codeg-mcp codeg-computer-helper; do
  file="src-tauri/binaries/${bin}-${TRIPLE}"
  if [ ! -s "$file" ]; then
    echo "== sidecar ${bin} 缺失 → 先构建（release，约几分钟；只需一次，改 src/computer/ 或 mcp 后重跑）=="
    node src-tauri/scripts/prepare-sidecars.mjs
    break
  fi
done

# 2. Say it out loud when dev would open an EMPTY roster.
if [ -n "${CODEG_DATA_DIR:-}" ] && [ ! -f "${CODEG_DATA_DIR}/codeg-dev.db" ] && [ -f "${CODEG_DATA_DIR}/codeg.db" ]; then
  echo "note: 桌面 dev 读的是 codeg-dev.db（当前不存在，dev 里会是空库）。"
  echo "      想让 dev 看到真花名册，先执行一次："
  echo "        cp \"${CODEG_DATA_DIR}/codeg.db\" \"${CODEG_DATA_DIR}/codeg-dev.db\""
  echo
fi

# 3. Next dev server (HMR) in the background…
echo "== Next dev server（:3000，前端热更新）=="
./node_modules/.bin/next dev --turbopack &
NEXT_PID=$!
cleanup() { kill "$NEXT_PID" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

for _ in $(seq 1 90); do
  if /usr/bin/curl -sf -o /dev/null http://localhost:3000; then break; fi
  sleep 1
done

# 4. …then the window, which loads http://localhost:3000 and watches Rust.
echo "== Tauri 窗口（Rust 改动自动增量重编 + 重启窗口）=="
./node_modules/.bin/tauri dev \
  --config '{"build":{"beforeDevCommand":"","beforeBuildCommand":""}}'
