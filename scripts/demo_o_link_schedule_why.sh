#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
    cat <<'EOF'
Usage: scripts/demo_o_link_schedule_why.sh [OPTIONS]

Literal-link a multi-file ordinary-O pipeline, inspect its admitted schedule,
select the first hosted operation in predicted layer 3 for `o why`, and compare
serial and graph returned-value semantics. A foreign-file link is checked as a
negative control: o-link assigns persistent [N] environments, so those ordinary
wrapped blocks remain coordinator-owned.

Options:
  --workers N         Graph worker capacity (default: 4)
  --output-dir PATH   Retain generated evidence here
                      (default: target/tmp/o_link_schedule_why_demo)
  -h, --help          Show this help

Environment overrides:
  O_BIN               O evaluator (default: target/release/O, then debug)
  OLANGC_BIN          olangc analyzer (default: target/release/olangc, then debug)
  O_LINK_BIN          o-link binary (default: target/release/o-link, then debug)
  O_BACKENDS_DIR      Adapter directory (default: backends)

The script never deletes its output directory. Existing named evidence files in
that directory are replaced by the current run.
EOF
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

default_binary() {
    local name=$1
    if [[ -x "$ROOT/target/release/$name" ]]; then
        printf '%s\n' "$ROOT/target/release/$name"
    elif [[ -x "$ROOT/target/debug/$name" ]]; then
        printf '%s\n' "$ROOT/target/debug/$name"
    else
        die "missing $name; build with: ./setup.sh -y --minimal"
    fi
}

require_pattern() {
    local path=$1 pattern=$2 claim=$3
    grep -Eq -- "$pattern" "$path" || die "$claim (see $path)"
}

reject_pattern() {
    local path=$1 pattern=$2 claim=$3
    if grep -Eq -- "$pattern" "$path"; then
        die "$claim (see $path)"
    fi
}

require_count() {
    local path=$1 pattern=$2 expected=$3 claim=$4 actual
    actual=$(grep -Ec -- "$pattern" "$path" || true)
    [[ "$actual" -eq "$expected" ]] ||
        die "$claim: expected $expected, observed $actual (see $path)"
}

