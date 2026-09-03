"""Fast contract tests for the real-world autonomous benchmark suite."""

from __future__ import annotations

import ast
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[1]
RUNNER = ROOT / "scripts" / "benchmark_real_world.sh"
FIXTURES = ROOT / "benchmarks" / "real_world"
BACKENDS = ROOT / "backends"
README = FIXTURES / "README.md"

WORKLOADS = {
    "asset_pipeline": {"tasks": 3, "width": 3, "span": 1},
    "ci_shards": {"tasks": 3, "width": 3, "span": 1},
    "video_previews": {"tasks": 9, "width": 9, "span": 1},
}

ASSET_INPUTS = {
    "Olang_Mascot_little-o/little-o/references/reference-01.png",
    "Olang_Mascot_little-o/little-o/references/canonical-base.png",
    "assets/olang-logo.png",
}

CI_SHARDS = {
    "test_o_cli_dispatch.py": "o-cli.log",
    "test_setup.py": "setup.log",
    "test_ostadix_boot_iso.py": "boot-iso.log",
}

VIDEO_NAMES = {
    "failed",
    "idle",
    "jumping",
    "review",
    "running-left",
    "running-right",
    "running",
    "waiting",
    "waving",
}


def executable_from_environment(name: str, fallback: Path) -> Path | None:
    candidate = Path(os.environ.get(name, fallback))
    return candidate if candidate.is_file() and os.access(candidate, os.X_OK) else None


