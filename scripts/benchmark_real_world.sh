#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

warmups=${REAL_WORLD_BENCH_WARMUPS:-1}
repetitions=${REAL_WORLD_BENCH_REPETITIONS:-4}
workers=${REAL_WORLD_BENCH_WORKERS:-4}
selected_workload=${REAL_WORLD_BENCH_WORKLOAD:-all}
missing_tool_policy=${REAL_WORLD_BENCH_MISSING_TOOL:-fail}
evidence_dir=${REAL_WORLD_BENCH_EVIDENCE_DIR:-}
o_bin=${O_RELEASE_BIN:-$ROOT/target/release/O}
olangc_bin=${OLANGC_RELEASE_BIN:-$(dirname -- "$o_bin")/olangc}
backends_dir=${O_BACKENDS_DIR:-$ROOT/backends}
fixture_dir=$ROOT/benchmarks/real_world
timed_exec=$fixture_dir/timed_exec.py
preview_seconds=${OSTADIX_PREVIEW_SECONDS:-3}

usage() {
    cat <<'EOF'
Usage: scripts/benchmark_real_world.sh [OPTIONS]

Compare serial and autonomous graph execution on real repository workflows.
The benchmark is descriptive and never enforces a speedup threshold.

Workloads:
  asset_pipeline  Generate 18 AVIF/WebP derivatives from real project images.
  ci_shards       Run three independent repository unit-test shards (78 tests today).
  video_previews  Transcode nine real GIF animations into HD VP9 WebM previews.

Options:
  --workload NAME       all, asset_pipeline, ci_shards, or video_previews (default: all)
  --warmups N           Paired warmups before measurement (default: 1)
  --repetitions N       Measured alternating-order pairs (default: 4)
  --workers N           Graph local-worker limit (default: 4)
  --missing-tool MODE   fail or skip (default: fail)
  --evidence-dir PATH   Retain plans, outputs, logs, and manifests in an empty path
  -h, --help            Show this help

Environment overrides:
  REAL_WORLD_BENCH_WARMUPS
  REAL_WORLD_BENCH_REPETITIONS
  REAL_WORLD_BENCH_WORKERS
  REAL_WORLD_BENCH_WORKLOAD
  REAL_WORLD_BENCH_MISSING_TOOL
  REAL_WORLD_BENCH_EVIDENCE_DIR
  OSTADIX_PREVIEW_SECONDS
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
        --workload)
            need_value "$@"
            selected_workload=$2
            shift 2
            ;;
        --workload=*) selected_workload=${1#*=}; shift ;;
        --warmups)
            need_value "$@"
            warmups=$2
            shift 2
            ;;
        --warmups=*) warmups=${1#*=}; shift ;;
        --repetitions)
            need_value "$@"
            repetitions=$2
            shift 2
            ;;
        --repetitions=*) repetitions=${1#*=}; shift ;;
        --workers)
            need_value "$@"
            workers=$2
            shift 2
            ;;
        --workers=*) workers=${1#*=}; shift ;;
        --missing-tool)
            need_value "$@"
            missing_tool_policy=$2
            shift 2
            ;;
        --missing-tool=*) missing_tool_policy=${1#*=}; shift ;;
        --evidence-dir)
            need_value "$@"
            evidence_dir=$2
            shift 2
            ;;
        --evidence-dir=*) evidence_dir=${1#*=}; shift ;;
        -h|--help) usage; exit 0 ;;
        *)
            printf 'unknown option: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

for numeric_name in warmups repetitions workers; do
    numeric_value=${!numeric_name}
    case "$numeric_value" in
        ''|*[!0-9]*)
            printf '%s must be a non-negative integer, got %s\n' \
                "$numeric_name" "$numeric_value" >&2
            exit 2
            ;;
    esac
done
if [[ "$repetitions" -lt 1 || "$workers" -lt 1 ]]; then
    printf 'repetitions and workers must both be at least one\n' >&2
    exit 2
fi
case "$preview_seconds" in
    ''|*[!0-9]*)
        printf 'OSTADIX_PREVIEW_SECONDS must be a positive integer, got %s\n' \
            "$preview_seconds" >&2
        exit 2
        ;;
esac
if [[ "$preview_seconds" -lt 1 || "$preview_seconds" -gt 30 ]]; then
    printf 'OSTADIX_PREVIEW_SECONDS must be between 1 and 30\n' >&2
    exit 2
fi
case "$selected_workload" in
    all|asset_pipeline|ci_shards|video_previews) ;;
    *) printf 'unknown workload: %s\n' "$selected_workload" >&2; exit 2 ;;
esac
case "$missing_tool_policy" in
    fail|skip) ;;
    *) printf 'missing-tool mode must be fail or skip\n' >&2; exit 2 ;;
