#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

warmups=${HGRAPH_BENCH_WARMUPS:-1}
repetitions=${HGRAPH_BENCH_REPETITIONS:-5}
sleep_seconds=${HGRAPH_BENCH_SLEEP_SECONDS:-0.25}
workers=${HGRAPH_BENCH_WORKERS:-4}
selected_shape=${HGRAPH_BENCH_SHAPE:-all}
missing_runtime_policy=${HGRAPH_BENCH_MISSING_RUNTIME:-skip}
o_bin=${O_RELEASE_BIN:-$ROOT/target/release/O}
olangc_bin=${OLANGC_RELEASE_BIN:-$(dirname -- "$o_bin")/olangc}
backends_dir=${O_BACKENDS_DIR:-$ROOT/backends}
fixture_dir=$ROOT/benchmarks/hgraph_hosted

usage() {
    cat <<'EOF'
Usage: scripts/benchmark_hgraph_hosted.sh [OPTIONS]

Benchmark four explicit hosted HGraph shapes through the release O CLI.
Results are descriptive; this script applies no performance threshold.

Options:
  --warmups N             Warmup pairs before measurement (default: 1)
  --repetitions N         Measured serial/graph pairs (default: 5)
  --sleep SECONDS         Hosted delay per benchmark node (default: 0.25)
  --workers N             Graph executor local-worker limit (default: 4)
  --shape NAME            all, heterogeneous, chained, mixed_width, or realistic
  --missing-runtime MODE  skip or fail (default: skip)
  --help                   Show this help

Environment overrides:
  HGRAPH_BENCH_WARMUPS
  HGRAPH_BENCH_REPETITIONS
  HGRAPH_BENCH_SLEEP_SECONDS
  HGRAPH_BENCH_WORKERS
  HGRAPH_BENCH_SHAPE
  HGRAPH_BENCH_MISSING_RUNTIME
  O_RELEASE_BIN
  OLANGC_RELEASE_BIN
  O_BACKENDS_DIR
EOF
}

need_value() {
    if [[ $# -lt 2 || -z ${2:-} ]]; then
        printf '%s requires a value\n' "$1" >&2
        exit 2
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --warmups)
            need_value "$@"
            warmups=$2
            shift 2
            ;;
        --warmups=*)
            warmups=${1#*=}
            shift
            ;;
        --repetitions)
            need_value "$@"
            repetitions=$2
            shift 2
            ;;
        --repetitions=*)
            repetitions=${1#*=}
            shift
            ;;
        --sleep)
            need_value "$@"
            sleep_seconds=$2
            shift 2
            ;;
        --sleep=*)
            sleep_seconds=${1#*=}
            shift
            ;;
        --workers)
            need_value "$@"
            workers=$2
            shift 2
            ;;
        --workers=*)
            workers=${1#*=}
            shift
            ;;
        --shape)
            need_value "$@"
            selected_shape=$2
            shift 2
            ;;
        --shape=*)
            selected_shape=${1#*=}
            shift
            ;;
        --missing-runtime)
            need_value "$@"
            missing_runtime_policy=$2
            shift 2
            ;;
        --missing-runtime=*)
            missing_runtime_policy=${1#*=}
            shift
            ;;
        --help|-h)
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

case "$warmups" in
    ''|*[!0-9]*)
        printf 'warmups must be a non-negative integer, got %s\n' "$warmups" >&2
        exit 2
        ;;
esac
case "$repetitions" in
    ''|*[!0-9]*)
        printf 'repetitions must be a positive integer, got %s\n' "$repetitions" >&2
        exit 2
        ;;
esac
case "$workers" in
    ''|*[!0-9]*)
        printf 'workers must be a positive integer, got %s\n' "$workers" >&2
        exit 2
        ;;
esac
if [[ "$repetitions" -eq 0 ]]; then
    printf 'repetitions must be at least one\n' >&2
    exit 2
