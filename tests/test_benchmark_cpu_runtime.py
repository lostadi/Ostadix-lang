"""Behavior tests for the backend-free CPU benchmark harness."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[1]
RUNNER = ROOT / "benchmarks" / "cpu_runtime" / "run.py"


class CpuRuntimeBenchmarkTests(unittest.TestCase):
    maxDiff = None

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary.name)
        self.backends = self.directory / "backends"
        self.backends.mkdir()
        self.log = self.directory / "calls.jsonl"
        self.fake_o = self.directory / "O"
        self.fake_o.write_text(
            textwrap.dedent(
                f"""\
                #!{sys.executable}
                import json
                import os
                from pathlib import Path
                import re
                import sys
                import time

                args = sys.argv[1:]
                with open(os.environ["FAKE_O_LOG"], "a", encoding="utf-8") as handle:
                    handle.write(json.dumps(args) + "\\n")

                if args == ["version", "--json"]:
                    print(json.dumps({{
                        "schema": "ostadix.version/v1",
                        "package_version": "test",
                        "graph_executor_enabled": True,
                    }}))
                    raise SystemExit(0)

                def delay(name):
                    milliseconds = float(os.environ.get(name, "0"))
                    time.sleep(milliseconds / 1000)
                    return int(milliseconds)

                if "--check" in args:
                    program = Path(args[-2])
                    delay("FAKE_O_CHECK_MS")
                    print(json.dumps({{
                        "ok": True,
                        "stage": "parse",
                        "input": str(program),
                    }}))
                    raise SystemExit(0)

                executor = args[args.index("--executor") + 1]
                program = Path(args[-2])
                values = {{}}
                final = None
                binding = re.compile(r"^let ([A-Za-z0-9_]+) = text\\^\\((.*)\\)_text$")
                expression = re.compile(r"^text\\^\\((.*)\\)_text$")
                variable = re.compile(r"\\$([A-Za-z0-9_]+)")
                for line in program.read_text(encoding="utf-8").splitlines():
                    match = binding.match(line)
                    if match:
                        name, body = match.groups()
                        values[name] = variable.sub(lambda item: values[item.group(1)], body)
                        continue
                    match = expression.match(line)
                    if match:
                        final = variable.sub(lambda item: values[item.group(1)], match.group(1))
                if final is None:
                    raise SystemExit("missing final expression")
                if executor == "serial":
                    elapsed = delay("FAKE_O_SERIAL_MS")
                elif executor == "graph":
                    elapsed = delay("FAKE_O_GRAPH_MS")
                    if os.environ.get("FAKE_O_DIVERGE") == "graph":
                        final += "-diverged"
                else:
                    raise SystemExit("bad executor")
                print(json.dumps({{
                    "ok": True,
                    "type": "text",
                    "value": {{
                        "t": "text",
                        "v": {{"utf8": final, "encoding": "utf-8"}},
                    }},
                    "elapsed_ms": elapsed,
                }}))
                """
            ),
            encoding="utf-8",
        )
        self.fake_o.chmod(0o755)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def run_benchmark(
        self,
        *extra: str,
        repetitions: int = 1,
        warmups: int = 0,
        environment: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env["FAKE_O_LOG"] = str(self.log)
        if environment:
            env.update(environment)
        command = [
            sys.executable,
            str(RUNNER),
            "--o-bin",
            str(self.fake_o),
            "--backends-dir",
            str(self.backends),
            "--warmups",
            str(warmups),
            "--repetitions",
            str(repetitions),
            "--workers",
            "2",
            "--parse-bindings",
            "5",
            "--dag-width",
            "2",
            "--dag-depth",
            "2",
            "--payload-bytes",
            "8",
            "--timeout-seconds",
            "5",
            *extra,
        ]
        return subprocess.run(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
            timeout=30,
            check=False,
        )

    def test_emits_raw_provenance_and_semantic_evidence_without_a_gate(self) -> None:
        completed = self.run_benchmark(repetitions=2, warmups=1)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)

        self.assertEqual(result["schema"], "ostadix.cpu-benchmark/v1")
        self.assertEqual(result["gate"], {"enabled": False, "status": "not_requested"})
        self.assertTrue(result["semantics"]["serial_graph_equivalent"])
        self.assertTrue(result["semantics"]["generated_oracle_match"])
        self.assertEqual(result["semantics"]["pairs_checked"], 3)
        self.assertEqual(len(result["samples"]["warmup"]), 1)
        self.assertEqual(len(result["samples"]["measured"]), 2)
        self.assertEqual(
            result["samples"]["measured"][0]["order"],
            ["evaluator_graph", "evaluator_serial", "parser_check", "startup_check"],
        )
        self.assertEqual(
            result["samples"]["measured"][1]["order"],
            ["startup_check", "parser_check", "evaluator_serial", "evaluator_graph"],
        )
        for case in (
            "startup_check",
            "parser_check",
            "evaluator_serial",
            "evaluator_graph",
        ):
            raw = result["measurements"][case]["wall_time"]["raw"]
            self.assertEqual(len(raw), 2)
            self.assertTrue(all(type(value) is int and value > 0 for value in raw))
        self.assertEqual(
            result["provenance"]["runtime"]["sha256"],
            __import__("hashlib").sha256(self.fake_o.read_bytes()).hexdigest(),
        )
        self.assertRegex(result["comparison_key_sha256"], r"^[0-9a-f]{64}$")

        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn("--check", calls[0])
        self.assertTrue(any("serial" in call for call in calls))
        self.assertTrue(any("graph" in call for call in calls))
        self.assertEqual(calls[-1], ["version", "--json"])

    def test_semantic_divergence_fails_before_reporting_measurements(self) -> None:
        completed = self.run_benchmark(environment={"FAKE_O_DIVERGE": "graph"})
        self.assertEqual(completed.returncode, 2)
        self.assertEqual(completed.stdout, "")
        self.assertIn("serial/graph semantic mismatch", completed.stderr)

    def test_baseline_gate_detects_a_large_graph_regression(self) -> None:
        baseline_run = self.run_benchmark(repetitions=5)
        self.assertEqual(baseline_run.returncode, 0, baseline_run.stderr)
        baseline = self.directory / "baseline.json"
        baseline.write_text(baseline_run.stdout, encoding="utf-8")

        candidate = self.run_benchmark(
            "--baseline",
            str(baseline),
            "--max-regression-percent",
            "10",
            "--min-regression-ms",
            "20",
            repetitions=5,
            environment={"FAKE_O_GRAPH_MS": "150"},
        )
        self.assertEqual(candidate.returncode, 3, candidate.stderr)
        result = json.loads(candidate.stdout)
        self.assertEqual(result["gate"]["status"], "regressed")
        self.assertIn("evaluator_graph", result["gate"]["regressed_cases"])
        self.assertEqual(
            result["gate"]["cases"]["evaluator_graph"]["status"], "regressed"
        )
        self.assertIn("benchmark regression gate failed", candidate.stderr)

    def test_gate_rejects_a_different_workload_identity(self) -> None:
        baseline_run = self.run_benchmark(repetitions=5)
        self.assertEqual(baseline_run.returncode, 0, baseline_run.stderr)
        baseline = self.directory / "baseline.json"
        baseline.write_text(baseline_run.stdout, encoding="utf-8")

        candidate = self.run_benchmark(
            "--baseline",
            str(baseline),
            "--dag-depth",
            "3",
            repetitions=5,
        )
        self.assertEqual(candidate.returncode, 2, candidate.stderr)
        result = json.loads(candidate.stdout)
        self.assertEqual(result["gate"]["status"], "incompatible")
        self.assertIn("workload", result["gate"]["reason"])

    def test_gate_requires_enough_samples(self) -> None:
        completed = self.run_benchmark(
            "--baseline", str(self.directory / "unused.json"), repetitions=4
        )
        self.assertEqual(completed.returncode, 2)
        self.assertIn("requires at least 5 repetitions", completed.stderr)


if __name__ == "__main__":
    unittest.main()
