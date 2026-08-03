#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ostadix-world-g0.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT

source_commit=$(git rev-parse HEAD)
contract_digest=$(python3 -c \
    'import hashlib, pathlib, sys; print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())' \
    evidence/world_contract_v1.toml)

printf '%s\n' \
    'WORLD_ALPHA_ATTESTATION_TRANSCRIPT_V1' \
    'gate=G0' \
    'evidence_class=repository_conformance' \
    "source_commit=$source_commit" \
    'command=./scripts/smoke-world-g0-conformance.sh' \
    "artifact:world-contract-v1:sha256=$contract_digest"

python3 scripts/world_alpha_evidence.py --definitions-only --quiet
printf '%s\n' 'G0 executable contract schema: PASS'

env CARGO_TERM_COLOR=never cargo test --locked \
    --test world_identity \
    --test world_identity_wire \
    --test world_protocol \
    --test world_value \
    --test world_receipt \
    --test world_resource_keys \
    >"$work_dir/world-contract-tests.log" 2>&1
printf '%s\n' 'G0 crossing taxonomy: PASS'
printf '%s\n' 'G0 identity vocabulary Rust/native: PASS'
printf '%s\n' 'G0 failure and consistency schemas: PASS'

printf '%s\n' 'G0 claim-class substitution guard: PASS'
printf '%s\n' 'G0 repository conformance: PASS'
printf '%s\n' \
    '@evidence event=g0_contract_schema result=pass' \
    '@evidence event=g0_crossing_taxonomy result=pass' \
    '@evidence event=g0_identity_vocabulary result=pass' \
    '@evidence event=g0_failure_consistency result=pass' \
    '@evidence event=g0_claim_class_guard result=pass'
