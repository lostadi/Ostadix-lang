#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
FIXTURE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/ostadix-world-project-runtime.XXXXXX")
RECEIPT_PATH="$FIXTURE_DIR/receipt.hex"
SEMANTIC_PATH="$FIXTURE_DIR/semantic.sha256"

cleanup() {
  rm -f -- "$RECEIPT_PATH" "$SEMANTIC_PATH"
  rmdir "$FIXTURE_DIR" 2>/dev/null || true
}
trap cleanup EXIT

cd "$REPO_ROOT"
O_PROJECT_WORLD_RECEIPT_HEX_OUT="$RECEIPT_PATH" \
O_PROJECT_WORLD_RECEIPT_SEMANTIC_OUT="$SEMANTIC_PATH" \
  cargo test --locked --package o-lang --test project_world_runtime \
    world_bound_success_observes_runtime_graph_and_emits_uncommitted_receipt -- --exact

if [[ ! -s "$RECEIPT_PATH" || ! -s "$SEMANTIC_PATH" ]]; then
  echo "World project runtime fixture generation: FAIL" >&2
  exit 1
fi

SEMANTIC_DIGEST=$(tr -d '\r\n' < "$SEMANTIC_PATH")
if [[ ! "$SEMANTIC_DIGEST" =~ ^[0-9a-f]{64}$ ]]; then
  echo "World project runtime semantic digest format: FAIL" >&2
  exit 1
fi

echo "World project runtime hosted fixture: PASS"
"$SCRIPT_DIR/smoke-world-project-receipt-qemu.sh" \
  "$RECEIPT_PATH" "$SEMANTIC_DIGEST"