esac

if ! command -v python3 >/dev/null 2>&1; then
    printf 'python3 is required by the benchmark harness\n' >&2
    exit 1
fi
for required_path in "$o_bin" "$olangc_bin" "$timed_exec"; do
    if [[ ! -x "$required_path" ]]; then
        printf 'required executable is missing: %s\n' "$required_path" >&2
        exit 1
    fi
done
if [[ ! -d "$backends_dir" ]]; then
    printf 'backend directory is missing: %s\n' "$backends_dir" >&2
    exit 1
fi

retained_evidence=false
if [[ -n "$evidence_dir" ]]; then
    case "$evidence_dir" in
        /|"$ROOT"|"$HOME")
            printf 'refusing unsafe evidence directory: %s\n' "$evidence_dir" >&2
            exit 2
            ;;
    esac
    mkdir -p -- "$evidence_dir"
    if [[ -n $(find "$evidence_dir" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
        printf 'evidence directory must be empty: %s\n' "$evidence_dir" >&2
        exit 2
    fi
    work_dir=$(CDPATH= cd -- "$evidence_dir" && pwd)
    retained_evidence=true
else
    work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ostadix-real-world-bench.XXXXXX")
fi

cleanup() {
    if [[ "$retained_evidence" == false && -d "$work_dir" ]]; then
        case "$(basename -- "$work_dir")" in
            ostadix-real-world-bench.*)
                chmod -R u+w "$work_dir" 2>/dev/null || true
                find "$work_dir" -depth -delete 2>/dev/null || true
                ;;
        esac
    fi
}
trap cleanup EXIT HUP INT TERM

cd "$ROOT"

sha256_file() {
    python3 - "$1" <<'PY'
import hashlib
from pathlib import Path
import sys

digest = hashlib.sha256()
with Path(sys.argv[1]).open("rb") as handle:
    for chunk in iter(lambda: handle.read(1024 * 1024), b""):
        digest.update(chunk)
print(digest.hexdigest())
PY
}

program_for() {
    case "$1" in
        asset_pipeline) printf '%s/asset_pipeline.O\n' "$fixture_dir" ;;
        ci_shards) printf '%s/ci_shards.O\n' "$fixture_dir" ;;
        video_previews) printf '%s/video_previews.O\n' "$fixture_dir" ;;
    esac
}

required_tools_for() {
    case "$1" in
        asset_pipeline) printf 'bash,magick,sha256sum,find,sort,xargs\n' ;;
        ci_shards) printf 'bash,python3,grep,sed,tail\n' ;;
        video_previews) printf 'bash,ffmpeg,ffprobe\n' ;;
    esac
}

missing_tools_for() {
    local required tool missing= old_ifs=$IFS
    required=$(required_tools_for "$1")
    IFS=,
    for tool in $required; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            [[ -z "$missing" ]] || missing=$missing,
            missing=$missing$tool
        fi
    done
    IFS=$old_ifs
    printf '%s\n' "${missing:-none}"
}

validate_fixture_inputs() {
    local workload=$1 path
    if [[ "$workload" == asset_pipeline ]]; then
        for path in \
            Olang_Mascot_little-o/little-o/references/reference-01.png \
            Olang_Mascot_little-o/little-o/references/canonical-base.png \
            assets/olang-logo.png
        do
            [[ -f "$path" ]] || {
                printf 'asset-pipeline input is missing: %s\n' "$path" >&2
                return 1
            }
        done
    elif [[ "$workload" == video_previews ]]; then
        if [[ ! -x benchmarks/real_world/transcode_preview.sh ]]; then
            printf 'video-preview wrapper is not executable: %s\n' \
                benchmarks/real_world/transcode_preview.sh >&2
            return 1
        fi
        for path in \
            failed.gif idle.gif jumping.gif review.gif running-left.gif \
            running-right.gif running.gif waiting.gif waving.gif
        do
            path=Olang_Mascot_little-o/little-o/qa/previews/$path
            [[ -f "$path" ]] || {
                printf 'video-preview input is missing: %s\n' "$path" >&2
                return 1
            }
        done
    fi
}

