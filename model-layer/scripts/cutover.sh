#!/usr/bin/env bash
# Stop CCR, relaunch AgentOS so it owns 3456/3458 with crewchat-model, then
# point the global Claude Code settings at it.
#
# Rollback: model-layer/scripts/rollback.sh
set -euo pipefail

PLIST="$HOME/Library/LaunchAgents/com.crewchat.ccr.plist"

echo "==> stopping CCR launchd job"
launchctl bootout "gui/$UID" "$PLIST" 2>/dev/null || true

echo "==> stopping processes still listening on 3456/3458"
for port in 3456 3458; do
  while read -r pid; do
    [[ -z "$pid" ]] && continue
    command="$(ps -p "$pid" -o command= 2>/dev/null || true)"
    if [[ "$command" == *claude-code-router* || "$command" == *ccr* ]]; then
      echo "    kill $pid ($port): $command"
      kill "$pid" 2>/dev/null || true
    else
      echo "    port $port is held by a non-CCR process; aborting: $command" >&2
      exit 1
    fi
  done < <(lsof -ti "tcp:$port" -sTCP:LISTEN 2>/dev/null || true)
done

echo "==> quitting AgentOS"
osascript -e 'tell application "AgentOS" to quit' 2>/dev/null || true
sleep 2

APP="$(find "$HOME/Library/Developer/Xcode/DerivedData" \
  -path '*/Build/Products/Debug/AgentOS.app' -maxdepth 6 -print 2>/dev/null | head -n 1)"
if [[ -z "$APP" ]]; then
  APP="/Applications/AgentOS.app"
fi
if [[ ! -d "$APP" ]]; then
  echo "AgentOS.app not found. Build it first." >&2
  exit 1
fi

echo "==> installing crewchat-model into the app"
"$(dirname "$0")/install-dev.sh" "$APP"

DATA="${MODEL_LAYER_DATA_DIR:-$HOME/Library/Application Support/AgentOS/model}"
mkdir -p "$DATA"
touch "$DATA/required"

echo "==> relaunching $APP"
open "$APP"

echo "==> waiting for the model layer"
for _ in $(seq 1 60); do
  if curl -sf http://127.0.0.1:3456/health >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
curl -sf http://127.0.0.1:3456/health >/dev/null || {
  echo "model layer did not become healthy; see ~/Library/Logs/AgentOS/model-layer.log" >&2
  exit 1
}

echo "==> pointing global Claude Code at it"
"$(dirname "$0")/apply-profile.sh"

echo
echo "==> listeners"
lsof -nP -iTCP -sTCP:LISTEN | grep -E ':3456|:3458' || true
echo
echo "CCR is no longer running. Rollback: model-layer/scripts/rollback.sh"
