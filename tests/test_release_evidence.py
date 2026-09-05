"""Focused regressions for the manifest-driven native evidence registry."""

from __future__ import annotations

import copy
import importlib.util
import re
import subprocess
import tempfile
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/release_evidence.py"
SPEC = importlib.util.spec_from_file_location("release_evidence", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
release_evidence = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = release_evidence
SPEC.loader.exec_module(release_evidence)


class G2Aarch64EvidenceTests(unittest.TestCase):
    def manifest(self):
        return release_evidence.load_manifest()

    def g2(self, manifest):
        return next(
            gate
            for gate in manifest["gate"]
            if gate["id"] == release_evidence.G2_AARCH64_GATE_ID
        )

    def test_checked_in_manifest_has_one_required_bounded_g2_gate(self):
        manifest = self.manifest()
        self.assertEqual(manifest["schema_version"], 2)
        gates = release_evidence.validated_gates(manifest)
        matches = [
            gate
            for gate in gates
            if gate["id"] == release_evidence.G2_AARCH64_GATE_ID
        ]
        self.assertEqual(len(matches), 1)
        gate = matches[0]
        self.assertTrue(gate["required"])
        self.assertEqual(gate["evidence_class"], "qemu_tcg_aarch64")
        self.assertEqual(gate["script"], release_evidence.G2_AARCH64_SCRIPT)
        self.assertIn("qemu-system-aarch64", gate["required_tools"])
        self.assertTrue(
            release_evidence.G2_AARCH64_REQUIRED_TOOLS
            <= set(gate["required_tools"])
        )

    def test_missing_aarch64_gate_cannot_leave_portable_aggregate_green(self):
        manifest = copy.deepcopy(self.manifest())
        gate = self.g2(manifest)
        gate["id"] = "renamed-nonqualifying-gate"
        gate["evidence_class"] = "portable_tcg"
        gate["required_tools"].append("qemu-system-x86_64")
        with self.assertRaisesRegex(
            release_evidence.EvidenceError,
            "exactly one qemu_tcg_aarch64 gate",
        ):
            release_evidence.validated_gates(manifest)

    def test_missing_aarch64_qemu_tool_is_rejected(self):
        manifest = copy.deepcopy(self.manifest())
        self.g2(manifest)["required_tools"].remove("qemu-system-aarch64")
        with self.assertRaisesRegex(
            release_evidence.EvidenceError,
            "qemu-system-aarch64.*qemu_tcg_aarch64",
        ):
            release_evidence.validated_gates(manifest)

    def test_missing_attestation_or_rebuild_tool_is_rejected(self):
        for tool in ("cmp", "git", "shasum"):
            with self.subTest(tool=tool):
                manifest = copy.deepcopy(self.manifest())
                self.g2(manifest)["required_tools"].remove(tool)
                with self.assertRaisesRegex(
                    release_evidence.EvidenceError,
                    "world-g2-aarch64-native required_tools is missing",
                ):
                    release_evidence.validated_gates(manifest)

    def test_false_physical_hardware_claim_is_rejected(self):
        manifest = copy.deepcopy(self.manifest())
        self.g2(manifest)["positive_claims"] = [
            "Physical AArch64 and SMMU isolation are proven"
        ]
        with self.assertRaisesRegex(
            release_evidence.EvidenceError,
            "positive claims differ from the bounded AArch64 QEMU/TCG contract",
        ):
            release_evidence.validated_gates(manifest)

    def test_linux_or_plan9_boot_claim_is_rejected(self):
        manifest = copy.deepcopy(self.manifest())
        self.g2(manifest)["nonclaims"][1] = (
            "This gate boots Linux and Plan 9 under a general foreign ABI"
        )
        with self.assertRaisesRegex(
            release_evidence.EvidenceError,
            "nonclaims must preserve.*foreign-OS",
        ):
            release_evidence.validated_gates(manifest)

    def test_missing_gate_script_fails_validation(self):
        gate = {
            "script": "ocore/kernel/definitely-absent-g2-gate.sh",
        }
        with self.assertRaisesRegex(
            release_evidence.EvidenceError,
            "required gate script is missing",
        ):
            release_evidence.validate_gate_scripts([gate])

    def test_g2_transcript_requires_every_marker_exactly_once(self):
        gate = {
            "id": release_evidence.G2_AARCH64_GATE_ID,
            "script": release_evidence.G2_AARCH64_SCRIPT,
            "expected_markers": list(release_evidence.G2_AARCH64_EXPECTED_MARKERS),
        }
        transcript = "\n".join(gate["expected_markers"]).encode("utf-8")
        gate_id, marker_count = release_evidence.verify_transcript(
            [gate], gate["script"], transcript
        )
        self.assertEqual(gate_id, release_evidence.G2_AARCH64_GATE_ID)
        self.assertEqual(marker_count, len(release_evidence.G2_AARCH64_EXPECTED_MARKERS))

        duplicated = transcript + b"\n" + gate["expected_markers"][-1].encode()
        with self.assertRaisesRegex(
            release_evidence.EvidenceError,
            "marker counts must each equal 1",
        ):
            release_evidence.verify_transcript([gate], gate["script"], duplicated)

    def test_ci_installs_and_runs_the_manifest_bound_aarch64_gate(self):
        manifest = self.manifest()
        gates = release_evidence.validated_gates(manifest)
        gate = self.g2(manifest)
        self.assertIn(gate, gates)
        release_evidence.validate(gates)

        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        self.assertIn("qemu-system-arm", workflow)


class AggregateOwnershipTests(unittest.TestCase):
    def test_ocore_reproducibility_gate_targets_the_engine_and_runs_one_test(self):
        root_source = (ROOT / "boot-and-test.sh").read_text(encoding="utf-8")
        mirror_source = (ROOT / "okernel-multikernel/boot-and-test.sh").read_text(
            encoding="utf-8"
        )
        self.assertEqual(root_source, mirror_source)
        self.assertNotIn("--package o-lang --lib", root_source)
        self.assertEqual(root_source.count("--package ostadix-api --lib"), 1)
        self.assertEqual(
            root_source.count(
                "ocore::driver::tests::"
                "ocore_object_is_byte_reproducible_across_source_directories"
            ),
            1,
        )
        self.assertIn("test result: ok. 1 passed; 0 failed;", root_source)


class ProjectExecutionClaimGuardTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        script = (ROOT / "scripts/check_release_claims.sh").read_text(encoding="utf-8")
        function = "require_fixed() {" + script.split("require_fixed() {", 1)[1].split("\n}\n", 1)[0] + "\n}\n"
        block = script.split("# PROJECT_EXECUTION_CLAIMS_BEGIN\n", 1)[1].split("# PROJECT_EXECUTION_CLAIMS_END", 1)[0]
        cls.guard = "set -eu\nfail=0\n" + function + block + '\nexit "$fail"\n'
        cls.requirements = re.findall(r"require_fixed (\S+) \\\n    '([^']+)' \\\n    '([^']+)'", block)
        if len(cls.requirements) < 27:
            raise AssertionError("current execution/history guard lost protected contracts")
        if len(cls.requirements) != block.count("require_fixed "):
            raise AssertionError("a current project guard requirement escaped regression coverage")
        cls.surfaces = {file: (ROOT / file).read_text(encoding="utf-8") for file, _, _ in cls.requirements}

    def run_guard(self, surfaces):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for file, contents in surfaces.items():
                path = root / file
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(contents, encoding="utf-8")
            return subprocess.run(["bash", "-c", self.guard], cwd=root,
                text=True, capture_output=True, timeout=15)

    def test_current_execution_and_history_contracts_satisfy_guard(self):
        result = self.run_guard(self.surfaces)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_each_contract_removal_fails_the_production_guard(self):
        for file, required, reason in self.requirements:
            with self.subTest(file=file, contract=required):
                self.assertIn(required, self.surfaces[file])
                changed = dict(self.surfaces)
                changed[file] = changed[file].replace(required, "contract omitted for regression test")
                result = self.run_guard(changed)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(reason, result.stdout)


if __name__ == "__main__":
    unittest.main()