input_paths_for() {
    case "$1" in
        asset_pipeline)
            printf '%s\n' \
                Olang_Mascot_little-o/little-o/references/reference-01.png \
                Olang_Mascot_little-o/little-o/references/canonical-base.png \
                assets/olang-logo.png
            ;;
        ci_shards)
            printf '%s\n' \
                tests/test_o_cli_dispatch.py \
                tests/test_setup.py \
                tests/test_ostadix_boot_iso.py
            ;;
        video_previews)
            printf '%s\n' \
                benchmarks/real_world/transcode_preview.sh \
                Olang_Mascot_little-o/little-o/qa/previews/failed.gif \
                Olang_Mascot_little-o/little-o/qa/previews/idle.gif \
                Olang_Mascot_little-o/little-o/qa/previews/jumping.gif \
                Olang_Mascot_little-o/little-o/qa/previews/review.gif \
                Olang_Mascot_little-o/little-o/qa/previews/running-left.gif \
                Olang_Mascot_little-o/little-o/qa/previews/running-right.gif \
                Olang_Mascot_little-o/little-o/qa/previews/running.gif \
                Olang_Mascot_little-o/little-o/qa/previews/waiting.gif \
                Olang_Mascot_little-o/little-o/qa/previews/waving.gif
            ;;
    esac
}

write_input_manifest() {
    local workload=$1 destination=$2
    local -a paths
    mapfile -t paths < <(input_paths_for "$workload")
    python3 - "$workload" "$destination" "${paths[@]}" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

workload, destination_arg, *path_args = sys.argv[1:]
records = []
for raw in path_args:
    path = Path(raw)
    payload = path.read_bytes()
    records.append(
        {
            "path": raw,
            "bytes": len(payload),
            "sha256": hashlib.sha256(payload).hexdigest(),
        }
    )
payload = {
    "schema": "ostadix.real-world-inputs/v1",
    "workload": workload,
    "files": records,
}
Path(destination_arg).write_text(
    json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY
}

analyze_workload() {
    local workload=$1 program=$2 workload_dir=$3 fields
    local expected_tasks expected_width expected_span
    fields=$workload_dir/prediction-fields.tsv
    "$olangc_bin" "$program" --target ir --explain-schedule --format json \
        --workers "$workers" --shim-dir "$backends_dir" \
        >"$workload_dir/schedule-explanation.json"
    python3 - "$workload_dir/schedule-explanation.json" "$fields" <<'PY'
import json
from pathlib import Path
import sys

source, destination = map(Path, sys.argv[1:])
root = json.loads(source.read_text(encoding="utf-8"))
if root.get("schema") != "oexec.schedule-explanation/v2":
    raise SystemExit(f"unexpected schedule schema: {root.get('schema')!r}")
admission = root.get("admission")
prediction = root.get("prediction")
if not isinstance(admission, dict) or not isinstance(prediction, dict):
    raise SystemExit("schedule omitted admission or prediction")
expected = {
    "schema": "oexec.schedule-prediction/v1",
    "status": "admitted-static",
    "provenance": "evidence-bound-admission",
    "model": "unit-cost-shim-hosted-tasks",
    "span_unit": "hosted-task-layers",
}
for key, value in expected.items():
    if prediction.get(key) != value:
        raise SystemExit(f"unexpected prediction {key}: {prediction.get(key)!r}")
digest = prediction.get("admission_sha256")
if digest != admission.get("bindings", {}).get("admission_sha256"):
    raise SystemExit("prediction is not bound to the enclosing admission")
task_count = prediction.get("task_count")
width = prediction.get("predicted_width")
span = prediction.get("predicted_span")
layers = prediction.get("layers")
if any(type(value) is not int or value < 1 for value in (task_count, width, span)):
    raise SystemExit("prediction counts must be positive integers")
if not isinstance(layers, list) or len(layers) != span:
    raise SystemExit("prediction layer count does not match span")
layer_widths = [len(layer.get("operations", [])) for layer in layers]
if sum(layer_widths) != task_count or max(layer_widths) != width:
    raise SystemExit("prediction topology is internally inconsistent")
destination.write_text(
    "\t".join(map(str, (digest, task_count, width, span))) + "\n",
    encoding="utf-8",
)
PY
    IFS=$'\t' read -r prediction_digest predicted_tasks predicted_width predicted_span \
        <"$fields"
    case "$workload" in
        asset_pipeline|ci_shards)
            expected_tasks=3
            expected_width=3
            expected_span=1
            ;;
        video_previews)
            expected_tasks=9
            expected_width=9
            expected_span=1
            ;;
    esac
    if [[ "$predicted_tasks" -ne "$expected_tasks" || \
          "$predicted_width" -ne "$expected_width" || \
          "$predicted_span" -ne "$expected_span" ]]; then
        printf 'workload=%s unexpected topology tasks=%s width=%s span=%s\n' \
            "$workload" "$predicted_tasks" "$predicted_width" "$predicted_span" >&2
        return 1
    fi
}

