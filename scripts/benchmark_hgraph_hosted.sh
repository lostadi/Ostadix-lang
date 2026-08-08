#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

warmups=${HGRAPH_BENCH_WARMUPS:-1}
repetitions=${HGRAPH_BENCH_REPETITIONS:-5}
sleep_seconds=${HGRAPH_BENCH_SLEEP_SECONDS:-0.25}
workers=${HGRAPH_BENCH_WORKERS:-4}
o_bin=${O_RELEASE_BIN:-$ROOT/target/release/O}
backends_dir=${O_BACKENDS_DIR:-$ROOT/backends}

usage() {
    cat <<'EOF'
Usage: scripts/benchmark_hgraph_hosted.sh [OPTIONS]

Benchmark explicit autonomous(batch(...)) ephemeral Python concurrency through
the release O CLI. Results are descriptive; this script applies no performance
threshold.

Options:
  --warmups N       Warmup pairs before measurement (default: 1)
  --repetitions N   Measured serial/graph pairs (default: 5)
  --sleep SECONDS   Sleep in each ephemeral Python task (default: 0.25)
  --workers N       Independent ephemeral Python tasks (default: 4)
  --help             Show this help

Environment overrides:
  HGRAPH_BENCH_WARMUPS
  HGRAPH_BENCH_REPETITIONS
  HGRAPH_BENCH_SLEEP_SECONDS
  HGRAPH_BENCH_WORKERS
  O_RELEASE_BIN
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

if ! command -v python3 >/dev/null 2>&1; then
    printf 'python3 is required to validate options and parse benchmark JSON\n' >&2
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
if [[ ! -d "$backends_dir" ]]; then
    printf 'backend directory does not exist: %s\n' "$backends_dir" >&2
    exit 1
fi

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ostadix-hgraph-hosted-bench.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT
program="$work_dir/hosted-batch.O"
serial_samples="$work_dir/serial-ms.txt"
graph_samples="$work_dir/graph-ms.txt"
: >"$serial_samples"
: >"$graph_samples"

{
    printf 'autonomous(batch(\n'
    index=1
    while [[ "$index" -le "$workers" ]]; do
        printf 'python^(\n'
        printf 'import time\n'
        printf 'time.sleep(%s)\n' "$sleep_seconds"
        printf '__oval_result__ = %s\n' "$index"
        if [[ "$index" -lt "$workers" ]]; then
            printf ')_python,\n'
        else
            printf ')_python\n'
        fi
        index=$((index + 1))
    done
    printf '))\n'
} >"$program"

run_once() {
    local executor=$1 output elapsed output_file
    if ! output=$("$o_bin" --executor "$executor" --workers "$workers" --json "$program" "$backends_dir"); then
        printf '%s executor failed\n' "$executor" >&2
        return 1
    fi
    output_file="$work_dir/${executor}-last-output.json"
    printf '%s\n' "$output" >"$output_file"
    elapsed=$(python3 - "$workers" "$output_file" <<'PY'
import json
import sys

expected_members = int(sys.argv[1])
with open(sys.argv[2], encoding="utf-8") as handle:
    payload = json.load(handle)
if payload.get("ok") is not True:
    raise SystemExit(f"O returned a non-success payload: {payload!r}")
elapsed = payload.get("elapsed_ms")
if type(elapsed) is not int or elapsed < 0:
    raise SystemExit(f"elapsed_ms is not a non-negative integer: {elapsed!r}")
value = payload.get("value")
if not isinstance(value, dict) or value.get("t") != "list":
    raise SystemExit(f"benchmark result is not an O list: {value!r}")
members = value.get("v")
if not isinstance(members, list) or len(members) != expected_members:
    raise SystemExit(
        f"benchmark result has {len(members) if isinstance(members, list) else 'invalid'} "
        f"members; expected {expected_members}"
    )
print(elapsed)
PY
    )
    printf '%s\n' "$elapsed"
}

run_pair() {
    local phase=$1 ordinal=$2 first second serial_ms graph_ms
    if [[ $((ordinal % 2)) -eq 1 ]]; then
        first=serial
        second=graph
    else
        first=graph
        second=serial
    fi
    printf '%s %s order=%s,%s\n' "$phase" "$ordinal" "$first" "$second" >&2

    if [[ "$first" == serial ]]; then
        serial_ms=$(run_once serial)
        graph_ms=$(run_once graph)
    else
        graph_ms=$(run_once graph)
        serial_ms=$(run_once serial)
    fi

    if [[ "$phase" == sample ]]; then
        printf '%s\n' "$serial_ms" >>"$serial_samples"
        printf '%s\n' "$graph_ms" >>"$graph_samples"
    fi
}

index=1
while [[ "$index" -le "$warmups" ]]; do
    run_pair warmup "$index"
    index=$((index + 1))
done

index=1
while [[ "$index" -le "$repetitions" ]]; do
    run_pair sample "$index"
    index=$((index + 1))
done

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

binary_sha256=unknown
if command -v shasum >/dev/null 2>&1; then
    binary_sha256=$(shasum -a 256 "$o_bin" | awk '{print $1}')
elif command -v sha256sum >/dev/null 2>&1; then
    binary_sha256=$(sha256sum "$o_bin" | awk '{print $1}')
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
printf 'backends_dir=%s\n' "$backends_dir"
printf 'warmups=%s\n' "$warmups"
printf 'repetitions=%s\n' "$repetitions"
printf 'sleep_seconds=%s\n' "$sleep_seconds"
printf 'worker_tasks=%s\n' "$workers"

python3 - "$serial_samples" "$graph_samples" <<'PY'
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