fi
if [[ "$workers" -eq 0 ]]; then
    printf 'workers must be at least one\n' >&2
    exit 2
fi
case "$selected_shape" in
    all|heterogeneous|chained|mixed_width|realistic) ;;
    *)
        printf 'unknown shape: %s\n' "$selected_shape" >&2
        exit 2
        ;;
esac
case "$missing_runtime_policy" in
    skip|fail) ;;
    *)
        printf 'missing-runtime mode must be skip or fail, got %s\n' "$missing_runtime_policy" >&2
        exit 2
        ;;
esac

if ! command -v python3 >/dev/null 2>&1; then
    printf 'python3 is required by the benchmark harness and Python fixtures\n' >&2
    exit 1
fi
python3 - "$sleep_seconds" <<'PY'
import math
import sys

try:
    value = float(sys.argv[1])
except ValueError as exc:
    raise SystemExit(f"sleep must be a finite non-negative number: {exc}")
if not math.isfinite(value) or value < 0:
    raise SystemExit("sleep must be a finite non-negative number")
PY

if [[ ! -x "$o_bin" ]]; then
    printf 'release O binary is missing or not executable: %s\n' "$o_bin" >&2
    printf 'build it with: cargo build --release --locked --bin O\n' >&2
    exit 1
fi
if [[ ! -x "$olangc_bin" ]]; then
    printf 'release olangc binary is missing or not executable: %s\n' "$olangc_bin" >&2
    printf 'build it with: cargo build --release --locked --bin olangc\n' >&2
    exit 1
fi
if [[ ! -d "$backends_dir" ]]; then
    printf 'backend directory does not exist: %s\n' "$backends_dir" >&2
    exit 1
fi

shape_runtimes() {
    case "$1" in
        heterogeneous|realistic) printf 'python3,bash,node\n' ;;
        chained|mixed_width) printf 'python3\n' ;;
    esac
}

runtime_path() {
    command -v "$1" 2>/dev/null || true
}

runtime_version() {
    local path=$1 output
    if [[ -z "$path" ]]; then
        printf 'unavailable\n'
        return
    fi
    output=$("$path" --version 2>&1 || true)
    output=${output%%$'\n'*}
    printf '%s\n' "${output:-unknown}"
}

python_path=$(runtime_path python3)
bash_path=$(runtime_path bash)
node_path=$(runtime_path node)
python_version=$(runtime_version "$python_path")
bash_version=$(runtime_version "$bash_path")
node_version=$(runtime_version "$node_path")

runtime_path_for() {
    case "$1" in
        python3) printf '%s\n' "$python_path" ;;
        bash) printf '%s\n' "$bash_path" ;;
        node) printf '%s\n' "$node_path" ;;
    esac
}

missing_runtimes_for() {
    local required=$1 runtime path missing=
    local old_ifs=$IFS
    IFS=,
    for runtime in $required; do
        path=$(runtime_path_for "$runtime")
        if [[ -z "$path" ]]; then
            if [[ -n "$missing" ]]; then
                missing=$missing,
            fi
            missing=$missing$runtime
        fi
    done
    IFS=$old_ifs
    printf '%s\n' "${missing:-none}"
}

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ostadix-hgraph-hosted-bench.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT

render_fixture() {
    local fixture=$1 output=$2
    python3 - "$fixture" "$output" "$sleep_seconds" <<'PY'
import math
from pathlib import Path
import sys

source = Path(sys.argv[1])
destination = Path(sys.argv[2])
delay = float(sys.argv[3])
if not math.isfinite(delay) or delay < 0:
    raise SystemExit("sleep must be finite and non-negative")

seconds = f"{delay:.9f}".rstrip("0").rstrip(".") or "0"
milliseconds = f"{delay * 1000:.6f}".rstrip("0").rstrip(".") or "0"
text = source.read_text(encoding="utf-8")
text = text.replace("__SLEEP_SECONDS__", seconds)
text = text.replace("__SLEEP_MILLISECONDS__", milliseconds)
if "__SLEEP_" in text:
    raise SystemExit(f"fixture contains an unresolved timing placeholder: {source}")
destination.write_text(text, encoding="utf-8")
PY
}