validate_artifacts() {
    python3 - "$1" "$2" "$3" "$preview_seconds" <<'PY'
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

workload, root_arg, destination_arg, preview_seconds_arg = sys.argv[1:]
root = Path(root_arg)
destination = Path(destination_arg)

if workload == "asset_pipeline":
    stems = {"reference": "reference", "canonical": "canonical", "logo": "logo"}
    sizes = (1536, 1024, 640)
    suffixes = ("avif", "webp")
    expected = {
        Path(lane) / f"{stem}-{size}.{suffix}"
        for lane, stem in stems.items()
        for size in sizes
        for suffix in suffixes
    }
    observed = {path.relative_to(root) for path in root.rglob("*") if path.is_file()}
    if observed != expected:
        raise SystemExit(
            f"asset inventory mismatch: missing={sorted(map(str, expected-observed))} "
            f"extra={sorted(map(str, observed-expected))}"
        )
    records = []
    for relative in sorted(expected, key=str):
        path = root / relative
        format_name, width, height = subprocess.check_output(
            ["magick", "identify", "-format", "%m\t%w\t%h", str(path)],
            text=True,
        ).split("\t")
        expected_format = relative.suffix[1:].upper()
        if format_name.upper() != expected_format:
            raise SystemExit(f"unexpected format for {relative}: {format_name}")
        requested_size = int(relative.stem.rsplit("-", 1)[1])
        if not (0 < int(width) <= requested_size and 0 < int(height) <= requested_size):
            raise SystemExit(f"unexpected dimensions for {relative}: {width}x{height}")
        payload = path.read_bytes()
        if not payload:
            raise SystemExit(f"empty generated asset: {relative}")
        records.append(
            {
                "path": str(relative),
                "format": format_name.upper(),
                "width": int(width),
                "height": int(height),
                "bytes": len(payload),
                "sha256": hashlib.sha256(payload).hexdigest(),
            }
        )
    manifest = {"schema": "ostadix.real-world-assets/v1", "files": records}
elif workload == "ci_shards":
    suites = {
        "o-cli.log": "o-cli",
        "setup.log": "setup",
        "boot-iso.log": "boot-iso",
    }
    observed = {path.name for path in root.iterdir() if path.is_file()}
    if observed != set(suites):
        raise SystemExit(f"CI log inventory mismatch: {sorted(observed)}")
    records = []
    for filename, suite in suites.items():
        text = (root / filename).read_text(encoding="utf-8")
        match = re.search(r"^Ran ([0-9]+) tests? in ", text, re.MULTILINE)
        if match is None or re.search(r"^OK$", text, re.MULTILINE) is None:
            raise SystemExit(f"CI shard did not pass: {filename}")
        records.append({"suite": suite, "tests": int(match.group(1)), "status": "pass"})
    manifest = {
        "schema": "ostadix.real-world-ci/v1",
        "total_tests": sum(record["tests"] for record in records),
        "suites": sorted(records, key=lambda record: record["suite"]),
    }
elif workload == "video_previews":
    names = {
        "failed", "idle", "jumping", "review", "running-left",
        "running-right", "running", "waiting", "waving",
    }
    expected = {f"{name}.webm" for name in names}
    observed = {path.name for path in root.iterdir() if path.is_file()}
    if observed != expected:
        raise SystemExit(
            f"video inventory mismatch: missing={sorted(expected-observed)} "
            f"extra={sorted(observed-expected)}"
        )
    expected_seconds = int(preview_seconds_arg)
    records = []
    for filename in sorted(expected):
        path = root / filename
        probe = json.loads(
            subprocess.check_output(
                [
                    "ffprobe", "-v", "error", "-select_streams", "v:0",
                    "-show_entries", "stream=codec_name,width,height,pix_fmt",
                    "-show_entries", "format=duration", "-of", "json", str(path),
                ],
                text=True,
            )
        )
        streams = probe.get("streams", [])
        if len(streams) != 1:
            raise SystemExit(f"expected exactly one video stream in {filename}")
        stream = streams[0]
        actual = (
            stream.get("codec_name"), stream.get("width"),
            stream.get("height"), stream.get("pix_fmt"),
        )
        if actual != ("vp9", 768, 832, "yuv420p"):
            raise SystemExit(f"unexpected video properties for {filename}: {actual!r}")
        duration = float(probe.get("format", {}).get("duration", 0))
        if abs(duration - expected_seconds) > 0.10:
            raise SystemExit(
                f"unexpected duration for {filename}: {duration} "
                f"(expected {expected_seconds})"
            )
        framemd5 = subprocess.check_output(
            [
                "ffmpeg", "-nostdin", "-v", "error", "-i", str(path),
                "-map", "0:v:0", "-f", "framemd5", "-",
            ]
        )
        frame_lines = [
            line for line in framemd5.splitlines()
            if line and not line.startswith(b"#")
        ]
        expected_frames = expected_seconds * 30
        if len(frame_lines) != expected_frames:
            raise SystemExit(
                f"unexpected decoded frame count for {filename}: "
                f"{len(frame_lines)} (expected {expected_frames})"
            )
        records.append(
            {
                "path": filename,
                "codec": "vp9",
                "width": 768,
                "height": 832,
                "pixel_format": "yuv420p",
                "duration_seconds": duration,
                "decoded_frames": len(frame_lines),
                "decoded_framemd5_sha256": hashlib.sha256(framemd5).hexdigest(),
            }
        )
    manifest = {"schema": "ostadix.real-world-video/v1", "files": records}
else:
    raise SystemExit(f"unknown workload: {workload}")

destination.write_text(
    json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY
}

parse_result() {
    python3 - "$1" "$2" <<'PY'
import json
from pathlib import Path
import sys

payload = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
if payload.get("ok") is not True:
    raise SystemExit(f"O execution failed: {payload!r}")
elapsed = payload.get("elapsed_ms")
if type(elapsed) is not int or elapsed < 0:
    raise SystemExit(f"invalid elapsed_ms: {elapsed!r}")
semantic = {key: payload[key] for key in ("ok", "type", "value")}
Path(sys.argv[2]).write_text(
    json.dumps(semantic, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
print(elapsed)
PY
}

parse_timing() {
    python3 - "$1" <<'PY'
import json
from pathlib import Path
import sys

payload = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
if payload.get("schema") != "ostadix.timed-exec/v1":
    raise SystemExit(f"unexpected timing schema: {payload.get('schema')!r}")
if payload.get("returncode") != 0 or payload.get("exit_code") != 0:
    raise SystemExit(f"timed child did not exit successfully: {payload!r}")
elapsed = payload.get("wall_time_ns")
if type(elapsed) is not int or elapsed <= 0:
    raise SystemExit(f"invalid wall_time_ns: {elapsed!r}")
print(elapsed)
PY
}

run_once() {
    local workload=$1 executor=$2 program=$3 artifact_dir=$4 output=$5 semantic=$6 manifest=$7 timing=$8
    local env_name elapsed wall_ns status stderr_output
    local -a timing_arguments
    mkdir -p -- "$artifact_dir"
    case "$workload" in
        asset_pipeline) env_name=O_ASSET_OUT ;;
        ci_shards) env_name=O_CI_OUT ;;
        video_previews) env_name=O_VIDEO_OUT ;;
    esac
    stderr_output=${output%.json}.stderr.log
    timing_arguments=(
        --stdout "$output"
        --stderr "$stderr_output"
        --env "$env_name=$artifact_dir"
        --unset-env O_EXECUTOR
        --unset-env O_GRAPH_WORKERS
    )
    if [[ "$workload" == video_previews ]]; then
        timing_arguments+=(--env "OSTADIX_PREVIEW_SECONDS=$preview_seconds")
    fi
    if "$timed_exec" "${timing_arguments[@]}" -- \
        "$o_bin" --executor "$executor" --workers "$workers" --json \
        "$program" "$backends_dir" >"$timing"
    then
        status=0
    else
        status=$?
    fi
    if [[ "$status" -ne 0 ]]; then
        printf 'workload=%s executor=%s status=failed exit_code=%s\n' \
            "$workload" "$executor" "$status" >&2
        if [[ -s "$stderr_output" ]]; then
            sed -n '1,80p' "$stderr_output" >&2
        fi
        return 1
    fi
    elapsed=$(parse_result "$output" "$semantic")
    wall_ns=$(parse_timing "$timing")
    validate_artifacts "$workload" "$artifact_dir" "$manifest"
    printf '%s\t%s\n' "$wall_ns" "$elapsed"
}

