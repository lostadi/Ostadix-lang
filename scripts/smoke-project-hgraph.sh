#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ostadix-project-hgraph.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT
transcript="$work_dir/transcript.log"
fixture="$ROOT/tests/fixtures/project_hgraph"
export PR7_NONEXEC_MARKER="$work_dir/project-command-executed"

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

require_count() {
    local log=$1 pattern=$2 expected=$3
    local actual
    actual=$(grep -Ec "$pattern" "$log" || true)
    if [[ "$actual" != "$expected" ]]; then
        printf 'expected %s occurrence(s) of %s, found %s\n' "$expected" "$pattern" "$actual" >&2
        exit 1
    fi
}

mark() {
    printf '%s\n' "$1" | tee -a "$transcript"
}

tests_log="$work_dir/project-hgraph-tests.log"
run_logged "$tests_log" env CARGO_TERM_COLOR=never \
    cargo test --locked --test project_hgraph
require_test_count "$tests_log" 10
for name in \
    real_bundle_constructs_all_five_project_operations \
    topology_preserves_logical_branches_prerequisites_compare_and_selection \
    project_plan_text_is_deterministic_and_omits_command_and_environment_values \
    directory_and_lifted_bundle_produce_identical_project_plans \
    every_route_policy_uses_shared_resolution_and_exact_policy_metadata \
    malformed_project_references_duplicates_and_cycles_fail_before_planning \
    project_source_validation_rejects_bundle_and_policy_substitution \
    project_projection_rejects_dependency_and_effect_forgery \
    governed_route_effect_spelling_is_rejected_and_pure_metadata_keeps_hostworld \
    real_cli_plans_directory_and_lifted_project_without_execution
do
    require_test "$tests_log" "$name"
done
mark 'ProjectBundle selection and exact provenance: PASS'
mark 'Project HGraph malformed/substitution rejection: PASS'

run cargo build --locked --bin olangc
first="$work_dir/project-plan-first.log"
second="$work_dir/project-plan-second.log"
run_logged "$first" "$ROOT/target/debug/olangc" "$fixture" \
    --target ir --route main
"$ROOT/target/debug/olangc" "$fixture" --target ir --route main \
    >"$second" 2>>"$transcript"
cmp "$first" "$second"

dispatcher_log="$work_dir/project-plan-dispatcher.log"
O_LANG_OLANGC_BIN="$ROOT/target/debug/olangc" \
    "$ROOT/scripts/o-cli.sh" plan "$fixture" --route main \
    >"$dispatcher_log" 2>>"$transcript"
cmp "$first" "$dispatcher_log"

installed_cargo_bin="$work_dir/installed-cargo/bin"
installed_local_bin="$work_dir/installed-local/bin"
run "$ROOT/scripts/install-o-cli-wrapper.sh" "$installed_cargo_bin/o"
run "$ROOT/scripts/install-o-cli-wrapper.sh" "$installed_local_bin/o"
installed_dispatch_log="$work_dir/project-plan-installed-dispatcher.log"
PATH="$installed_local_bin:$installed_cargo_bin:$ROOT/target/release:/usr/bin:/bin" \
    O_LANG_OLANGC_BIN="$ROOT/target/debug/olangc" \
    o plan "$fixture" --route main \
    >"$installed_dispatch_log" 2>>"$transcript"
cmp "$first" "$installed_dispatch_log"

fallback_log="$work_dir/project-dispatcher-fallback.log"
PATH="$installed_cargo_bin:$installed_local_bin:$ROOT/target/release:/usr/bin:/bin" \
    O_LANG_EVALUATOR_BIN=/bin/echo \
    o pr7-fallback-probe preserved-argument \
    >"$fallback_log" 2>>"$transcript"
grep -Fqx 'pr7-fallback-probe preserved-argument' "$fallback_log"

grep -Fqx '; ProjectExecutionPlan' "$first"
grep -Fqx 'selection target=main policy=verify_equivalent alternatives=[impl-a,impl-b] cancellation=none equivalence=required' "$first"
require_count "$first" '^project-op p[0-9]+ kind=materialize-project ' 2
require_count "$first" '^project-op p[0-9]+ kind=build-route:' 4
require_count "$first" '^project-op p[0-9]+ kind=run-route:' 4
require_count "$first" '^project-op p[0-9]+ kind=compare-route-results ' 1
require_count "$first" '^project-op p[0-9]+ kind=select-route:verify_equivalent ' 1
grep -Fq 'deps=[p3,p2]' "$first"
grep -Fq 'deps=[p8,p7]' "$first"
grep -Fq 'guards=[env:PR7_REQUIRED_ENV] env=[PLAN_VARIANT]' "$first"
grep -Fq 'declared-pure=true' "$first"
grep -Fq 'reads=[HostWorld,project:input.txt]' "$first"
if grep -Fq 'PR7_IMPL_A_EXECUTED' "$first"; then
    printf 'project plan exposed or executed the poison command\n' >&2
    exit 1
fi
for marker in SHOULD_NOT_EXIST SHOULD_NOT_EXIST_A SHOULD_NOT_EXIST_B; do
    if [[ -e "$fixture/$marker" ]]; then
        printf 'project planning executed a route and created %s\n' "$marker" >&2
        exit 1
    fi
