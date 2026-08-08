"""Deterministic contract tests for the hosted HGraph benchmark suite."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
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
            fake_o = temp / "O"
            fake_o.write_text(
                textwrap.dedent(
                    """\
                    #!/bin/bash
                    set -eu
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

        combined = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, combined)
        self.assertIn("runtime_python3_version=", result.stdout)
        self.assertIn("runtime_bash_version=", result.stdout)
        self.assertIn("runtime_node_version=", result.stdout)
        for shape, (width, span) in SHAPES.items():
            marker = f"shape={shape}.O\n"
            self.assertIn(marker, result.stdout)
            block = result.stdout.split(marker, 1)[1].split("\nshape=", 1)[0]
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
            ):
                resolved = shutil.which(command)
                if resolved is not None:
                    (tools / command).symlink_to(resolved)

            fake_o = temp / "O"
            fake_o.write_text("#!/bin/bash\nexit 99\n", encoding="utf-8")
            fake_o.chmod(0o755)
            backends = temp / "backends"
            backends.mkdir()
            temp_space = temp / "tmp"
            temp_space.mkdir()
            base_env = {
                "PATH": str(tools),
                "TMPDIR": str(temp_space),
                "O_RELEASE_BIN": str(fake_o),
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
            self.assertIn("status=skipped", result.stdout)
            self.assertIn("semantic_equivalence=not-measured", result.stdout)
            self.assertIn("expected_output_match=not-measured", result.stdout)
            self.assertIn("median_speedup_serial_over_graph=not-measured", result.stdout)


if __name__ == "__main__":
    unittest.main()