run_pair() {
    local workload=$1 phase=$2 ordinal=$3 program=$4 workload_dir=$5
    local first second prefix serial_measurement graph_measurement
    local serial_wall_ns graph_wall_ns serial_elapsed_ms graph_elapsed_ms
    local serial_semantic graph_semantic serial_manifest graph_manifest
    prefix=$workload_dir/$phase-$ordinal
    if [[ $((ordinal % 2)) -eq 1 ]]; then
        first=serial
        second=graph
    else
        first=graph
        second=serial
    fi
    printf 'workload=%s phase=%s pair=%s order=%s,%s\n' \
        "$workload" "$phase" "$ordinal" "$first" "$second" >&2

    serial_semantic=$prefix-serial-semantic.json
    graph_semantic=$prefix-graph-semantic.json
    serial_manifest=$prefix-serial-manifest.json
    graph_manifest=$prefix-graph-manifest.json

    if [[ "$first" == serial ]]; then
        serial_measurement=$(run_once "$workload" serial "$program" \
            "$prefix-serial-artifacts" "$prefix-serial-output.json" \
            "$serial_semantic" "$serial_manifest" "$prefix-serial-timing.json")
        graph_measurement=$(run_once "$workload" graph "$program" \
            "$prefix-graph-artifacts" "$prefix-graph-output.json" \
            "$graph_semantic" "$graph_manifest" "$prefix-graph-timing.json")
    else
        graph_measurement=$(run_once "$workload" graph "$program" \
            "$prefix-graph-artifacts" "$prefix-graph-output.json" \
            "$graph_semantic" "$graph_manifest" "$prefix-graph-timing.json")
        serial_measurement=$(run_once "$workload" serial "$program" \
            "$prefix-serial-artifacts" "$prefix-serial-output.json" \
            "$serial_semantic" "$serial_manifest" "$prefix-serial-timing.json")
    fi
    IFS=$'\t' read -r serial_wall_ns serial_elapsed_ms <<<"$serial_measurement"
    IFS=$'\t' read -r graph_wall_ns graph_elapsed_ms <<<"$graph_measurement"

    if ! cmp -s "$serial_semantic" "$graph_semantic"; then
        printf 'workload=%s semantic_equivalence=false\n' "$workload" >&2
        diff -u "$serial_semantic" "$graph_semantic" >&2 || true
        return 1
    fi
    if ! cmp -s "$serial_manifest" "$graph_manifest"; then
        printf 'workload=%s artifact_equivalence=false\n' "$workload" >&2
        diff -u "$serial_manifest" "$graph_manifest" >&2 || true
        return 1
    fi
    if [[ "$phase" == sample ]]; then
        printf '%s\t%s\t%s\t%s\n' \
            "$serial_wall_ns" "$graph_wall_ns" \
            "$serial_elapsed_ms" "$graph_elapsed_ms" \
            >>"$workload_dir/pairs.tsv"
    fi
    printf 'workload=%s pair_phase=%s pair_ordinal=%s order=%s,%s serial_wall_ns=%s graph_wall_ns=%s serial_internal_ms=%s graph_internal_ms=%s semantic_equivalence=true artifact_equivalence=true\n' \
        "$workload" "$phase" "$ordinal" "$first" "$second" \
        "$serial_wall_ns" "$graph_wall_ns" \
        "$serial_elapsed_ms" "$graph_elapsed_ms" >&2
}

