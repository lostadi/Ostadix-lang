#!/usr/bin/env bash
set -euo pipefail

# Bounded hosted Project HGraph smoke: ordered and parallel policies, drained
# race cancellation, validated selection, trace replay, and execution contracts.
# This does not establish Governor authority, native execution, or G1 passage.
# Retry, placement, OWRECEIPT attestation, and exactly-once effects remain outside
# this hosted test corpus.

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ostadix-project-hgraph-exec.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT

if ! env CARGO_TERM_COLOR=never cargo test --locked --quiet --package o-lang \
    --test project_hgraph_exec >"$work_dir/test.log" 2>&1; then
    sed -n '1,240p' "$work_dir/test.log" >&2
    exit 1
fi

printf '%s\n' 'Project HGraph ordered hosted execution: PASS'