done
if [[ -e "$PR7_NONEXEC_MARKER" ]]; then
    printf 'project planning executed a route outside its disposable workspace\n' >&2
    exit 1
fi
mark 'Project route operation construction: PASS'
mark 'Project logical-branch topology and residual shared HostWorld: PASS'
mark 'Project route policy and equivalence metadata: PASS'
mark 'Project hosted HostWorld and source-authority boundary: PASS'

default_log="$work_dir/project-plan-default.log"
run_logged "$default_log" "$ROOT/target/debug/olangc" "$fixture" --target ir
grep -Fqx 'selection target=impl-a policy=explicit:impl-a alternatives=[impl-a] cancellation=none equivalence=none' "$default_log"
require_count "$default_log" '^project-op p[0-9]+ kind=materialize-project ' 1
require_count "$default_log" '^project-op p[0-9]+ kind=select-route:explicit:impl-a ' 1

ordinary_log="$work_dir/ordinary-oir.log"
run_logged "$ordinary_log" "$ROOT/target/debug/olangc" \
    "$ROOT/examples/hello.O" --target ir
grep -Fqx '; OIrProgram' "$ordinary_log"
grep -Fqx '; HGraph' "$ordinary_log"

dot_first="$work_dir/project-first.dot"
dot_second="$work_dir/project-second.dot"
"$ROOT/target/debug/olangc" "$fixture" --target dot --route main \
    >"$dot_first" 2>>"$transcript"
"$ROOT/target/debug/olangc" "$fixture" --target dot --route main \
    >"$dot_second" 2>>"$transcript"
cmp "$dot_first" "$dot_second"
grep -Fq 'digraph hgraph {' "$dot_first"
grep -Fq 'materialize-project' "$dot_first"
grep -Fq 'compare-route-results' "$dot_first"
grep -Fq 'select-route:verify_equivalent' "$dot_first"
if [[ -e "$PR7_NONEXEC_MARKER" ]]; then
    printf 'project DOT planning executed a route outside its disposable workspace\n' >&2
    exit 1
fi
mark 'Project IR/DOT nonexecuting CLI and ordinary OIR compatibility: PASS'
mark 'Installed repository-owned o plan and evaluator fallback parity: PASS'

compiled_project="$work_dir/project"
run "$ROOT/target/debug/olangc" "$fixture" -o "$compiled_project"
routes_log="$work_dir/compiled-project-routes.log"
run_logged "$routes_log" "$compiled_project" --list-routes
grep -Fq 'Project: pr7-project-hgraph' "$routes_log"
grep -Fq 'set provides=main policy=verify_equivalent alternatives=[impl-a, impl-b]' "$routes_log"

missing_route_log="$work_dir/compiled-project-missing-route.log"
if "$compiled_project" --route >"$missing_route_log" 2>&1; then
    printf 'compiled project accepted --route without a value\n' >&2
    exit 1
fi
grep -Fq -- '--route requires a value' "$missing_route_log"

flag_as_route_log="$work_dir/compiled-project-flag-route.log"
if "$compiled_project" --route --list-routes >"$flag_as_route_log" 2>&1; then
    printf 'compiled project accepted another flag as the --route value\n' >&2
    exit 1
fi
grep -Fq -- '--route requires a value' "$flag_as_route_log"

missing_policy_log="$work_dir/compiled-project-missing-policy.log"
if "$compiled_project" --routes-policy >"$missing_policy_log" 2>&1; then
    printf 'compiled project accepted --routes-policy without a value\n' >&2
    exit 1
fi
grep -Fq -- '--routes-policy requires a value' "$missing_policy_log"

flag_as_policy_log="$work_dir/compiled-project-flag-policy.log"
if "$compiled_project" --routes-policy --list-routes >"$flag_as_policy_log" 2>&1; then
    printf 'compiled project accepted another flag as the --routes-policy value\n' >&2
    exit 1
fi
grep -Fq -- '--routes-policy requires a value' "$flag_as_policy_log"

invalid_policy_log="$work_dir/compiled-project-invalid-policy.log"
if "$compiled_project" --route main --routes-policy definitely-not-a-policy \
    >"$invalid_policy_log" 2>&1
then
    printf 'compiled project accepted an unknown route policy\n' >&2
    exit 1
fi
grep -Fq 'unknown route policy' "$invalid_policy_log"
if [[ -e "$PR7_NONEXEC_MARKER" ]]; then
    printf 'compiled-project inspection or policy rejection executed a route\n' >&2
    exit 1
fi
mark 'Generated project binary embedding and checked policy CLI: PASS'

for forbidden in \
    'project HGraph execution: PASS' \
    'project commands executed: PASS' \
    'G1: PASS' \
    'G10: PASS' \
    'Mode 31: PASS' \
    'Governor: online' \
    'placement: PASS' \
    'native project planning: PASS' \
    'hardware isolation: PASS'
do
    if grep -Fq "$forbidden" "$transcript"; then
        printf 'forbidden overclaim marker in hosted transcript: %s\n' "$forbidden" >&2
        exit 1
    fi
done

mark 'Project HGraph hosted logical planning: PASS'
