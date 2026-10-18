#!/usr/bin/env bash
#
# [crewchat] Local macOS packaging in one command.
#
# Why not just `pnpm tauri build`:
#   1. The system pnpm is 9.x and refuses this repo's `pnpm-workspace.yaml`
#      (`packages field missing or empty`), so `beforeBuildCommand`
#      (`pnpm build && …`) cannot run. This script does those steps itself.
#   2. `bundle.createUpdaterArtifacts` needs the updater signing key, which only
#      the release workflow has — a local build would fail at the LAST step, so
#      it is turned off here.
#   3. `src-tauri/binaries/<bin>-<triple>` must be non-zero: cargo's build script
#      writes 0-byte placeholders, and 0 bytes reads as "not found" — the bundle
#      installs fine with delegation silently missing. Step 2 checks it.
#
# See NOTICE.md «开发回路与打包» — and prefer `pnpm tauri dev` for daily work
# (frontend HMR + automatic Rust rebuild); this script is the release action.
set -euo pipefail

cd "$(dirname "$0")/.."

TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"

echo "== 1/3 前端静态导出 =="
./node_modules/.bin/next build

echo "== 2/3 sidecar（release 编译）=="
node src-tauri/scripts/prepare-sidecars.mjs
for bin in codeg-mcp codeg-computer-helper; do
  file="src-tauri/binaries/${bin}-${TRIPLE}"
  if [ ! -s "$file" ]; then
    echo "!! ${file} 是 0 字节——打出来的包会静默缺失委派 / computer-use 能力" >&2
    exit 1
  fi
  echo "   ok ${bin}（$(du -h "$file" | cut -f1)）"
done

echo "== 3/3 tauri build（release，关闭更新产物）=="
./node_modules/.bin/tauri build \
  --config '{"build":{"beforeBuildCommand":""},"bundle":{"createUpdaterArtifacts":false}}'

echo "== 产物 =="
ls -la src-tauri/target/release/bundle/macos 2>/dev/null || true
ls -la src-tauri/target/release/bundle/dmg 2>/dev/null || true