print_metrics() {
    python3 - "$1" "$2" "$3" "$4" <<'PY'
import math
import random
import statistics
import sys

pairs = []
with open(sys.argv[1], encoding="utf-8") as handle:
    for line in handle:
        serial_ns, graph_ns, serial_internal_ms, graph_internal_ms = map(int, line.split())
        pairs.append((serial_ns, graph_ns, serial_internal_ms, graph_internal_ms))
serial = [pair[0] / 1_000_000 for pair in pairs]
graph = [pair[1] / 1_000_000 for pair in pairs]
serial_internal = [pair[2] for pair in pairs]
graph_internal = [pair[3] for pair in pairs]
ratios = [left / right for left, right in zip(serial, graph)]
serial_median = statistics.median(serial)
graph_median = statistics.median(graph)
geometric_speedup = math.exp(statistics.fmean(math.log(value) for value in ratios))
paired_median_speedup = statistics.median(ratios)
serial_mad = statistics.median(abs(value - serial_median) for value in serial)
graph_mad = statistics.median(abs(value - graph_median) for value in graph)
saved = [left - right for left, right in zip(serial, graph)]
rng = random.Random(0x05AD1A)
bootstrap = sorted(
    statistics.median(rng.choices(ratios, k=len(ratios)))
    for _ in range(20_000)
)
ci_low = bootstrap[int(0.025 * (len(bootstrap) - 1))]
ci_high = bootstrap[int(0.975 * (len(bootstrap) - 1))]
topology_reference = int(sys.argv[2]) / int(sys.argv[3])
worker_reference = int(sys.argv[4])
effective_reference = min(topology_reference, worker_reference)
print("timing_boundary=complete_O_child_process_perf_counter_ns")
print(f"sample_pairs={len(pairs)}")
print(f"serial_wall_ms median={serial_median:.3f} mad={serial_mad:.3f} min={min(serial):.3f} max={max(serial):.3f}")
print(f"graph_wall_ms median={graph_median:.3f} mad={graph_mad:.3f} min={min(graph):.3f} max={max(graph):.3f}")
print("paired_wall_ms=" + ",".join(f"{left:.3f}/{right:.3f}" for left, right in zip(serial, graph)))
print(f"ratio_of_median_wall_times={serial_median / graph_median:.6f}")
print(f"paired_median_speedup={paired_median_speedup:.6f}")
print(f"paired_median_speedup_bootstrap_95pct_ci={ci_low:.6f},{ci_high:.6f}")
print("bootstrap_method=paired_resampling_fixed_seed iterations=20000")
print(f"paired_geometric_mean_speedup={geometric_speedup:.6f}")
print(f"median_wall_time_saved_ms={statistics.median(saved):.3f}")
print(f"median_latency_reduction_percent={(1 - graph_median / serial_median) * 100:.3f}")
print(f"paired_throughput_increase_percent={(geometric_speedup - 1) * 100:.3f}")
print(f"serial_internal_elapsed_ms median={statistics.median(serial_internal):g} min={min(serial_internal)} max={max(serial_internal)}")
print(f"graph_internal_elapsed_ms median={statistics.median(graph_internal):g} min={min(graph_internal)} max={max(graph_internal)}")
print(f"unit_cost_work_span_reference={topology_reference:.6f}")
print(f"worker_count_reference={worker_reference:.6f}")
print(f"effective_unit_cost_reference={effective_reference:.6f}")
PY
}

