#!/usr/bin/env bash
# Point the global Claude Code settings at the running model layer.
#
# This is the explicit "terminal claude should use the new gateway" step. Agent
# sessions do not need it — they get a per-role home from cli-layer.
set -euo pipefail

DATA="${MODEL_LAYER_DATA_DIR:-$HOME/Library/Application Support/AgentOS/model}"
RUNTIME="$DATA/model-layer.json"

if [[ ! -f "$RUNTIME" ]]; then
  echo "model layer runtime file not found: $RUNTIME" >&2
  echo "Start AgentOS (or crewchat-model) first." >&2
  exit 1
fi

URL="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["controlUrl"])' "$RUNTIME")"
TOKEN="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["controlToken"])' "$RUNTIME")"

curl -sS "$URL/api/model/rpc" \
  -H "content-type: application/json" \
  -H "x-model-web-auth: $TOKEN" \
  -d '{"method":"applyProfile","args":[]}' \
  | python3 -m json.tool
