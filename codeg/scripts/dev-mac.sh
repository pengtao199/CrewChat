#!/usr/bin/env bash
#
# [crewchat] One command for this machine.
#
#   bash scripts/dev-mac.sh          # 热更新回路（默认）：窗口＝你的数据，改前端秒级、改 Rust 自动重编重启
#   bash scripts/dev-mac.sh install  # 独立正式 app：完整构建 → 覆盖安装 /Applications/codeg.app（不启动，慢）
#
# 默认这条就是「热更新」：
#   - 前端（TSX / CSS / i18n）：Next dev + HMR，改完**秒级**出现在窗口里，不重启、不丢状态。
#   - Rust（src-tauri/src/**）：Tauri 监听 crate，**增量重编**（debug，比 release 快得多）并自动重启窗口。
#   - 不需要 `next build` / `tauri build`——那是 `install` 那条路（几分钟，因为 Tauri 把前端
#     嵌进二进制，任何改动都要重新链接一次）。
#
# 为什么这条回路「就是你本机的那个」：debug 桌面版默认读 `codeg-dev.db`（**另一个**库，所以以前
# 看起来像另一个 app）。脚本给进程设 `CODEG_DB_FILE=codeg.db`，让它读**真库**——真花名册、真会话、
# 真设置。副作用：dev 会跑迁移改真库（这正是你想要的：它就是你的 app 的最新代码）；**别同时开
# dev 窗口和 /Applications 的 app**（同一个库两个进程）。
#
# 什么时候用 `install`：要一个不依赖 dev server 的独立 app（日常用 / 给别人）时。它跑完整 release
# 打包再覆盖安装，**不启动、不唤醒**。
#
# 为什么不用 `pnpm tauri dev` / `pnpm tauri build`：系统 pnpm 是 9.x，会拒掉本仓库的
# pnpm-workspace.yaml（`packages field missing or empty`），Tauri 的 before*Command 跑不起来
# ——两条路都自己替掉那两个钩子。
set -euo pipefail

cd "$(dirname "$0")/.."

TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"

if [ "${1:-}" = "install" ]; then
  # 完整构建 + 覆盖安装（不启动）。真正干活的步骤只有一份，在 package-mac.sh。
  if pgrep -qf "/Applications/codeg.app/Contents/MacOS/codeg"; then
    echo "note: codeg.app 正在运行——装完要退出并重新打开才会生效（脚本不会动你的窗口）。"
    echo
  fi
  echo "== 构建 $(git rev-parse --short HEAD 2>/dev/null || echo '?') @ $(date '+%Y-%m-%d %H:%M') → 覆盖安装 /Applications/codeg.app（不启动）=="
  exec bash scripts/package-mac.sh
fi

# --- 默认：热更新回路（窗口＝你的真库） ---------------------------------------

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

# 1.5 构建产物自检：feature 集来回切会让 target/debug 长到几十上百 G（实测 115G：
#     同一份 libcodeg_lib.a 1.5G × 各 feature 集 + 1G 级 incremental dep-graph）。
#     超过 30G 就清掉**本 crate** 的产物——依赖全部保留，本 crate 重编约 1-3 分钟。
TARGET_MB=$(du -sm src-tauri/target 2>/dev/null | cut -f1 || echo 0)
if [ "${TARGET_MB:-0}" -gt 30000 ]; then
  echo "== target 已 ${TARGET_MB}MB（>30G）→ 清理本 crate 的构建产物（依赖保留）=="
  (cd src-tauri && cargo clean -p codeg)
fi

# 2. 读真库。`CODEG_DB_FILE` 只换文件名（目录仍是 `CODEG_DATA_DIR`，默认平台目录）。
export CODEG_DB_FILE="${CODEG_DB_FILE:-codeg.db}"
DATA_DIR="${CODEG_DATA_DIR:-$HOME/Library/Application Support/app.codeg}"
if [ ! -f "${DATA_DIR}/${CODEG_DB_FILE}" ]; then
  echo "!! ${DATA_DIR}/${CODEG_DB_FILE} 不存在。" >&2
  echo "   直接跑会新建一个空库（那就又变成\"另一个 app\"了）。先确认路径，或用 CODEG_DATA_DIR 指过去。" >&2
  exit 1
fi
echo "== 库：${DATA_DIR}/${CODEG_DB_FILE}（真库；dev 会跑迁移，别同时开 /Applications 的 app）=="

# 3. CCR CLI 只影响「Codeg 能不能自己拉起 CCR」。网关的 URL / token 是从 CCR 自己的
#    service.json 自动发现的，所以 CCR 已经在跑时，按角色的模型注入与这一项无关。
if [ -z "${CCR_BIN:-}" ] && command -v ccr >/dev/null 2>&1; then
  export CCR_BIN="$(command -v ccr)"
fi
if [ -z "${CCR_BIN:-}" ]; then
  echo "== CCR_BIN：未找到 ccr（Codeg 不能自启 CCR；它已在跑就无影响）=="
else
  echo "== CCR_BIN：${CCR_BIN} =="
fi

# 4. Next dev server (HMR) in the background…
echo "== Next dev server（:3000，前端热更新）=="
./node_modules/.bin/next dev --turbopack &
NEXT_PID=$!
cleanup() { kill "$NEXT_PID" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

for _ in $(seq 1 90); do
  if /usr/bin/curl -sf -o /dev/null http://localhost:3000; then break; fi
  sleep 1
done

# 5. …then the window, which loads http://localhost:3000 and watches Rust.
echo "== Tauri 窗口（Rust 改动自动增量重编 + 重启窗口）=="
./node_modules/.bin/tauri dev \
  --config '{"build":{"beforeDevCommand":"","beforeBuildCommand":""}}'