logical_cpus=$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)
if [[ -z "$logical_cpus" ]] && command -v nproc >/dev/null 2>&1; then
    logical_cpus=$(nproc 2>/dev/null || true)
fi
[[ -n "$logical_cpus" ]] || logical_cpus=unknown
cpu_model=$(getprop ro.soc.model 2>/dev/null || true)
[[ -n "$cpu_model" ]] || cpu_model=$(awk -F: '/model name/ {sub(/^[[:space:]]+/, "", $2); print $2; exit}' /proc/cpuinfo 2>/dev/null || true)
[[ -n "$cpu_model" ]] || cpu_model=unknown

printf 'benchmark=ostadix-real-world/v1\n'
printf 'timestamp_utc=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
printf 'os=%s\n' "$(uname -srm)"
printf 'cpu_model=%s\n' "$cpu_model"
printf 'logical_cpus=%s\n' "$logical_cpus"
python3 <<'PY'
import json
import os
from pathlib import Path
import shutil
import subprocess

try:
    affinity = ",".join(map(str, sorted(os.sched_getaffinity(0))))
except (AttributeError, OSError):
    affinity = "unknown"
try:
    load_average = ",".join(f"{value:.3f}" for value in os.getloadavg())
except (AttributeError, OSError):
    load_average = "unknown"
memory_total = "unknown"
try:
    for line in Path("/proc/meminfo").read_text(encoding="ascii").splitlines():
        if line.startswith("MemTotal:"):
            memory_total = str(int(line.split()[1]) * 1024)
            break
except (OSError, ValueError, IndexError):
    pass
governors = set()
for path in Path("/sys/devices/system/cpu").glob("cpu[0-9]*/cpufreq/scaling_governor"):
    try:
        value = path.read_text(encoding="ascii").strip()
    except OSError:
        continue
    if value:
        governors.add(value)
print(f"cpu_affinity={affinity}")
print(f"load_average_1m_5m_15m={load_average}")
print(f"memory_total_bytes={memory_total}")
print(f"cpu_governors={','.join(sorted(governors)) or 'unknown'}")
battery_tool = shutil.which("termux-battery-status")
if battery_tool:
    try:
        completed = subprocess.run(
            [battery_tool], capture_output=True, text=True, timeout=5, check=False
        )
        battery = json.loads(completed.stdout) if completed.returncode == 0 else {}
    except (OSError, subprocess.TimeoutExpired, json.JSONDecodeError):
        battery = {}
    if battery:
        print(f"battery_level_percent={battery.get('percentage', 'unknown')}")
        print(f"battery_status={battery.get('status', 'unknown')}")
        print(f"battery_plugged={battery.get('plugged', 'unknown')}")
        print(f"battery_temperature_c={battery.get('temperature', 'unknown')}")
PY
if command -v getprop >/dev/null 2>&1; then
    printf 'android_product=%s\n' "$(getprop ro.product.manufacturer) $(getprop ro.product.model)"
    printf 'android_build_fingerprint=%s\n' "$(getprop ro.build.fingerprint)"
fi
if command -v dumpsys >/dev/null 2>&1; then
    battery_report=$(dumpsys battery 2>/dev/null || true)
    battery_level=$(sed -nE 's/^[[:space:]]*level: ([0-9]+)$/\1/p' <<<"$battery_report" | head -n 1)
    battery_status=$(sed -nE 's/^[[:space:]]*status: ([0-9]+)$/\1/p' <<<"$battery_report" | head -n 1)
    battery_temperature=$(sed -nE 's/^[[:space:]]*temperature: ([0-9]+)$/\1/p' <<<"$battery_report" | head -n 1)
    printf 'battery_level_percent=%s\n' "${battery_level:-unknown}"
    printf 'battery_android_status_code=%s\n' "${battery_status:-unknown}"
    printf 'battery_temperature_tenths_c=%s\n' "${battery_temperature:-unknown}"
