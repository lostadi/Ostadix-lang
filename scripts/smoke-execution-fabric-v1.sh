#!/usr/bin/env bash
# Same-host, distinct-process M3 proof for two independently configured
# `o-node` processes. This is not distinct-kernel, physical-multinode, or
# heterogeneous-hardware evidence.
#
# Exact M3 nonclaims: no arbitrary OIR region execution; no general `.O`
# distribution; no automatic placement; no capacity scheduler; no scope
# transport; no object plane; no bulk node-to-node data transfer; no actors;
# no external effects; no automatic retry; no coordinator crash recovery; no
# hardware-resource execution; no GPU or camera driver mediation; no process
# migration; no shared address space; no physical multinode claim; no
# distinct-kernel claim; no heterogeneous-architecture claim; no exactly-once
# external effect claim.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ostadix-execution-fabric-v1.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT HUP INT TERM

test_name=two_real_o_nodes_execute_provisional_pure_candidates_without_graph_authority
test_log="$work_dir/two-node.log"
if ! env CARGO_TERM_COLOR=never OSTADIX_TEST_RUNTIME_POLICY=required \
    cargo test --locked --package o-lang \
    --test execution_fabric_two_node "$test_name" -- \
    --exact --nocapture --test-threads=1 >"$test_log" 2>&1; then
    sed -n '1,320p' "$test_log" >&2
    exit 1
fi

expected_result="test $test_name ... ok"
if ! grep -Fq -- "running 1 test" "$test_log" \
    || [ "$(grep -Fc -- "$expected_result" "$test_log" || true)" -ne 1 ]; then
    echo "error: the exact two-o-node integration test did not run once" >&2
    sed -n '1,320p' "$test_log" >&2
    exit 1
fi

printf '%s\n' \
    'Fabric V1 authenticated two-o-node pure execution: PASS' \
    'Fabric V1 wrong-node lease rejection: PASS' \
    'Fabric V1 wrong-node result no-commit boundary: PASS' \
    'Fabric V1 stopped-node infrastructure failure and no fallback: PASS' \
    'Fabric V1 Hosted and Mesh ALPN preservation: PASS' \
    'Fabric V1 same-host distinct-process boundary: PASS' \
    'Execution Fabric V1 M3: PASS'
