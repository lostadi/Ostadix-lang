"""Deterministic contract tests for the hosted HGraph benchmark suite."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import textwrap
import unittest


PROJECT_ROOT = Path(__file__).resolve().parents[1]
RUNNER = PROJECT_ROOT / "scripts" / "benchmark_hgraph_hosted.sh"
FIXTURES = PROJECT_ROOT / "benchmarks" / "hgraph_hosted"
CURRENT_RESULT = FIXTURES / "RESULTS-2026-08-08-f216771.md"
CURRENT_TRANSCRIPT = FIXTURES / "TRANSCRIPT-2026-08-08-f216771.log"
BASH = Path("/bin/bash")
SHAPES = {
    "heterogeneous": (3, 1),
    "chained": (1, 4),
    "mixed_width": (4, 3),
    "realistic": (2, 3),
}


class HostedHGraphBenchmarkTests(unittest.TestCase):
    maxDiff = None

    def write_fake_o(self, directory: Path) -> Path:
        fake_o = directory / "O"
        fake_o.write_text(
            textwrap.dedent(
                """\
                #!/bin/bash
                set -eu
                if [ -n "${FAKE_O_MARKER:-}" ]; then
                    printf 'called\n' >> "$FAKE_O_MARKER"
                fi
                executor=
                program=
                while [ "$#" -gt 0 ]; do
                    case "$1" in
                        --executor)
                            executor=$2
                            shift 2
                            ;;
                        --workers)
                            shift 2
                            ;;
                        --json)
                            shift
                            ;;
                        *)
                            if [ -z "$program" ]; then
                                program=$1
                            fi
                            shift
                            ;;
                    esac
                done
                case "$executor" in
                    serial) elapsed=20 ;;
                    graph) elapsed=10 ;;
                    *) exit 64 ;;
                esac
                case "$program" in
                    */heterogeneous.O)
                        type=list
                        value='{"t":"list","v":[{"t":"text","v":{"utf8":"python","encoding":"utf-8"}},{"t":"text","v":{"utf8":"bash","encoding":"utf-8"}},{"t":"text","v":{"utf8":"node","encoding":"utf-8"}}]}'
                        ;;
                    */chained.O)
                        type=list
                        value='{"t":"list","v":[{"t":"number","v":{"kind":"int","v":"4"}}]}'
                        ;;
                    */mixed_width.O)
                        type=number
                        value='{"t":"number","v":{"kind":"int","v":"50"}}'
                        ;;
                    */realistic.O)
                        type=text
                        value='{"t":"text","v":{"utf8":"aggregate|transform:alpha-beta-gamma|format:ALPHA-BETA-GAMMA","encoding":"utf-8"}}'
                        ;;
                    *) exit 65 ;;
                esac
                printf '{"ok":true,"value":%s,"type":"%s","elapsed_ms":%s}\n' "$value" "$type" "$elapsed"
                """
            ),
            encoding="utf-8",
        )
        fake_o.chmod(0o755)
        return fake_o

    def write_fake_olangc(self, directory: Path) -> Path:
        fake_olangc = directory / "olangc"
        fake_olangc.write_text(
            textwrap.dedent(
                f"""\
                #!{sys.executable}
                import hashlib
                import json
                import os
                from pathlib import Path
                import sys

                args = sys.argv[1:]
                if not args:
                    raise SystemExit(64)
                program = Path(args[0])
                required = [
                    "--target",
                    "ir",
                    "--explain-schedule",
                    "--format",
                    "json",
                    "--workers",
                    "--shim-dir",
                ]
                for item in required:
                    if item not in args:
                        raise SystemExit(f"missing analyzer argument: {{item}}")
                if args[args.index("--format") + 1] != "json":
                    raise SystemExit("benchmark did not request JSON schedule output")
                source = program.read_text(encoding="utf-8")
                if "__SLEEP_" in source:
                    raise SystemExit("analyzer received an unresolved timing placeholder")
                if log := os.environ.get("FAKE_OLANGC_LOG"):
                    with open(log, "a", encoding="utf-8") as handle:
                        handle.write(program.name + " " + " ".join(args[1:]) + "\\n")

                mode = os.environ.get("FAKE_OLANGC_MODE", "valid")
                if mode == "exit":
                    raise SystemExit(72)
                layers = {{
                    "heterogeneous": [[2, 4, 6]],
                    "chained": [[3], [9], [15], [20]],
                    "mixed_width": [[1], [7, 9, 11, 13], [16]],
                    "realistic": [[1], [7, 9], [12]],
                }}[program.stem]
                if mode == "alternate":
                    layers = [[2], [4]]
                task_count = sum(map(len, layers))
                width = max(map(len, layers))
                span = len(layers)
                digest = hashlib.sha256(program.stem.encode()).hexdigest()
                admission_digest = digest
                if mode == "mismatched-admission":
                    admission_digest = hashlib.sha256(b"different-admission").hexdigest()
                placement_admission_digest = hashlib.sha256(
                    b"placement-admission"
                ).hexdigest()
                if mode == "malformed-placement-admission-binding":
                    placement_admission_digest = "not-a-digest"
                if mode == "nonlowercase-placement-admission-binding":
                    placement_admission_digest = placement_admission_digest.upper()
                bindings = {{
                    "lowered_oir_sha256": hashlib.sha256(b"oir").hexdigest(),
                    "plan_sha256": hashlib.sha256(b"plan").hexdigest(),
                    "analyzed_graph_sha256": hashlib.sha256(b"analyzed-graph").hexdigest(),
                    "backend_catalog_projection_sha256": hashlib.sha256(b"catalog").hexdigest(),
                    "backend_set_sha256": hashlib.sha256(b"backend-set").hexdigest(),
                    "direct_executable_manifest_sha256": hashlib.sha256(b"executables").hexdigest(),
                    "launch_context_sha256": hashlib.sha256(b"launch-context").hexdigest(),
                    "environment_sha256": hashlib.sha256(b"environment").hexdigest(),
                    "ambient_world_sha256": hashlib.sha256(b"ambient-world").hexdigest(),
                    "analyzer_sha256": hashlib.sha256(b"analyzer").hexdigest(),
                    "evidence_sha256": hashlib.sha256(b"evidence").hexdigest(),
                    "admitted_graph_sha256": hashlib.sha256(b"graph").hexdigest(),
                    "placement_admission_sha256": placement_admission_digest,
                    "admission_sha256": admission_digest,
                }}
                workers = int(args[args.index("--workers") + 1])
                coverage = "yes" if workers >= width else "no"
                document = {{
                    "schema": "oexec.schedule-explanation/v1",
                    "admission": {{
                        "schema": "oexec.admission/v5",
                        "analyzer": "fixture-analyzer/v5",
                        "runtime_snapshot_kind": "inspection",
                        "base_policy": "eager",
                        "bindings": bindings,
                    }},
                    "realizability": {{
                        "schema": "oexec.realizability/v1",
                        "status": "inspection-only",
                        "execution_realizable": "unknown",
                        "dispatch": "not-run",
                        "scope": "local-worker-static-wave",
                        "worker_count_covers_static_wave": coverage,
                        "runtime_readiness": "unknown",
                        "placement_lease": "none",
                        "observed_overlap": "not-run",
                        "source": "cli-override",
                        "available_parallelism": 8,
                        "admitted_static_max_wave_width": width,
                        "admitted_max_local_worker_wave_width": width,
                        "selected_workers": workers,
                    }},
                    "prediction": {{
                        "schema": "oexec.schedule-prediction/v1",
                        "status": "admitted-static",
                        "provenance": "evidence-bound-admission",
                        "model": "unit-cost-shim-hosted-tasks",
                        "admission_sha256": digest,
                        "task_count": task_count,
                        "predicted_width": width,
                        "predicted_span": span,
                        "span_unit": "hosted-task-layers",
                        "layers": [
                            {{
                                "index": index,
                                "operations": [f"P{{operation}}" for operation in operations],
                            }}
                            for index, operations in enumerate(layers, 1)
                        ],
                    }},
                }}
                if mode == "invalid-json":
                    print("{{")
                    raise SystemExit(0)
                if mode == "wrong-explanation-schema":
                    document["schema"] = "oexec.schedule-explanation/v999"
                if mode == "extra-top-level-field":
                    document["unexpected"] = True
                if mode == "missing-admission":
                    del document["admission"]
                if mode == "wrong-admission-schema":
                    document["admission"]["schema"] = "oexec.admission/v999"
                if mode == "missing-binding-field":
                    del bindings["placement_admission_sha256"]
                if mode == "extra-binding-field":
                    bindings["unexpected_sha256"] = hashlib.sha256(b"unexpected").hexdigest()
                if mode == "missing-prediction":
                    del document["prediction"]
                if mode == "extra-prediction-field":
                    document["prediction"]["unexpected"] = True
                if mode == "wrong-prediction-schema":
                    document["prediction"]["schema"] = "oexec.schedule-prediction/v999"
                if mode == "bad-digest":
                    document["prediction"]["admission_sha256"] = "not-a-digest"
                if mode == "bad-span":
                    document["prediction"]["predicted_span"] += 1
                if mode == "duplicate-operation":
                    document["prediction"]["layers"][-1]["operations"] = [
                        document["prediction"]["layers"][0]["operations"][0]
                    ]
                if mode == "noncanonical-operation":
                    document["prediction"]["layers"][0]["operations"][0] = "P01"
                if mode == "wrong-realizability-source":
                    document["realizability"]["source"] = "machine-default"
                print(json.dumps(document, separators=(",", ":")))
                """
            ),
            encoding="utf-8",
        )
        fake_olangc.chmod(0o755)
        return fake_olangc

    def test_fixtures_encode_the_documented_shapes_and_expected_results(self) -> None:
        for shape in SHAPES:
            source = (FIXTURES / f"{shape}.O").read_text(encoding="utf-8")
            self.assertIn("__SLEEP_SECONDS__", source)
            expected = json.loads(
                (FIXTURES / f"{shape}.expected.json").read_text(encoding="utf-8")
            )
            self.assertEqual(set(expected), {"ok", "type", "value"})
            self.assertIs(expected["ok"], True)

        heterogeneous = (FIXTURES / "heterogeneous.O").read_text(encoding="utf-8")
        self.assertIn("autonomous(batch(", heterogeneous)
        self.assertIn("python^(", heterogeneous)
        self.assertIn("bash^(", heterogeneous)
        self.assertIn("javascript^(", heterogeneous)

        chained = (FIXTURES / "chained.O").read_text(encoding="utf-8")
        self.assertEqual(chained.count("autonomous(batch("), 4)
        self.assertIn("stage_1[0] + 1", chained)
        self.assertIn("stage_2[0] + 1", chained)
        self.assertIn("stage_3[0] + 1", chained)
        self.assertNotIn("$stage_", chained)

        mixed = (FIXTURES / "mixed_width.O").read_text(encoding="utf-8")
        self.assertEqual(mixed.count("python^("), 6)
        self.assertIn("autonomous(batch(", mixed)
        self.assertIn("seed + 1", mixed)
        self.assertNotIn("$seed", mixed)
        self.assertIn("sum($branches)", mixed)

        realistic = (FIXTURES / "realistic.O").read_text(encoding="utf-8")
        self.assertIn('"${fetched}"', realistic)
        self.assertIn("const source = fetched;", realistic)
        self.assertNotIn('"$fetched"', realistic)
        self.assertIn('"aggregate|" + "|".join(parts)', realistic)

    def test_analyzer_bound_result_matches_its_raw_transcript(self) -> None:
        transcript_bytes = CURRENT_TRANSCRIPT.read_bytes()
        transcript = transcript_bytes.decode("utf-8")
        result = CURRENT_RESULT.read_text(encoding="utf-8")
        digest = hashlib.sha256(transcript_bytes).hexdigest()

        self.assertIn(f"| Raw transcript bytes | `{len(transcript_bytes)}` |", result)
        self.assertIn(f"| Raw transcript SHA-256 | `{digest}` |", result)
        marker = "benchmark=hgraph-hosted-ephemeral-autonomous-batch\n"
        sections = transcript.split(marker)[1:]
        self.assertEqual(len(sections), 3)

        def exact_field(text: str, key: str) -> str:
            values = re.findall(rf"(?m)^{re.escape(key)}=(.*)$", text)
            self.assertEqual(len(values), 1, f"{key}: {values!r}")
            return values[0]

        pair_pattern = re.compile(
            r"^shape=(\w+) pair_phase=(warmup|sample) pair_ordinal=([0-9]+) "
            r"order=(serial,graph|graph,serial) serial_elapsed_ms=([0-9]+) "
            r"graph_elapsed_ms=([0-9]+) semantic_equivalence=true "
            r"expected_output_match=true$",
            re.MULTILINE,
        )
        rows = []
        headers = []
        observed_pair_keys = set()
        expected_shapes = list(SHAPES)
        invariant_header_keys = (
            "os",
            "cpu_model",
            "logical_cpus",
            "memory_bytes",
            "git_commit",
            "git_tree_state",
            "o_binary",
            "o_binary_sha256",
            "olangc_binary",
            "olangc_binary_sha256",
            "backends_dir",
            "warmups",
            "repetitions",
            "sleep_seconds",
            "selected_shape",
            "missing_runtime_policy",
            "runtime_python3",
            "runtime_python3_version",
            "runtime_bash",
            "runtime_bash_version",
            "runtime_node",
            "runtime_node_version",
        )

        for section in sections:
            header = {key: exact_field(section, key) for key in invariant_header_keys}
            header["timestamp_utc"] = exact_field(section, "timestamp_utc")
            header["worker_tasks"] = exact_field(section, "worker_tasks")
            headers.append(header)
            workers = int(header["worker_tasks"])
            shape_headers = list(re.finditer(r"(?m)^shape=(\w+)\.O$", section))
            self.assertEqual(
                [shape.group(1) for shape in shape_headers], expected_shapes
            )
            for index, header in enumerate(shape_headers):
                body_end = (
                    shape_headers[index + 1].start()
                    if index + 1 < len(shape_headers)
                    else len(section)
                )
                body = section[header.end() : body_end]
                shape = header.group(1)
                self.assertEqual(exact_field(body, "status"), "measured")
                self.assertEqual(exact_field(body, "semantic_equivalence"), "true")
                self.assertEqual(exact_field(body, "expected_output_match"), "true")
                self.assertEqual(
                    exact_field(body, "prediction_schema"),
                    "oexec.schedule-prediction/v1",
                )
                self.assertEqual(
                    exact_field(body, "prediction_model"),
                    "unit-cost-shim-hosted-tasks",
                )

                pairs = list(pair_pattern.finditer(body))
                self.assertEqual(len(pairs), 6, f"workers={workers} shape={shape}")
                actual_pairs = {(pair.group(2), int(pair.group(3))) for pair in pairs}
                expected_pairs = {("warmup", 1)} | {
                    ("sample", ordinal) for ordinal in range(1, 6)
                }
                self.assertEqual(actual_pairs, expected_pairs)
                for pair in pairs:
                    phase = pair.group(2)
                    ordinal = int(pair.group(3))
                    self.assertEqual(pair.group(1), shape)
                    self.assertEqual(
                        pair.group(4),
                        "serial,graph" if ordinal % 2 == 1 else "graph,serial",
                    )
                    key = (workers, shape, phase, ordinal)
                    self.assertNotIn(key, observed_pair_keys)
                    observed_pair_keys.add(key)

                samples = sorted(
                    (pair for pair in pairs if pair.group(2) == "sample"),
                    key=lambda pair: int(pair.group(3)),
                )
                serial_samples = [int(pair.group(5)) for pair in samples]
                graph_samples = [int(pair.group(6)) for pair in samples]
                serial_median = statistics.median(serial_samples)
                graph_median = statistics.median(graph_samples)
                serial_summary = (
                    f"{serial_median:g}",
                    str(min(serial_samples)),
                    str(max(serial_samples)),
                )
                graph_summary = (
                    f"{graph_median:g}",
                    str(min(graph_samples)),
                    str(max(graph_samples)),
                )

                def timing_summary(executor: str) -> tuple[str, str, str]:
                    matches = re.findall(
                        rf"(?m)^{executor}_elapsed_ms median=([^ ]+) "
                        r"min=([0-9]+) max=([0-9]+)$",
                        body,
                    )
                    self.assertEqual(len(matches), 1)
                    return matches[0]

                self.assertEqual(timing_summary("serial"), serial_summary)
                self.assertEqual(timing_summary("graph"), graph_summary)
                speedup = f"{serial_median / graph_median:.6f}"
                self.assertEqual(
                    exact_field(body, "median_speedup_serial_over_graph"), speedup
                )

                rows.append(
                    {
                        "shape": shape,
                        "workers": workers,
                        "tasks": int(exact_field(body, "predicted_hosted_tasks")),
                        "width": int(exact_field(body, "predicted_width")),
                        "span": int(exact_field(body, "predicted_span")),
                        "admission": exact_field(
                            body, "prediction_admission_sha256"
                        ),
                        "serial": serial_summary[0],
                        "graph": graph_summary[0],
                        "speedup": speedup,
                    }
                )

        self.assertEqual(len(rows), 12)
        self.assertEqual(len(observed_pair_keys), 72)
        self.assertEqual([int(header["worker_tasks"]) for header in headers], [1, 4, 8])
        baseline = {key: headers[0][key] for key in invariant_header_keys}
        for header in headers:
            self.assertEqual(
                {key: header[key] for key in invariant_header_keys}, baseline
            )

        self.assertEqual(baseline["git_tree_state"], "clean")
        self.assertEqual(baseline["warmups"], "1")
        self.assertEqual(baseline["repetitions"], "5")
        self.assertEqual(baseline["sleep_seconds"], "0.25")
        self.assertEqual(baseline["selected_shape"], "all")
        self.assertEqual(baseline["missing_runtime_policy"], "fail")
        self.assertRegex(baseline["git_commit"], r"^[0-9a-f]{40}$")
        self.assertRegex(baseline["o_binary_sha256"], r"^[0-9a-f]{64}$")
        self.assertRegex(baseline["olangc_binary_sha256"], r"^[0-9a-f]{64}$")

        timestamps = ", ".join(f'`{header["timestamp_utc"]}`' for header in headers)
        self.assertIn(f"| Run-start timestamps | {timestamps} |", result)
        self.assertIn(f'| Source commit | `{baseline["git_commit"]}` |', result)
        self.assertIn("| Source tree during every run | `clean` |", result)
        self.assertIn(
            f'| `target/release/O` SHA-256 | `{baseline["o_binary_sha256"]}` |',
            result,
        )
        self.assertIn(
            "| `target/release/olangc` SHA-256 | "
            f'`{baseline["olangc_binary_sha256"]}` |',
            result,
        )
        memory_bytes = int(baseline["memory_bytes"])
        self.assertEqual(memory_bytes % (1024**3), 0)
        self.assertIn(
            f'| Machine | {baseline["cpu_model"]}, {memory_bytes // (1024**3)} GiB |',
            result,
        )
        self.assertIn(f'| OS | {baseline["os"]} |', result)
        self.assertIn(
            f'| Harness-reported logical CPUs | {baseline["logical_cpus"]} |', result
        )
        for label, runtime in (("Python", "python3"), ("Bash", "bash"), ("Node.js", "node")):
            self.assertIn(
                f'| {label} | {baseline[f"runtime_{runtime}_version"]} at '
                f'`{baseline[f"runtime_{runtime}"]}` |',
                result,
            )
        self.assertIn("| Warmup pairs per shape and capacity | 1 |", result)
        self.assertIn("| Measured pairs per shape and capacity | 5 |", result)
        self.assertIn("| Hosted delay per task | 0.25 seconds |", result)
        self.assertIn("| Graph worker overrides | 1, 4, and 8 |", result)
        self.assertIn("| Missing-runtime policy | `fail` |", result)

        predictions_by_shape = {}
        for row in rows:
            predictions_by_shape.setdefault(row["shape"], set()).add(
                (row["tasks"], row["width"], row["span"])
            )
        self.assertEqual(set(predictions_by_shape), set(SHAPES))
        self.assertTrue(all(len(values) == 1 for values in predictions_by_shape.values()))
        self.assertEqual(max(row["width"] for row in rows), 4)
        self.assertTrue(all(4 >= row["width"] for row in rows))
        self.assertTrue(all(8 > row["width"] for row in rows))

        for row in rows:
            capacity_line = (
                f'| `{row["shape"]}.O` | {row["workers"]} | {row["serial"]} | '
                f'{row["graph"]} | {row["speedup"]}× |'
            )
            self.assertIn(capacity_line, result)
            if row["workers"] == 4:
                ideal_speedup = row["tasks"] / row["span"]
                main_line = (
                    f'| `{row["shape"]}.O` | {row["tasks"]} | {row["width"]} | '
                    f'{row["span"]} | {ideal_speedup:.2f}× | {row["serial"]} | '
                    f'{row["graph"]} | {row["speedup"]}× | true | true |'
                )
                self.assertIn(main_line, result)
                self.assertIn(
                    f'| `{row["shape"]}.O` | `{row["admission"]}` |', result
                )

    def test_runner_reports_all_shapes_and_checks_exact_semantics(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            temp = Path(temp_dir)
            fake_o = self.write_fake_o(temp)
            fake_olangc = self.write_fake_olangc(temp)
            analyzer_log = temp / "olangc-invocations.log"
            fake_node = temp / "node"
            fake_node.write_text("#!/bin/bash\nexit 0\n", encoding="utf-8")
            fake_node.chmod(0o755)
            backends = temp / "backends"
            backends.mkdir()

            env = os.environ.copy()
            env.update(
                {
                    "PATH": f"{temp}{os.pathsep}{env.get('PATH', '')}",
                    "O_RELEASE_BIN": str(fake_o),
                    "O_BACKENDS_DIR": str(backends),
                    "FAKE_OLANGC_LOG": str(analyzer_log),
                }
            )
            result = subprocess.run(
                [
                    str(BASH),
                    str(RUNNER),
                    "--warmups",
                    "0",
                    "--repetitions",
                    "2",
                    "--sleep",
                    "0",
                    "--workers",
                    "4",
                    "--missing-runtime",
                    "fail",
                ],
                cwd=PROJECT_ROOT,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=30,
                check=False,
            )
            analyzer_invocations = analyzer_log.read_text(encoding="utf-8")

        combined = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, combined)
        self.assertIn("runtime_python3_version=", result.stdout)
        self.assertIn("runtime_bash_version=", result.stdout)
        self.assertIn("runtime_node_version=", result.stdout)
        self.assertIn(f"olangc_binary={fake_olangc}\n", result.stdout)
        self.assertRegex(result.stdout, r"(?m)^o_binary_sha256=[0-9a-f]{64}$")
        self.assertRegex(result.stdout, r"(?m)^olangc_binary_sha256=[0-9a-f]{64}$")
        self.assertEqual(analyzer_invocations.count("--explain-schedule"), 4)
        for shape, (width, span) in SHAPES.items():
            marker = f"shape={shape}.O\n"
            self.assertIn(marker, result.stdout)
            block = result.stdout.split(marker, 1)[1].split("\nshape=", 1)[0]
            self.assertIn("prediction_source=olangc--explain-schedule-json\n", block)
            self.assertIn(
                "prediction_schema=oexec.schedule-prediction/v1\n", block
            )
            self.assertIn("prediction_provenance=evidence-bound-admission\n", block)
            self.assertIn("prediction_model=unit-cost-shim-hosted-tasks\n", block)
            self.assertRegex(block, r"prediction_admission_sha256=[0-9a-f]{64}\n")
            self.assertIn(f"predicted_width={width}\n", block)
            self.assertIn(f"predicted_span={span}\n", block)
            self.assertIn("predicted_span_unit=hosted-task-layers\n", block)
            self.assertIn("status=measured\n", block)
            self.assertIn("semantic_equivalence=true\n", block)
            self.assertIn(
                "semantic_equivalence_basis=ok+type+canonical-o-value-json\n",
                block,
            )
            self.assertIn("expected_output_match=true\n", block)
            self.assertIn("serial_elapsed_ms median=20 min=20 max=20\n", block)
            self.assertIn("graph_elapsed_ms median=10 min=10 max=10\n", block)
            self.assertIn("median_speedup_serial_over_graph=2.000000\n", block)
            self.assertIn(
                f"{shape}.O --target ir --explain-schedule --format json "
                "--workers 4 --shim-dir",
                analyzer_invocations,
            )
        self.assertEqual(result.stderr.count("semantic_equivalence=true"), 8)

    def test_runner_reports_analyzer_values_instead_of_fixture_name_constants(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            temp = Path(temp_dir)
            fake_o = self.write_fake_o(temp)
            fake_olangc = self.write_fake_olangc(temp)
            fake_node = temp / "node"
            fake_node.write_text("#!/bin/bash\nexit 0\n", encoding="utf-8")
            fake_node.chmod(0o755)
            backends = temp / "backends"
            backends.mkdir()
            env = os.environ.copy()
            env.update(
                {
                    "PATH": f"{temp}{os.pathsep}{env.get('PATH', '')}",
                    "O_RELEASE_BIN": str(fake_o),
                    "OLANGC_RELEASE_BIN": str(fake_olangc),
                    "O_BACKENDS_DIR": str(backends),
                    "FAKE_OLANGC_MODE": "alternate",
                }
            )
            result = subprocess.run(
                [
                    str(BASH),
                    str(RUNNER),
                    "--shape",
                    "heterogeneous",
                    "--warmups",
                    "0",
                    "--repetitions",
                    "1",
                    "--sleep",
                    "0",
                    "--missing-runtime",
                    "fail",
                ],
                cwd=PROJECT_ROOT,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=30,
                check=False,
            )

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("predicted_hosted_tasks=2\n", result.stdout)
        self.assertIn("predicted_width=1\n", result.stdout)
        self.assertIn("predicted_span=2\n", result.stdout)
        self.assertNotIn("predicted_width=3\n", result.stdout)

    def test_invalid_analyzer_prediction_fails_before_execution(self) -> None:
        modes = (
            "exit",
            "invalid-json",
            "wrong-explanation-schema",
            "extra-top-level-field",
            "missing-admission",
            "wrong-admission-schema",
            "missing-binding-field",
            "malformed-placement-admission-binding",
            "nonlowercase-placement-admission-binding",
            "extra-binding-field",
            "mismatched-admission",
            "missing-prediction",
            "extra-prediction-field",
            "wrong-prediction-schema",
            "bad-digest",
            "bad-span",
            "duplicate-operation",
            "noncanonical-operation",
            "wrong-realizability-source",
        )
        for mode in modes:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temp_dir:
                temp = Path(temp_dir)
                fake_o = self.write_fake_o(temp)
                fake_olangc = self.write_fake_olangc(temp)
                marker = temp / "o-was-called"
                fake_node = temp / "node"
                fake_node.write_text("#!/bin/bash\nexit 0\n", encoding="utf-8")
                fake_node.chmod(0o755)
                backends = temp / "backends"
                backends.mkdir()
                env = os.environ.copy()
                env.update(
                    {
                        "PATH": f"{temp}{os.pathsep}{env.get('PATH', '')}",
                        "O_RELEASE_BIN": str(fake_o),
                        "OLANGC_RELEASE_BIN": str(fake_olangc),
                        "O_BACKENDS_DIR": str(backends),
                        "FAKE_OLANGC_MODE": mode,
                        "FAKE_O_MARKER": str(marker),
                    }
                )
                result = subprocess.run(
                    [
                        str(BASH),
                        str(RUNNER),
                        "--shape",
                        "heterogeneous",
                        "--warmups",
                        "0",
                        "--repetitions",
                        "1",
                        "--sleep",
                        "0",
                        "--missing-runtime",
                        "fail",
                    ],
                    cwd=PROJECT_ROOT,
                    env=env,
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=30,
                    check=False,
                )

                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertFalse(marker.exists(), "O ran after invalid analyzer output")
                self.assertRegex(
                    result.stderr,
                    r"schedule_analysis=failed|schedule_prediction=invalid",
                )

    def test_missing_node_is_reported_and_policy_controls_exit_status(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            temp = Path(temp_dir)
            tools = temp / "tools"
            tools.mkdir()
            for command in (
                "awk",
                "bash",
                "python3",
                "dirname",
                "mktemp",
                "rm",
                "uname",
                "date",
                "getconf",
                "mkdir",
            ):
                resolved = shutil.which(command)
                if resolved is not None:
                    (tools / command).symlink_to(resolved)

            fake_o = temp / "O"
            fake_o.write_text("#!/bin/bash\nexit 99\n", encoding="utf-8")
            fake_o.chmod(0o755)
            fake_olangc = self.write_fake_olangc(temp)
            backends = temp / "backends"
            backends.mkdir()
            temp_space = temp / "tmp"
            temp_space.mkdir()
            base_env = {
                "PATH": str(tools),
                "TMPDIR": str(temp_space),
                "O_RELEASE_BIN": str(fake_o),
                "OLANGC_RELEASE_BIN": str(fake_olangc),
                "O_BACKENDS_DIR": str(backends),
                "LANG": "C",
                "LC_ALL": "C",
            }

            results = {}
            for policy in ("skip", "fail"):
                results[policy] = subprocess.run(
                    [
                        str(BASH),
                        str(RUNNER),
                        "--shape",
                        "heterogeneous",
                        "--warmups",
                        "0",
                        "--repetitions",
                        "1",
                        "--sleep",
                        "0",
                        "--missing-runtime",
                        policy,
                    ],
                    cwd=PROJECT_ROOT,
                    env=base_env,
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=30,
                    check=False,
                )

        self.assertEqual(results["skip"].returncode, 0, results["skip"].stderr)
        self.assertEqual(results["fail"].returncode, 1, results["fail"].stderr)
        for result in results.values():
            self.assertIn("runtime_node=unavailable", result.stdout)
            self.assertIn("runtime_node_version=unavailable", result.stdout)
            self.assertIn("missing_runtimes=node", result.stdout)
            self.assertIn("prediction_source=olangc--explain-schedule-json", result.stdout)
            self.assertIn("predicted_width=3", result.stdout)
            self.assertIn("predicted_span=1", result.stdout)
            self.assertIn("status=skipped", result.stdout)
            self.assertIn("semantic_equivalence=not-measured", result.stdout)
            self.assertIn("expected_output_match=not-measured", result.stdout)
            self.assertIn("median_speedup_serial_over_graph=not-measured", result.stdout)


if __name__ == "__main__":
    unittest.main()
