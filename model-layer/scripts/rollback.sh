#!/usr/bin/env bash
# Undo cutover.sh: stop AgentOS, restore the global Claude settings, start CCR.
set -euo pipefail

PLIST="$HOME/Library/LaunchAgents/com.crewchat.ccr.plist"
SETTINGS="$HOME/.claude/settings.json"
BACKUP="$HOME/.claude/settings.json.crewchat-backup"

echo "==> quitting AgentOS"
osascript -e 'tell application "AgentOS" to quit' 2>/dev/null || true
sleep 2

if [[ -f "$BACKUP" ]]; then
  echo "==> restoring $SETTINGS"
  cp "$BACKUP" "$SETTINGS"
else
  echo "    no settings backup found; leaving $SETTINGS alone"
fi

if [[ -f "$PLIST" ]]; then
  echo "==> starting CCR"
  launchctl bootstrap "gui/$UID" "$PLIST" 2>/dev/null || true
else
  echo "    CCR launchd plist not found: $PLIST" >&2
  echo "    Start CCR manually (ccr start) before relaunching AgentOS." >&2
fi

DATA="${MODEL_LAYER_DATA_DIR:-$HOME/Library/Application Support/AgentOS/model}"
rm -f "$DATA/required"
mkdir -p "$DATA"
touch "$DATA/disabled"

echo "==> relaunching AgentOS (it will fall back to CCR if the model layer is absent)"
APP="$(find "$HOME/Library/Developer/Xcode/DerivedData" \
  -path '*/Build/Products/Debug/AgentOS.app' -maxdepth 6 -print 2>/dev/null | head -n 1)"
if [[ -z "$APP" ]]; then APP="/Applications/AgentOS.app"; fi
[[ -d "$APP" ]] && open "$APP" || true
