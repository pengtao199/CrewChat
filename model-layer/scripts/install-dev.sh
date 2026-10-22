#!/usr/bin/env bash
# Build crewchat-model and drop it into the Debug AgentOS.app so the shell can
# start it as a sidecar. Usage:
#
#   model-layer/scripts/install-dev.sh [path/to/AgentOS.app]
#
# With no argument the newest Debug build under Xcode's DerivedData is used.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP="${1:-}"

if [[ -z "$APP" ]]; then
  APP="$(find "$HOME/Library/Developer/Xcode/DerivedData" \
    -path '*/Build/Products/Debug/AgentOS.app' -maxdepth 6 -print 2>/dev/null \
    | head -n 1)"
fi

if [[ -z "$APP" || ! -d "$APP" ]]; then
  echo "AgentOS.app not found. Build it in Xcode first, or pass the path." >&2
  exit 1
fi

echo "==> building crewchat-model (release)"
cargo build --release --manifest-path "$ROOT/model-layer/Cargo.toml"

echo "==> installing into $APP/Contents/MacOS"
mkdir -p "$APP/Contents/MacOS"
cp "$ROOT/model-layer/target/release/crewchat-model" "$APP/Contents/MacOS/crewchat-model"
chmod +x "$APP/Contents/MacOS/crewchat-model"

echo "==> re-signing (ad-hoc, local only)"
codesign --force --deep --sign - "$APP" >/dev/null

echo "done. Restart AgentOS; the model layer will start before codeg-server."
echo "Logs: ~/Library/Logs/AgentOS/model-layer.log"