fi
printf 'git_commit=%s\n' "$(git rev-parse HEAD 2>/dev/null || printf unknown)"
if [[ -n $(git status --porcelain --untracked-files=normal 2>/dev/null) ]]; then
    printf 'git_tree_state=dirty\n'
else
    printf 'git_tree_state=clean\n'
fi
printf 'benchmark_runner_sha256=%s\n' "$(sha256_file "$ROOT/scripts/benchmark_real_world.sh")"
printf 'timed_exec_sha256=%s\n' "$(sha256_file "$timed_exec")"
printf 'bash_shim_sha256=%s\n' "$(sha256_file "$backends_dir/bash_shim.py")"
printf 'o_binary=%s\n' "$o_bin"
printf 'o_binary_sha256=%s\n' "$(sha256_file "$o_bin")"
printf 'olangc_binary=%s\n' "$olangc_bin"
printf 'olangc_binary_sha256=%s\n' "$(sha256_file "$olangc_bin")"
printf 'workers=%s\n' "$workers"
printf 'warmups=%s\n' "$warmups"
printf 'repetitions=%s\n' "$repetitions"
printf 'selected_workload=%s\n' "$selected_workload"
printf 'python=%s\n' "$(python3 --version 2>&1)"
if command -v magick >/dev/null 2>&1; then
    printf 'imagemagick=%s\n' "$(magick --version | sed -n '1p')"
fi
if command -v ffmpeg >/dev/null 2>&1; then
    printf 'ffmpeg=%s\n' "$(ffmpeg -version | sed -n '1p')"
fi
if [[ "$retained_evidence" == true ]]; then
    printf 'evidence_dir=%s\n' "$work_dir"
fi

workloads=(asset_pipeline ci_shards video_previews)
overall_status=0
for workload in "${workloads[@]}"; do
    if [[ "$selected_workload" != all && "$selected_workload" != "$workload" ]]; then
        continue
    fi
    program=$(program_for "$workload")
    missing=$(missing_tools_for "$workload")
    printf '\nworkload=%s\n' "$workload"
    printf 'program=%s\n' "$program"
    printf 'program_sha256=%s\n' "$(sha256_file "$program")"
    printf 'required_tools=%s\n' "$(required_tools_for "$workload")"
    printf 'missing_tools=%s\n' "$missing"
    if [[ "$missing" != none ]]; then
        printf 'status=skipped\n'
        if [[ "$missing_tool_policy" == fail ]]; then
            overall_status=1
        fi
        continue
    fi
    if ! validate_fixture_inputs "$workload"; then
        printf 'status=invalid-input\n'
        overall_status=1
        continue
    fi
    workload_dir=$work_dir/$workload
    mkdir -p -- "$workload_dir"
    write_input_manifest "$workload" "$workload_dir/input-manifest.json"
    printf 'input_manifest_sha256=%s\n' \
        "$(sha256_file "$workload_dir/input-manifest.json")"
    if [[ "$workload" == video_previews ]]; then
        printf 'preview_seconds=%s\n' "$preview_seconds"
        printf 'expected_decoded_frames_per_preview=%s\n' "$((preview_seconds * 30))"
    fi
    if ! analyze_workload "$workload" "$program" "$workload_dir"; then
        printf 'status=invalid-plan\n'
        overall_status=1
        continue
    fi
    printf 'prediction_schema=oexec.schedule-prediction/v1\n'
    printf 'prediction_provenance=evidence-bound-admission\n'
    printf 'prediction_admission_sha256=%s\n' "$prediction_digest"
    printf 'predicted_tasks=%s\n' "$predicted_tasks"
    printf 'predicted_width=%s\n' "$predicted_width"
    printf 'predicted_span=%s\n' "$predicted_span"

    workload_status=0
    for ((ordinal = 1; ordinal <= warmups; ordinal++)); do
        run_pair "$workload" warmup "$ordinal" "$program" "$workload_dir" || {
            workload_status=1
            break
        }
    done
    if [[ "$workload_status" -eq 0 ]]; then
        for ((ordinal = 1; ordinal <= repetitions; ordinal++)); do
            run_pair "$workload" sample "$ordinal" "$program" "$workload_dir" || {
                workload_status=1
                break
            }
        done
    fi
    if [[ "$workload_status" -ne 0 ]]; then
        printf 'status=failed\n'
        overall_status=1
        continue
    fi
    printf 'status=measured\n'
    printf 'semantic_equivalence=true\n'
    printf 'artifact_equivalence=true\n'
    print_metrics "$workload_dir/pairs.tsv" "$predicted_tasks" "$predicted_span" "$workers"
done

exit "$overall_status"
