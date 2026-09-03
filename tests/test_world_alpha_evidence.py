from __future__ import annotations

import copy
import hashlib
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import tomllib
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

    def test_sealed_engine_source_coordinates_resolve_without_rewriting(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            engine_source = root / "crates/ostadix-api/src/world/identity.rs"
            engine_source.parent.mkdir(parents=True)
            engine_source.write_text("pub struct WorldIdentity;\n", encoding="utf-8")

            recorded, resolved = world_alpha_evidence._repo_file(
                root, "src/world/identity.rs", "sealed source"
            )
            self.assertEqual(recorded, "src/world/identity.rs")
            self.assertEqual(resolved, engine_source.resolve())

            shell_main = root / "src/main.rs"
            shell_main.parent.mkdir(parents=True)
            shell_main.write_text("fn main() {}\n", encoding="utf-8")
            _, resolved_main = world_alpha_evidence._repo_file(
                root, "src/main.rs", "shell source"
            )
            self.assertEqual(resolved_main, shell_main.resolve())

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
        claims = world_alpha_evidence._derive_claims(
            transcript,
            "fixture",
            {
                "evidence_class": "qemu_tcg_aarch64",
                "topology": {
                    "kind": "virtual",
                    "architecture": "aarch64",
                    "acceleration": "tcg",
                    "cpu_count": 1,
                },
                "source_paths": set(),
                "artifact_names": {"g2-kernel-elf", "g2-kernel-object"},
                "artifact_kinds": set(),
                "artifact_name_kinds": {},
                "artifact_bindings": set(),
            },
        )
        self.assertIn("counter.progress_after_lifecycle", claims)
        self.assertIn("execution.post_lifecycle_reached", claims)
        self.assertNotIn("timer.interrupt_delivery", claims)

    def test_derivation_hash_binds_artifact_context_normalization(self):
        source = MODULE_PATH.read_bytes()
        needle = b'artifact["path"] if artifact["retained"] else ""'
        self.assertEqual(source.count(needle), 1)
        mutated = source.replace(needle, b'""', 1)
        current_implementation = hashlib.sha256(
            world_alpha_evidence._derivation_implementation_bytes(source)
        ).hexdigest()
        mutated_implementation = hashlib.sha256(
            world_alpha_evidence._derivation_implementation_bytes(mutated)
        ).hexdigest()
        self.assertNotEqual(mutated_implementation, current_implementation)
        mutated_spec = copy.deepcopy(world_alpha_evidence.DERIVATION_SPEC)
        mutated_spec["implementation_source_sha256"] = mutated_implementation
        mutated_derivation = "sha256:" + hashlib.sha256(
            world_alpha_evidence._canonical_json_bytes(mutated_spec)
        ).hexdigest()
        self.assertNotEqual(
            mutated_derivation, world_alpha_evidence.CURRENT_DERIVATION_HASH
        )

    def test_observation_values_are_canonical_printable_ascii(self):
        with self.assertRaisesRegex(
            world_alpha_evidence.WorldEvidenceError,
            "invalid/duplicate evidence field",
        ):
            world_alpha_evidence._parse_observations(
                "@evidence event=résultat result=pass", "fixture"
            )

    def test_g7_compound_claim_requires_one_context_bound_lifecycle(self):
        transcript = (
            "@evidence event=g7_class_withdrawal_lifecycle "
            "resource_class=MachineBlock async=pending_query_complete "
            "host_ack=complete guest_error=consumed_guest_healthy "
            "join_before_memory=host_ack_and_guest_error "
            "memory_order=stop_quiesce_unmap_guest_host_tlbi_drain_generation_ack "
            "reclaim=after_memory_ack guest_authority_hvc=absent result=pass"
        )
        context = {
            "evidence_class": "qemu_virtualization",
            "topology": {"kind": "virtual"},
            "source_paths": {
                "docs/O_MACHINE_CONTRACT.md",
                "evidence/o_machine_contract_v1.toml",
            },
            "artifact_names": set(),
            "artifact_kinds": {
                "foreign-kernel-image",
                "guest-native-error-trace",
                "machine-withdrawal-trace",
            },
            "artifact_name_kinds": {},
            "artifact_bindings": set(),
        }
        claim = "kernel_world.g7_class_withdrawal_lifecycle"
        self.assertIn(
            claim,
            world_alpha_evidence._derive_claims(transcript, "fixture", context),
        )
        for field, replacement in (
            ("evidence_class", "fault_injection"),
            ("topology", {"kind": "physical"}),
            (
                "artifact_kinds",
                {"foreign-kernel-image", "guest-native-error-trace"},
            ),
        ):
            with self.subTest(field=field):
                weakened = copy.deepcopy(context)
                weakened[field] = replacement
                self.assertNotIn(
                    claim,
                    world_alpha_evidence._derive_claims(
                        transcript, "fixture", weakened
                    ),
                )
        self.assertNotIn(
            claim,
            world_alpha_evidence._derive_claims(
                transcript.replace(" host_ack=complete", ""),
                "fixture",
                context,
            ),
        )
        self.assertNotIn(
            claim,
            world_alpha_evidence._derive_claims(
                transcript.replace(
                    "stop_quiesce_unmap_guest_host_tlbi_drain_generation_ack",
                    "stop_unmap_tlbi_drain_generation_ack",
                ),
                "fixture",
                context,
            ),
        )

    def test_g8_compound_claim_requires_physical_artifact_context(self):
        transcript = (
            "@evidence event=g8_physical_withdrawal_lifecycle "
            "device_class=nvme withdraw_operation=begin_withdraw_nvme "
            "order=quiesce_dma_iommu_irq_reset_generation_ack_replacement "
            "reset_verification=class_specific_pass "
            "reset_failure=quarantine_no_ack_no_replacement "
            "shared_group=dedicated_or_all_affected_quiesced_survival_proven "
            "unrelated_world=healthy guest_machine_abi=none "
            "handle_mac=not_required key_lifecycle=not_applicable result=pass"
        )
        context = {
            "evidence_class": "hardware_aarch64_smmu",
            "topology": {"kind": "physical"},
            "source_paths": {
                "docs/O_MACHINE_CONTRACT.md",
                "evidence/o_machine_contract_v1.toml",
            },
            "artifact_names": {"device-class-nvme", "nvme-withdrawal-trace"},
            "artifact_kinds": {
                "physical-device-inventory",
                "dma-iommu-withdrawal-trace",
                "interrupt-reset-trace",
                "unrelated-world-survival-trace",
            },
            "artifact_name_kinds": {
                "device-class-nvme": "physical-device-inventory",
                "nvme-withdrawal-trace": "dma-iommu-withdrawal-trace",
            },
            "artifact_bindings": set(),
        }
        claim = "driver.g8_physical_withdrawal_lifecycle"
        self.assertIn(
            claim,
            world_alpha_evidence._derive_claims(transcript, "fixture", context),
        )
        context["artifact_kinds"].remove("physical-device-inventory")
        self.assertNotIn(
            claim,
            world_alpha_evidence._derive_claims(transcript, "fixture", context),
        )
        symbolic = (
            "@evidence event=g8_physical_withdrawal_lifecycle "
            "class_contract=concrete_named "
            "order=quiesce_dma_iommu_irq_reset_generation_ack_replacement "
            "unrelated_world=healthy guest_interface_policy=decided result=pass"
        )
        self.assertNotIn(
            claim,
            world_alpha_evidence._derive_claims(symbolic, "fixture", context),
        )
        context["artifact_kinds"].add("physical-device-inventory")
        for field in ("reset_verification", "reset_failure", "shared_group"):
            with self.subTest(field=field):
                weakened = " ".join(
                    token
                    for token in transcript.split(" ")
                    if not token.startswith(f"{field}=")
                )
                self.assertNotIn(
                    claim,
                    world_alpha_evidence._derive_claims(
                        weakened, "fixture", context
                    ),
                )
        direct_context = copy.deepcopy(context)
        direct_context["artifact_kinds"].add("handle-mac-key-lifecycle-trace")
        direct = transcript.replace(
            "guest_machine_abi=none handle_mac=not_required key_lifecycle=not_applicable",
            "guest_machine_abi=direct handle_mac=verified key_lifecycle=verified",
        )
        self.assertIn(
            claim,
            world_alpha_evidence._derive_claims(
                direct, "direct-abi-fixture", direct_context
            ),
        )
        self.assertNotIn(
            claim,
            world_alpha_evidence._derive_claims(
                direct.replace("handle_mac=verified", "handle_mac=absent"),
                "direct-abi-fixture",
                direct_context,
            ),
        )

    def test_g0_claims_bind_artifact_name_kind_and_retained_path(self):
        transcript = "\n".join(
            (
                "@evidence event=g0_contract_schema result=pass",
                "@evidence event=g0_machine_contract result=pass",
            )
        )
        v1 = (
            "world-contract-v1",
            "executable-constitutional-schema",
            "evidence/world_contract_v1.toml",
        )
        v2 = (
            "world-contract-v2",
            "executable-constitutional-schema",
            "evidence/world_contract_v2.toml",
        )
        machine = (
            "o-machine-contract-v1",
            "executable-machine-contract-schema",
            "evidence/o_machine_contract_v1.toml",
        )
        context = {
            "evidence_class": "repository_conformance",
            "topology": {"kind": "repository", "acceleration": "none"},
            "source_paths": {
                "docs/O_MACHINE_CONTRACT.md",
                "evidence/o_machine_contract_v1.toml",
                "evidence/world_contract_v1.toml",
                "evidence/world_contract_v2.toml",
                "evidence/world_alpha_gates.toml",
            },
            "artifact_names": {item[0] for item in (v1, v2, machine)},
            "artifact_kinds": {item[1] for item in (v1, v2, machine)},
            "artifact_name_kinds": {
                item[0]: item[1] for item in (v1, v2, machine)
            },
            "artifact_bindings": {v1, v2, machine},
        }
        claims = world_alpha_evidence._derive_claims(transcript, "fixture", context)
        self.assertIn("world.contract_schema_consistent", claims)
        self.assertIn("world.machine_contract_consistent", claims)

        substituted = copy.deepcopy(context)
        substituted["artifact_bindings"] = {
            (v1[0], v1[1], v2[2]),
            (v2[0], v2[1], v1[2]),
            machine,
        }
        claims = world_alpha_evidence._derive_claims(
            transcript, "fixture", substituted
        )
        self.assertNotIn("world.contract_schema_consistent", claims)
        self.assertIn("world.machine_contract_consistent", claims)

        substituted = copy.deepcopy(context)
        substituted["artifact_bindings"].remove(machine)
        substituted["artifact_bindings"].add(
            (machine[0], "executable-constitutional-schema", machine[2])
        )
        claims = world_alpha_evidence._derive_claims(
            transcript, "fixture", substituted
        )
        self.assertIn("world.contract_schema_consistent", claims)
        self.assertNotIn("world.machine_contract_consistent", claims)

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
            with mock.patch.object(world_alpha_evidence, "_require_git_commit"):
                event = world_alpha_evidence._validate_evidence_event(root, path)
            self.assertEqual(event["subject"], "old-attestation")
            self.assertEqual(event["replacement"], "new-attestation")

    def test_checked_in_rederive_event_has_a_bound_payload(self):
        path = ROOT / "evidence/world/g2-derivation-rederive-2026-08-03.toml"
        event = world_alpha_evidence._validate_evidence_event(ROOT, path)
        self.assertEqual(event["event"], "rederive")
        self.assertEqual(event["current_derivation"], world_alpha_evidence.CURRENT_DERIVATION_HASH)
        self.assertEqual(event["claims_lost"], set())
        self.assertEqual(event["claims_gained"], set())

    def test_rederive_payload_tamper_is_rejected(self):
        source = ROOT / "evidence/world/g0-derivation-rederive-2026-08-03.toml"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "evidence/world/rederive.toml"
            path.parent.mkdir(parents=True)
            path.write_text(
                source.read_text(encoding="utf-8").replace(
                    "this historical record retained only v1",
                    "this historical record retained v1 and v2",
                ),
                encoding="utf-8",
            )
            with mock.patch.object(world_alpha_evidence, "_require_git_commit"):
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError,
                    "payload_sha256 does not bind",
                ):
                    world_alpha_evidence._validate_evidence_event(root, path)

    def test_schema_v2_validator_bytes_are_reconstructible_from_history(self):
        attestation = tomllib.loads(
            (ROOT / "evidence/world/g2-aarch64-qemu-2026-08-03.toml").read_text(
                encoding="utf-8"
            )
        )
        source_path = "scripts/world_alpha_evidence.py"
        digest = attestation["validator_sha256"]
        current_digest = hashlib.sha256(
            (ROOT / source_path).read_bytes()
        ).hexdigest()
        self.assertNotEqual(current_digest, digest)
        mapped_source_commit = world_alpha_evidence._require_git_commit(
            ROOT, attestation["source_commit"], "fixture"
        )
        base = subprocess.run(
            [
                "git",
                "-C",
                str(ROOT),
                "show",
                f"{mapped_source_commit}:{source_path}",
            ],
            check=True,
            stdout=subprocess.PIPE,
        )
        self.assertNotEqual(hashlib.sha256(base.stdout).hexdigest(), digest)
        source_digests = {
            item["path"]: item["sha256"] for item in attestation["source"]
        }
        source_digests[source_path] = digest
        self.assertEqual(
            world_alpha_evidence._resolve_source_snapshot(
                ROOT, attestation["source_commit"], source_digests
            ),
            "a5962984e97171cfd1897fccd2c2166e903c8a35",
        )

    def test_checked_in_attribution_rewrite_map_is_complete_and_tree_preserving(
        self,
    ) -> None:
        mappings = world_alpha_evidence._load_attribution_rewrite_map(ROOT)
        self.assertEqual(
            len(mappings),
            world_alpha_evidence.EXPECTED_ATTRIBUTION_REWRITE_MAP_ROWS,
        )
        head_ancestors = set(
            subprocess.run(
                ["git", "-C", str(ROOT), "rev-list", "HEAD"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout.splitlines()
        )
        commit_trees = dict(
            line.split(" ", 1)
            for line in subprocess.run(
                ["git", "-C", str(ROOT), "log", "--all", "--format=%H %T"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout.splitlines()
        )
        for old, new in mappings.items():
            with self.subTest(mapped_source=old):
                self.assertIn(new, head_ancestors)
                self.assertIn(new, commit_trees)
                if old in commit_trees:
                    self.assertEqual(commit_trees[old], commit_trees[new])
        source_commits = {
            record["source_commit"]
            for path in (ROOT / "evidence/world").glob("*.toml")
            if "source_commit"
            in (record := tomllib.loads(path.read_text(encoding="utf-8")))
        }
        for source_commit in source_commits:
            with self.subTest(source_commit=source_commit):
                source_is_current_lineage = (
                    world_alpha_evidence._git_commit_exists(ROOT, source_commit)
                    and world_alpha_evidence._git_is_head_ancestor(
                        ROOT, source_commit
                    )
                )
                resolved = world_alpha_evidence._require_git_commit(
                    ROOT, source_commit, "checked-in source commit"
                )
                self.assertTrue(
                    world_alpha_evidence._git_is_head_ancestor(ROOT, resolved)
                )
                if source_is_current_lineage:
                    self.assertEqual(resolved, source_commit)
                else:
                    self.assertIn(source_commit, mappings)
                    self.assertEqual(resolved, mappings[source_commit])

    def test_attribution_rewrite_map_rejects_digest_and_format_tamper(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_PATH
            path.parent.mkdir(parents=True)
            path.write_bytes(
                (
                    world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_HEADER
                    + "\n"
                    + f"{'1' * 40} {'2' * 40}\n"
                ).encode("ascii")
            )
            with self.assertRaisesRegex(
                world_alpha_evidence.WorldEvidenceError, "trusted SHA-256 seal"
            ):
                world_alpha_evidence._load_attribution_rewrite_map(root)

            header = world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_HEADER
            one = "1" * 40
            two = "2" * 40
            malformed_cases = (
                ("header", f"wrong header\n{one} {two}\n".encode(), "header differs"),
                (
                    "double space",
                    f"{header}\n{one}  {two}\n".encode(),
                    "not one strict mapping",
                ),
                (
                    "uppercase",
                    f"{header}\n{'A' * 40} {two}\n".encode(),
                    "not one strict mapping",
                ),
                ("no final LF", f"{header}\n{one} {two}".encode(), "end with LF"),
                ("CRLF", f"{header}\r\n{one} {two}\r\n".encode(), "use LF lines"),
                ("non-ASCII", f"{header}\n".encode() + b"\xff\n", "must be ASCII"),
            )
            for label, malformed, expected_error in malformed_cases:
                with self.subTest(format=label):
                    path.write_bytes(malformed)
                    with mock.patch.object(
                        world_alpha_evidence,
                        "EXPECTED_ATTRIBUTION_REWRITE_MAP_SHA256",
                        hashlib.sha256(malformed).hexdigest(),
                    ), mock.patch.object(
                        world_alpha_evidence,
                        "EXPECTED_ATTRIBUTION_REWRITE_MAP_ROWS",
                        1,
                    ):
                        with self.assertRaisesRegex(
                            world_alpha_evidence.WorldEvidenceError,
                            expected_error,
                        ):
                            world_alpha_evidence._load_attribution_rewrite_map(root)

    def test_attribution_rewrite_map_rejects_ambiguous_rows(self):
        one = "1" * 40
        two = "2" * 40
        three = "3" * 40
        four = "4" * 40
        zero = "0" * 40
        cases = (
            ("duplicate source", ((one, three), (one, four)), "source IDs"),
            ("duplicate target", ((one, three), (two, three)), "target IDs"),
            ("zero source", ((zero, three),), "two distinct commits"),
            ("zero target", ((one, zero),), "two distinct commits"),
            ("identity", ((one, one),), "two distinct commits"),
            ("unsorted", ((two, three), (one, four)), "source IDs"),
        )
        for label, rows, expected_error in cases:
            with self.subTest(case=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_PATH
                path.parent.mkdir(parents=True)
                map_bytes = (
                    world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_HEADER
                    + "\n"
                    + "".join(f"{old} {new}\n" for old, new in rows)
                ).encode("ascii")
                path.write_bytes(map_bytes)
                with mock.patch.object(
                    world_alpha_evidence,
                    "EXPECTED_ATTRIBUTION_REWRITE_MAP_SHA256",
                    hashlib.sha256(map_bytes).hexdigest(),
                ), mock.patch.object(
                    world_alpha_evidence,
                    "EXPECTED_ATTRIBUTION_REWRITE_MAP_ROWS",
                    len(rows),
                ):
                    with self.assertRaisesRegex(
                        world_alpha_evidence.WorldEvidenceError, expected_error
                    ):
                        world_alpha_evidence._load_attribution_rewrite_map(root)

    def test_required_attribution_rewrite_map_fails_closed_when_absent(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(
                world_alpha_evidence.WorldEvidenceError,
                "required attribution rewrite map is missing",
            ):
                world_alpha_evidence._load_attribution_rewrite_map(
                    Path(directory), required=True
                )

    def test_attribution_rewrite_rejects_missing_or_nonancestor_target(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(
                ["git", "init", "-q", "-b", "main", str(root)], check=True
            )
            subprocess.run(
                ["git", "-C", str(root), "config", "user.name", "Evidence Test"],
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(root),
                    "config",
                    "user.email",
                    "evidence@example.invalid",
                ],
                check=True,
            )
            (root / "source").write_text("base\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "source"], check=True)
            subprocess.run(
                ["git", "-C", str(root), "commit", "-q", "-m", "base"],
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(root), "switch", "-q", "-c", "side"],
                check=True,
            )
            (root / "source").write_text("side\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "source"], check=True)
            subprocess.run(
                ["git", "-C", str(root), "commit", "-q", "-m", "side"],
                check=True,
            )
            side = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout.strip()
            subprocess.run(
                ["git", "-C", str(root), "switch", "-q", "main"], check=True
            )
            old = "4" * 40
            cases = (
                ("missing", "f" * 40, "does not resolve to a Git commit"),
                ("nonancestor", side, "is not an ancestor of HEAD"),
            )
            for label, target, expected_error in cases:
                with self.subTest(case=label):
                    map_bytes = (
                        world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_HEADER
                        + "\n"
                        + f"{old} {target}\n"
                    ).encode("ascii")
                    map_path = (
                        root / world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_PATH
                    )
                    map_path.parent.mkdir(parents=True, exist_ok=True)
                    map_path.write_bytes(map_bytes)
                    with mock.patch.object(
                        world_alpha_evidence,
                        "EXPECTED_ATTRIBUTION_REWRITE_MAP_SHA256",
                        hashlib.sha256(map_bytes).hexdigest(),
                    ), mock.patch.object(
                        world_alpha_evidence,
                        "EXPECTED_ATTRIBUTION_REWRITE_MAP_ROWS",
                        1,
                    ):
                        with self.assertRaisesRegex(
                            world_alpha_evidence.WorldEvidenceError,
                            expected_error,
                        ):
                            world_alpha_evidence._require_git_commit(
                                root, old, "fixture.source_commit"
                            )

    def test_source_snapshot_survives_missing_old_commit_via_rewrite_map(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(
                ["git", "init", "-q", "-b", "legacy", str(root)], check=True
            )
            subprocess.run(
                ["git", "-C", str(root), "config", "user.name", "Evidence Test"],
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(root),
                    "config",
                    "user.email",
                    "evidence@example.invalid",
                ],
                check=True,
            )
            (root / "a").write_text("a0\n", encoding="utf-8")
            (root / "b").write_text("b0\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "a", "b"], check=True)
            subprocess.run(
                ["git", "-C", str(root), "commit", "-q", "-m", "base"],
                check=True,
            )
            base = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout.strip()
            tree = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD^{tree}"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout.strip()
            rewritten_base = subprocess.run(
                ["git", "-C", str(root), "commit-tree", tree],
                check=True,
                input="attribution-only rewrite\n",
                stdout=subprocess.PIPE,
                text=True,
            ).stdout.strip()
            self.assertNotEqual(base, rewritten_base)
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(root),
                    "switch",
                    "-q",
                    "-c",
                    "rewritten",
                    rewritten_base,
                ],
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(root), "branch", "-D", "legacy"], check=True
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(root),
                    "reflog",
                    "expire",
                    "--expire=now",
                    "--all",
                ],
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(root), "gc", "--prune=now"], check=True
            )
            self.assertFalse(world_alpha_evidence._git_commit_exists(root, base))
            (root / "a").write_text("a1\n", encoding="utf-8")
            (root / "b").write_text("b1\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "a", "b"], check=True)
            subprocess.run(
                ["git", "-C", str(root), "commit", "-q", "-m", "coherent"],
                check=True,
            )
            coherent = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout.strip()
            map_bytes = (
                world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_HEADER
                + "\n"
                + f"{base} {rewritten_base}\n"
            ).encode("ascii")
            map_path = root / world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_PATH
            map_path.parent.mkdir(parents=True)
            map_path.write_bytes(map_bytes)
            digests = {
                "a": hashlib.sha256(b"a1\n").hexdigest(),
                "b": hashlib.sha256(b"b1\n").hexdigest(),
            }
            with mock.patch.object(
                world_alpha_evidence,
                "EXPECTED_ATTRIBUTION_REWRITE_MAP_SHA256",
                hashlib.sha256(map_bytes).hexdigest(),
            ), mock.patch.object(
                world_alpha_evidence, "EXPECTED_ATTRIBUTION_REWRITE_MAP_ROWS", 1
            ):
                self.assertEqual(
                    world_alpha_evidence._require_git_commit(
                        root, base, "fixture.source_commit"
                    ),
                    rewritten_base,
                )
                self.assertEqual(
                    world_alpha_evidence._resolve_source_snapshot(
                        root, base, digests
                    ),
                    coherent,
                )

    def test_attribution_rewrite_rejects_tree_changing_mapping(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(
                ["git", "-C", str(root), "config", "user.name", "Evidence Test"],
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(root),
                    "config",
                    "user.email",
                    "evidence@example.invalid",
                ],
                check=True,
            )
            source = root / "source"
            source.write_text("old\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "source"], check=True)
            subprocess.run(
                ["git", "-C", str(root), "commit", "-q", "-m", "old"],
                check=True,
            )
            old = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout.strip()
            source.write_text("new\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "source"], check=True)
            subprocess.run(
                ["git", "-C", str(root), "commit", "-q", "-m", "new"],
                check=True,
            )
            new = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout.strip()
            map_bytes = (
                world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_HEADER
                + "\n"
                + f"{old} {new}\n"
            ).encode("ascii")
            map_path = root / world_alpha_evidence.ATTRIBUTION_REWRITE_MAP_PATH
            map_path.parent.mkdir(parents=True)
            map_path.write_bytes(map_bytes)
            with mock.patch.object(
                world_alpha_evidence,
                "EXPECTED_ATTRIBUTION_REWRITE_MAP_SHA256",
                hashlib.sha256(map_bytes).hexdigest(),
            ), mock.patch.object(
                world_alpha_evidence, "EXPECTED_ATTRIBUTION_REWRITE_MAP_ROWS", 1
            ):
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError,
                    "does not preserve the Git tree",
                ):
                    world_alpha_evidence._require_git_commit(
                        root, old, "fixture.source_commit"
                    )

    def test_source_snapshot_requires_one_coherent_descendant_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(
                ["git", "-C", str(root), "config", "user.name", "Evidence Test"],
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(root),
                    "config",
                    "user.email",
                    "evidence@example.invalid",
                ],
                check=True,
            )

            def commit(message):
                subprocess.run(["git", "-C", str(root), "add", "a", "b"], check=True)
                subprocess.run(
                    ["git", "-C", str(root), "commit", "-q", "-m", message],
                    check=True,
                )
                return subprocess.run(
                    ["git", "-C", str(root), "rev-parse", "HEAD"],
                    check=True,
                    stdout=subprocess.PIPE,
                    text=True,
                ).stdout.strip()

            (root / "a").write_text("a0\n", encoding="utf-8")
            (root / "b").write_text("b0\n", encoding="utf-8")
            base = commit("base")
            subprocess.run(
                ["git", "-C", str(root), "switch", "-q", "-c", "branch-a"],
                check=True,
            )
            (root / "a").write_text("a1\n", encoding="utf-8")
            branch_a = commit("a1")
            subprocess.run(
                ["git", "-C", str(root), "switch", "-q", "-c", "branch-b", base],
                check=True,
            )
            (root / "b").write_text("b1\n", encoding="utf-8")
            branch_b = commit("b1")
            digests = {
                "a": hashlib.sha256(b"a1\n").hexdigest(),
                "b": hashlib.sha256(b"b1\n").hexdigest(),
            }
            self.assertIsNone(
                world_alpha_evidence._resolve_source_snapshot(root, base, digests)
            )
            subprocess.run(
                ["git", "-C", str(root), "switch", "-q", "branch-a"],
                check=True,
            )
            (root / "b").write_text("b1\n", encoding="utf-8")
            coherent = commit("coherent")
            self.assertEqual(
                world_alpha_evidence._resolve_source_snapshot(root, base, digests),
                coherent,
            )
            self.assertNotEqual(coherent, branch_a)
            unrelated_digests = {
                "a": hashlib.sha256(b"a0\n").hexdigest(),
                "b": hashlib.sha256(b"b1\n").hexdigest(),
            }
            with self.assertRaisesRegex(
                world_alpha_evidence.WorldEvidenceError,
                "is not an ancestor of HEAD",
            ):
                world_alpha_evidence._resolve_source_snapshot(
                    root, branch_b, unrelated_digests
                )

    def test_unmapped_disconnected_commit_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(
                ["git", "init", "-q", "-b", "main", str(root)], check=True
            )
            subprocess.run(
                ["git", "-C", str(root), "config", "user.name", "Evidence Test"],
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(root),
                    "config",
                    "user.email",
                    "evidence@example.invalid",
                ],
                check=True,
            )
            (root / "source").write_text("base\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "source"], check=True)
            subprocess.run(
                ["git", "-C", str(root), "commit", "-q", "-m", "base"],
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(root), "switch", "-q", "-c", "side"],
                check=True,
            )
            (root / "source").write_text("side\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "source"], check=True)
            subprocess.run(
                ["git", "-C", str(root), "commit", "-q", "-m", "side"],
                check=True,
            )
            disconnected = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout.strip()
            subprocess.run(
                ["git", "-C", str(root), "switch", "-q", "main"], check=True
            )
            with self.assertRaisesRegex(
                world_alpha_evidence.WorldEvidenceError,
                "is not an ancestor of HEAD",
            ):
                world_alpha_evidence._require_git_commit(
                    root, disconnected, "fixture.source_commit"
                )

    def test_source_commit_must_resolve_to_a_real_commit(self):
        with self.assertRaisesRegex(
            world_alpha_evidence.WorldEvidenceError,
            "does not resolve to a Git commit",
        ):
            world_alpha_evidence._require_git_commit(
                ROOT, "0" * 40, "fixture.source_commit"
            )

    def test_rederive_chain_updates_claims_without_retiring_attestation(self):
        prior = "sha256:" + "1" * 64
        attestation_id = "g2-aarch64-qemu-tcg-2026-08-03"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger = root / "evidence/world"
            ledger.mkdir(parents=True)
            (ledger / "a.toml").write_text('gate = "G2"\n', encoding="utf-8")
            (ledger / "rederive.toml").write_text(
                'event = "rederive"\n', encoding="utf-8"
            )
            attestation = {
                "id": attestation_id,
                "path": "evidence/world/a.toml",
                "gate": "G2",
                "class": "qemu_tcg_aarch64",
                "recorded_claims": {"claim.old"},
                "current_derived_claims": {"claim.new"},
                "derived_claims": set(),
                "derivation_hash": prior,
                "schema_version": 2,
            }
            event = {
                "id": "rederive-a",
                "path": "evidence/world/rederive.toml",
                "event": "rederive",
                "subject": attestation_id,
                "replacement": "",
                "prior_derivation": prior,
                "current_derivation": world_alpha_evidence.CURRENT_DERIVATION_HASH,
                "claims_lost": {"claim.old"},
                "claims_gained": {"claim.new"},
            }
            with mock.patch.object(
                world_alpha_evidence,
                "_validate_attestation",
                return_value=attestation,
            ), mock.patch.object(
                world_alpha_evidence,
                "_validate_evidence_event",
                return_value=event,
            ):
                active, events = world_alpha_evidence._active_evidence_ledger(
                    root, {"qemu_tcg_aarch64"}, "0" * 64
                )
            self.assertEqual([item["id"] for item in active], [attestation_id])
            self.assertEqual(active[0]["derived_claims"], {"claim.new"})
            self.assertEqual([item["event"] for item in events], ["rederive"])

    def test_external_unverified_witness_is_status_inert(self):
        prior = "sha256:" + "1" * 64
        attestation_id = "g2-aarch64-qemu-tcg-2026-08-03"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger = root / "evidence/world"
            ledger.mkdir(parents=True)
            for name, marker in (
                ("a.toml", 'gate = "G2"\n'),
                ("rederive.toml", 'event = "rederive"\n'),
                ("witness.toml", 'event = "witness"\n'),
            ):
                (ledger / name).write_text(marker, encoding="utf-8")
            attestation = {
                "id": attestation_id,
                "path": "evidence/world/a.toml",
                "gate": "G2",
                "class": "qemu_tcg_aarch64",
                "recorded_claims": {"claim"},
                "current_derived_claims": {"claim"},
                "derived_claims": set(),
                "derivation_hash": prior,
                "schema_version": 2,
            }
            events = {
                "rederive.toml": {
                    "id": "rederive-a",
                    "path": "evidence/world/rederive.toml",
                    "event": "rederive",
                    "subject": attestation_id,
                    "replacement": "",
                    "prior_derivation": prior,
                    "current_derivation": world_alpha_evidence.CURRENT_DERIVATION_HASH,
                    "claims_lost": set(),
                    "claims_gained": set(),
                    "payload_sha256": "2" * 64,
                    "record_sha256": "2" * 64,
                },
                "witness.toml": {
                    "id": "witness-a",
                    "path": "evidence/world/witness.toml",
                    "event": "witness",
                    "subject": "rederive-a",
                    "subject_record_sha256": "2" * 64,
                    "verification": "external_unverified",
                },
            }

            def fake_event(_root, path):
                return copy.deepcopy(events[path.name])

            with mock.patch.object(
                world_alpha_evidence,
                "_validate_attestation",
                return_value=attestation,
            ), mock.patch.object(
                world_alpha_evidence,
                "_validate_evidence_event",
                side_effect=fake_event,
            ):
                active, validated_events = world_alpha_evidence._active_evidence_ledger(
                    root, {"qemu_tcg_aarch64"}, "0" * 64
                )
            self.assertEqual([item["id"] for item in active], [attestation_id])
            self.assertEqual(active[0]["derived_claims"], {"claim"})
            self.assertEqual(
                {item["event"] for item in validated_events},
                {"rederive", "witness"},
            )

    def test_external_witness_schema_remains_explicitly_unverified(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "evidence/world/witness.toml"
            path.parent.mkdir(parents=True)
            witness_record = {
                "schema_version": 1,
                "id": "witness-a",
                "event": "witness",
                "subject": "rederive-a",
                "subject_record_sha256": "1" * 64,
                "algorithm": "ed25519",
                "key_id": "external-key-1",
                "public_key": "2" * 64,
                "run_identity": "external-run-1",
                "source_commit": "4" * 40,
                "verification": "external_unverified",
            }
            witness_payload_sha256 = world_alpha_evidence._witness_payload_sha256(
                witness_record
            )
            path.write_text(
                "\n".join(
                    (
                        "schema_version = 1",
                        'id = "witness-a"',
                        'event = "witness"',
                        'subject = "rederive-a"',
                        f'subject_record_sha256 = "{"1" * 64}"',
                        f'witness_payload_sha256 = "{witness_payload_sha256}"',
                        'algorithm = "ed25519"',
                        'key_id = "external-key-1"',
                        f'public_key = "{"2" * 64}"',
                        f'signature = "{"3" * 128}"',
                        'run_identity = "external-run-1"',
                        f'source_commit = "{"4" * 40}"',
                        'verification = "external_unverified"',
                        "",
                    )
                ),
                encoding="utf-8",
            )
            with mock.patch.object(world_alpha_evidence, "_require_git_commit"):
                event = world_alpha_evidence._validate_evidence_event(root, path)
            self.assertEqual(event["event"], "witness")
            self.assertEqual(event["verification"], "external_unverified")
            original = path.read_text(encoding="utf-8")
            path.write_text(
                original.replace(
                    'run_identity = "external-run-1"',
                    'run_identity = "external-run-2"',
                ),
                encoding="utf-8",
            )
            with mock.patch.object(world_alpha_evidence, "_require_git_commit"):
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError,
                    "does not bind the detached signature preimage",
                ):
                    world_alpha_evidence._validate_evidence_event(root, path)
            path.write_text(original, encoding="utf-8")
            path.write_text(
                original.replace(f'public_key = "{"2" * 64}"', f'public_key = "{"0" * 64}"'),
                encoding="utf-8",
            )
            with mock.patch.object(world_alpha_evidence, "_require_git_commit"):
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError,
                    "public_key must not be all zero",
                ):
                    world_alpha_evidence._validate_evidence_event(root, path)
            path.write_text(original, encoding="utf-8")
            path.write_text(
                path.read_text(encoding="utf-8").replace(
                    'verification = "external_unverified"',
                    'verification = "verified"',
                ),
                encoding="utf-8",
            )
            with mock.patch.object(world_alpha_evidence, "_require_git_commit"):
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError,
                    "must state external_unverified",
                ):
                    world_alpha_evidence._validate_evidence_event(root, path)

    def test_rederive_chain_rejects_an_inexact_claim_delta(self):
        prior = "sha256:" + "1" * 64
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger = root / "evidence/world"
            ledger.mkdir(parents=True)
            (ledger / "a.toml").write_text('gate = "G2"\n', encoding="utf-8")
            (ledger / "rederive.toml").write_text(
                'event = "rederive"\n', encoding="utf-8"
            )
            attestation = {
                "id": "a",
                "path": "evidence/world/a.toml",
                "gate": "G2",
                "class": "qemu_tcg_aarch64",
                "recorded_claims": {"claim.old"},
                "current_derived_claims": set(),
                "derived_claims": set(),
                "derivation_hash": prior,
                "schema_version": 2,
            }
            event = {
                "id": "rederive-a",
                "path": "evidence/world/rederive.toml",
                "event": "rederive",
                "subject": "a",
                "replacement": "",
                "prior_derivation": prior,
                "current_derivation": world_alpha_evidence.CURRENT_DERIVATION_HASH,
                "claims_lost": set(),
                "claims_gained": set(),
            }
            with mock.patch.object(
                world_alpha_evidence,
                "_validate_attestation",
                return_value=attestation,
            ), mock.patch.object(
                world_alpha_evidence,
                "_validate_evidence_event",
                return_value=event,
            ):
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError,
                    "rederive delta differs from current derivation",
                ):
                    world_alpha_evidence._active_evidence_ledger(
                        root, {"qemu_tcg_aarch64"}, "0" * 64
                    )

    def test_rederive_and_supersession_are_independent_edges(self):
        prior = "sha256:" + "1" * 64
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger = root / "evidence/world"
            ledger.mkdir(parents=True)
            for name, marker in (
                ("a.toml", 'gate = "G2"\n'),
                ("b.toml", 'gate = "G2"\n'),
                ("rederive.toml", 'event = "rederive"\n'),
                ("supersede.toml", 'event = "supersede"\n'),
                ("witness.toml", 'event = "witness"\n'),
            ):
                (ledger / name).write_text(marker, encoding="utf-8")
            attestations = {
                "a.toml": {
                    "id": "a",
                    "path": "evidence/world/a.toml",
                    "gate": "G2",
                    "class": "qemu_tcg_aarch64",
                    "recorded_claims": {"claim"},
                    "current_derived_claims": {"claim"},
                    "derived_claims": set(),
                    "derivation_hash": prior,
                    "schema_version": 2,
                },
                "b.toml": {
                    "id": "b",
                    "path": "evidence/world/b.toml",
                    "gate": "G2",
                    "class": "qemu_tcg_aarch64",
                    "recorded_claims": {"claim"},
                    "current_derived_claims": {"claim"},
                    "derived_claims": set(),
                    "derivation_hash": world_alpha_evidence.CURRENT_DERIVATION_HASH,
                    "schema_version": 3,
                },
            }
            events = {
                "rederive.toml": {
                    "id": "rederive-a",
                    "path": "evidence/world/rederive.toml",
                    "event": "rederive",
                    "subject": "a",
                    "replacement": "",
                    "prior_derivation": prior,
                    "current_derivation": world_alpha_evidence.CURRENT_DERIVATION_HASH,
                    "claims_lost": set(),
                    "claims_gained": set(),
                    "payload_sha256": "2" * 64,
                    "record_sha256": "2" * 64,
                },
                "supersede.toml": {
                    "id": "supersede-a",
                    "path": "evidence/world/supersede.toml",
                    "event": "supersede",
                    "subject": "a",
                    "replacement": "b",
                    "prior_derivation": "",
                    "current_derivation": "",
                    "claims_lost": set(),
                    "claims_gained": set(),
                    "payload_sha256": "3" * 64,
                    "record_sha256": "3" * 64,
                },
                "witness.toml": {
                    "id": "witness-supersede-a",
                    "path": "evidence/world/witness.toml",
                    "event": "witness",
                    "subject": "supersede-a",
                    "subject_record_sha256": "3" * 64,
                    "verification": "external_unverified",
                },
            }

            def fake_attestation(_root, path, *_args):
                return copy.deepcopy(attestations[Path(path).name])

            def fake_event(_root, path):
                return copy.deepcopy(events[path.name])

            with mock.patch.object(
                world_alpha_evidence,
                "_validate_attestation",
                side_effect=fake_attestation,
            ), mock.patch.object(
                world_alpha_evidence,
                "_validate_evidence_event",
                side_effect=fake_event,
            ):
                active, validated_events = world_alpha_evidence._active_evidence_ledger(
                    root, {"qemu_tcg_aarch64"}, "0" * 64
                )
            self.assertEqual([item["id"] for item in active], ["b"])
            self.assertEqual(active[0]["derived_claims"], {"claim"})
            self.assertIn("witness", {item["event"] for item in validated_events})

    def test_schema_v1_attestation_cannot_be_an_active_head(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger = root / "evidence/world"
            ledger.mkdir(parents=True)
            (ledger / "legacy.toml").write_text('gate = "G0"\n', encoding="utf-8")
            legacy = {
                "id": "legacy",
                "path": "evidence/world/legacy.toml",
                "gate": "G0",
                "class": "repository_conformance",
                "recorded_claims": {"world.contract_schema_consistent"},
                "current_derived_claims": {"world.contract_schema_consistent"},
                "derived_claims": set(),
                "derivation_hash": None,
                "schema_version": 1,
            }
            with mock.patch.object(
                world_alpha_evidence,
                "_validate_attestation",
                return_value=legacy,
            ):
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError,
                    "schema-v1 attestation legacy cannot be an active ledger head",
                ):
                    world_alpha_evidence._active_evidence_ledger(
                        root, {"repository_conformance"}, "0" * 64
                    )

    def test_unpinned_schema_v2_attestation_cannot_be_an_active_head(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger = root / "evidence/world"
            ledger.mkdir(parents=True)
            (ledger / "legacy.toml").write_text('gate = "G0"\n', encoding="utf-8")
            legacy = {
                "id": "unlisted-schema-v2",
                "path": "evidence/world/legacy.toml",
                "gate": "G0",
                "class": "repository_conformance",
                "recorded_claims": {"claim"},
                "current_derived_claims": {"claim"},
                "derived_claims": set(),
                "derivation_hash": world_alpha_evidence.CURRENT_DERIVATION_HASH,
                "schema_version": 2,
            }
            with mock.patch.object(
                world_alpha_evidence,
                "_validate_attestation",
                return_value=legacy,
            ):
                with self.assertRaisesRegex(
                    world_alpha_evidence.WorldEvidenceError,
                    "active attestation unlisted-schema-v2 must use schema v3",
                ):
                    world_alpha_evidence._active_evidence_ledger(
                        root, {"repository_conformance"}, "0" * 64
                    )

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

    def test_registry_semantics_hash_binds_validator_owned_qualification_floors(self):
        manifest = self.manifest()
        baseline = world_alpha_evidence._registry_semantics_sha256(manifest)
        weakened = {
            gate: set(claims)
            for gate, claims in world_alpha_evidence.REQUIRED_CLAIM_FLOORS.items()
        }
        weakened["G0"].remove("world.machine_contract_consistent")
        with mock.patch.object(
            world_alpha_evidence, "REQUIRED_CLAIM_FLOORS", weakened
        ):
            changed = world_alpha_evidence._registry_semantics_sha256(manifest)
        self.assertNotEqual(changed, baseline)

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