analyze_fixture() {
    local shape=$1 program=$2 shape_dir=$3
    local explanation=$shape_dir/explain-schedule.txt
    local fields=$shape_dir/prediction-fields.txt

    if ! "$olangc_bin" "$program" \
        --target ir \
        --explain-schedule \
        --workers "$workers" \
        --shim-dir "$backends_dir" >"$explanation"
    then
        printf 'shape=%s schedule_analysis=failed\n' "$shape" >&2
        return 1
    fi

    if ! python3 - "$explanation" >"$fields" <<'PY'
import re
from pathlib import Path
import sys

path = Path(sys.argv[1])
lines = path.read_text(encoding="utf-8").splitlines()
admission_schema = "oexec.admission/v5"
admission_header = f"; ExecutionAdmission {admission_schema}"
admission_headers = [
    line for line in lines if line.startswith("; ExecutionAdmission ")
]
if admission_headers != [admission_header]:
    raise SystemExit(f"expected exactly one {admission_header!r} marker")

digest_pattern = r"[0-9a-f]{64}"
binding_pattern = re.compile(
    rf"binding analyzer-sha256=({digest_pattern}) "
    rf"evidence-sha256=({digest_pattern}) "
    rf"admitted-graph-sha256=({digest_pattern}) "
    rf"admission-sha256=({digest_pattern})"
)
admission_binding_lines = [
    line for line in lines if line.startswith("binding analyzer-sha256=")
]
if len(admission_binding_lines) != 1:
    raise SystemExit("expected exactly one canonical admission digest binding")
admission_binding = binding_pattern.fullmatch(admission_binding_lines[0])
if admission_binding is None:
    raise SystemExit("malformed canonical admission digest binding")
admission_sha256 = admission_binding.group(4)

schema = "oexec.schedule-prediction/v1"
header = f"; SchedulePrediction {schema}"
prediction_headers = [
    line for line in lines if line.startswith("; SchedulePrediction ")
]
if prediction_headers != [header]:
    raise SystemExit(f"expected exactly one {header!r} marker")

summaries = [line for line in lines if line.startswith("schedule-prediction ")]
if len(summaries) != 1:
    raise SystemExit("expected exactly one schedule-prediction record")

fields = {}
for item in summaries[0].split()[1:]:
    if "=" not in item:
        raise SystemExit(f"malformed schedule-prediction field: {item!r}")
    key, value = item.split("=", 1)
    if key in fields:
        raise SystemExit(f"duplicate schedule-prediction field: {key}")
    fields[key] = value

required = {
    "schema",
    "status",
    "provenance",
    "model",
    "admission-sha256",
    "task-count",
    "predicted-width",
    "predicted-span",
    "span-unit",
}
if set(fields) != required:
    raise SystemExit(
        "schedule-prediction fields differ from the v1 contract: "
        f"missing={sorted(required - set(fields))}, "
        f"unexpected={sorted(set(fields) - required)}"
    )
if fields["schema"] != schema:
    raise SystemExit(f"unsupported schedule-prediction schema: {fields['schema']}")
if fields["status"] != "admitted-static":
    raise SystemExit(f"unexpected prediction status: {fields['status']}")
if fields["provenance"] != "evidence-bound-admission":
    raise SystemExit(f"unexpected prediction provenance: {fields['provenance']}")
if fields["model"] != "unit-cost-shim-hosted-tasks":
    raise SystemExit(f"unexpected prediction model: {fields['model']}")
if fields["span-unit"] != "hosted-task-layers":
    raise SystemExit(f"unexpected prediction span unit: {fields['span-unit']}")
if re.fullmatch(r"[0-9a-f]{64}", fields["admission-sha256"]) is None:
    raise SystemExit("prediction admission digest is not lowercase SHA-256")
if fields["admission-sha256"] != admission_sha256:
    raise SystemExit("prediction digest does not match the enclosing admission")

numeric = {}
for key in ("task-count", "predicted-width", "predicted-span"):
    if re.fullmatch(r"[1-9][0-9]*", fields[key]) is None:
        raise SystemExit(f"{key} must be a positive integer")
    numeric[key] = int(fields[key])

layer_pattern = re.compile(
    r"schedule-prediction-layer index=([1-9][0-9]*) operations=\[(P[0-9]+(?:,P[0-9]+)*)\]"
)
layers = []
seen_operations = set()
for line in lines:
    if not line.startswith("schedule-prediction-layer "):
        continue
    match = layer_pattern.fullmatch(line)
    if match is None:
        raise SystemExit(f"malformed schedule-prediction layer: {line!r}")
    index = int(match.group(1))
    if index != len(layers) + 1:
        raise SystemExit("schedule-prediction layer indices are not contiguous")
    operations = match.group(2).split(",")
    duplicate = seen_operations.intersection(operations)
    if duplicate:
        raise SystemExit(f"hosted operation appears in multiple layers: {sorted(duplicate)}")
    seen_operations.update(operations)
    layers.append(operations)

if len(layers) != numeric["predicted-span"]:
    raise SystemExit("predicted span does not match the emitted layer count")
if sum(map(len, layers)) != numeric["task-count"]:
    raise SystemExit("predicted task count does not match the emitted layers")
if max(map(len, layers)) != numeric["predicted-width"]:
    raise SystemExit("predicted width does not match the emitted layers")

print(
    "\t".join(
        [
            fields["schema"],
            fields["provenance"],
            fields["model"],
            fields["admission-sha256"],
            fields["task-count"],
            fields["predicted-width"],
            fields["predicted-span"],
            fields["span-unit"],
        ]
    )
)
PY
    then
        printf 'shape=%s schedule_prediction=invalid\n' "$shape" >&2
        return 1
    fi

    IFS=$'\t' read -r \
        prediction_schema \
        prediction_provenance \
        prediction_model \
        prediction_admission_sha256 \
        predicted_hosted_tasks \
        predicted_width \
        predicted_span \
        predicted_span_unit <"$fields"
}

