#!/usr/bin/env python3
"""Bounded, backend-free CPU benchmark and regression gate for Ostadix-lang."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable, Sequence


SCHEMA = "ostadix.cpu-benchmark/v1"
BENCHMARK_NAME = "ostadix-backend-free-cpu"
ROOT = Path(__file__).resolve().parents[2]
RUNNER = Path(__file__).resolve()
MAX_SOURCE_BYTES = 64 * 1024 * 1024
MAX_RESULT_BYTES = 16 * 1024 * 1024
GATE_CASES = ("parser_check", "evaluator_serial", "evaluator_graph")


class BenchmarkError(RuntimeError):
    """An expected command, input, or result-contract failure."""


def canonical_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError as error:
        raise BenchmarkError(f"cannot hash {path}: {error}") from error
    return digest.hexdigest()


def finite_float(raw: str) -> float:
    try:
        value = float(raw)
    except ValueError as error:
        raise argparse.ArgumentTypeError(str(error)) from error
    if not math.isfinite(value):
        raise argparse.ArgumentTypeError("must be finite")
    return value


def bounded_integer(name: str, minimum: int, maximum: int) -> Callable[[str], int]:
    def parse(raw: str) -> int:
        try:
            value = int(raw)
        except ValueError as error:
            raise argparse.ArgumentTypeError(f"{name} must be an integer") from error
        if not minimum <= value <= maximum:
            raise argparse.ArgumentTypeError(
                f"{name} must be between {minimum} and {maximum}, got {value}"
            )
        return value

    return parse


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Measure Ostadix parser/check and backend-free evaluator CPU work. "
            "No performance threshold is applied unless --baseline is supplied."
        )
    )
    parser.add_argument(
        "--o-bin",
        type=Path,
        default=Path(os.environ.get("O_RELEASE_BIN", ROOT / "target/release/O")),
        help="O executable (default: target/release/O or O_RELEASE_BIN)",
    )
    parser.add_argument(
        "--backends-dir",
        type=Path,
        default=Path(os.environ.get("O_BACKENDS_DIR", ROOT / "backends")),
        help="backend shim directory passed to O (no foreign backend is executed)",
    )
    parser.add_argument(
        "--warmups",
        type=bounded_integer("warmups", 0, 20),
        default=2,
    )
    parser.add_argument(
        "--repetitions",
        type=bounded_integer("repetitions", 1, 100),
        default=7,
    )
    parser.add_argument(
        "--workers",
        type=bounded_integer("workers", 1, 256),
        default=min(4, os.cpu_count() or 1),
    )
    parser.add_argument(
        "--parse-bindings",
        type=bounded_integer("parse-bindings", 1, 100_000),
        default=1_000,
        help="number of bindings in the parser/check workload",
    )
    parser.add_argument(
        "--dag-width",
        type=bounded_integer("dag-width", 2, 512),
        default=32,
    )
    parser.add_argument(
        "--dag-depth",
        type=bounded_integer("dag-depth", 1, 64),
        default=4,
    )
    parser.add_argument(
        "--payload-bytes",
        type=bounded_integer("payload-bytes", 1, 4096),
        default=64,
    )
    parser.add_argument(
        "--timeout-seconds",
        type=finite_float,
        default=120.0,
        help="per-process timeout, in seconds (default: 120)",
    )
    parser.add_argument(
        "--output",
        type=Path,
        help="write JSON here instead of stdout",
    )
    parser.add_argument(
        "--baseline",
        type=Path,
        help="opt in to a regression gate using a prior v1 result",
    )
    parser.add_argument(
        "--max-regression-percent",
        type=finite_float,
        default=15.0,
        help="relative gate guard used with --baseline (default: 15)",
    )
    parser.add_argument(
        "--min-regression-ms",
        type=finite_float,
        default=2.0,
        help="absolute noise guard used with --baseline (default: 2)",
    )
    args = parser.parse_args(argv)
    if not 0.05 <= args.timeout_seconds <= 3600:
        parser.error("--timeout-seconds must be between 0.05 and 3600")
    if not 0 <= args.max_regression_percent <= 1000:
        parser.error("--max-regression-percent must be between 0 and 1000")
    if not 0 <= args.min_regression_ms <= 60_000:
        parser.error("--min-regression-ms must be between 0 and 60000")
    if args.baseline is not None and args.repetitions < 5:
        parser.error("--baseline requires at least 5 repetitions")
    return args


def resolve_executable(path: Path) -> Path:
    try:
        resolved = path.expanduser().resolve(strict=True)
    except OSError as error:
        raise BenchmarkError(f"O executable does not exist: {path}: {error}") from error
    if not resolved.is_file() or not os.access(resolved, os.X_OK):
        raise BenchmarkError(f"O executable is not an executable file: {resolved}")
    return resolved


def resolve_directory(path: Path) -> Path:
    try:
        resolved = path.expanduser().resolve(strict=True)
    except OSError as error:
        raise BenchmarkError(f"backend directory does not exist: {path}: {error}") from error
    if not resolved.is_dir():
        raise BenchmarkError(f"backend path is not a directory: {resolved}")
    return resolved


def payload(label: str, size: int) -> str:
    seed = hashlib.sha256(label.encode("utf-8")).hexdigest()
    return (seed * ((size + len(seed) - 1) // len(seed)))[:size]


def generate_parse_workload(bindings: int, payload_bytes: int) -> str:
    lines = ["# generated parser/check CPU workload"]
    for index in range(bindings):
        name = f"parse_{index:06d}"
        lines.append(
            f"let {name} = text^(parse:{index:06d}:{payload(name, payload_bytes)})_text"
        )
    lines.append(f"text^($parse_{bindings - 1:06d})_text")
    return "\n".join(lines) + "\n"


def substitute(body: str, values: dict[str, str]) -> str:
    """Expand the simple generated $identifier form for the expected result."""
    output: list[str] = []
    index = 0
    while index < len(body):
        if body[index] != "$":
            output.append(body[index])
            index += 1
            continue
        end = index + 1
        while end < len(body) and (body[end].isalnum() or body[end] == "_"):
            end += 1
        name = body[index + 1 : end]
        if not name or name not in values:
            raise BenchmarkError(f"generated workload contains unresolved binding ${name}")
        output.append(values[name])
        index = end
    return "".join(output)


def generate_dag_workload(width: int, depth: int, payload_bytes: int) -> tuple[str, str]:
    lines = ["# generated backend-free evaluator DAG"]
    values: dict[str, str] = {}
    seed_body = f"seed:{payload('seed', payload_bytes)}"
    lines.append(f"let seed = text^({seed_body})_text")
    values["seed"] = seed_body

    previous = ["seed"] * width
    for layer in range(depth):
        current = []
        for column in range(width):
            name = f"n_{layer:03d}_{column:03d}"
            parent = previous[(column * 17 + layer) % width]
            body = (
                f"layer:{layer:03d}:node:{column:03d}:"
                f"${parent}:{payload(name, payload_bytes)}"
            )
            lines.append(f"let {name} = text^({body})_text")
            values[name] = substitute(body, values)
            current.append(name)
        previous = current

    final_body = "result:" + "|".join(f"${name}" for name in previous)
    lines.append(f"text^({final_body})_text")
    expected = substitute(final_body, values)
    return "\n".join(lines) + "\n", expected


def enforce_workload_bounds(name: str, source: str, expected: str | None = None) -> None:
    source_size = len(source.encode("utf-8"))
    if source_size > MAX_SOURCE_BYTES:
        raise BenchmarkError(
            f"{name} source is {source_size} bytes; maximum is {MAX_SOURCE_BYTES}"
        )
    if expected is not None:
        result_size = len(expected.encode("utf-8"))
        if result_size > MAX_RESULT_BYTES:
            raise BenchmarkError(
                f"{name} result is {result_size} bytes; maximum is {MAX_RESULT_BYTES}"
            )


def preflight_workload_bounds(
    parse_bindings: int, width: int, depth: int, payload_bytes: int
) -> None:
    """Reject configurations that cannot fit before constructing large strings."""
    parse_source_upper = 128 + parse_bindings * (payload_bytes + 96)
    dag_source_upper = 256 + width * depth * (payload_bytes + 160)
    dag_result_upper = 256 + width * (depth * (payload_bytes + 64) + payload_bytes)
    if parse_source_upper > MAX_SOURCE_BYTES:
        raise BenchmarkError(
            "requested parser workload can exceed the 64 MiB source limit; "
            "reduce --parse-bindings or --payload-bytes"
        )
    if dag_source_upper > MAX_SOURCE_BYTES:
        raise BenchmarkError(
            "requested evaluator DAG can exceed the 64 MiB source limit; "
            "reduce its width, depth, or payload"
        )
    if dag_result_upper > MAX_RESULT_BYTES:
        raise BenchmarkError(
            "requested evaluator DAG can exceed the 16 MiB result limit; "
            "reduce its width, depth, or payload"
        )


def command_environment() -> dict[str, str]:
    environment = os.environ.copy()
    environment["LC_ALL"] = "C"
    environment["TZ"] = "UTC"
    return environment


def run_process(
    command: Sequence[str], timeout: float, *, measure: bool = True
) -> tuple[int, str, str]:
    start = time.perf_counter_ns()
    try:
        completed = subprocess.run(
            list(command),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            env=command_environment(),
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        display = " ".join(command[:4])
        raise BenchmarkError(f"command timed out after {timeout:g}s: {display}") from error
    elapsed_ns = time.perf_counter_ns() - start if measure else 0
    if completed.returncode != 0:
        stderr = completed.stderr.strip()
        if len(stderr) > 2000:
            stderr = stderr[:2000] + "..."
        display = " ".join(command[:4])
        raise BenchmarkError(
            f"command failed with exit {completed.returncode}: {display}\n{stderr}"
        )
    return elapsed_ns, completed.stdout, completed.stderr


def parse_json_output(stdout: str, label: str) -> dict[str, Any]:
    try:
        value = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise BenchmarkError(f"{label} emitted invalid JSON: {error}") from error
    if not isinstance(value, dict):
        raise BenchmarkError(f"{label} JSON must be an object")
    return value


def run_check(command: Sequence[str], timeout: float, label: str) -> int:
    elapsed_ns, stdout, _ = run_process(command, timeout)
    value = parse_json_output(stdout, label)
    if value.get("ok") is not True or value.get("stage") != "parse":
        raise BenchmarkError(f"{label} did not report a successful parse stage: {value!r}")
    return elapsed_ns


def semantic_result(value: dict[str, Any], label: str) -> tuple[dict[str, Any], int]:
    if value.get("ok") is not True:
        raise BenchmarkError(f"{label} reported failure: {value!r}")
    if not isinstance(value.get("type"), str) or "value" not in value:
        raise BenchmarkError(f"{label} omitted its type or value")
    elapsed_ms = value.get("elapsed_ms")
    if isinstance(elapsed_ms, bool) or not isinstance(elapsed_ms, int) or elapsed_ms < 0:
        raise BenchmarkError(f"{label} has invalid elapsed_ms: {elapsed_ms!r}")
    return {
        "ok": True,
        "type": value["type"],
        "value": value["value"],
    }, elapsed_ms


def run_evaluator(
    command: Sequence[str], timeout: float, label: str
) -> tuple[int, int, dict[str, Any]]:
    wall_ns, stdout, _ = run_process(command, timeout)
    semantic, elapsed_ms = semantic_result(parse_json_output(stdout, label), label)
    return wall_ns, elapsed_ms, semantic


def median(values: Sequence[int | float]) -> int | float:
    value = statistics.median(values)
    if isinstance(value, float) and value.is_integer():
        return int(value)
    return value


def summarize(values: Sequence[int], unit: str) -> dict[str, Any]:
    if not values:
        raise BenchmarkError("cannot summarize an empty sample")
    center = median(values)
    deviations = [abs(value - float(center)) for value in values]
    return {
        "unit": unit,
        "raw": list(values),
        "count": len(values),
        "min": min(values),
        "median": center,
        "max": max(values),
        "median_absolute_deviation": median(deviations),
    }


def read_first_cpu_model() -> str:
    path = Path("/proc/cpuinfo")
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return platform.processor() or "unknown"
    candidates: dict[str, str] = {}
    for line in lines:
        if ":" not in line:
            continue
        key, value = (part.strip() for part in line.split(":", 1))
        if value and key not in candidates:
            candidates[key] = value
    for key in ("model name", "Hardware", "Processor", "cpu model"):
        if key in candidates:
            return candidates[key]
    return platform.processor() or "unknown"


def cpu_governors() -> list[str]:
    values = set()
    root = Path("/sys/devices/system/cpu")
    for path in root.glob("cpu[0-9]*/cpufreq/scaling_governor"):
        try:
            values.add(path.read_text(encoding="ascii").strip())
        except OSError:
            pass
    return sorted(value for value in values if value)


def memory_total_bytes() -> int | None:
    try:
        for line in Path("/proc/meminfo").read_text(encoding="ascii").splitlines():
            if line.startswith("MemTotal:"):
                return int(line.split()[1]) * 1024
    except (OSError, ValueError, IndexError):
        return None
    return None


def git_provenance() -> dict[str, str]:
    def git(*arguments: str) -> str | None:
        try:
            completed = subprocess.run(
                ["git", "-C", str(ROOT), *arguments],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                timeout=3,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired):
            return None
        return completed.stdout.strip() if completed.returncode == 0 else None

    commit = git("rev-parse", "HEAD") or "unknown"
    status = git("status", "--porcelain", "--untracked-files=normal")
    tree_state = "unknown" if status is None else "dirty" if status else "clean"
    return {"commit": commit, "tree_state": tree_state}


def host_provenance() -> dict[str, Any]:
    uname = platform.uname()
    try:
        affinity: list[int] | str = sorted(os.sched_getaffinity(0))
    except (AttributeError, OSError):
        affinity = "unknown"
    governors = cpu_governors()
    stable = {
        "system": uname.system,
        "release": uname.release,
        "machine": uname.machine,
        "cpu_model": read_first_cpu_model(),
        "logical_cpus": os.cpu_count(),
        "affinity": affinity,
        "cpu_governors": governors,
    }
    return {
        **stable,
        "version": uname.version,
        "memory_total_bytes": memory_total_bytes(),
        "load_average": list(os.getloadavg()) if hasattr(os, "getloadavg") else None,
        "comparison_fingerprint_sha256": sha256_bytes(canonical_json(stable).encode()),
    }


def relevant_environment() -> dict[str, str]:
    names = (
        "O_EXECUTOR",
        "O_GRAPH_WORKERS",
        "RAYON_NUM_THREADS",
        "OMP_NUM_THREADS",
        "ANDROID_ROOT",
        "TERMUX_VERSION",
    )
    return {name: os.environ[name] for name in names if name in os.environ}


def runtime_provenance(o_bin: Path, timeout: float) -> dict[str, Any]:
    _, stdout, stderr = run_process([str(o_bin), "version", "--json"], timeout, measure=False)
    report = parse_json_output(stdout, "O version --json")
    return {
        "path": str(o_bin),
        "sha256": sha256_file(o_bin),
        "size_bytes": o_bin.stat().st_size,
        "version_report": report,
        "version_stderr": stderr.strip(),
        "profile_hint": (
            "release"
            if "/target/release/" in f"/{o_bin.as_posix().lstrip('/')}"
            else "unknown"
        ),
    }


def workload_record(source: str, **dimensions: int | str) -> dict[str, Any]:
    encoded = source.encode("utf-8")
    return {
        "sha256": sha256_bytes(encoded),
        "bytes": len(encoded),
        "generation": "deterministic-v1",
        "dimensions": dimensions,
    }


def expected_semantic(text: str) -> dict[str, Any]:
    return {
        "ok": True,
        "type": "text",
        "value": {"t": "text", "v": {"utf8": text, "encoding": "utf-8"}},
    }


def load_baseline(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise BenchmarkError(f"cannot read baseline {path}: {error}") from error
    if not isinstance(value, dict) or value.get("schema") != SCHEMA:
        raise BenchmarkError(f"baseline must use schema {SCHEMA}")
    return value


def comparison_key(
    runner_sha256: str,
    host: dict[str, Any],
    configuration: dict[str, Any],
    workloads: dict[str, Any],
) -> str:
    comparable = {
        "schema": SCHEMA,
        "runner_sha256": runner_sha256,
        "host_fingerprint_sha256": host["comparison_fingerprint_sha256"],
        "configuration": {
            name: configuration[name]
            for name in (
                "workers",
                "parse_bindings",
                "dag_width",
                "dag_depth",
                "payload_bytes",
            )
        },
        "workload_sha256": {
            name: value["sha256"] for name, value in sorted(workloads.items())
        },
    }
    return sha256_bytes(canonical_json(comparable).encode("utf-8"))


def measured_raw(result: dict[str, Any], case: str) -> list[int]:
    try:
        raw = result["measurements"][case]["wall_time"]["raw"]
    except (KeyError, TypeError) as error:
        raise BenchmarkError(f"baseline is missing {case} wall-time samples") from error
    if (
        not isinstance(raw, list)
        or len(raw) < 5
        or any(isinstance(value, bool) or not isinstance(value, int) or value <= 0 for value in raw)
    ):
        raise BenchmarkError(f"baseline {case} must contain at least 5 positive integer samples")
    return raw


def evaluate_gate(
    result: dict[str, Any],
    baseline: dict[str, Any] | None,
    baseline_path: Path | None,
    max_percent: float,
    min_ms: float,
) -> tuple[dict[str, Any], int]:
    if baseline is None:
        return {"enabled": False, "status": "not_requested"}, 0

    baseline_key = baseline.get("comparison_key_sha256")
    current_key = result["comparison_key_sha256"]
    gate: dict[str, Any] = {
        "enabled": True,
        "baseline_path": str(baseline_path),
        "baseline_result_sha256": sha256_bytes(canonical_json(baseline).encode("utf-8")),
        "max_regression_percent": max_percent,
        "min_regression_ms": min_ms,
        "rule": "regressed only when both relative and absolute guards are exceeded",
        "cases": {},
    }
    if not isinstance(baseline_key, str) or baseline_key != current_key:
        gate.update(
            {
                "status": "incompatible",
                "reason": "host, runner, workload, or performance configuration differs",
                "baseline_comparison_key_sha256": baseline_key,
                "current_comparison_key_sha256": current_key,
            }
        )
        return gate, 2

    min_ns = min_ms * 1_000_000
    regressed = []
    for case in GATE_CASES:
        baseline_values = measured_raw(baseline, case)
        current_values = result["measurements"][case]["wall_time"]["raw"]
        if len(current_values) < 5:
            raise BenchmarkError("internal error: current gate has fewer than 5 samples")
        baseline_median = float(statistics.median(baseline_values))
        current_median = float(statistics.median(current_values))
        delta_ns = current_median - baseline_median
        delta_percent = (
            (delta_ns / baseline_median) * 100 if baseline_median > 0 else math.inf
        )
        failed = delta_percent > max_percent and delta_ns > min_ns
        gate["cases"][case] = {
            "status": "regressed" if failed else "pass",
            "baseline_median_ns": baseline_median,
            "current_median_ns": current_median,
            "delta_ns": delta_ns,
            "delta_percent": delta_percent,
        }
        if failed:
            regressed.append(case)
    gate["status"] = "regressed" if regressed else "pass"
    gate["regressed_cases"] = regressed
    return gate, 3 if regressed else 0


def write_result(result: dict[str, Any], output: Path | None) -> None:
    rendered = json.dumps(result, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    if output is None:
        sys.stdout.write(rendered)
        return
    try:
        output = output.expanduser()
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(rendered, encoding="utf-8")
    except OSError as error:
        raise BenchmarkError(f"cannot write result {output}: {error}") from error


def execute(args: argparse.Namespace) -> tuple[dict[str, Any], int]:
    started_at_utc = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    total_start_ns = time.perf_counter_ns()
    o_bin = resolve_executable(args.o_bin)
    backends_dir = resolve_directory(args.backends_dir)
    runner_sha256 = sha256_file(RUNNER)
    host = host_provenance()
    configuration = {
        "warmups": args.warmups,
        "repetitions": args.repetitions,
        "workers": args.workers,
        "parse_bindings": args.parse_bindings,
        "dag_width": args.dag_width,
        "dag_depth": args.dag_depth,
        "payload_bytes": args.payload_bytes,
        "timeout_seconds": args.timeout_seconds,
    }

    preflight_workload_bounds(
        args.parse_bindings, args.dag_width, args.dag_depth, args.payload_bytes
    )
    startup_source = "text^(startup-control)_text\n"
    parse_source = generate_parse_workload(args.parse_bindings, args.payload_bytes)
    dag_source, expected_text = generate_dag_workload(
        args.dag_width, args.dag_depth, args.payload_bytes
    )
    enforce_workload_bounds("startup_control", startup_source)
    enforce_workload_bounds("parser_check", parse_source)
    enforce_workload_bounds("evaluator_dag", dag_source, expected_text)
    workloads = {
        "startup_control": workload_record(startup_source, expressions=1),
        "parser_check": workload_record(
            parse_source, bindings=args.parse_bindings, payload_bytes=args.payload_bytes
        ),
        "evaluator_dag": workload_record(
            dag_source,
            width=args.dag_width,
            depth=args.dag_depth,
            bindings=1 + args.dag_width * args.dag_depth,
            payload_bytes=args.payload_bytes,
            expected_result_bytes=len(expected_text.encode("utf-8")),
        ),
    }
    key = comparison_key(runner_sha256, host, configuration, workloads)
    baseline = load_baseline(args.baseline) if args.baseline is not None else None

    expected = expected_semantic(expected_text)
    expected_digest = sha256_bytes(canonical_json(expected).encode("utf-8"))
    warmup_samples: list[dict[str, Any]] = []
    measured_samples: list[dict[str, Any]] = []

    with tempfile.TemporaryDirectory(prefix="ostadix-cpu-benchmark-") as raw_directory:
        directory = Path(raw_directory)
        startup_path = directory / "startup_control.O"
        parse_path = directory / "parser_check.O"
        dag_path = directory / "evaluator_dag.O"
        startup_path.write_text(startup_source, encoding="utf-8")
        parse_path.write_text(parse_source, encoding="utf-8")
        dag_path.write_text(dag_source, encoding="utf-8")

        commands = {
            "startup_check": [
                str(o_bin),
                "--check",
                "--json",
                str(startup_path),
                str(backends_dir),
            ],
            "parser_check": [
                str(o_bin),
                "--check",
                "--json",
                str(parse_path),
                str(backends_dir),
            ],
            "evaluator_serial": [
                str(o_bin),
                "--executor",
                "serial",
                "--workers",
                str(args.workers),
                "--json",
                str(dag_path),
                str(backends_dir),
            ],
            "evaluator_graph": [
                str(o_bin),
                "--executor",
                "graph",
                "--workers",
                str(args.workers),
                "--json",
                str(dag_path),
                str(backends_dir),
            ],
        }

        total = args.warmups + args.repetitions
        for global_ordinal in range(1, total + 1):
            phase = "warmup" if global_ordinal <= args.warmups else "measured"
            phase_ordinal = (
                global_ordinal
                if phase == "warmup"
                else global_ordinal - args.warmups
            )
            if global_ordinal % 2:
                order = [
                    "startup_check",
                    "parser_check",
                    "evaluator_serial",
                    "evaluator_graph",
                ]
            else:
                order = [
                    "evaluator_graph",
                    "evaluator_serial",
                    "parser_check",
                    "startup_check",
                ]
            print(
                f"{phase} {phase_ordinal}/{args.warmups if phase == 'warmup' else args.repetitions} "
                f"order={','.join(order)}",
                file=sys.stderr,
                flush=True,
            )
            sample: dict[str, Any] = {
                "ordinal": phase_ordinal,
                "order": order,
                "wall_time_ns": {},
                "runtime_elapsed_ms": {},
            }
            semantics: dict[str, dict[str, Any]] = {}
            for case in order:
                if case in ("startup_check", "parser_check"):
                    sample["wall_time_ns"][case] = run_check(
                        commands[case], args.timeout_seconds, case
                    )
                    continue
                wall_ns, runtime_ms, semantic = run_evaluator(
                    commands[case], args.timeout_seconds, case
                )
                sample["wall_time_ns"][case] = wall_ns
                sample["runtime_elapsed_ms"][case] = runtime_ms
                semantics[case] = semantic

            serial = semantics["evaluator_serial"]
            graph = semantics["evaluator_graph"]
            if serial != graph:
                raise BenchmarkError(
                    f"serial/graph semantic mismatch in {phase} sample {phase_ordinal}: "
                    f"serial={canonical_json(serial)} graph={canonical_json(graph)}"
                )
            if serial != expected:
                raise BenchmarkError(
                    f"evaluator result differs from the generated oracle in {phase} "
                    f"sample {phase_ordinal}"
                )
            sample["semantic_sha256"] = sha256_bytes(
                canonical_json(serial).encode("utf-8")
            )
            (warmup_samples if phase == "warmup" else measured_samples).append(sample)

    measurements: dict[str, Any] = {}
    for case in ("startup_check", "parser_check", "evaluator_serial", "evaluator_graph"):
        wall_values = [sample["wall_time_ns"][case] for sample in measured_samples]
        measurements[case] = {"wall_time": summarize(wall_values, "ns")}
        if case.startswith("evaluator_"):
            runtime_values = [
                sample["runtime_elapsed_ms"][case] for sample in measured_samples
            ]
            measurements[case]["runtime_elapsed"] = summarize(runtime_values, "ms")

    runtime = runtime_provenance(o_bin, args.timeout_seconds)
    result: dict[str, Any] = {
        "schema": SCHEMA,
        "benchmark": BENCHMARK_NAME,
        "started_at_utc": started_at_utc,
        "completed_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "total_duration_ns": time.perf_counter_ns() - total_start_ns,
        "clock": "time.perf_counter_ns",
        "comparison_key_sha256": key,
        "configuration": configuration,
        "provenance": {
            "runner_path": str(RUNNER),
            "runner_sha256": runner_sha256,
            "repository_root": str(ROOT),
            "git": git_provenance(),
            "host": host,
            "python": {
                "executable": sys.executable,
                "version": platform.python_version(),
                "implementation": platform.python_implementation(),
            },
            "runtime": runtime,
            "backends_dir": str(backends_dir),
            "environment": relevant_environment(),
        },
        "command_templates": {
            "startup_check": "O --check --json startup_control.O BACKENDS",
            "parser_check": "O --check --json parser_check.O BACKENDS",
            "evaluator_serial": "O --executor serial --workers N --json evaluator_dag.O BACKENDS",
            "evaluator_graph": "O --executor graph --workers N --json evaluator_dag.O BACKENDS",
        },
        "workloads": workloads,
        "samples": {"warmup": warmup_samples, "measured": measured_samples},
        "measurements": measurements,
        "semantics": {
            "serial_graph_equivalent": True,
            "generated_oracle_match": True,
            "pairs_checked": args.warmups + args.repetitions,
            "canonical_result_sha256": expected_digest,
        },
    }
    gate, exit_code = evaluate_gate(
        result,
        baseline,
        args.baseline,
        args.max_regression_percent,
        args.min_regression_ms,
    )
    result["gate"] = gate
    return result, exit_code


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        result, exit_code = execute(args)
        write_result(result, args.output)
        if exit_code == 2:
            print(f"benchmark gate incompatible: {result['gate']['reason']}", file=sys.stderr)
        elif exit_code == 3:
            print(
                "benchmark regression gate failed: "
                + ", ".join(result["gate"]["regressed_cases"]),
                file=sys.stderr,
            )
        return exit_code
    except BenchmarkError as error:
        print(f"benchmark error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
