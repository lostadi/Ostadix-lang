"""Deterministic contract tests for the hosted HGraph benchmark suite."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest


PROJECT_ROOT = Path(__file__).resolve().parents[1]
RUNNER = PROJECT_ROOT / "scripts" / "benchmark_hgraph_hosted.sh"
FIXTURES = PROJECT_ROOT / "benchmarks" / "hgraph_hosted"
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
                import os
                from pathlib import Path
                import sys

                args = sys.argv[1:]
                if not args:
                    raise SystemExit(64)
                program = Path(args[0])
                required = ["--target", "ir", "--explain-schedule", "--workers", "--shim-dir"]
                for item in required:
                    if item not in args:
                        raise SystemExit(f"missing analyzer argument: {{item}}")
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
                header = "; SchedulePrediction oexec.schedule-prediction/v1"
                if mode != "missing-header":
                    print(header)
                if mode == "duplicate-header":
                    print(header)
                if mode == "bad-digest":
                    digest = "not-a-digest"
                if mode == "bad-span":
                    span += 1
                admission_digest = digest
                if mode == "mismatched-admission":
                    admission_digest = hashlib.sha256(b"different-admission").hexdigest()
                admission_header = "; ExecutionAdmission oexec.admission/v3"
                if mode == "wrong-admission-schema":
                    admission_header = "; ExecutionAdmission oexec.admission/v999"
                if mode != "missing-admission":
                    print(admission_header)
                if mode == "duplicate-admission":
                    print(admission_header)
                binding = (
                    f"binding analyzer-sha256={{hashlib.sha256(b'analyzer').hexdigest()}} "
                    f"evidence-sha256={{hashlib.sha256(b'evidence').hexdigest()}} "
                    f"admitted-graph-sha256={{hashlib.sha256(b'graph').hexdigest()}} "
                    f"admission-sha256={{admission_digest}}"
                )
                if mode == "malformed-admission-binding":
                    binding += " unexpected=true"
                if mode != "missing-admission-binding":
                    print(binding)
                if mode == "duplicate-admission-binding":
                    print(binding)
                print(
                    "schedule-prediction "
                    "schema=oexec.schedule-prediction/v1 "
                    "status=admitted-static "
                    "provenance=evidence-bound-admission "
                    "model=unit-cost-shim-hosted-tasks "
                    f"admission-sha256={{digest}} task-count={{task_count}} "
                    f"predicted-width={{width}} predicted-span={{span}} "
                    "span-unit=hosted-task-layers"
                )
                for index, operations in enumerate(layers, 1):
                    if mode == "duplicate-operation" and index == len(layers):
                        operations = [layers[0][0]]
                    labels = ",".join(f"P{{operation}}" for operation in operations)
                    print(f"schedule-prediction-layer index={{index}} operations=[{{labels}}]")
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
            self.assertIn("prediction_source=olangc--explain-schedule\n", block)
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
                f"{shape}.O --target ir --explain-schedule --workers 4 --shim-dir",
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
            "missing-header",
            "duplicate-header",
            "missing-admission",
            "duplicate-admission",
            "wrong-admission-schema",
            "missing-admission-binding",
            "duplicate-admission-binding",
            "malformed-admission-binding",
            "mismatched-admission",
            "bad-digest",
            "bad-span",
            "duplicate-operation",
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
            self.assertIn("prediction_source=olangc--explain-schedule", result.stdout)
            self.assertIn("predicted_width=3", result.stdout)
            self.assertIn("predicted_span=1", result.stdout)
            self.assertIn("status=skipped", result.stdout)
            self.assertIn("semantic_equivalence=not-measured", result.stdout)
            self.assertIn("expected_output_match=not-measured", result.stdout)
            self.assertIn("median_speedup_serial_over_graph=not-measured", result.stdout)


if __name__ == "__main__":
    unittest.main()