class RealWorldBenchmarkContractTests(unittest.TestCase):
    maxDiff = None

    def test_shell_and_python_sources_have_valid_syntax(self) -> None:
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("bash is unavailable")

        shell_sources = (RUNNER, FIXTURES / "transcode_preview.sh")
        for source in shell_sources:
            with self.subTest(source=source.name):
                completed = subprocess.run(
                    [bash, "-n", str(source)],
                    cwd=ROOT,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    timeout=10,
                    check=False,
                )
                self.assertEqual(
                    completed.returncode,
                    0,
                    f"{source} failed bash -n:\n{completed.stderr}",
                )

        timer = FIXTURES / "timed_exec.py"
        ast.parse(timer.read_text(encoding="utf-8"), filename=str(timer))

    def test_help_lists_the_exact_public_workload_inventory(self) -> None:
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("bash is unavailable")

        completed = subprocess.run(
            [bash, str(RUNNER), "--help"],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stderr, "")

        section = re.search(
            r"(?ms)^Workloads:\n(?P<body>.*?)^Options:\n", completed.stdout
        )
        self.assertIsNotNone(section, completed.stdout)
        names = set(
            re.findall(r"(?m)^  ([a-z][a-z0-9_]*)\s{2,}", section.group("body"))
        )
        self.assertEqual(names, set(WORKLOADS))
        self.assertIn(
            "all, asset_pipeline, ci_shards, or video_previews", completed.stdout
        )
        self.assertIn("descriptive", completed.stdout)
        self.assertIn("never enforces a speedup threshold", completed.stdout)

    def test_unknown_workload_is_rejected_before_any_benchmark_work(self) -> None:
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("bash is unavailable")

        completed = subprocess.run(
            [bash, str(RUNNER), "--workload", "not-a-workload"],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(completed.returncode, 2)
        self.assertEqual(completed.stdout, "")
        self.assertIn("unknown workload: not-a-workload", completed.stderr)

    def test_fixture_source_inventory_and_autonomous_shapes(self) -> None:
        programs = {path.stem for path in FIXTURES.glob("*.O")}
        helpers = {
            path.name
            for path in FIXTURES.iterdir()
            if path.suffix in {".py", ".sh"}
        }
        self.assertEqual(programs, set(WORKLOADS))
        self.assertEqual(helpers, {"timed_exec.py", "transcode_preview.sh"})

        for workload, expected in WORKLOADS.items():
            source = (FIXTURES / f"{workload}.O").read_text(encoding="utf-8")
            with self.subTest(workload=workload):
                self.assertEqual(source.count("autonomous(batch("), 1)
                self.assertEqual(source.count("bash^("), expected["tasks"])
                self.assertEqual(source.count(")_bash"), expected["tasks"])
                # O parses only the unbraced $IDENT form as an O-level load.
                # Shell regex anchors such as ^OK$ are ordinary raw text.
                self.assertNotRegex(source, r"(?<!\\)\$[A-Za-z_]")
                self.assertNotRegex(source, r"(?m)^\s*sleep(?:\s|$)")
                self.assertNotIn("bash[", source)

    def test_asset_pipeline_uses_real_inputs_and_owns_three_lanes(self) -> None:
        source = (FIXTURES / "asset_pipeline.O").read_text(encoding="utf-8")
        for relative in ASSET_INPUTS:
            with self.subTest(input=relative):
                self.assertTrue((ROOT / relative).is_file(), relative)
                self.assertEqual(source.count(relative), 2)

        self.assertEqual(source.count("for size in 1536 1024 640; do"), 3)
        self.assertEqual(source.count("export MAGICK_THREAD_LIMIT=1"), 3)
        self.assertEqual(source.count(".avif\""), 3)
        self.assertEqual(source.count(".webp\""), 3)
        for lane in ("reference", "canonical", "logo"):
            self.assertIn(f'lane="\\${{O_ASSET_OUT}}/{lane}"', source)

    def test_ci_shards_reference_three_real_test_modules(self) -> None:
        source = (FIXTURES / "ci_shards.O").read_text(encoding="utf-8")
        self.assertEqual(source.count("python3 -m unittest discover"), 3)
        self.assertEqual(source.count("status=pass"), 3)
        for module, log in CI_SHARDS.items():
            with self.subTest(module=module):
                self.assertTrue((ROOT / "tests" / module).is_file(), module)
                self.assertEqual(source.count(module), 1)
                self.assertEqual(source.count(log), 1)

    def test_video_pipeline_maps_nine_real_gifs_to_nine_webms(self) -> None:
        source = (FIXTURES / "video_previews.O").read_text(encoding="utf-8")
        calls = re.findall(
            r"transcode_preview\.sh\s+([^\s]+\.gif)\s+"
            r'"\\\$\{O_VIDEO_OUT\}/([^"/]+)\.webm"\s+([^\s]+)',
            source,
        )
        self.assertEqual(len(calls), len(VIDEO_NAMES), source)
        self.assertEqual({Path(path).stem for path, _, _ in calls}, VIDEO_NAMES)
        self.assertEqual({output for _, output, _ in calls}, VIDEO_NAMES)
        self.assertEqual({label for _, _, label in calls}, VIDEO_NAMES)
        for path, _, _ in calls:
            self.assertTrue((ROOT / path).is_file(), path)

        wrapper = FIXTURES / "transcode_preview.sh"
        self.assertTrue(os.access(wrapper, os.X_OK), f"not executable: {wrapper}")
        wrapper_source = wrapper.read_text(encoding="utf-8")
        for required in (
            "libvpx-vp9",
            "scale=768:832",
            "-threads 1",
            "-row-mt 0",
            "-map_metadata -1",
        ):
            self.assertIn(required, wrapper_source)

    def test_runner_preserves_pairing_correctness_and_plan_validation(self) -> None:
        source = RUNNER.read_text(encoding="utf-8")
        for required in (
            "--executor \"$executor\" --workers \"$workers\"",
            "--explain-schedule --format json",
            "prediction is not bound to the enclosing admission",
            'cmp -s "$serial_semantic" "$graph_semantic"',
            'cmp -s "$serial_manifest" "$graph_manifest"',
            "semantic_equivalence=true",
            "artifact_equivalence=true",
            "paired_geometric_mean_speedup",
            "effective_unit_cost_reference",
            "timed_exec=$fixture_dir/timed_exec.py",
            '"$timed_exec" "${timing_arguments[@]}" --',
            'payload.get("schema") != "ostadix.timed-exec/v1"',
            'elapsed = payload.get("wall_time_ns")',
            "timing_boundary=complete_O_child_process_perf_counter_ns",
            "serial_internal_elapsed_ms",
            "graph_internal_elapsed_ms",
        ):
            self.assertIn(required, source)
        self.assertNotIn("effective_ceiling_captured_percent", source)
        self.assertRegex(source, r"ordinal % 2")
        self.assertIn("first=serial", source)
        self.assertIn("first=graph", source)
        self.assertIn("refusing unsafe evidence directory", source)
        self.assertIn("evidence directory must be empty", source)

    def test_documentation_keeps_measurement_and_nonclaim_boundaries_explicit(
        self,
    ) -> None:
        documentation = README.read_text(encoding="utf-8")
        normalized = " ".join(documentation.split())
        for workload, expected in WORKLOADS.items():
            with self.subTest(workload=workload):
                self.assertIn(f"`{workload}`", documentation)
                self.assertIn(
                    f"{expected['tasks']} tasks, width {expected['width']}, "
                    f"span {expected['span']}",
                    normalized,
                )

        for required in (
            "contains no artificial sleeps",
            "not a claim that every program becomes faster",
            "disjoint output subtree or filename",
            "two independent equivalence checks",
            "whole-process wall time",
            "O's own `elapsed_ms` is retained as a diagnostic",
            "odd pairs run serial then graph",
            "paired geometric-mean wall-time speedup",
            "not a speedup ceiling",
            "does not report a percentage",
            "cannot establish a universal speedup",
            "termux-open",
            "ffplay",
            "sample-1-graph-artifacts/idle.webm",
        ):
            self.assertIn(required, normalized)
        self.assertIn("18-file inventory", normalized)
        self.assertIn("nine 768×832, 30-fps VP9 WebM previews", normalized)

    def test_release_o_parser_accepts_every_program_when_available(self) -> None:
        o_bin = executable_from_environment(
            "O_RELEASE_BIN", ROOT / "target" / "release" / "O"
        )
        if o_bin is None:
            self.skipTest("release O executable is unavailable")

        for workload in WORKLOADS:
            program = FIXTURES / f"{workload}.O"
            with self.subTest(workload=workload):
                completed = subprocess.run(
                    [str(o_bin), "--check", "--json", str(program), str(BACKENDS)],
                    cwd=ROOT,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    timeout=20,
                    check=False,
                )
                self.assertEqual(
                    completed.returncode,
                    0,
                    completed.stdout + completed.stderr,
                )
                payload = json.loads(completed.stdout)
                self.assertIs(payload.get("ok"), True)
                self.assertEqual(payload.get("stage"), "parse")

    def test_release_analyzer_reports_expected_topology_when_available(self) -> None:
        olangc = executable_from_environment(
            "OLANGC_RELEASE_BIN", ROOT / "target" / "release" / "olangc"
        )
        if olangc is None:
            self.skipTest("release olangc executable is unavailable")

        for workload, expected in WORKLOADS.items():
            program = FIXTURES / f"{workload}.O"
            with self.subTest(workload=workload):
                completed = subprocess.run(
                    [
                        str(olangc),
                        str(program),
                        "--target",
                        "ir",
                        "--explain-schedule",
                        "--format",
                        "json",
                        "--workers",
                        "16",
                        "--shim-dir",
                        str(BACKENDS),
                    ],
                    cwd=ROOT,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    timeout=20,
                    check=False,
                )
                self.assertEqual(
                    completed.returncode,
                    0,
                    completed.stdout + completed.stderr,
                )
                report = json.loads(completed.stdout)
                self.assertEqual(report.get("schema"), "oexec.schedule-explanation/v2")
                prediction = report.get("prediction", {})
                self.assertEqual(
                    prediction.get("schema"), "oexec.schedule-prediction/v1"
                )
                self.assertEqual(prediction.get("task_count"), expected["tasks"])
                self.assertEqual(prediction.get("predicted_width"), expected["width"])
                self.assertEqual(prediction.get("predicted_span"), expected["span"])
                self.assertEqual(
                    prediction.get("admission_sha256"),
                    report.get("admission", {})
                    .get("bindings", {})
                    .get("admission_sha256"),
                )


if __name__ == "__main__":
    unittest.main()
