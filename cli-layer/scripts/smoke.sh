#!/usr/bin/env bash
# End-to-end smoke for the ACP host, using tests/fixtures/fake_acp.py.
# No model provider or network is needed.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${CLI_LAYER_SMOKE_PORT:-3097}"
TOKEN="smoke-token"
DATA="$(mktemp -d)"
PID=""

cleanup() {
  [[ -n "$PID" ]] && kill "$PID" 2>/dev/null || true
  rm -rf "$DATA"
}
trap cleanup EXIT

echo "==> building"
cargo build --manifest-path "$ROOT/Cargo.toml" >/dev/null

echo "==> starting cli-layer on :$PORT"
CLI_LAYER_DATA_DIR="$DATA" \
CLI_LAYER_PORT="$PORT" \
CLI_LAYER_TOKEN="$TOKEN" \
CLI_LAYER_CLAUDE_COMMAND="python3 $ROOT/tests/fixtures/fake_acp.py" \
  "$ROOT/target/debug/crewchat-cli" >"$DATA/cli.log" 2>&1 &
PID=$!

for _ in $(seq 1 50); do
  if curl -sf -H "Authorization: Bearer $TOKEN" \
    -X POST "http://127.0.0.1:$PORT/api/health" -d '{}' >/dev/null; then
    break
  fi
  sleep 0.2
done

python3 - "$PORT" "$TOKEN" "$DATA/cli-layer.db" <<'PY'
import json, sqlite3, sys, time, urllib.request

port, token, db_path = sys.argv[1], sys.argv[2], sys.argv[3]
base = f"http://127.0.0.1:{port}"
headers = {"Authorization": f"Bearer {token}", "content-type": "application/json"}

def call(path, body):
    request = urllib.request.Request(
        base + path, data=json.dumps(body).encode(), headers=headers
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.load(response)

health = call("/api/health", {})
assert health["status"] == "ok", health

permission_face = call("/api/agent_role_permission_face", {"agentType": "claude_code"})
assert permission_face["supports_session_mode"] is True, permission_face
assert any(mode["id"] == "acceptEdits" for mode in permission_face["modes"]), permission_face

connection = call("/api/acp_connect", {"agentType": "claude_code", "workingDir": "/tmp"})
connection_id = connection["connection_id"]
call("/api/acp_prompt", {
    "connectionId": connection_id,
    "blocks": [{"type": "text", "text": "hi"}],
})

deadline = time.time() + 10
rows = []
while time.time() < deadline:
    rows = [json.loads(row[0]) for row in sqlite3.connect(db_path).execute(
        "SELECT envelope_json FROM events WHERE connection_id = ? ORDER BY seq",
        (connection_id,),
    )]
    if any(row.get("type") == "turn_complete" for row in rows):
        break
    time.sleep(0.2)

types = [row.get("type") for row in rows]
assert "content_delta" in types, rows
assert "turn_complete" in types, rows
text = "".join(row.get("text", "") for row in rows if row.get("type") == "content_delta")
assert text == "hello from fake", text

call("/api/acp_disconnect", {"connectionId": connection_id})
print(f"PASS: {types} text={text!r}")
PY

echo "smoke: ok"
