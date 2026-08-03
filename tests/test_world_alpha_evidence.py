from __future__ import annotations

import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/world_alpha_evidence.py"
SPEC = importlib.util.spec_from_file_location("world_alpha_evidence", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
world_alpha_evidence = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(world_alpha_evidence)


class WorldAlphaEvidenceTests(unittest.TestCase):
    def manifest(self):
        return world_alpha_evidence.load_manifest()

    def test_checked_in_registry_passes_only_dependency_complete_g0_and_g2(self):
        gates = world_alpha_evidence.validated_gates(self.manifest(), ROOT)
        self.assertEqual(
            [gate["id"] for gate in gates],
            [f"G{number}" for number in range(14)],
        )
        statuses = {gate["id"]: gate["status"] for gate in gates}
        self.assertEqual(
            {gate_id for gate_id, status in statuses.items() if status == "passed"},
            {"G0", "G2"},
        )
        self.assertEqual(statuses["G13"], "defined")
        self.assertEqual(gates[0]["evidence"][0]["class"], "repository_conformance")
        self.assertEqual(gates[2]["evidence"][0]["class"], "qemu_tcg_aarch64")

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

    def test_status_is_derived_from_active_evidence(self):
        manifest = copy.deepcopy(self.manifest())
        with mock.patch.object(
            world_alpha_evidence, "_active_evidence_ledger", return_value=([], [])
        ):
            gates = world_alpha_evidence.validated_gates(manifest, ROOT)
        self.assertTrue(all(gate["status"] == "defined" for gate in gates))

    def test_registry_rejects_manually_supplied_status_or_evidence(self):
        for field, value in (("status", "passed"), ("evidence", [])):
            with self.subTest(field=field):
                manifest = copy.deepcopy(self.manifest())
                manifest["gate"][0][field] = value
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError,
                    "keys differ from schema",
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

    def test_claim_derivation_uses_typed_observations(self):
        transcript = "\n".join(
            (
                "@evidence event=lifecycle_terminal result=pass",
                "@evidence event=counter_progress phase=post_lifecycle poll_bound=1000000 result=pass",
            )
        )
        claims = world_alpha_evidence._derive_claims(transcript, "fixture")
        self.assertIn("counter.progress_after_lifecycle", claims)
        self.assertIn("execution.post_lifecycle_reached", claims)
        self.assertNotIn("timer.interrupt_delivery", claims)

    def test_supersession_event_is_a_separate_strict_record(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger = root / "evidence/world"
            ledger.mkdir(parents=True)
            path = ledger / "correction.toml"
            path.write_text(
                "\n".join(
                    (
                        "schema_version = 1",
                        'id = "correction-1"',
                        'event = "supersede"',
                        'subject = "old-attestation"',
                        'replacement = "new-attestation"',
                        'reason_code = "semantic-overclaim"',
                        'reason = "Counter progress was mislabeled as a timer."',
                        f'source_commit = "{"0" * 40}"',
                        "signatures = []",
                        "",
                    )
                ),
                encoding="utf-8",
            )
            event = world_alpha_evidence._validate_evidence_event(root, path)
            self.assertEqual(event["subject"], "old-attestation")
            self.assertEqual(event["replacement"], "new-attestation")

    def test_supersession_cycle_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger = root / "evidence/world"
            ledger.mkdir(parents=True)
            for name in ("a", "b"):
                (ledger / f"{name}.toml").write_text('gate = "G2"\n')
            for name in ("a-to-b", "b-to-a"):
                (ledger / f"{name}.toml").write_text('event = "supersede"\n')

            attestations = {
                "evidence/world/a.toml": {
                    "id": "a",
                    "path": "evidence/world/a.toml",
                    "gate": "G2",
                    "class": "qemu_tcg_aarch64",
                    "derived_claims": set(),
                    "schema_version": 1,
                },
                "evidence/world/b.toml": {
                    "id": "b",
                    "path": "evidence/world/b.toml",
                    "gate": "G2",
                    "class": "qemu_tcg_aarch64",
                    "derived_claims": set(),
                    "schema_version": 1,
                },
            }
            events = {
                "a-to-b.toml": {
                    "id": "event-a-to-b",
                    "path": "evidence/world/a-to-b.toml",
                    "event": "supersede",
                    "subject": "a",
                    "replacement": "b",
                },
                "b-to-a.toml": {
                    "id": "event-b-to-a",
                    "path": "evidence/world/b-to-a.toml",
                    "event": "supersede",
                    "subject": "b",
                    "replacement": "a",
                },
            }

            def fake_attestation(_root, path, *_args):
                return attestations[path]

            def fake_event(_root, path):
                return events[path.name]

            with mock.patch.object(
                world_alpha_evidence,
                "_validate_attestation",
                side_effect=fake_attestation,
            ), mock.patch.object(
                world_alpha_evidence,
                "_validate_evidence_event",
                side_effect=fake_event,
            ):
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError, "contains a cycle"
                ):
                    world_alpha_evidence._active_evidence_ledger(
                        root, {"qemu_tcg_aarch64"}, "0" * 64
                    )

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
