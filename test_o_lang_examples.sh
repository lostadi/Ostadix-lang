#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
O_BIN="${O_BIN:-$ROOT_DIR/target/release/O}"

if [[ ! -x "$O_BIN" ]]; then
  printf '[FAIL] missing O binary at %s\n' "$O_BIN" >&2
  exit 1
fi

exec python3 "$ROOT_DIR/tests/example_manifest.py" run \
  --edition rust \
  --runner "$O_BIN" \
  --backends "$ROOT_DIR/backends" \
  --classification unit \
  --classification integration
