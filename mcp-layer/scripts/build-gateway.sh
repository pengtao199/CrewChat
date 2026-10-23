#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
UPSTREAM="$ROOT/vendor/gateway"
OUT="$ROOT/bin/crewchat-mcp-gateway"

mkdir -p "$ROOT/bin"
cd "$UPSTREAM"
go build -trimpath -ldflags="-s -w" -o "$OUT" .
echo "built $OUT"
