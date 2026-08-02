from __future__ import annotations

import copy
import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/world_alpha_evidence.py"
SPEC = importlib.util.spec_from_file_location("world_alpha_evidence", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
world_alpha_evidence = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(world_alpha_evidence)


class WorldAlphaEvidenceTests(unittest.TestCase):
    def manifest(self):
        return world_alpha_evidence.load_manifest()

    def test_checked_in_registry_defines_every_gate_without_claiming_passage(self):
        gates = world_alpha_evidence.validated_gates(self.manifest(), ROOT)
        self.assertEqual(
            [gate["id"] for gate in gates],
            [f"G{number}" for number in range(14)],
        )
        self.assertEqual({gate["status"] for gate in gates}, {"defined"})
        self.assertTrue(all(not gate["evidence"] for gate in gates))

    def test_hosted_reference_cannot_become_a_qualifying_class(self):
        manifest = copy.deepcopy(self.manifest())
        manifest["gate"][0]["required_classes"].append("hosted_reference")
        with self.assertRaisesRegex(
            world_alpha_evidence.WorldEvidenceError,
            "nonqualifying classes",
        ):
            world_alpha_evidence.validated_gates(manifest, ROOT)

    def test_physical_multinode_floor_cannot_be_replaced_by_virtual_evidence(self):
        manifest = copy.deepcopy(self.manifest())
        manifest["gate"][4]["required_classes"] = [
            "multinode_virtual",
            "security_adversarial",
        ]
        with self.assertRaisesRegex(
            world_alpha_evidence.WorldEvidenceError,
            "weakens qualification",
        ):
            world_alpha_evidence.validated_gates(manifest, ROOT)

    def test_gate_cannot_pass_without_evidence(self):
        manifest = copy.deepcopy(self.manifest())
        manifest["gate"][0]["status"] = "passed"
        with self.assertRaisesRegex(
            world_alpha_evidence.WorldEvidenceError,
            "definition-only and cannot certify passed gates",
        ):
            world_alpha_evidence.validated_gates(manifest, ROOT)

    def test_definition_only_schema_rejects_self_declared_evidence(self):
        manifest = copy.deepcopy(self.manifest())
        manifest["gate"][0]["evidence"] = [
            {"path": "evidence/world/empty.txt", "class": "repository_conformance"}
        ]
        with self.assertRaisesRegex(
            world_alpha_evidence.WorldEvidenceError,
            "schema v1 has no attestation format",
        ):
            world_alpha_evidence.validated_gates(manifest, ROOT)

    def test_dependency_graph_is_pinned(self):
        manifest = copy.deepcopy(self.manifest())
        manifest["gate"][12]["depends_on"] = ["G11"]
        with self.assertRaisesRegex(
            world_alpha_evidence.WorldEvidenceError,
            "depends_on must be",
        ):
            world_alpha_evidence.validated_gates(manifest, ROOT)

    def test_acceptance_and_prohibition_semantics_are_pinned(self):
        for field in ("acceptance", "prohibited_substitutes"):
            with self.subTest(field=field):
                manifest = copy.deepcopy(self.manifest())
                if field == "acceptance":
                    manifest["gate"][0][field] += " Weakened."
                else:
                    manifest["gate"][0][field].append("unreviewed substitute")
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError,
                    "registry semantics drifted",
                ):
                    world_alpha_evidence.validated_gates(manifest, ROOT)

    def test_windows_style_constitution_path_is_rejected(self):
        manifest = copy.deepcopy(self.manifest())
        manifest["constitution"] = r"docs\OSTADIX_WORLD.md"
        with self.assertRaisesRegex(
            world_alpha_evidence.WorldEvidenceError,
            "normalized repository-relative path",
        ):
            world_alpha_evidence.validated_gates(manifest, ROOT)

    def test_boolean_schema_version_is_not_accepted_as_integer_one(self):
        manifest = copy.deepcopy(self.manifest())
        manifest["schema_version"] = True
        with self.assertRaisesRegex(
            world_alpha_evidence.WorldEvidenceError,
            "schema_version must be",
        ):
            world_alpha_evidence.validated_gates(manifest, ROOT)

    def test_release_claim_guard_and_ci_run_the_registry_checks(self):
        claim_guard = (ROOT / "scripts/check_release_claims.sh").read_text()
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn(
            "python3 scripts/world_alpha_evidence.py --quiet",
            claim_guard,
        )
        self.assertIn(
            "python3 -m unittest -v tests.test_world_alpha_evidence",
            workflow,
        )


if __name__ == "__main__":
    unittest.main()