run_once() {
    local shape=$1 executor=$2 program=$3 output_file=$4 semantic_file=$5 output elapsed
    if ! output=$(
        "$o_bin" --executor "$executor" --workers "$workers" --json "$program" "$backends_dir"
    ); then
        printf 'shape=%s executor=%s status=failed\n' "$shape" "$executor" >&2
        return 1
    fi
    printf '%s\n' "$output" >"$output_file"
    elapsed=$(python3 - "$output_file" "$semantic_file" <<'PY'
import json
from pathlib import Path
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
if payload.get("ok") is not True:
    raise SystemExit(f"O returned a non-success payload: {payload!r}")
elapsed = payload.get("elapsed_ms")
if type(elapsed) is not int or elapsed < 0:
    raise SystemExit(f"elapsed_ms is not a non-negative integer: {elapsed!r}")
if not isinstance(payload.get("type"), str):
    raise SystemExit(f"O JSON output has no string type: {payload.get('type')!r}")
if "value" not in payload:
    raise SystemExit("O JSON output omitted value")
semantic_output = {
    "ok": payload["ok"],
    "type": payload["type"],
    "value": payload["value"],
}
Path(sys.argv[2]).write_text(
    json.dumps(semantic_output, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
print(elapsed)
PY
    )
    printf '%s\n' "$elapsed"
}

run_pair() {
    local shape=$1 phase=$2 ordinal=$3 program=$4 shape_dir=$5 expected_semantic=$6
    local first second serial_ms graph_ms serial_semantic graph_semantic
    local serial_output graph_output

    if [[ $((ordinal % 2)) -eq 1 ]]; then
        first=serial
        second=graph
    else
        first=graph
        second=serial
    fi
    printf 'shape=%s %s=%s order=%s,%s\n' "$shape" "$phase" "$ordinal" "$first" "$second" >&2

    serial_output=$shape_dir/$phase-$ordinal-serial-output.json
    graph_output=$shape_dir/$phase-$ordinal-graph-output.json
    serial_semantic=$shape_dir/$phase-$ordinal-serial-semantic.json
    graph_semantic=$shape_dir/$phase-$ordinal-graph-semantic.json

    if [[ "$first" == serial ]]; then
        serial_ms=$(run_once "$shape" serial "$program" "$serial_output" "$serial_semantic")
        graph_ms=$(run_once "$shape" graph "$program" "$graph_output" "$graph_semantic")
    else
        graph_ms=$(run_once "$shape" graph "$program" "$graph_output" "$graph_semantic")
        serial_ms=$(run_once "$shape" serial "$program" "$serial_output" "$serial_semantic")
    fi

    if ! cmp -s "$serial_semantic" "$graph_semantic"; then
        printf 'shape=%s semantic_equivalence=false\n' "$shape" >&2
        diff -u "$serial_semantic" "$graph_semantic" >&2 || true
        return 1
    fi

    if ! cmp -s "$expected_semantic" "$serial_semantic"; then
        printf 'shape=%s expected_output_match=false\n' "$shape" >&2
        diff -u "$expected_semantic" "$serial_semantic" >&2 || true
        return 1
    fi

    if [[ "$phase" == sample ]]; then
        printf '%s\n' "$serial_ms" >>"$shape_dir/serial-ms.txt"
        printf '%s\n' "$graph_ms" >>"$shape_dir/graph-ms.txt"
    fi
    printf 'shape=%s pair_phase=%s pair_ordinal=%s order=%s,%s serial_elapsed_ms=%s graph_elapsed_ms=%s semantic_equivalence=true expected_output_match=true\n' \
        "$shape" "$phase" "$ordinal" "$first" "$second" "$serial_ms" "$graph_ms" >&2
}

canonicalize_expected_output() {
    python3 - "$1" "$2" <<'PY'
import json
from pathlib import Path
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    expected = json.load(handle)
if set(expected) != {"ok", "type", "value"}:
    raise SystemExit(
        "expected output must contain exactly ok, type, and value: "
        f"{sorted(expected)!r}"
    )
if expected["ok"] is not True:
    raise SystemExit("expected output must describe a successful result")
if not isinstance(expected["type"], str):
    raise SystemExit("expected output type must be a string")
Path(sys.argv[2]).write_text(
    json.dumps(expected, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY
}

print_metrics() {
    python3 - "$1" "$2" <<'PY'
import statistics
import sys


def load(path):
    with open(path, encoding="utf-8") as handle:
        return [int(line) for line in handle if line.strip()]


serial = load(sys.argv[1])
graph = load(sys.argv[2])
serial_median = statistics.median(serial)
graph_median = statistics.median(graph)

print(
    f"serial_elapsed_ms median={serial_median:g} min={min(serial)} max={max(serial)}"
)
print(f"graph_elapsed_ms median={graph_median:g} min={min(graph)} max={max(graph)}")
if graph_median == 0:
    print("median_speedup_serial_over_graph=undefined")
else:
    print(f"median_speedup_serial_over_graph={serial_median / graph_median:.6f}")
PY
}

logical_cpus=unknown
if command -v sysctl >/dev/null 2>&1; then
    logical_cpus=$(sysctl -n hw.logicalcpu 2>/dev/null || true)
fi
if [[ -z "$logical_cpus" || "$logical_cpus" == unknown ]]; then
    logical_cpus=$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf unknown)
fi

cpu_model=unknown
if command -v sysctl >/dev/null 2>&1; then
    cpu_model=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)
fi
if [[ -z "$cpu_model" || "$cpu_model" == unknown ]] && [[ -r /proc/cpuinfo ]]; then
    cpu_model=$(awk -F: '/model name/ {sub(/^[[:space:]]+/, "", $2); print $2; exit}' /proc/cpuinfo)
fi
[[ -n "$cpu_model" ]] || cpu_model=unknown

memory_bytes=unknown
if command -v sysctl >/dev/null 2>&1; then
    memory_bytes=$(sysctl -n hw.memsize 2>/dev/null || true)
fi
if [[ -z "$memory_bytes" || "$memory_bytes" == unknown ]] && [[ -r /proc/meminfo ]]; then
    memory_bytes=$(awk '/MemTotal:/ {print $2 * 1024; exit}' /proc/meminfo)
fi
[[ -n "$memory_bytes" ]] || memory_bytes=unknown

sha256_file() {
    python3 - "$1" <<'PY'
import hashlib
from pathlib import Path
import sys

path = Path(sys.argv[1])
digest = hashlib.sha256()
try:
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
except OSError as exc:
    raise SystemExit(f"cannot hash executable {path}: {exc}")
print(digest.hexdigest())
PY
}

if ! binary_sha256=$(sha256_file "$o_bin"); then
    printf 'failed to hash O binary: %s\n' "$o_bin" >&2
    exit 1
fi
if ! olangc_binary_sha256=$(sha256_file "$olangc_bin"); then
    printf 'failed to hash olangc binary: %s\n' "$olangc_bin" >&2
    exit 1
fi
if [[ ! "$binary_sha256" =~ ^[0-9a-f]{64}$ ]]; then
    printf 'O binary hash is not lowercase SHA-256: %s\n' "$binary_sha256" >&2
    exit 1
fi
if [[ ! "$olangc_binary_sha256" =~ ^[0-9a-f]{64}$ ]]; then
    printf 'olangc binary hash is not lowercase SHA-256: %s\n' "$olangc_binary_sha256" >&2
    exit 1
fi

git_commit=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || printf unknown)
git_tree_state=unknown
if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    if [[ -n $(git -C "$ROOT" status --porcelain --untracked-files=normal) ]]; then
        git_tree_state=dirty
    else
        git_tree_state=clean
    fi
fi

printf 'benchmark=hgraph-hosted-ephemeral-autonomous-batch\n'
printf 'timestamp_utc=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
printf 'os=%s\n' "$(uname -srm)"
printf 'cpu_model=%s\n' "$cpu_model"
printf 'logical_cpus=%s\n' "$logical_cpus"
printf 'memory_bytes=%s\n' "$memory_bytes"
printf 'git_commit=%s\n' "$git_commit"
printf 'git_tree_state=%s\n' "$git_tree_state"
printf 'o_binary=%s\n' "$o_bin"
printf 'o_binary_sha256=%s\n' "$binary_sha256"
printf 'olangc_binary=%s\n' "$olangc_bin"
printf 'olangc_binary_sha256=%s\n' "$olangc_binary_sha256"
printf 'backends_dir=%s\n' "$backends_dir"
printf 'warmups=%s\n' "$warmups"
printf 'repetitions=%s\n' "$repetitions"
printf 'sleep_seconds=%s\n' "$sleep_seconds"
printf 'worker_tasks=%s\n' "$workers"
printf 'selected_shape=%s\n' "$selected_shape"
printf 'missing_runtime_policy=%s\n' "$missing_runtime_policy"
printf 'runtime_python3=%s\n' "${python_path:-unavailable}"
printf 'runtime_python3_version=%s\n' "$python_version"
printf 'runtime_bash=%s\n' "${bash_path:-unavailable}"
printf 'runtime_bash_version=%s\n' "$bash_version"
printf 'runtime_node=%s\n' "${node_path:-unavailable}"
printf 'runtime_node_version=%s\n' "$node_version"

shapes=(heterogeneous chained mixed_width realistic)
overall_status=0
for shape in "${shapes[@]}"; do
    if [[ "$selected_shape" != all && "$selected_shape" != "$shape" ]]; then
        continue
    fi

    fixture=$fixture_dir/$shape.O
    expected_output=$fixture_dir/$shape.expected.json
    if [[ ! -f "$fixture" ]]; then
        printf 'benchmark fixture is missing: %s\n' "$fixture" >&2
        exit 1
    fi
    if [[ ! -f "$expected_output" ]]; then
        printf 'benchmark expected output is missing: %s\n' "$expected_output" >&2
        exit 1
    fi
    required=$(shape_runtimes "$shape")
    missing=$(missing_runtimes_for "$required")
    shape_dir=$work_dir/$shape
    mkdir -p "$shape_dir"
    program=$shape_dir/$shape.O
    expected_semantic=$shape_dir/expected-semantic.json
    render_fixture "$fixture" "$program"
    if ! analyze_fixture "$shape" "$program" "$shape_dir"; then
        exit 1
    fi

    printf '\nshape=%s.O\n' "$shape"
    printf 'fixture=%s\n' "$fixture"
    printf 'required_runtimes=%s\n' "$required"
    printf 'missing_runtimes=%s\n' "$missing"
    printf 'prediction_source=olangc--explain-schedule\n'
    printf 'prediction_schema=%s\n' "$prediction_schema"
    printf 'prediction_provenance=%s\n' "$prediction_provenance"
    printf 'prediction_model=%s\n' "$prediction_model"
    printf 'prediction_admission_sha256=%s\n' "$prediction_admission_sha256"
    printf 'predicted_hosted_tasks=%s\n' "$predicted_hosted_tasks"
    printf 'predicted_width=%s\n' "$predicted_width"
    printf 'predicted_span=%s\n' "$predicted_span"
    printf 'predicted_span_unit=%s\n' "$predicted_span_unit"

    if [[ "$missing" != none ]]; then
        printf 'status=skipped\n'
        printf 'semantic_equivalence=not-measured\n'
        printf 'expected_output_match=not-measured\n'
        printf 'serial_elapsed_ms=not-measured\n'
        printf 'graph_elapsed_ms=not-measured\n'
        printf 'median_speedup_serial_over_graph=not-measured\n'
        if [[ "$missing_runtime_policy" == fail ]]; then
            overall_status=1
        fi
        continue
    fi

    canonicalize_expected_output "$expected_output" "$expected_semantic"
    : >"$shape_dir/serial-ms.txt"
    : >"$shape_dir/graph-ms.txt"

    index=1
    shape_failed=0
    while [[ "$index" -le "$warmups" ]]; do
        if ! run_pair "$shape" warmup "$index" "$program" "$shape_dir" "$expected_semantic"; then
            shape_failed=1
            break
        fi
        index=$((index + 1))
    done

    index=1
    while [[ "$shape_failed" -eq 0 && "$index" -le "$repetitions" ]]; do
        if ! run_pair "$shape" sample "$index" "$program" "$shape_dir" "$expected_semantic"; then
            shape_failed=1
            break
        fi
        index=$((index + 1))
    done

    if [[ "$shape_failed" -ne 0 ]]; then
        printf 'status=failed\n'
        printf 'semantic_equivalence=false-or-unverified\n'
        overall_status=1
        continue
    fi

    printf 'status=measured\n'
    printf 'semantic_equivalence=true\n'
    printf 'semantic_equivalence_basis=ok+type+canonical-o-value-json\n'
    printf 'expected_output_match=true\n'
    print_metrics "$shape_dir/serial-ms.txt" "$shape_dir/graph-ms.txt"
done

exit "$overall_status"
