#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ostadix-world-resource-keys.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT
transcript="$work_dir/transcript.log"

run() {
    "$@" 2>&1 | tee -a "$transcript"
}

run_logged() {
    local log=$1
    shift
    : >"$log"
    "$@" 2>&1 | tee -a "$transcript" "$log"
}

require_test() {
    local log=$1 name=$2
    if ! grep -Fq "test $name ... ok" "$log"; then
        printf 'required test did not execute successfully: %s\n' "$name" >&2
        exit 1
    fi
}

require_test_count() {
    local log=$1 expected=$2
    if ! grep -Eq "^test result: ok\\. ${expected} passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;" "$log"; then
        printf 'required test count was not observed: %s\n' "$expected" >&2
        exit 1
    fi
}

mark() {
    printf '%s\n' "$1" | tee -a "$transcript"
}

resource_tests="$work_dir/world-resource-keys-tests.log"
run_logged "$resource_tests" env CARGO_TERM_COLOR=never \
    cargo test --locked --package o-lang --test world_resource_keys
require_test_count "$resource_tests" 5
require_test "$resource_tests" roadmap_vocabulary_has_one_typed_classification_and_stable_display
require_test "$resource_tests" device_and_accelerator_views_share_the_generic_resource_dependency
require_test "$resource_tests" source_effect_declarations_cannot_mint_any_governed_class
require_test "$resource_tests" typed_payloads_reject_caller_supplied_stale_references
require_test "$resource_tests" typed_payloads_reject_logical_substitution_and_governor_log_drift
mark 'World ResourceKey roadmap vocabulary: PASS'
mark 'World ResourceKey underlying identity helper caller-pair comparison: PASS'
mark 'World ResourceKey source-forgery rejection: PASS'

hgraph_test="$work_dir/world-resource-keys-hgraph.log"
hgraph_name='hgraph::from_oir::world_resource_key_tests::world_resource_keys_share_the_generic_hgraph_state_chain'
run_logged "$hgraph_test" env CARGO_TERM_COLOR=never cargo test --locked --package o-lang --lib \
    hgraph::from_oir::world_resource_key_tests::world_resource_keys_share_the_generic_hgraph_state_chain \
    -- --exact
require_test_count "$hgraph_test" 1
require_test "$hgraph_test" "$hgraph_name"
mark 'World ResourceKey HGraph state-transition corpus: PASS'

grounding_test="$work_dir/world-resource-keys-grounding.log"
grounding_name='world::grounding::tests::governed_resource_keys_project_into_governed_and_ambient_fields'
run_logged "$grounding_test" env CARGO_TERM_COLOR=never cargo test --locked --package o-lang --lib \
    world::grounding::tests::governed_resource_keys_project_into_governed_and_ambient_fields \
    -- --exact
require_test_count "$grounding_test" 1
require_test "$grounding_test" "$grounding_name"
mark 'World ResourceKey governed/ambient grounding projection: PASS'

run cargo build --locked --package o-lang --bin olangc
cli_log="$work_dir/world-resource-keys-cli.log"
run_logged "$cli_log" "$ROOT/target/debug/olangc" \
    "$ROOT/examples/hello.O" \
    --target ir \
    --grounding \
    --world-id desk \
    --world-epoch 4
grep -Fqx 'governed-effects none' "$cli_log"
grep -Fqx 'ambient-effects P0 reads=[HostWorld] writes=[HostWorld] hostworld=residual' "$cli_log"
mark 'World ResourceKey residual HostWorld CLI: PASS'

for forbidden in \
    'Mode 31: PASS' \
    'G0: PASS' \
    'G1: PASS' \
    'Acceptance A: PASS' \
    'Governor: online' \
    'HostWorld eliminated' \
    'device assignment: PASS' \
    'hardware isolation: PASS'
do
    if grep -Fq "$forbidden" "$transcript"; then
        printf 'forbidden overclaim marker in hosted transcript: %s\n' "$forbidden" >&2
        exit 1
    fi
done

mark 'World ResourceKey hosted repository-conformance: PASS'
