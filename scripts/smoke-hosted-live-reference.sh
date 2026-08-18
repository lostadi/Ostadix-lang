#!/usr/bin/env bash
# Executable acceptance gate for the hosted Live-World semantic oracle.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
  echo "error: smoke-hosted-live-reference.sh must run inside a Git worktree" >&2
  exit 2
}
state_dir="$(mktemp -d "${TMPDIR:-/tmp}/ostadix-live-host.XXXXXX")"
cleanup() {
  if [[ -d "$state_dir" ]]; then
    # Store objects are deliberately read-only. Restore only owner write bits
    # beneath this exact mktemp root so recursive cleanup is quiet on macOS.
    chmod -R u+w "$state_dir" 2>/dev/null || true
    rm -rf -- "$state_dir"
  fi
}
trap cleanup EXIT HUP INT TERM

output="$(
  cd "$repo_root"
  cargo run --quiet --package o-lang --bin o-live-host -- demo --state "$state_dir"
)"

python3 - "$output" <<'PY'
import sys

output = sys.argv[1]
markers = [
    "HOSTED live reference: immutable package CAS PASS",
    "HOSTED live reference: over-broad capability denied",
    "HOSTED live reference: health-gated activation PASS",
    "HOSTED live reference: cross-world OValue composition PASS",
    "HOSTED live reference: failed upgrade rollback PASS",
    "HOSTED live reference: stale service bearer denied",
    "HOSTED live reference: crash isolation and restart PASS",
    "HOSTED live reference: active-set reconstruction PASS",
    "HOSTED live reference: PASS",
]
missing = [marker for marker in markers if output.count(marker) == 0]
duplicated = [marker for marker in markers if output.count(marker) > 1]
positions = [output.find(marker) for marker in markers]
forbidden = [
    marker
    for marker in (
        "O-core live system: PASS",
        "Milestone 5 complete",
        "Linux personality: PASS",
        "CAPABILITY LEAKED",
    )
    if marker in output
]
if missing or duplicated or positions != sorted(positions) or forbidden:
    print("HOSTED live reference smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("duplicated:", repr(duplicated), file=sys.stderr)
    if positions != sorted(positions):
        print("marker order is invalid", file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    raise SystemExit(1)
print(output)
PY
