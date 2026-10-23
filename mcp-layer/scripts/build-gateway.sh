#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
UPSTREAM="$ROOT/vendor/gateway"
OUT="$ROOT/bin/crewchat-mcp-gateway"

log() {
  printf '[%s] %s\n' "$(date '+%H:%M:%S')" "$*"
}

mkdir -p "$ROOT/bin"
cd "$UPSTREAM"

log "gateway source: $UPSTREAM"
log "module cache: $(go env GOMODCACHE)"
log "step 1/3: download modules"
if [ "${VERBOSE:-1}" = "0" ]; then
  go mod download
else
  go mod download -x
fi

log "step 2/3: build sidecar"
if [ "${VERBOSE:-1}" = "0" ]; then
  go build -trimpath -ldflags="-s -w" -o "$OUT" .
else
  go build -v -trimpath -ldflags="-s -w" -o "$OUT" .
fi

log "step 3/3: done"
ls -lh "$OUT"
log "binary: $OUT"