compare_semantics() {
    local label=$1 serial_path=$2 graph_path=$3 expected_integer=$4
    local serial_semantic=$5 graph_semantic=$6
    python3 - \
        "$label" "$serial_path" "$graph_path" "$expected_integer" \
        "$serial_semantic" "$graph_semantic" <<'PY'
import json
from pathlib import Path
import sys

label, serial_path, graph_path, expected_integer, serial_out, graph_out = sys.argv[1:]

def read_result(path):
    with open(path, encoding="utf-8") as handle:
        payload = json.load(handle)
    if payload.get("ok") is not True:
        raise SystemExit(f"{label}: unsuccessful O result in {path}: {payload!r}")
    return payload, {key: payload[key] for key in ("ok", "type", "value")}

serial_payload, serial = read_result(serial_path)
graph_payload, graph = read_result(graph_path)
Path(serial_out).write_text(
    json.dumps(serial, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
Path(graph_out).write_text(
    json.dumps(graph, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
if serial != graph:
    raise SystemExit(
        f"{label}: serial and graph returned-value semantics differ: "
        f"serial={serial!r} graph={graph!r}"
    )
expected = {
    "ok": True,
    "type": "number",
    "value": {"t": "number", "v": {"kind": "int", "v": expected_integer}},
}
if serial != expected:
    raise SystemExit(f"{label}: result differs from fixture expectation: {serial!r}")
print(
    f"{label}: semantic-equivalence=true value={expected_integer} "
    f"serial_elapsed_ms={serial_payload['elapsed_ms']} "
    f"graph_elapsed_ms={graph_payload['elapsed_ms']}"
)
PY
}

workers=4
output_dir=$ROOT/target/tmp/o_link_schedule_why_demo
while [[ $# -gt 0 ]]; do
    case "$1" in
        --workers)
            [[ $# -ge 2 ]] || die "--workers requires a value"
            workers=$2
            shift 2
            ;;
        --workers=*)
            workers=${1#*=}
            shift
            ;;
        --output-dir)
            [[ $# -ge 2 ]] || die "--output-dir requires a path"
            output_dir=$2
            shift 2
            ;;
        --output-dir=*)
            output_dir=${1#*=}
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            printf 'unknown option: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$workers" in
    ''|*[!0-9]*) die "--workers must be a positive integer" ;;
esac
[[ "$workers" -ge 1 ]] || die "--workers must be at least 1"

command -v python3 >/dev/null 2>&1 || die "python3 is required for semantic comparison"

o_bin=${O_BIN:-$(default_binary O)}
olangc_bin=${OLANGC_BIN:-$(default_binary olangc)}
o_link_bin=${O_LINK_BIN:-$(default_binary o-link)}
backends_dir=${O_BACKENDS_DIR:-$ROOT/backends}
o_cli=$ROOT/scripts/o-cli.sh

for executable in "$o_bin" "$olangc_bin" "$o_link_bin" "$o_cli"; do
    [[ -x "$executable" ]] || die "not executable: $executable"
done
[[ -d "$backends_dir" ]] || die "adapter directory does not exist: $backends_dir"
mkdir -p -- "$output_dir"

positive_fixture=$ROOT/tests/fixtures/o_link_schedule_why/o_pipeline
foreign_fixture=$ROOT/tests/fixtures/o_link_schedule_why/foreign_files
positive_program=$output_dir/complex-linked.O
foreign_program=$output_dir/foreign-linked.O
positive_explain=$output_dir/complex-explain-schedule.txt
foreign_explain=$output_dir/foreign-explain-schedule.txt
positive_why=$output_dir/complex-layer-3-why.txt
foreign_why=$output_dir/foreign-first-why.txt

printf '== Link ordinary .O stages (link-only) ==\n'
"$o_link_bin" "$positive_fixture" --literal --verbose-skips -o "$positive_program"
require_count "$positive_program" '^# ── [^ ]+\.O ──$' 4 \
    "ordinary-O link did not preserve all four source sections"
reject_pattern "$positive_program" '^[a-z_][a-z0-9_]*\[[0-9]+\]\^\(' \
    "ordinary .O input was unexpectedly wrapped in a persistent backend environment"

printf '\n== Explain one admitted 1 -> 4 -> 3 -> 1 hosted topology ==\n'
"$olangc_bin" "$positive_program" \
    --target ir --explain-schedule --workers "$workers" --shim-dir "$backends_dir" \
    >"$positive_explain"
require_count "$positive_explain" '^; ExecutionAdmission oexec\.admission/v3$' 1 \
    "missing unique v3 admission"
require_count "$positive_explain" '^; SchedulePrediction oexec\.schedule-prediction/v1$' 1 \
    "missing unique hosted-task prediction"
require_pattern "$positive_explain" \
    '^schedule-prediction schema=oexec\.schedule-prediction/v1 .* task-count=9 predicted-width=4 predicted-span=4 span-unit=hosted-task-layers$' \
    "linked pipeline did not admit the reviewed 9-task, width-4, span-4 topology"
for expected_layer in \
    '^schedule-prediction-layer index=1 operations=\[P[0-9]+\]$' \
    '^schedule-prediction-layer index=2 operations=\[P[0-9]+,P[0-9]+,P[0-9]+,P[0-9]+\]$' \
    '^schedule-prediction-layer index=3 operations=\[P[0-9]+,P[0-9]+,P[0-9]+\]$' \
    '^schedule-prediction-layer index=4 operations=\[P[0-9]+\]$'
do
    require_pattern "$positive_explain" "$expected_layer" \
        "linked pipeline emitted an unexpected hosted-task layer"
done
require_count "$positive_explain" \
    '^  dispatch lane=local-worker adapter=autonomous-ephemeral-shim/v1 semantics=explicit-autonomous-unordered ' \
    7 "expected seven autonomous hosted local-worker admissions"

layer_three_line=$(grep -E \
    '^schedule-prediction-layer index=3 operations=\[P[0-9]+(,P[0-9]+)*\]$' \
    "$positive_explain")
layer_three_members=$(printf '%s\n' "$layer_three_line" |
    sed -E 's/^.*operations=\[([^]]+)\]$/\1/')
why_target=${layer_three_members%%,*}
[[ "$why_target" =~ ^P[0-9]+$ ]] || die "could not select an operation from layer 3"

printf '\n== Focused why for first layer-3 operation: %s ==\n' "$why_target"
O_LANG_OLANGC_BIN="$olangc_bin" "$o_cli" why \
    "$positive_program" "$why_target" --shim-dir "$backends_dir" >"$positive_why"
require_pattern "$positive_why" \
    "^why operation=$why_target status=admitted-static inspection-only=yes dispatch=not-run " \
    "focused why did not select $why_target"
require_pattern "$positive_why" \
    '^  dispatch lane=local-worker adapter=autonomous-ephemeral-shim/v1 semantics=explicit-autonomous-unordered ' \
    "layer-3 target was not admitted through the autonomous worker adapter"
require_pattern "$positive_why" '^blocker-witness predecessor=P[0-9]+ ' \
    "layer-3 target has no exact blocker witness"
require_pattern "$positive_why" \
    '^hosted-task-layer index=3 operations=\[P[0-9]+,P[0-9]+,P[0-9]+\]$' \
    "focused why lost the target's hosted-task layer"
require_pattern "$positive_why" '^; SourceOrigin oexec\.source-origin/v1$' \
    "focused why omitted descriptive source provenance"
grep -E -- \
    '^why operation=|^  dispatch lane=|^blocker-witness |^wave index=|^hosted-task-layer index=' \
    "$positive_why"

printf '\n== Execute serial and graph; compare returned semantics ==\n'
"$o_bin" --executor serial --workers "$workers" --json \
    "$positive_program" "$backends_dir" >"$output_dir/complex-serial.json"
"$o_bin" --executor graph --workers "$workers" --json \
    "$positive_program" "$backends_dir" >"$output_dir/complex-graph.json"
compare_semantics complex \
    "$output_dir/complex-serial.json" "$output_dir/complex-graph.json" 111 \
    "$output_dir/complex-serial-semantic.json" "$output_dir/complex-graph-semantic.json"

printf '\n== Foreign-file persistent-environment boundary ==\n'
"$o_link_bin" "$foreign_fixture" --literal --verbose-skips -o "$foreign_program"
require_pattern "$foreign_program" '^python\[0\]\^\($' \
    "first foreign Python file did not receive environment [0]"
require_pattern "$foreign_program" '^python\[1\]\^\($' \
    "second foreign Python file did not receive environment [1]"
"$olangc_bin" "$foreign_program" \
    --target ir --explain-schedule --workers "$workers" --shim-dir "$backends_dir" \
    >"$foreign_explain"
require_pattern "$foreign_explain" \
    '^schedule-prediction schema=oexec\.schedule-prediction/v1 .* task-count=2 predicted-width=1 predicted-span=2 span-unit=hosted-task-layers$' \
    "foreign control did not retain the reviewed serialized hosted topology"
require_count "$foreign_explain" \
    '^  dispatch lane=coordinator adapter=coordinator/v1 semantics=strict-equivalent preparation=coordinator-owned$' \
    2 "foreign wrapped operations were not both coordinator-owned"
reject_pattern "$foreign_explain" '^  dispatch lane=local-worker ' \
    "persistent foreign-file operation entered the local-worker lane"

foreign_target=$(awk '/^operation P[0-9]+ admitted=yes / { print $2; exit }' "$foreign_explain")
[[ "$foreign_target" =~ ^P[0-9]+$ ]] || die "could not select the foreign control operation"
O_LANG_OLANGC_BIN="$olangc_bin" "$o_cli" why \
    "$foreign_program" "$foreign_target" --shim-dir "$backends_dir" >"$foreign_why"
require_pattern "$foreign_why" "^plan-node $foreign_target kind=exec python \[env 0\] " \
    "foreign focused why did not preserve persistent environment identity"
require_pattern "$foreign_why" \
    '^  dispatch lane=coordinator adapter=coordinator/v1 semantics=strict-equivalent preparation=coordinator-owned$' \
    "foreign focused why did not report coordinator placement"
require_pattern "$foreign_why" 'actor:python\[0\]' \
    "foreign focused why omitted persistent actor-state effects"

"$o_bin" --executor serial --workers "$workers" --json \
    "$foreign_program" "$backends_dir" >"$output_dir/foreign-serial.json"
"$o_bin" --executor graph --workers "$workers" --json \
    "$foreign_program" "$backends_dir" >"$output_dir/foreign-graph.json"
compare_semantics foreign-control \
    "$output_dir/foreign-serial.json" "$output_dir/foreign-graph.json" 2 \
    "$output_dir/foreign-serial-semantic.json" "$output_dir/foreign-graph-semantic.json"

printf '\nPASS: linked schedule-why demonstration\n'
printf '  positive program: %s\n' "$positive_program"
printf '  full admission:   %s\n' "$positive_explain"
printf '  focused why:      %s (%s)\n' "$positive_why" "$why_target"
printf '  foreign control:  %s\n' "$foreign_explain"
printf '  artifacts:        %s\n' "$output_dir"
printf '%s\n' \
    '  boundary: batch preserves member/result order; admitted overlap does not prove CPU parallelism.'
printf '%s\n' \
    '  note: separate inspection processes bind distinct ambient snapshots, so their admission digests need not match.'
