#!/usr/bin/env python3
"""Validate the OSTADIX Alpha G0-G13 qualification registry.

This registry defines future integrated release gates. It is intentionally
separate from evidence/gates.toml, which records executable evidence for the
current bounded O-core slices.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tomllib
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "evidence/world_alpha_gates.toml"

EXPECTED_SCHEMA_VERSION = 4
EXPECTED_CONSTITUTION_VERSION = 3
IMPORTED_WORLD_CONTRACT_CONSTITUTION_VERSION = 2
EXPECTED_CONSTITUTION_SHA256 = "e7d47d7a8e0e8f6d35bf3bb6b1f86f2bddffe27a67a3415c3d4ea8c76e13bcea"
EXPECTED_HOSTED_PROFILE_SHA256 = "647da49edfc4b7d53a9248e8fdcda5cdb62be3c47756c59b7523ee09461d2e1d"
EXPECTED_WORLD_CONTRACT_V1_SHA256 = "4b2d92596ab46294894a4127cc5c603b121a3a3d7e942f0013dd419330921bf8"
EXPECTED_WORLD_CONTRACT_V2_SHA256 = "af1334bb4d0aca30e7f722890e819c0a597c4c4b42db0006c452dec2e755b74b"
EXPECTED_MACHINE_CONTRACT_SHA256 = "eb759ce5695e8080baa3acbd0fcb3f97fc2a97e430679cd8c836aba3a3d2be50"
EXPECTED_MACHINE_SPEC_SHA256 = "7958677cbf178003b47f475a265857a42dc6e3b51a33fe408c1863b8afa64880"
EXPECTED_IMPORTED_CONSTITUTION_V2_SHA256 = (
    "2a56a9b54297c9b6190505055bad3f2e8760a501498b1a55da72a0fd4d298643"
)
EXPECTED_REGISTRY_SEMANTICS_SHA256 = (
    "23ead9813a067917ac5ea5d08c7be34616865286e9f45622212eb0ff3676686e"
)
# Append-only attestations retain the registry identity under which they were
# produced. The prior identity differs only in the retired release-facing name;
# supersession, not record rewriting, moves G0 to the current identity.
ACCEPTED_REGISTRY_SEMANTICS_SHA256 = frozenset(
    {
        "bcc377e68c3b0d35c879430181b9b9763111fd9507ee903142030050447000ea",
        EXPECTED_REGISTRY_SEMANTICS_SHA256,
    }
)
EXPECTED_GATE_IDS = tuple(f"G{number}" for number in range(14))
EXPECTED_CLASS_SCOPES = {
    "repository_conformance": "supporting",
    "hosted_reference": "reference_only",
    "qemu_tcg_x86_64": "virtual_native",
    "qemu_tcg_aarch64": "virtual_native",
    "qemu_virtualization": "virtual_native",
    "hardware_x86_64": "physical_native",
    "hardware_x86_64_iommu": "physical_native",
    "hardware_aarch64": "physical_native",
    "hardware_aarch64_smmu": "physical_native",
    "multinode_virtual": "supporting",
    "multinode_physical": "physical_native",
    "fault_injection": "supporting",
    "security_adversarial": "supporting",
    "performance_characterization": "descriptive_only",
}
EXPECTED_DEPENDENCIES = {
    "G0": (),
    "G1": ("G0",),
    "G2": ("G0",),
    "G3": ("G2",),
    "G4": ("G0",),
    "G5": ("G4",),
    "G6": ("G5",),
    "G7": ("G0",),
    "G8": ("G7",),
    "G9": ("G0",),
    "G10": ("G0",),
    "G11": ("G10",),
    "G12": ("G1", "G3", "G6", "G8", "G9", "G11"),
    "G13": ("G12",),
}

# These floors prevent a future manifest edit from silently substituting TCG,
# hosted processes, or virtual multinode tests for a physical/native gate.
REQUIRED_CLASS_FLOORS = {
    "G0": {"repository_conformance"},
    "G1": {"repository_conformance", "qemu_tcg_x86_64"},
    "G2": {"qemu_tcg_aarch64"},
    "G3": {"hardware_x86_64", "hardware_aarch64", "fault_injection"},
    "G4": {"multinode_physical", "security_adversarial"},
    "G5": {"multinode_physical", "fault_injection", "security_adversarial"},
    "G6": {"multinode_physical", "fault_injection"},
    "G7": {"qemu_virtualization"},
    "G8": {"fault_injection", "security_adversarial"},
    "G9": {"qemu_tcg_x86_64"},
    "G10": {"multinode_physical", "fault_injection"},
    "G11": {"multinode_physical", "fault_injection"},
    "G12": {
        "multinode_physical",
        "hardware_aarch64",
        "fault_injection",
        "security_adversarial",
    },
    "G13": {
        "multinode_physical",
        "hardware_aarch64",
        "fault_injection",
        "security_adversarial",
        "performance_characterization",
    },
}
ONE_OF_CLASS_FLOORS = {
    "G8": {frozenset({"hardware_x86_64_iommu", "hardware_aarch64_smmu"})},
    "G11": {frozenset({"hardware_x86_64_iommu", "hardware_aarch64_smmu"})},
}

# Claims are validator-owned identifiers.  Attestations may record the derived
# set, but they never define claim meaning or gate qualification.
REQUIRED_CLAIM_FLOORS = {
    "G0": {
        "world.contract_schema_consistent",
        "world.machine_contract_consistent",
        "world.crossing_taxonomy_consistent",
        "world.identity_vocabulary_consistent",
        "world.failure_consistency_schema_consistent",
        "evidence.claim_class_guarded",
    },
    "G1": {"hgraph.world_identity_continuity"},
    "G2": {
        "aarch64.native_object",
        "aarch64.el2_resident",
        "aarch64.el1_execution",
        "aarch64.hvc_roundtrip",
        "aarch64.el0_execution",
        "aarch64.svc_eret_roundtrip",
        "ipc.request_reply",
        "capability.attenuation",
        "capability.stale_generation_rejected",
        "lifecycle.reclamation",
        "counter.progress_after_lifecycle",
    },
    "G3": {
        "aarch64.smp_execution",
        "lifecycle.multicore_linearization",
        "machine_memory.cross_cpu_tlbi_drain_acknowledged",
    },
    "G4": {"transport.native_authenticated", "transport.three_physical_nodes"},
    "G5": {"governor.authoritative_log", "governor.partition_fencing"},
    "G6": {"worldfs.live_multinode_namespace", "worldfs.stale_fid_rejected"},
    "G7": {
        "foreign_kernel.booted",
        "foreign_kernel.userspace_reached",
        "foreign_kernel.fresh_challenge_answered",
        "kernel_world.guest_machine_authority_hvc_absent",
        "kernel_world.async_completion_tombstone",
        "kernel_world.class_specific_host_ack",
        "kernel_world.device_withdraw_error_observed",
        "kernel_world.guest_error_before_memory_teardown",
        "kernel_world.memory_teardown_complete",
        "kernel_world.page_owner_quarantine_scrubbed",
        "kernel_world.pages_reclaimed",
        "kernel_world.g7_class_withdrawal_lifecycle",
    },
    "G8": {
        "driver.physical_device_service",
        "driver.dma_revocation_complete",
        "driver.interrupt_revocation_complete",
        "driver.device_reset_complete",
        "driver.class_specific_withdraw_complete",
        "driver.guest_interface_policy_decided",
        "driver.g8_physical_withdrawal_lifecycle",
    },
    "G9": {"personality.debian_dynamic_userland"},
    "G10": {"execution.distributed_exactly_one_commit"},
    "G11": {"accelerator.multinode_governed_execution"},
    "G12": {"world.three_node_integrated"},
    "G13": {"world.eight_node_alpha"},
}

# A rule is a claim plus the normalized observations that must all be present.
# Required fields are matched as subsets, so producers may append descriptive
# fields without changing claim meaning.
CLAIM_RULES: tuple[tuple[str, tuple[tuple[str, tuple[tuple[str, str], ...]], ...]], ...] = (
    (
        "world.contract_schema_consistent",
        (("g0_contract_schema", (("result", "pass"),)),),
    ),
    (
        "world.machine_contract_consistent",
        (("g0_machine_contract", (("result", "pass"),)),),
    ),
    (
        "world.crossing_taxonomy_consistent",
        (("g0_crossing_taxonomy", (("result", "pass"),)),),
    ),
    (
        "world.identity_vocabulary_consistent",
        (("g0_identity_vocabulary", (("result", "pass"),)),),
    ),
    (
        "world.failure_consistency_schema_consistent",
        (("g0_failure_consistency", (("result", "pass"),)),),
    ),
    (
        "evidence.claim_class_guarded",
        (("g0_claim_class_guard", (("result", "pass"),)),),
    ),
    (
        "aarch64.native_object",
        (
            (
                "aarch64_native_object",
                (("format", "elf64"), ("machine", "183"), ("result", "pass")),
            ),
        ),
    ),
    ("aarch64.el2_resident", (("el2_resident", (("result", "pass"),)),)),
    ("aarch64.el1_execution", (("el1_execution", (("result", "pass"),)),)),
    (
        "aarch64.hvc_roundtrip",
        (
            (
                "el2_hvc_roundtrip",
                (
                    ("domain", "0x4f4d"),
                    ("registers", "preserved"),
                    ("stack", "preserved"),
                    ("result", "pass"),
                ),
            ),
        ),
    ),
    (
        "aarch64.el0_execution",
        (("el0_execution", (("principals", "2"), ("result", "pass"))),),
    ),
    (
        "aarch64.svc_eret_roundtrip",
        (("svc_eret_roundtrip", (("result", "pass"),)),),
    ),
    ("ipc.request_reply", (("ipc_request_reply", (("result", "pass"),)),)),
    (
        "capability.attenuation",
        (("capability_attenuation", (("result", "pass"),)),),
    ),
    (
        "capability.stale_generation_rejected",
        (
            (
                "stale_generation_rejected",
                (("kinds", "process,capability"), ("result", "pass")),
            ),
        ),
    ),
    ("lifecycle.terminal", (("lifecycle_terminal", (("result", "pass"),)),)),
    ("lifecycle.reclamation", (("reclamation", (("result", "pass"),)),)),
    (
        "execution.post_lifecycle_reached",
        (
            ("lifecycle_terminal", (("result", "pass"),)),
            (
                "counter_progress",
                (("phase", "post_lifecycle"), ("result", "pass")),
            ),
        ),
    ),
    (
        "counter.progress_after_lifecycle",
        (
            ("lifecycle_terminal", (("result", "pass"),)),
            (
                "counter_progress",
                (
                    ("phase", "post_lifecycle"),
                    ("poll_bound", "1000000"),
                    ("result", "pass"),
                ),
            ),
        ),
    ),
    (
        "kernel_world.g7_class_withdrawal_lifecycle",
        (
            (
                "g7_class_withdrawal_lifecycle",
                (
                    ("resource_class", "MachineBlock"),
                    ("async", "pending_query_complete"),
                    ("host_ack", "complete"),
                    ("guest_error", "consumed_guest_healthy"),
                    ("join_before_memory", "host_ack_and_guest_error"),
                    (
                        "memory_order",
                        "stop_quiesce_unmap_guest_host_tlbi_drain_generation_ack",
                    ),
                    ("reclaim", "after_memory_ack"),
                    ("guest_authority_hvc", "absent"),
                    ("result", "pass"),
                ),
            ),
        ),
    ),
    (
        "kernel_world.g7_class_withdrawal_lifecycle",
        (
            (
                "g7_class_withdrawal_lifecycle",
                (
                    ("resource_class", "Machine9P"),
                    ("async", "pending_query_complete"),
                    ("host_ack", "complete"),
                    ("guest_error", "consumed_guest_healthy"),
                    ("join_before_memory", "host_ack_and_guest_error"),
                    (
                        "memory_order",
                        "stop_quiesce_unmap_guest_host_tlbi_drain_generation_ack",
                    ),
                    ("reclaim", "after_memory_ack"),
                    ("guest_authority_hvc", "absent"),
                    ("result", "pass"),
                ),
            ),
        ),
    ),
)

_REPOSITORY_CONTEXT = {
    "evidence_classes": ("repository_conformance",),
    "topology": (("kind", "repository"), ("acceleration", "none")),
}
_WORLD_CONTRACT_V1_ARTIFACT = (
    "world-contract-v1",
    "executable-constitutional-schema",
    "evidence/world_contract_v1.toml",
)
_WORLD_CONTRACT_V2_ARTIFACT = (
    "world-contract-v2",
    "executable-constitutional-schema",
    "evidence/world_contract_v2.toml",
)
_MACHINE_CONTRACT_V1_ARTIFACT = (
    "o-machine-contract-v1",
    "executable-machine-contract-schema",
    "evidence/o_machine_contract_v1.toml",
)
_G2_AARCH64_CONTEXT = {
    "evidence_classes": ("qemu_tcg_aarch64",),
    "topology": (
        ("kind", "virtual"),
        ("architecture", "aarch64"),
        ("acceleration", "tcg"),
        ("cpu_count", 1),
    ),
    "artifact_names": ("g2-kernel-elf", "g2-kernel-object"),
}
CLAIM_CONTEXT_RULES: dict[str, dict[str, Any]] = {
    "world.contract_schema_consistent": {
        **_REPOSITORY_CONTEXT,
        "artifact_bindings": (
            _WORLD_CONTRACT_V1_ARTIFACT,
            _WORLD_CONTRACT_V2_ARTIFACT,
        ),
        "source_paths": (
            "evidence/world_contract_v1.toml",
            "evidence/world_contract_v2.toml",
            "evidence/world_alpha_gates.toml",
        ),
    },
    "world.machine_contract_consistent": {
        **_REPOSITORY_CONTEXT,
        "artifact_bindings": (_MACHINE_CONTRACT_V1_ARTIFACT,),
        "source_paths": (
            "docs/O_MACHINE_CONTRACT.md",
            "evidence/o_machine_contract_v1.toml",
            "evidence/world_contract_v2.toml",
        ),
    },
    "world.crossing_taxonomy_consistent": {
        **_REPOSITORY_CONTEXT,
        "artifact_bindings": (_WORLD_CONTRACT_V1_ARTIFACT,),
    },
    "world.identity_vocabulary_consistent": {
        **_REPOSITORY_CONTEXT,
        "artifact_bindings": (_WORLD_CONTRACT_V1_ARTIFACT,),
    },
    "world.failure_consistency_schema_consistent": {
        **_REPOSITORY_CONTEXT,
        "artifact_bindings": (_WORLD_CONTRACT_V1_ARTIFACT,),
    },
    "evidence.claim_class_guarded": {
        **_REPOSITORY_CONTEXT,
        "source_paths": (
            "evidence/world_alpha_gates.toml",
            "scripts/world_alpha_evidence.py",
        ),
    },
    **{
        claim: _G2_AARCH64_CONTEXT
        for claim in (
            "aarch64.native_object",
            "aarch64.el2_resident",
            "aarch64.el1_execution",
            "aarch64.hvc_roundtrip",
            "aarch64.el0_execution",
            "aarch64.svc_eret_roundtrip",
            "ipc.request_reply",
            "capability.attenuation",
            "capability.stale_generation_rejected",
            "lifecycle.terminal",
            "lifecycle.reclamation",
            "execution.post_lifecycle_reached",
            "counter.progress_after_lifecycle",
        )
    },
    "kernel_world.g7_class_withdrawal_lifecycle": {
        "evidence_classes": ("qemu_virtualization",),
        "topology": (("kind", "virtual"),),
        "source_paths": (
            "docs/O_MACHINE_CONTRACT.md",
            "evidence/o_machine_contract_v1.toml",
        ),
        "artifact_kinds": (
            "foreign-kernel-image",
            "guest-native-error-trace",
            "machine-withdrawal-trace",
        ),
    },
    "driver.g8_physical_withdrawal_lifecycle": {
        "evidence_classes": (
            "hardware_x86_64_iommu",
            "hardware_aarch64_smmu",
        ),
        "topology": (("kind", "physical"),),
        "source_paths": (
            "docs/O_MACHINE_CONTRACT.md",
            "evidence/o_machine_contract_v1.toml",
        ),
        "artifact_kinds": (
            "physical-device-inventory",
            "dma-iommu-withdrawal-trace",
            "interrupt-reset-trace",
            "unrelated-world-survival-trace",
        ),
    },
}

DERIVATION_HASH_PREFIX = "sha256:"
DERIVATION_SPEC_VERSION = "ostadix-world-claim-derivation-v1"
REDERIVE_PAYLOAD_DOMAIN = "ostadix.world.evidence.rederive.v1"
WITNESS_PAYLOAD_DOMAIN = "ostadix.world.evidence.witness.v1"
LEGACY_ACTIVE_SCHEMA2_IDS = frozenset(
    {"g2-aarch64-qemu-tcg-2026-08-03"}
)

NONCLAIM_FLOORS = {
    "qemu_tcg_aarch64": (
        "physical AArch64",
        "KVM/SVM",
        "Linux or Plan 9",
        "PCI/DMA/IOMMU",
    ),
}
NONQUALIFYING_CLASSES = {"hosted_reference", "multinode_virtual"}
HEX_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
HEX_COMMIT = re.compile(r"[0-9a-f]{40,64}\Z")
ATTESTATION_ID = re.compile(r"[a-z0-9][a-z0-9._-]*\Z")
EXPECTED_CROSSINGS = (
    ("ovalue", "portable_data", "none", "capsule"),
    (
        "capability",
        "transferable_authority",
        "authenticated_attenuating_delegation",
        "deny",
    ),
    ("capsule", "explicit_affinity", "origin_bound", "capsule"),
)
EXPECTED_IDENTITY_ATOMS = (
    ("WorldId", "bounded_string"),
    ("WorldEpoch", "nonzero_u64"),
    ("GovernorTerm", "nonzero_u64"),
    ("GovernorLogIndex", "nonzero_u64"),
    ("NodeId", "bounded_string"),
    ("NodeGeneration", "nonzero_u64"),
    ("DomainId", "bounded_string"),
    ("DomainGeneration", "nonzero_u64"),
    ("ProcessId", "bounded_string"),
    ("ProcessGeneration", "nonzero_u64"),
    ("ResourceId", "bounded_resource_path"),
    ("ResourceGeneration", "nonzero_u64"),
    ("ObjectId", "bounded_string"),
    ("ObjectVersion", "nonzero_u64"),
    ("CapabilityId", "bounded_string"),
    ("LeaseId", "bounded_string"),
    ("TaskId", "bounded_string"),
    ("AttemptGeneration", "nonzero_u64"),
    ("CheckpointId", "bounded_string"),
    ("ReceiptId", "bounded_string"),
)
EXPECTED_FAILURE_CLASSES = (
    ("ephemeral", "loss_is_final"),
    ("restartable", "replay_from_immutable_inputs"),
    ("checkpointable", "resume_from_committed_checkpoint"),
    ("replicated", "multiple_attempts_exactly_one_global_commit"),
    ("affinity-bound", "report_capsule_owner_loss"),
    ("transactional", "require_governor_commit_token"),
    ("compensatable", "invoke_declared_compensation"),
)
EXPECTED_CONSISTENCY_RULES = (
    ("authority-replication", "three_replica_raft_style_group"),
    ("authoritative-mutations", "linearizable_replicated_log"),
    ("telemetry", "recent_snapshot_labelled_with_log_index"),
    ("clocks", "failure_detection_only_not_authority"),
    (
        "commit-fencing",
        "governor_term_log_or_epoch_and_attempt_generation",
    ),
    ("partition", "majority_authoritative_minority_island_noncommitting"),
    ("rejoin", "fresh_node_generation_stale_work_fenced"),
    ("memory", "aggregate_locality_visible_not_transparent_dsm"),
)
EXPECTED_MACHINE_CONTRACT = {
    "schema": "ostadix.o-machine/v1",
    "schema_version": 1,
    "constitution_version": 3,
    "specification": "docs/O_MACHINE_CONTRACT.md",
    "specification_sha256": EXPECTED_MACHINE_SPEC_SHA256,
    "g7_caller_model": "host_el1_only",
    "g7_guest_machine_abi": "none",
    "g7_device_transport": "virtio_mmio_or_pci_doorbell",
    "g7_platform_call_exemption": (
        "psci_without_machine_handles_or_resource_authority"
    ),
    "g8_guest_machine_abi_decision": "required",
    "async_abi_frozen_before_gate": "G3",
    "tcb": {
        "authority": "ocore_el1",
        "memory_safety": "omachine_el2",
        "el2_authoritative_facts": [
            "machine_incarnation",
            "world_generation",
            "resource_generation",
            "page_owner",
        ],
        "el2_protocol_semantics": "none",
        "machine_incarnation": (
            "nonzero_128bit_durable_monotonic_or_cryptographically_unique"
        ),
        "machine_incarnation_failure": "fail_closed_no_resource_assignment",
        "host_el1_physical_access": (
            "el2_stage2_mediated_owner_generation_checked"
        ),
        "unrestricted_host_direct_map_world_frames": "forbidden",
        "g7_dma_assumption": "no_unfenced_physical_dma_to_world_frames",
        "protected_state": [
            "machine_incarnation",
            "world_generation",
            "resource_generation",
            "page_owner",
            "completion_tombstones",
            "stage2_roots",
            "el2_code",
            "el2_data",
        ],
        "protected_state_host_mapping": "forbidden",
        "protected_state_device_dma": "iommu_smmu_deny_or_no_dma_capable_device",
        "protected_state_failure": "fail_closed_no_guest_or_device_execution",
    },
    "handles": {
        "format": [
            "abi_version",
            "machine_incarnation",
            "domain_tag",
            "world_slot",
            "world_generation",
            "resource_slot",
            "generation",
            "rights",
        ],
        "g7_authenticator": "none",
        "domain_tag": "required_reserved_for_future_auth",
        "domains": [
            "memory",
            "stage2",
            "vcpu",
            "interrupt",
            "dma",
            "entry",
            "completion",
        ],
        "cross_domain_use": "reject",
        "generation_width_bits": 64,
        "generation_zero": "reject",
        "world_generation_advance": "checked_monotonic_no_wrap_no_reuse",
        "resource_generation_advance": "checked_monotonic_no_wrap_no_reuse",
        "generation_exhaustion": "fail_closed_retire_slot",
        "g8_mac_policy": "required_only_if_untrusted_guest_presents_handles",
        "g8_mac_framing": (
            "canonical_length_prefixed_all_handle_fields_including_abi_version"
        ),
        "g8_key_lifecycle": [
            "generation",
            "enrollment",
            "rotation",
            "suspend_resume",
            "migration",
            "el1_restart",
            "crash_recovery",
            "destruction",
        ],
    },
    "page_owner": {
        "states": [
            "unowned_clean",
            "owned_machine_plus_world_slot_plus_generation",
            "quarantined_prior_or_unknown_owner",
        ],
        "initial_state": "quarantined_unless_independent_cleanliness_proof",
        "missing_state": "never_implies_unowned_clean",
        "transitions": [
            "unowned_clean_to_owned",
            "owned_to_quarantined_after_exact_memory_ack",
            "quarantined_to_unowned_clean_after_trusted_scrub",
        ],
        "assign": "unowned_clean_only",
        "relabel_live_or_quarantined": "reject",
        "blockers": ["mapping", "dma_window", "machine_pin"],
        "checks_source": "live_el2_tables",
        "scrub_trust": (
            "el2_performed_or_hardware_or_cryptographic_verified_not_el1_assertion"
        ),
    },
    "completion": {
        "begin_result": ["rejected", "pending"],
        "query_operation": "query_completion",
        "query_result": ["pending", "complete", "failed"],
        "idempotency_key": (
            "operation_id_bound_to_machine_opcode_resource_world_old_generation_reason"
        ),
        "handle_binding": [
            "machine_incarnation",
            "resource_class",
            "world_slot",
            "world_generation",
            "resource_slot",
            "old_generation",
            "operation_id",
        ],
        "tombstone": (
            "generation_independent_until_durable_consume_and_explicit_retire"
        ),
        "rejected": "no_state_change",
        "retry_while_pending": "same_operation_id_continues_idempotently",
        "post_begin_failed": (
            "durable_idempotent_tombstone_resource_fenced_no_ack_no_resume_no_reuse"
        ),
        "failed_recovery": (
            "class_specific_new_operation_linked_to_failed_tombstone_teardown_or_reset_only"
        ),
        "failed_tombstone_retention": "until_recovery_ack_and_durable_consume",
        "query_after_generation_advance": (
            "exact_old_tuple_observation_only_no_live_authority"
        ),
        "el2_recovery": "machine_ack_tombstone_survives_host_el1_restart",
        "ocore_recovery": (
            "composite_host_ack_uses_crash_consistent_write_ahead_journal"
        ),
        "lost_state_policy": (
            "whole_machine_reset_selects_fresh_machine_incarnation_quarantines_resource_and_never_synthesizes_ack"
        ),
        "full_table": "reject_without_state_change",
    },
    "ack": {
        "machine_revoke_ack_fields": [
            "abi_version",
            "machine_incarnation",
            "domain_tag",
            "world_slot",
            "world_generation",
            "resource_slot",
            "old_generation",
            "new_generation",
            "operation_id",
            "result",
        ],
        "cross_domain_world_resource_or_operation_use": "reject",
    },
    "world_retirement": {
        "ocore_world_retire_requires": [
            "all_old_world_class_host_resource_acks",
            "broker_operations_terminal_and_journaled",
        ],
        "world_generation_advance_requires": [
            "all_old_world_vcpu_mapping_host_pin_dma_interrupt_effects_machine_acknowledged",
            "no_page_owned_by_old_world_generation",
        ],
        "quarantined_old_pages": (
            "safe_for_world_generation_advance_but_not_reassignment_until_scrubbed"
        ),
        "logical_bump_over_live_hardware": "forbidden",
    },
    "resource_class": [
        {
            "name": "MachineMemory",
            "begin": "begin_teardown_memory",
            "layer": "ocore_composite_with_el2_machine_ack",
            "prerequisites": ["dependent_dma_windows_acknowledged"],
            "el2_order": [
                "stop_or_exit_all_vcpus_and_quiesce_host_broker_access",
                "unmap_guest_and_host_broker_stage2_and_release_pins",
                "tlbi_all_affected_cpus",
                "architectural_drain",
                "checked_increment_generation",
                "machine_ack",
            ],
            "host_completion": "machine_ack",
            "guest_outcome": "teardown_not_recoverable_error",
        },
        {
            "name": "MachineBlock",
            "begin": "begin_withdraw_block",
            "layer": "ocore_protocol_broker_composite",
            "el1_order": [
                "stop_new_old_generation_descriptors",
                "classify_accepted_requests_at_backend_commit_point",
                "preserve_result_of_already_committed_operation",
                "complete_accepted_uncommitted_with_VIRTIO_BLK_S_IOERR",
                "drain_old_generation_backing",
                "publish_used_ring_while_queue_mapped",
                "enqueue_terminal_notification",
                "fence_old_interrupt_route",
                "retire_endpoint_generation",
            ],
            "el2_obligation": "fence_dependent_mapping_dma_interrupt_effects",
            "host_completion": "composite_broker_plus_machine_acks",
            "guest_outcome": "pinned_linux_consumes_completion_and_returns_EIO",
            "guest_health": "remains_healthy_after_error",
            "terminal_result_lifetime": (
                "immutable_nonreused_until_guest_consumed_or_explicit_memory_teardown"
            ),
        },
        {
            "name": "Machine9P",
            "begin": "begin_withdraw_9p",
            "layer": "ocore_protocol_broker_composite",
            "el1_order": [
                "stop_new_old_generation_tags_and_fids",
                "classify_accepted_requests_at_backend_commit_point",
                "preserve_result_of_already_committed_operation",
                "complete_accepted_uncommitted_with_negotiated_Rerror_or_Rlerror",
                "drain_old_generation_backend",
                "publish_terminal_response_or_used_ring_while_queue_mapped",
                "enqueue_terminal_notification_while_queue_mapped",
                "fence_old_interrupt_route",
                "retire_endpoint_generation",
            ],
            "el2_obligation": "fence_dependent_machine_effects",
            "host_completion": "composite_broker_plus_machine_acks",
            "guest_outcome": "pinned_linux_consumes_protocol_error",
            "guest_health": "remains_healthy_after_error",
            "terminal_result_lifetime": (
                "immutable_nonreused_until_guest_consumed_or_explicit_memory_teardown"
            ),
            "stale_fid": "reject_without_rebind",
        },
    ],
    "g8_physical_device": {
        "contract_status": (
            "concrete_device_class_extension_required_before_g8_qualification"
        ),
        "generic_revoke_verb": "forbidden",
        "minimum_obligations": [
            "quiesce_class_specific_broker",
            "fence_and_unmap_dma",
            "drain_and_invalidate_iommu_or_smmu",
            "withdraw_and_drain_generation_bound_interrupt_routes",
            "reset_device",
            "retire_resource_generation",
            "complete_composite_host_ack",
        ],
        "replacement": "after_composite_host_ack_only",
        "unrelated_worlds": "must_survive",
        "reset_verification": "class_specific_required",
        "reset_failure": "quarantine_no_ack_no_replacement",
        "shared_isolation_or_reset_group": (
            "dedicated_or_all_affected_worlds_quiesced_and_survival_proven"
        ),
    },
    "invariants": {
        "machine_ack": "no_machine_effect_authorized_by_old_generation_remains_reachable",
        "terminal_bytes": "inert_completed_observation_not_machine_effect_or_authority",
        "g7_memory_teardown_requires": ["host_resource_ack", "guest_error_consumed"],
        "g7_join_order": "no_order_required_between_host_ack_and_guest_consumption",
        "generation_increment": "not_completion_without_ack",
        "generation_alias": "zero_wrap_and_reuse_forbidden",
        "cold_reset_alias": "old_machine_incarnation_rejected",
        "old_generation_host_access_after_ack": "none",
    },
}


class WorldEvidenceError(RuntimeError):
    """The OSTADIX Alpha qualification registry is malformed or overclaims."""


def _require_string(value: Any, location: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise WorldEvidenceError(f"{location} must be a non-empty string")
    if value != value.strip():
        raise WorldEvidenceError(
            f"{location} must not have leading or trailing whitespace"
        )
    return value


def _require_string_list(
    value: Any, location: str, *, minimum: int = 0
) -> list[str]:
    if not isinstance(value, list) or len(value) < minimum:
        raise WorldEvidenceError(
            f"{location} must contain at least {minimum} string(s)"
        )
    result = [
        _require_string(item, f"{location}[{index}]")
        for index, item in enumerate(value)
    ]
    if len(result) != len(set(result)):
        raise WorldEvidenceError(f"{location} contains a duplicate")
    return result


def _working_tree_source_candidate(root: Path, text: str) -> Path:
    """Map only a removed pre-extraction engine path to its current owner."""
    path = PurePosixPath(text)
    candidate = root.joinpath(*path.parts)
    if (
        not candidate.exists()
        and not candidate.is_symlink()
        and text.startswith("src/")
        and not text.startswith("src/bin/")
        and text not in {"src/lib.rs", "src/main.rs"}
    ):
        return root / "crates" / "ostadix-api" / text
    return candidate


def _repo_file(root: Path, value: Any, location: str) -> tuple[str, Path]:
    text = _require_string(value, location)
    path = PurePosixPath(text)
    if (
        path.is_absolute()
        or ".." in path.parts
        or str(path) != text
        or "\\" in text
        or (len(text) >= 2 and text[1] == ":")
        or any(ord(character) < 0x20 for character in text)
    ):
        raise WorldEvidenceError(
            f"{location} must be a normalized repository-relative path"
        )
    root_resolved = root.resolve()
    candidate = _working_tree_source_candidate(root, text)
    # Exact-byte-sealed World records retain the pre-extraction `src/...`
    # coordinate they were minted with. Resolve only the physical lookup into
    # the independent engine; keep `text` unchanged for claim derivation and
    # diagnostics. Root library/CLI entrypoints are not engine records.
    try:
        resolved = candidate.resolve(strict=True)
        resolved.relative_to(root_resolved)
    except (FileNotFoundError, OSError, ValueError) as error:
        raise WorldEvidenceError(
            f"{location} references unsafe or absent file {text}"
        ) from error
    if candidate.is_symlink() or not resolved.is_file():
        raise WorldEvidenceError(f"{location} references absent file {text}")
    return text, resolved


def load_manifest(path: Path = MANIFEST) -> dict[str, Any]:
    try:
        with path.open("rb") as handle:
            value = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise WorldEvidenceError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise WorldEvidenceError("manifest root must be a TOML table")
    return value


def _strict_toml_file(path: Path, location: str) -> dict[str, Any]:
    try:
        with path.open("rb") as handle:
            value = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise WorldEvidenceError(f"cannot read {location}: {error}") from error
    if not isinstance(value, dict):
        raise WorldEvidenceError(f"{location} root must be a TOML table")
    return value


def _require_sha256(value: Any, location: str) -> str:
    digest = _require_string(value, location)
    if HEX_SHA256.fullmatch(digest) is None:
        raise WorldEvidenceError(f"{location} must be a lowercase SHA-256 digest")
    return digest


def _require_derivation_hash(value: Any, location: str) -> str:
    digest = _require_string(value, location)
    if not digest.startswith(DERIVATION_HASH_PREFIX) or HEX_SHA256.fullmatch(
        digest[len(DERIVATION_HASH_PREFIX) :]
    ) is None:
        raise WorldEvidenceError(
            f"{location} must be sha256 followed by a lowercase SHA-256 digest"
        )
    return digest


def _require_exact_table_rows(
    raw_rows: Any,
    location: str,
    keys: tuple[str, ...],
    expected: tuple[tuple[str, ...], ...],
) -> None:
    if not isinstance(raw_rows, list) or len(raw_rows) != len(expected):
        raise WorldEvidenceError(
            f"{location} must contain exactly {len(expected)} ordered tables"
        )
    actual: list[tuple[str, ...]] = []
    expected_keys = set(keys)
    for index, row in enumerate(raw_rows):
        owner = f"{location}[{index}]"
        if not isinstance(row, dict) or set(row) != expected_keys:
            raise WorldEvidenceError(f"{owner} keys differ from schema")
        actual.append(
            tuple(_require_string(row[key], f"{owner}.{key}") for key in keys)
        )
    if tuple(actual) != expected:
        raise WorldEvidenceError(f"{location} vocabulary or order differs from schema")


def _validate_world_contract(
    data: dict[str, Any], root: Path, class_scopes: dict[str, str]
) -> None:
    wrapper_text, wrapper_path = _repo_file(
        root, data["contract_schema"], "manifest.contract_schema"
    )
    if hashlib.sha256(wrapper_path.read_bytes()).hexdigest() != EXPECTED_WORLD_CONTRACT_V2_SHA256:
        raise WorldEvidenceError(
            "live World contract bytes drifted without a schema/version update"
        )
    wrapper = _strict_toml_file(wrapper_path, wrapper_text)
    expected_wrapper = {
        "schema": "ostadix.world-contract/v2",
        "schema_version": 2,
        "constitution_version": 3,
        "constitution": "docs/OSTADIX_WORLD.md",
        "constitution_sha256": EXPECTED_CONSTITUTION_SHA256,
        "world_gate_registry": "evidence/world_alpha_gates.toml",
        "imported_vocabulary": "evidence/world_contract_v1.toml",
        "imported_vocabulary_schema_version": 1,
        "imported_vocabulary_constitution_version": 2,
        "imported_vocabulary_constitution_sha256": (
            EXPECTED_IMPORTED_CONSTITUTION_V2_SHA256
        ),
        "imported_vocabulary_sha256": EXPECTED_WORLD_CONTRACT_V1_SHA256,
        "machine_contract": "evidence/o_machine_contract_v1.toml",
        "machine_contract_schema_version": 1,
        "machine_contract_sha256": EXPECTED_MACHINE_CONTRACT_SHA256,
        "composition": {
            "crossings": "imported_vocabulary",
            "identity_atoms": "imported_vocabulary",
            "failure_classes": "imported_vocabulary",
            "consistency_rules": "imported_vocabulary",
            "evidence_classes": "imported_vocabulary",
            "machine_authority_and_revocation": "machine_contract",
        },
    }
    if wrapper != expected_wrapper:
        raise WorldEvidenceError("live World contract composition differs from schema")
    if wrapper["constitution"] != data["constitution"]:
        raise WorldEvidenceError("live World contract constitution differs from registry")
    if wrapper["machine_contract"] != data["machine_contract_schema"]:
        raise WorldEvidenceError("live World contract machine schema differs from registry")
    imported_text, imported_path = _repo_file(
        root,
        wrapper["imported_vocabulary"],
        "World contract imported_vocabulary",
    )
    if hashlib.sha256(imported_path.read_bytes()).hexdigest() != EXPECTED_WORLD_CONTRACT_V1_SHA256:
        raise WorldEvidenceError("imported World vocabulary bytes differ from frozen v1")
    contract = _strict_toml_file(imported_path, imported_text)
    expected_keys = {
        "schema_version",
        "constitution_version",
        "constitution",
        "hosted_identity_schema",
        "native_identity_schema",
        "world_gate_registry",
        "crossing",
        "identity_atom",
        "failure_class",
        "consistency_rule",
        "claim_class",
    }
    if set(contract) != expected_keys:
        raise WorldEvidenceError("World contract root keys differ from schema")
    if type(contract["schema_version"]) is not int or contract["schema_version"] != 1:
        raise WorldEvidenceError("World contract schema_version must be 1")
    if (
        type(contract["constitution_version"]) is not int
        or contract["constitution_version"]
        != IMPORTED_WORLD_CONTRACT_CONSTITUTION_VERSION
    ):
        raise WorldEvidenceError(
            "World contract must remain the imported frozen constitution-v2 vocabulary"
        )
    if contract["world_gate_registry"] != "evidence/world_alpha_gates.toml":
        raise WorldEvidenceError("World contract must name the World gate registry")

    _require_exact_table_rows(
        contract["crossing"],
        "World contract crossing",
        ("id", "kind", "authority", "unknown_policy"),
        EXPECTED_CROSSINGS,
    )
    _require_exact_table_rows(
        contract["identity_atom"],
        "World contract identity_atom",
        ("id", "representation"),
        EXPECTED_IDENTITY_ATOMS,
    )
    _require_exact_table_rows(
        contract["failure_class"],
        "World contract failure_class",
        ("id", "terminal_rule"),
        EXPECTED_FAILURE_CLASSES,
    )
    _require_exact_table_rows(
        contract["consistency_rule"],
        "World contract consistency_rule",
        ("id", "rule"),
        EXPECTED_CONSISTENCY_RULES,
    )
    expected_claims = tuple(class_scopes.items())
    _require_exact_table_rows(
        contract["claim_class"],
        "World contract claim_class",
        ("id", "scope"),
        expected_claims,
    )

    _, constitution_path = _repo_file(
        root, data["constitution"], "World contract constitution"
    )
    _, hosted_identity_path = _repo_file(
        root,
        contract["hosted_identity_schema"],
        "World contract hosted_identity_schema",
    )
    _, native_identity_path = _repo_file(
        root,
        contract["native_identity_schema"],
        "World contract native_identity_schema",
    )
    constitution = constitution_path.read_text(encoding="utf-8")
    hosted_identity = hosted_identity_path.read_text(encoding="utf-8")
    native_identity = native_identity_path.read_text(encoding="utf-8")
    for crossing, *_ in EXPECTED_CROSSINGS:
        marker = {"ovalue": "OValue", "capability": "Capability", "capsule": "Capsule"}[
            crossing
        ]
        if marker not in constitution:
            raise WorldEvidenceError(
                f"constitution is missing crossing vocabulary {marker}"
            )
    for atom, _ in EXPECTED_IDENTITY_ATOMS:
        if re.search(rf"\b{re.escape(atom)}\b", constitution) is None:
            raise WorldEvidenceError(f"constitution is missing identity atom {atom}")
        if re.search(rf"\b{re.escape(atom)}\b", hosted_identity) is None:
            raise WorldEvidenceError(f"hosted identity schema is missing {atom}")
        if re.search(rf"\b{re.escape(atom)}\b", native_identity) is None:
            raise WorldEvidenceError(f"native identity schema is missing {atom}")
    for failure_class, _ in EXPECTED_FAILURE_CLASSES:
        if f"**{failure_class}**" not in constitution:
            raise WorldEvidenceError(
                f"constitution is missing failure class {failure_class}"
            )
    for marker in (
        "three-replica Raft-style consensus group",
        "A minority partition enters **island mode**.",
        "not transparent DSM",
        "## Evidence taxonomy",
    ):
        if marker not in constitution:
            raise WorldEvidenceError(
                f"constitution is missing consistency/claim marker {marker!r}"
            )


def _validate_machine_contract(data: dict[str, Any], root: Path) -> None:
    path_text, path = _repo_file(
        root,
        data["machine_contract_schema"],
        "manifest.machine_contract_schema",
    )
    if hashlib.sha256(path.read_bytes()).hexdigest() != EXPECTED_MACHINE_CONTRACT_SHA256:
        raise WorldEvidenceError(
            "O-Machine contract bytes drifted without a schema/version update"
        )
    contract = _strict_toml_file(path, path_text)
    if contract != EXPECTED_MACHINE_CONTRACT:
        raise WorldEvidenceError(
            "O-Machine contract vocabulary or resource-class semantics differ from schema"
        )
    specification_text, specification_path = _repo_file(
        root,
        contract["specification"],
        "O-Machine contract specification",
    )
    if (
        hashlib.sha256(specification_path.read_bytes()).hexdigest()
        != EXPECTED_MACHINE_SPEC_SHA256
    ):
        raise WorldEvidenceError(
            "O-Machine specification bytes drifted without a schema/version update"
        )
    try:
        specification = specification_path.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        raise WorldEvidenceError(
            f"{specification_text} must be valid UTF-8"
        ) from error
    for marker in (
        "# O-Machine EL2 and O-core Resource Contract",
        "MachineMemory",
        "MachineBlock",
        "Machine9P",
        "no machine effect authorized by s.old_generation remains reachable",
        "no `MachineHandle`, no revocation-completion handle",
        "a doorbell is not an authority-bearing hypercall",
    ):
        if marker not in specification:
            raise WorldEvidenceError(
                f"O-Machine specification is missing contract marker {marker!r}"
            )


LEGACY_CLAIM_MARKERS = {
    "world.contract_schema_consistent": ("G0 executable contract schema: PASS",),
    "world.crossing_taxonomy_consistent": ("G0 crossing taxonomy: PASS",),
    "world.identity_vocabulary_consistent": (
        "G0 identity vocabulary Rust/native: PASS",
    ),
    "world.failure_consistency_schema_consistent": (
        "G0 failure and consistency schemas: PASS",
    ),
    "evidence.claim_class_guarded": ("G0 claim-class substitution guard: PASS",),
    "aarch64.native_object": ("G2 AArch64 ocorec object: PASS",),
    "aarch64.el1_execution": ("G2 AArch64 EL1 kernel: online",),
    "aarch64.el0_execution": ("G2 AArch64 EL0 process lifecycle: PASS",),
    "aarch64.svc_eret_roundtrip": (
        "G2 AArch64 real SVC/ERET path: PASS",
    ),
    "ipc.request_reply": ("G2 AArch64 endpoint request/reply: PASS",),
    "capability.attenuation": (
        "G2 AArch64 attenuated capability read: PASS",
        "G2 AArch64 attenuated capability write: denied",
    ),
    "capability.stale_generation_rejected": (
        "G2 AArch64 process slot reuse stale denial: PASS",
        "G2 AArch64 capability slot reuse stale denial: PASS",
    ),
    "lifecycle.terminal": ("G2 AArch64 IPC capability lifecycle: PASS",),
    "lifecycle.reclamation": ("G2 AArch64 teardown and reclamation: PASS",),
}


def _canonical_json_bytes(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=True,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("ascii")


# This digest names only the claim derivation semantics, not the whole
# validator.  Changes elsewhere in this file therefore do not invalidate an
# attestation, while a rule/parser change must update this explicit spec and
# causes a re-derivation ledger edge.
DERIVATION_SPEC = {
    "version": DERIVATION_SPEC_VERSION,
    "observation_prefix": "@evidence ",
    "observation_syntax": "ascii-space-separated-unique-key-value-fields",
    "typed_match": "all-requirements-existential-subset",
    "claim_rules": CLAIM_RULES,
    "claim_context_rules": CLAIM_CONTEXT_RULES,
}


def _derivation_implementation_bytes(source: bytes) -> bytes:
    source = source.replace(b"\r\n", b"\n").replace(b"\r", b"\n")
    begin = b"# WORLD_CLAIM_DERIVATION_IMPLEMENTATION_BEGIN\n"
    end = b"# WORLD_CLAIM_DERIVATION_IMPLEMENTATION_END\n"
    if source.count(begin) != 1 or source.count(end) != 1:
        raise RuntimeError("claim derivation implementation markers are missing or duplicated")
    return source.split(begin, 1)[1].split(end, 1)[0]


def _derivation_implementation_sha256() -> str:
    try:
        source = Path(__file__).read_bytes()
    except OSError as error:
        raise RuntimeError(f"cannot bind claim derivation implementation: {error}") from error
    return hashlib.sha256(_derivation_implementation_bytes(source)).hexdigest()


DERIVATION_SPEC["implementation_source_sha256"] = _derivation_implementation_sha256()
CURRENT_DERIVATION_HASH = DERIVATION_HASH_PREFIX + hashlib.sha256(
    _canonical_json_bytes(DERIVATION_SPEC)
).hexdigest()
# The legacy field remains for attestation readability, but now binds the
# complete D specification (rules, context requirements, parser, and exact
# normalization/derivation implementation) instead of only declarative rows.
CLAIM_RULE_POLICY_SHA256 = CURRENT_DERIVATION_HASH[len(DERIVATION_HASH_PREFIX) :]


# WORLD_CLAIM_DERIVATION_IMPLEMENTATION_BEGIN
def _make_derivation_context(
    evidence_class: str,
    topology: dict[str, Any],
    sources: list[dict[str, Any]],
    artifacts: list[dict[str, Any]],
) -> dict[str, Any]:
    source_paths = {source["path"] for source in sources}
    artifact_names = {artifact["name"] for artifact in artifacts}
    artifact_kinds = {artifact["kind"] for artifact in artifacts}
    artifact_name_kinds = {
        artifact["name"]: artifact["kind"] for artifact in artifacts
    }
    artifact_bindings = {
        (
            artifact["name"],
            artifact["kind"],
            artifact["path"] if artifact["retained"] else "",
        )
        for artifact in artifacts
    }
    return {
        "evidence_class": evidence_class,
        "topology": dict(topology),
        "source_paths": set(source_paths),
        "artifact_names": set(artifact_names),
        "artifact_kinds": set(artifact_kinds),
        "artifact_name_kinds": dict(artifact_name_kinds),
        "artifact_bindings": set(artifact_bindings),
    }


def _parse_observations(transcript: str, location: str) -> list[dict[str, str]]:
    observations: list[dict[str, str]] = []
    for line_number, line in enumerate(transcript.splitlines(), 1):
        if not line.startswith("@evidence "):
            continue
        payload = line[len("@evidence ") :]
        tokens = payload.split(" ")
        if not payload or any(not token for token in tokens):
            raise WorldEvidenceError(
                f"{location}:{line_number} contains noncanonical evidence spacing"
            )
        fields: dict[str, str] = {}
        for token in tokens:
            if "=" not in token:
                raise WorldEvidenceError(
                    f"{location}:{line_number} evidence token lacks '='"
                )
            key, value = token.split("=", 1)
            if (
                re.fullmatch(r"[a-z][a-z0-9_]*", key) is None
                or not value
                or any(not 0x21 <= ord(character) <= 0x7E for character in value)
                or key in fields
            ):
                raise WorldEvidenceError(
                    f"{location}:{line_number} has an invalid/duplicate evidence field"
                )
            fields[key] = value
        if "event" not in fields:
            raise WorldEvidenceError(
                f"{location}:{line_number} evidence observation lacks event"
            )
        observations.append(fields)
    return observations


def _claim_context_satisfied(claim: str, context: dict[str, Any]) -> bool:
    rule = CLAIM_CONTEXT_RULES.get(claim)
    if rule is None:
        return True
    if context["evidence_class"] not in rule.get("evidence_classes", ()):
        return False
    topology = context["topology"]
    if any(topology.get(key) != value for key, value in rule.get("topology", ())):
        return False
    if not set(rule.get("source_paths", ())) <= context["source_paths"]:
        return False
    if not set(rule.get("artifact_names", ())) <= context["artifact_names"]:
        return False
    if not set(rule.get("artifact_kinds", ())) <= context["artifact_kinds"]:
        return False
    if not set(rule.get("artifact_bindings", ())) <= context["artifact_bindings"]:
        return False
    return True


def _g8_physical_observation_satisfied(
    observation: dict[str, str], context: dict[str, Any]
) -> bool:
    required = {
        "event": "g8_physical_withdrawal_lifecycle",
        "order": "quiesce_dma_iommu_irq_reset_generation_ack_replacement",
        "reset_verification": "class_specific_pass",
        "reset_failure": "quarantine_no_ack_no_replacement",
        "shared_group": "dedicated_or_all_affected_quiesced_survival_proven",
        "unrelated_world": "healthy",
        "result": "pass",
    }
    if any(observation.get(key) != value for key, value in required.items()):
        return False
    device_class = observation.get("device_class", "")
    if re.fullmatch(r"[a-z][a-z0-9_-]*", device_class) is None:
        return False
    if device_class in {"device", "generic", "physical_device"}:
        return False
    if observation.get("withdraw_operation") != f"begin_withdraw_{device_class}":
        return False
    artifact_name_kinds = context["artifact_name_kinds"]
    if artifact_name_kinds.get(
        f"device-class-{device_class}"
    ) != "physical-device-inventory":
        return False
    if artifact_name_kinds.get(
        f"{device_class}-withdrawal-trace"
    ) != "dma-iommu-withdrawal-trace":
        return False
    guest_machine_abi = observation.get("guest_machine_abi")
    if guest_machine_abi == "none":
        return (
            observation.get("handle_mac") == "not_required"
            and observation.get("key_lifecycle") == "not_applicable"
        )
    if guest_machine_abi == "direct":
        return (
            observation.get("handle_mac") == "verified"
            and observation.get("key_lifecycle") == "verified"
            and "handle-mac-key-lifecycle-trace" in context["artifact_kinds"]
        )
    return False


def _derive_claims(
    transcript: str,
    location: str,
    context: dict[str, Any],
    *,
    allow_legacy_markers: bool = False,
) -> set[str]:
    observations = _parse_observations(transcript, location)
    claims: set[str] = set()
    for claim, requirements in CLAIM_RULES:
        if _claim_context_satisfied(claim, context) and all(
            any(
                observation.get("event") == event
                and all(observation.get(key) == value for key, value in fields)
                for observation in observations
            )
            for event, fields in requirements
        ):
            claims.add(claim)
    g8_claim = "driver.g8_physical_withdrawal_lifecycle"
    if _claim_context_satisfied(g8_claim, context) and any(
        _g8_physical_observation_satisfied(observation, context)
        for observation in observations
    ):
        claims.add(g8_claim)
    if allow_legacy_markers:
        transcript_lines = set(transcript.splitlines())
        for claim, markers in LEGACY_CLAIM_MARKERS.items():
            if _claim_context_satisfied(claim, context) and all(
                marker in transcript_lines for marker in markers
            ):
                claims.add(claim)
    return claims
# WORLD_CLAIM_DERIVATION_IMPLEMENTATION_END


def _resolve_source_snapshot(
    root: Path, source_commit: str, source_digests: dict[str, str]
) -> str | None:
    try:
        working_tree_matches = all(
            hashlib.sha256(
                _working_tree_source_candidate(root, path).read_bytes()
            ).hexdigest()
            == digest
            for path, digest in source_digests.items()
        )
    except OSError:
        working_tree_matches = False

    def commit_matches(commit: str) -> bool:
        for path, digest in source_digests.items():
            candidate = subprocess.run(
                ["git", "-C", str(root), "show", f"{commit}:{path}"],
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
            )
            if (
                candidate.returncode != 0
                or hashlib.sha256(candidate.stdout).hexdigest() != digest
            ):
                return False
        return True

    try:
        head = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        if head.returncode != 0:
            return None
        head_commit = head.stdout.strip()
        source_on_head_lineage = subprocess.run(
            [
                "git",
                "-C",
                str(root),
                "merge-base",
                "--is-ancestor",
                source_commit,
                head_commit,
            ],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if source_on_head_lineage.returncode != 0:
            return None
        if working_tree_matches:
            if commit_matches(head_commit):
                return head_commit
            return "content-addressed-working-tree"
        if commit_matches(source_commit):
            return source_commit
        history = subprocess.run(
            ["git", "-C", str(root), "rev-list", "HEAD", "--topo-order"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        if history.returncode != 0:
            return None
        for commit in history.stdout.splitlines():
            ancestry = subprocess.run(
                [
                    "git",
                    "-C",
                    str(root),
                    "merge-base",
                    "--is-ancestor",
                    source_commit,
                    commit,
                ],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            if ancestry.returncode == 0 and commit_matches(commit):
                return commit
    except OSError:
        return None
    return None


def _source_digest_matches(
    root: Path, source_path: Path, source_text: str, source_commit: str, digest: str
) -> bool:
    # Compatibility wrapper for focused callers; full attestations use one
    # coherent snapshot for their complete source set.
    del source_path
    return _resolve_source_snapshot(root, source_commit, {source_text: digest}) is not None


def _require_git_commit(root: Path, commit: str, location: str) -> None:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "cat-file", "-e", f"{commit}^{{commit}}"],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except OSError as error:
        raise WorldEvidenceError(f"{location} cannot be verified as a Git commit") from error
    if result.returncode != 0:
        raise WorldEvidenceError(f"{location} does not resolve to a Git commit")


def _validate_attestation(
    root: Path,
    path_value: Any,
    gate_id: str,
    class_ids: set[str],
    registry_semantics_sha256: str,
) -> dict[str, Any]:
    path_text, attestation_path = _repo_file(
        root, path_value, f"{gate_id}.evidence path"
    )
    attestation = _strict_toml_file(attestation_path, path_text)
    common_keys = {
        "schema_version",
        "id",
        "gate",
        "evidence_class",
        "source_commit",
        "source_state",
        "command",
        "transcript",
        "transcript_sha256",
        "topology",
        "nonclaims",
        "expected_markers",
        "source",
        "artifact",
        "signatures",
    }
    schema_version = attestation.get("schema_version")
    if type(schema_version) is not int or schema_version not in {1, 2, 3}:
        raise WorldEvidenceError(
            f"attestation {path_text} schema_version must be 1, 2, or 3"
        )
    if schema_version == 1:
        version_keys = {"claims"}
    else:
        version_keys = {
            "derived_claims",
            "validator_sha256",
            "claim_rule_policy_sha256",
            "registry_semantics_sha256",
        }
        if schema_version == 3:
            version_keys.add("derivation_hash")
    if set(attestation) != common_keys | version_keys:
        raise WorldEvidenceError(f"attestation {path_text} keys differ from schema")
    attestation_id = _require_string(attestation["id"], f"{path_text}.id")
    if ATTESTATION_ID.fullmatch(attestation_id) is None:
        raise WorldEvidenceError(f"{path_text}.id is not a normalized identifier")
    if attestation["gate"] != gate_id:
        raise WorldEvidenceError(f"{path_text}.gate must be {gate_id}")
    evidence_class = _require_string(
        attestation["evidence_class"], f"{path_text}.evidence_class"
    )
    if evidence_class not in class_ids:
        raise WorldEvidenceError(f"{path_text} references unknown evidence class")
    source_commit = _require_string(
        attestation["source_commit"], f"{path_text}.source_commit"
    )
    if HEX_COMMIT.fullmatch(source_commit) is None:
        raise WorldEvidenceError(f"{path_text}.source_commit must be a Git object ID")
    _require_git_commit(root, source_commit, f"{path_text}.source_commit")
    if attestation["source_state"] != "content-addressed-working-tree":
        raise WorldEvidenceError(
            f"{path_text}.source_state must be content-addressed-working-tree"
        )
    command = _require_string_list(
        attestation["command"], f"{path_text}.command", minimum=1
    )
    if not command[0].startswith("./") or len(command) != 1:
        raise WorldEvidenceError(
            f"{path_text}.command must be one repository-owned executable"
        )
    command_path = command[0][2:]
    _, command_file = _repo_file(root, command_path, f"{path_text}.command[0]")
    if command_file.stat().st_mode & 0o111 == 0:
        raise WorldEvidenceError(f"{path_text}.command[0] is not executable")

    transcript_text, transcript_path = _repo_file(
        root, attestation["transcript"], f"{path_text}.transcript"
    )
    transcript_bytes = transcript_path.read_bytes()
    expected_transcript_digest = _require_sha256(
        attestation["transcript_sha256"], f"{path_text}.transcript_sha256"
    )
    if hashlib.sha256(transcript_bytes).hexdigest() != expected_transcript_digest:
        raise WorldEvidenceError(f"{path_text} transcript digest does not match")
    try:
        transcript = transcript_bytes.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise WorldEvidenceError(f"{transcript_text} must be UTF-8") from error
    required_header_lines = (
        "WORLD_ALPHA_ATTESTATION_TRANSCRIPT_V1",
        f"gate={gate_id}",
        f"evidence_class={evidence_class}",
        f"source_commit={source_commit}",
        f"command={command[0]}",
    )
    for line in required_header_lines:
        if transcript.splitlines().count(line) != 1:
            raise WorldEvidenceError(
                f"{transcript_text} must contain exactly one {line!r} line"
            )
    recorded_claims: set[str] = set()
    derivation_hash: str | None = None
    if schema_version >= 2:
        recorded_claims = _require_string_list(
            attestation["derived_claims"],
            f"{path_text}.derived_claims",
            minimum=1,
        )
        if recorded_claims != sorted(set(recorded_claims)):
            raise WorldEvidenceError(
                f"{path_text}.derived_claims must be sorted and unique"
            )
        recorded_claims = set(recorded_claims)
        validator_sha256 = _require_sha256(
            attestation["validator_sha256"], f"{path_text}.validator_sha256"
        )
        claim_rule_policy_sha256 = _require_sha256(
            attestation["claim_rule_policy_sha256"],
            f"{path_text}.claim_rule_policy_sha256",
        )
        registry_semantics_sha256_value = _require_sha256(
            attestation["registry_semantics_sha256"],
            f"{path_text}.registry_semantics_sha256",
        )
        if schema_version == 2:
            # Schema v2 predates a derivation-only identifier.  Its exact
            # validator digest is the only immutable identifier that captures
            # the derivation implementation used for its recorded claims.
            derivation_hash = DERIVATION_HASH_PREFIX + validator_sha256
        else:
            derivation_hash = _require_derivation_hash(
                attestation["derivation_hash"], f"{path_text}.derivation_hash"
            )
    markers = _require_string_list(
        attestation["expected_markers"],
        f"{path_text}.expected_markers",
        minimum=1,
    )
    marker_positions: list[int] = []
    for marker in markers:
        if transcript.splitlines().count(marker) != 1:
            raise WorldEvidenceError(
                f"{transcript_text} must contain marker exactly once: {marker}"
            )
        marker_positions.append(transcript.index(marker))
    if marker_positions != sorted(marker_positions):
        raise WorldEvidenceError(f"{transcript_text} markers are not in causal order")

    topology = attestation["topology"]
    topology_keys = {
        "kind",
        "architecture",
        "machine",
        "acceleration",
        "cpu_count",
        "inventory",
    }
    if not isinstance(topology, dict) or set(topology) != topology_keys:
        raise WorldEvidenceError(f"{path_text}.topology keys differ from schema")
    for field in ("kind", "architecture", "machine", "acceleration"):
        _require_string(topology[field], f"{path_text}.topology.{field}")
    if type(topology["cpu_count"]) is not int or topology["cpu_count"] < 0:
        raise WorldEvidenceError(f"{path_text}.topology.cpu_count must be nonnegative")
    _require_string_list(
        topology["inventory"], f"{path_text}.topology.inventory", minimum=1
    )
    if evidence_class == "repository_conformance":
        if topology["kind"] != "repository" or topology["acceleration"] != "none":
            raise WorldEvidenceError(
                f"{path_text} repository evidence has an invalid topology"
            )
    if evidence_class == "qemu_tcg_aarch64":
        if (
            topology["kind"] != "virtual"
            or topology["architecture"] != "aarch64"
            or topology["acceleration"] != "tcg"
            or topology["cpu_count"] != 1
            or "virt" not in topology["machine"]
        ):
            raise WorldEvidenceError(
                f"{path_text} does not describe the required one-vCPU AArch64 TCG virt topology"
            )

    if schema_version == 1:
        _require_string_list(attestation["claims"], f"{path_text}.claims", minimum=1)
    nonclaims = _require_string_list(
        attestation["nonclaims"], f"{path_text}.nonclaims", minimum=1
    )
    if evidence_class in NONCLAIM_FLOORS:
        nonclaim_text = " ".join(nonclaims)
        for fragment in NONCLAIM_FLOORS[evidence_class]:
            if fragment not in nonclaim_text:
                raise WorldEvidenceError(
                    f"{path_text}.nonclaims is missing boundary {fragment!r}"
                )

    sources = attestation["source"]
    if not isinstance(sources, list) or not sources:
        raise WorldEvidenceError(f"{path_text}.source must contain file digests")
    seen_sources: set[str] = set()
    source_digests: dict[str, str] = {}
    for index, source in enumerate(sources):
        owner = f"{path_text}.source[{index}]"
        if not isinstance(source, dict) or set(source) != {"path", "sha256"}:
            raise WorldEvidenceError(f"{owner} keys differ from schema")
        source_text, source_path = _repo_file(root, source["path"], f"{owner}.path")
        if source_text in seen_sources:
            raise WorldEvidenceError(f"{path_text}.source contains a duplicate path")
        seen_sources.add(source_text)
        digest = _require_sha256(source["sha256"], f"{owner}.sha256")
        source_digests[source_text] = digest
        del source_path
    snapshot_digests = dict(source_digests)
    if schema_version >= 2:
        validator_source_digest = source_digests.get(
            "scripts/world_alpha_evidence.py"
        )
        if validator_source_digest is not None:
            if validator_source_digest != validator_sha256:
                raise WorldEvidenceError(
                    f"{path_text}.validator_sha256 must match its validator source digest"
                )
        else:
            # The immutable G2 schema-v2 record omitted the validator row. Add
            # its pinned digest to the coherent snapshot lookup rather than
            # mutating the historical attestation.
            snapshot_digests["scripts/world_alpha_evidence.py"] = validator_sha256
    resolved_source_snapshot = _resolve_source_snapshot(
        root, source_commit, snapshot_digests
    )
    if resolved_source_snapshot is None:
        raise WorldEvidenceError(
            f"{path_text}.source digests do not resolve to one working tree, base commit, "
            "or descendant commit"
        )

    artifacts = attestation["artifact"]
    if not isinstance(artifacts, list) or not artifacts:
        raise WorldEvidenceError(f"{path_text}.artifact must contain artifact digests")
    artifact_names: set[str] = set()
    for index, artifact in enumerate(artifacts):
        owner = f"{path_text}.artifact[{index}]"
        if not isinstance(artifact, dict) or set(artifact) != {
            "name",
            "kind",
            "sha256",
            "retained",
            "path",
        }:
            raise WorldEvidenceError(f"{owner} keys differ from schema")
        name = _require_string(artifact["name"], f"{owner}.name")
        if name in artifact_names:
            raise WorldEvidenceError(f"{path_text}.artifact contains a duplicate name")
        artifact_names.add(name)
        kind = _require_string(artifact["kind"], f"{owner}.kind")
        digest = _require_sha256(artifact["sha256"], f"{owner}.sha256")
        if type(artifact["retained"]) is not bool:
            raise WorldEvidenceError(f"{owner}.retained must be boolean")
        if artifact["retained"]:
            artifact_text, artifact_path = _repo_file(
                root, artifact["path"], f"{owner}.path"
            )
            current_digest = hashlib.sha256(artifact_path.read_bytes()).hexdigest()
            # A superseded attestation may retain a path whose current bytes
            # belong to a later constitutional revision. Its immutable source
            # snapshot still has to bind the exact historical artifact digest;
            # never require append-only evidence records to rewrite that path.
            historical_digest = source_digests.get(artifact_text)
            if current_digest != digest and historical_digest != digest:
                raise WorldEvidenceError(
                    f"{owner} digest matches neither current nor source-snapshot "
                    f"bytes for retained {artifact_text}"
                )
        else:
            if artifact["path"] != "":
                raise WorldEvidenceError(f"{owner}.path must be empty when not retained")
            digest_line = f"artifact:{name}:sha256={digest}"
            if transcript.splitlines().count(digest_line) != 1:
                raise WorldEvidenceError(
                    f"{transcript_text} must bind non-retained artifact {name}"
                )
    if attestation["signatures"] != []:
        raise WorldEvidenceError(
            f"{path_text}.signatures must be empty for repository/virtual evidence"
        )
    current_derived_claims = _derive_claims(
        transcript,
        transcript_text,
        _make_derivation_context(
            evidence_class,
            topology,
            sources,
            artifacts,
        ),
        allow_legacy_markers=schema_version == 1,
    )
    if schema_version == 3 and derivation_hash == CURRENT_DERIVATION_HASH:
        if recorded_claims != current_derived_claims:
            raise WorldEvidenceError(
                f"{path_text}.derived_claims differs from its pinned derivation"
            )
        if claim_rule_policy_sha256 != CLAIM_RULE_POLICY_SHA256:
            raise WorldEvidenceError(
                f"{path_text}.claim_rule_policy_sha256 differs from claim rules"
            )
        if registry_semantics_sha256_value not in ACCEPTED_REGISTRY_SEMANTICS_SHA256:
            raise WorldEvidenceError(
                f"{path_text}.registry_semantics_sha256 is not an accepted immutable registry identity"
            )
    return {
        "id": attestation_id,
        "path": path_text,
        "gate": gate_id,
        "class": evidence_class,
        "recorded_claims": recorded_claims,
        "current_derived_claims": current_derived_claims,
        "derived_claims": set(),
        "derivation_hash": derivation_hash,
        "schema_version": schema_version,
        "resolved_source_snapshot": resolved_source_snapshot,
        "record_sha256": hashlib.sha256(attestation_path.read_bytes()).hexdigest(),
    }


def _rederive_payload_sha256(event: dict[str, Any]) -> str:
    payload_keys = (
        "schema_version",
        "id",
        "event",
        "subject",
        "prior_derivation",
        "current_derivation",
        "claims_lost",
        "claims_gained",
        "reason_code",
        "reason",
        "source_commit",
    )
    payload = {key: event[key] for key in payload_keys}
    preimage = (
        REDERIVE_PAYLOAD_DOMAIN.encode("ascii")
        + b"\0"
        + _canonical_json_bytes(payload)
    )
    return hashlib.sha256(preimage).hexdigest()


def _witness_payload_sha256(event: dict[str, Any]) -> str:
    payload_keys = (
        "schema_version",
        "id",
        "event",
        "subject",
        "subject_record_sha256",
        "algorithm",
        "key_id",
        "public_key",
        "run_identity",
        "source_commit",
        "verification",
    )
    payload = {key: event[key] for key in payload_keys}
    preimage = (
        WITNESS_PAYLOAD_DOMAIN.encode("ascii")
        + b"\0"
        + _canonical_json_bytes(payload)
    )
    return hashlib.sha256(preimage).hexdigest()


def _validate_witness_event(
    root: Path, path_text: str, event: dict[str, Any]
) -> dict[str, Any]:
    expected_keys = {
        "schema_version",
        "id",
        "event",
        "subject",
        "subject_record_sha256",
        "witness_payload_sha256",
        "algorithm",
        "key_id",
        "public_key",
        "signature",
        "run_identity",
        "source_commit",
        "verification",
    }
    if set(event) != expected_keys:
        raise WorldEvidenceError(f"witness event {path_text} keys differ from schema")
    if type(event["schema_version"]) is not int or event["schema_version"] != 1:
        raise WorldEvidenceError(f"witness event {path_text} schema_version must be 1")
    event_id = _require_string(event["id"], f"{path_text}.id")
    subject = _require_string(event["subject"], f"{path_text}.subject")
    key_id = _require_string(event["key_id"], f"{path_text}.key_id")
    for field, value in (("id", event_id), ("subject", subject), ("key_id", key_id)):
        if ATTESTATION_ID.fullmatch(value) is None:
            raise WorldEvidenceError(f"{path_text}.{field} is not normalized")
    record_sha256 = _require_sha256(
        event["subject_record_sha256"], f"{path_text}.subject_record_sha256"
    )
    if event["algorithm"] != "ed25519":
        raise WorldEvidenceError(f"{path_text}.algorithm must be ed25519")
    if re.fullmatch(r"[0-9a-f]{64}", str(event["public_key"])) is None:
        raise WorldEvidenceError(f"{path_text}.public_key must be 32-byte lowercase hex")
    if event["public_key"] == "0" * 64:
        raise WorldEvidenceError(f"{path_text}.public_key must not be all zero")
    if re.fullmatch(r"[0-9a-f]{128}", str(event["signature"])) is None:
        raise WorldEvidenceError(f"{path_text}.signature must be 64-byte lowercase hex")
    if event["signature"] == "0" * 128:
        raise WorldEvidenceError(f"{path_text}.signature must not be all zero")
    _require_string(event["run_identity"], f"{path_text}.run_identity")
    if event["verification"] != "external_unverified":
        raise WorldEvidenceError(
            f"{path_text}.verification must state external_unverified"
        )
    source_commit = _require_string(
        event["source_commit"], f"{path_text}.source_commit"
    )
    if HEX_COMMIT.fullmatch(source_commit) is None:
        raise WorldEvidenceError(f"{path_text}.source_commit must be a Git object ID")
    _require_git_commit(root, source_commit, f"{path_text}.source_commit")
    witness_payload_sha256 = _require_sha256(
        event["witness_payload_sha256"], f"{path_text}.witness_payload_sha256"
    )
    if witness_payload_sha256 != _witness_payload_sha256(event):
        raise WorldEvidenceError(
            f"{path_text}.witness_payload_sha256 does not bind the detached signature preimage"
        )
    return {
        "id": event_id,
        "path": path_text,
        "event": "witness",
        "subject": subject,
        "subject_record_sha256": record_sha256,
        "witness_payload_sha256": witness_payload_sha256,
        "verification": "external_unverified",
        "record_sha256": hashlib.sha256((root / path_text).read_bytes()).hexdigest(),
    }


def _validate_evidence_event(root: Path, path: Path) -> dict[str, Any]:
    path_text = path.relative_to(root).as_posix()
    event = _strict_toml_file(path, path_text)
    kind = _require_string(event.get("event"), f"{path_text}.event")
    if kind == "witness":
        return _validate_witness_event(root, path_text, event)
    lifecycle_keys = {
        "schema_version",
        "id",
        "event",
        "subject",
        "replacement",
        "reason_code",
        "reason",
        "source_commit",
        "signatures",
    }
    rederive_keys = {
        "schema_version",
        "id",
        "event",
        "subject",
        "prior_derivation",
        "current_derivation",
        "claims_lost",
        "claims_gained",
        "reason_code",
        "reason",
        "source_commit",
        "payload_sha256",
        "signatures",
    }
    expected_keys = rederive_keys if kind == "rederive" else lifecycle_keys
    if set(event) != expected_keys:
        raise WorldEvidenceError(f"evidence event {path_text} keys differ from schema")
    if type(event["schema_version"]) is not int or event["schema_version"] != 1:
        raise WorldEvidenceError(f"evidence event {path_text} schema_version must be 1")
    event_id = _require_string(event["id"], f"{path_text}.id")
    subject = _require_string(event["subject"], f"{path_text}.subject")
    for field, value in (("id", event_id), ("subject", subject)):
        if ATTESTATION_ID.fullmatch(value) is None:
            raise WorldEvidenceError(f"{path_text}.{field} is not normalized")
    if kind not in {"supersede", "retract", "rederive"}:
        raise WorldEvidenceError(
            f"{path_text}.event must be supersede, retract, rederive, or witness"
        )
    replacement = ""
    prior_derivation = ""
    current_derivation = ""
    claims_lost: set[str] = set()
    claims_gained: set[str] = set()
    if kind in {"supersede", "retract"}:
        replacement = event["replacement"]
        if not isinstance(replacement, str):
            raise WorldEvidenceError(f"{path_text}.replacement must be a string")
        if kind == "supersede":
            replacement = _require_string(replacement, f"{path_text}.replacement")
            if ATTESTATION_ID.fullmatch(replacement) is None:
                raise WorldEvidenceError(f"{path_text}.replacement is not normalized")
            if replacement == subject:
                raise WorldEvidenceError(
                    f"{path_text} cannot supersede an attestation with itself"
                )
        elif replacement != "":
            raise WorldEvidenceError(
                f"{path_text}.replacement must be empty for retraction"
            )
    else:
        prior_derivation = _require_derivation_hash(
            event["prior_derivation"], f"{path_text}.prior_derivation"
        )
        current_derivation = _require_derivation_hash(
            event["current_derivation"], f"{path_text}.current_derivation"
        )
        if current_derivation == prior_derivation:
            raise WorldEvidenceError(
                f"{path_text} must change the derivation identifier"
            )
        lost_list = _require_string_list(
            event["claims_lost"], f"{path_text}.claims_lost"
        )
        gained_list = _require_string_list(
            event["claims_gained"], f"{path_text}.claims_gained"
        )
        if lost_list != sorted(lost_list) or gained_list != sorted(gained_list):
            raise WorldEvidenceError(
                f"{path_text} claim deltas must be sorted and unique"
            )
        claims_lost = set(lost_list)
        claims_gained = set(gained_list)
        overlap = claims_lost & claims_gained
        if overlap:
            raise WorldEvidenceError(
                f"{path_text} cannot both lose and gain claims {sorted(overlap)}"
            )
    _require_string(event["reason_code"], f"{path_text}.reason_code")
    _require_string(event["reason"], f"{path_text}.reason")
    source_commit = _require_string(event["source_commit"], f"{path_text}.source_commit")
    if HEX_COMMIT.fullmatch(source_commit) is None:
        raise WorldEvidenceError(f"{path_text}.source_commit must be a Git object ID")
    _require_git_commit(root, source_commit, f"{path_text}.source_commit")
    if kind == "rederive":
        payload_sha256 = _require_sha256(
            event["payload_sha256"], f"{path_text}.payload_sha256"
        )
        if payload_sha256 != _rederive_payload_sha256(event):
            raise WorldEvidenceError(
                f"{path_text}.payload_sha256 does not bind the rederive event"
            )
    if event["signatures"] != []:
        raise WorldEvidenceError(
            f"{path_text}.signatures must be empty for repository-authored events"
        )
    return {
        "id": event_id,
        "path": path_text,
        "event": kind,
        "subject": subject,
        "replacement": replacement,
        "prior_derivation": prior_derivation,
        "current_derivation": current_derivation,
        "claims_lost": claims_lost,
        "claims_gained": claims_gained,
        "payload_sha256": payload_sha256 if kind == "rederive" else "",
        "record_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }


def _active_evidence_ledger(
    root: Path, class_ids: set[str], registry_semantics_sha256: str
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    ledger_dir = root / "evidence/world"
    paths = sorted(ledger_dir.glob("*.toml"))
    attestations: list[dict[str, Any]] = []
    events: list[dict[str, Any]] = []
    for path in paths:
        raw = _strict_toml_file(path, path.relative_to(root).as_posix())
        if "event" in raw:
            events.append(_validate_evidence_event(root, path))
            continue
        gate_id = _require_string(raw.get("gate"), f"{path}.gate")
        if gate_id not in EXPECTED_GATE_IDS:
            raise WorldEvidenceError(f"{path} references unknown gate {gate_id}")
        attestations.append(
            _validate_attestation(
                root,
                path.relative_to(root).as_posix(),
                gate_id,
                class_ids,
                registry_semantics_sha256,
            )
        )

    by_id: dict[str, dict[str, Any]] = {}
    for item in [*attestations, *events]:
        if item["id"] in by_id:
            raise WorldEvidenceError(f"evidence ledger ID {item['id']} is reused")
        by_id[item["id"]] = item
    attestations_by_id = {item["id"]: item for item in attestations}
    events_by_id = {item["id"]: item for item in events}
    for witness in (item for item in events if item["event"] == "witness"):
        subject = witness["subject"]
        target = events_by_id.get(subject)
        if target is None or target["event"] == "witness":
            raise WorldEvidenceError(
                f"{witness['path']} must witness one exact non-witness event record"
            )
        if witness["subject_record_sha256"] != target["record_sha256"]:
            raise WorldEvidenceError(
                f"{witness['path']} does not bind its exact subject record"
            )
    lifecycle_by_subject: dict[str, dict[str, Any]] = {}
    rederive_by_subject: dict[str, dict[str, dict[str, Any]]] = {}
    for event in events:
        if event["event"] == "witness":
            continue
        subject = event["subject"]
        if subject not in attestations_by_id:
            raise WorldEvidenceError(
                f"{event['path']} references missing subject {subject}"
            )
        if event["event"] == "rederive":
            attestation = attestations_by_id[subject]
            if attestation["derivation_hash"] is None:
                raise WorldEvidenceError(
                    f"{event['path']} cannot rederive schema-v1 prose claims"
                )
            transitions = rederive_by_subject.setdefault(subject, {})
            prior = event["prior_derivation"]
            if prior in transitions:
                raise WorldEvidenceError(
                    f"attestation {subject} has competing rederive events from {prior}"
                )
            transitions[prior] = event
            continue
        if subject in lifecycle_by_subject:
            raise WorldEvidenceError(
                f"attestation {subject} has multiple competing lifecycle events"
            )
        replacement = event["replacement"]
        if replacement:
            if replacement not in attestations_by_id:
                raise WorldEvidenceError(
                    f"{event['path']} references missing replacement {replacement}"
                )
            if attestations_by_id[replacement]["gate"] != attestations_by_id[subject]["gate"]:
                raise WorldEvidenceError(
                    f"{event['path']} supersedes evidence across unrelated gates"
                )
        lifecycle_by_subject[subject] = event

    # Every successor chain must terminate, and no attestation may be the
    # replacement of two unrelated active histories.
    replacement_predecessors: dict[str, str] = {}
    for subject, event in lifecycle_by_subject.items():
        replacement = event["replacement"]
        if replacement:
            previous = replacement_predecessors.setdefault(replacement, subject)
            if previous != subject:
                raise WorldEvidenceError(
                    f"replacement {replacement} has competing predecessors"
                )
        seen: set[str] = set()
        current = subject
        while current in lifecycle_by_subject:
            if current in seen:
                raise WorldEvidenceError("evidence supersession graph contains a cycle")
            seen.add(current)
            current = lifecycle_by_subject[current]["replacement"]
            if not current:
                break

    inactive_ids = set(lifecycle_by_subject)

    # Re-derivation is independent of lifecycle history: it neither retires nor
    # replaces an attestation.  Follow an exact, non-forking chain from the
    # immutable record's derivation and replay each declared claim delta.
    for attestation in attestations:
        if attestation["schema_version"] == 1:
            if attestation["id"] not in inactive_ids:
                raise WorldEvidenceError(
                    f"schema-v1 attestation {attestation['id']} cannot be an active ledger head"
                )
            continue
        subject = attestation["id"]
        transitions = rederive_by_subject.get(subject, {})
        derivation = attestation["derivation_hash"]
        claims = set(attestation["recorded_claims"])
        seen_derivations = {derivation}
        used_priors: set[str] = set()
        while derivation in transitions:
            transition = transitions[derivation]
            used_priors.add(derivation)
            missing = transition["claims_lost"] - claims
            if missing:
                raise WorldEvidenceError(
                    f"{transition['path']} loses absent claims {sorted(missing)}"
                )
            already_present = transition["claims_gained"] & claims
            if already_present:
                raise WorldEvidenceError(
                    f"{transition['path']} gains existing claims {sorted(already_present)}"
                )
            claims = (claims - transition["claims_lost"]) | transition["claims_gained"]
            derivation = transition["current_derivation"]
            if derivation in seen_derivations:
                raise WorldEvidenceError(
                    f"attestation {subject} rederive graph contains a cycle"
                )
            seen_derivations.add(derivation)
        unreachable = set(transitions) - used_priors
        if unreachable:
            raise WorldEvidenceError(
                f"attestation {subject} has unreachable rederive prior(s) {sorted(unreachable)}"
            )
        if derivation != CURRENT_DERIVATION_HASH:
            raise WorldEvidenceError(
                f"active derivation for attestation {subject} is {derivation}, "
                f"expected {CURRENT_DERIVATION_HASH}; append a rederive event"
            )
        if claims != attestation["current_derived_claims"]:
            lost = sorted(claims - attestation["current_derived_claims"])
            gained = sorted(attestation["current_derived_claims"] - claims)
            raise WorldEvidenceError(
                f"attestation {subject} rederive delta differs from current derivation; "
                f"undeclared_lost={lost}, undeclared_gained={gained}"
            )
        attestation["derived_claims"] = claims

    active = [item for item in attestations if item["id"] not in inactive_ids]
    for attestation in active:
        if (
            attestation["schema_version"] != 3
            and attestation["id"] not in LEGACY_ACTIVE_SCHEMA2_IDS
        ):
            raise WorldEvidenceError(
                f"active attestation {attestation['id']} must use schema v3; "
                "only the explicitly pinned G2 legacy exception is allowed"
            )
    return active, events


def _validate_constitution(data: dict[str, Any], root: Path) -> None:
    _, constitution_path = _repo_file(
        root, data["constitution"], "manifest.constitution"
    )
    _, hosted_path = _repo_file(
        root,
        data["hosted_reference_profile"],
        "manifest.hosted_reference_profile",
    )
    constitution_bytes = constitution_path.read_bytes()
    hosted_bytes = hosted_path.read_bytes()
    if hashlib.sha256(constitution_bytes).hexdigest() != EXPECTED_CONSTITUTION_SHA256:
        raise WorldEvidenceError(
            "constitution bytes drifted without a validator/schema version update"
        )
    if hashlib.sha256(hosted_bytes).hexdigest() != EXPECTED_HOSTED_PROFILE_SHA256:
        raise WorldEvidenceError(
            "hosted reference profile drifted without a validator/schema version update"
        )
    try:
        constitution = constitution_bytes.decode("utf-8", "strict")
        hosted = hosted_bytes.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise WorldEvidenceError("World constitution documents must be UTF-8") from error
    required_constitution_text = (
        "normative OSTADIX Alpha constitution",
        "They do not satisfy the native release gates in this roadmap.",
        "# 21. Integration gate ladder",
        "G13 -- eight-node OSTADIX Alpha",
        "# 28. OSTADIX Alpha non-claims",
        "not transparent DSM",
    )
    for required in required_constitution_text:
        if required not in constitution:
            raise WorldEvidenceError(
                f"constitution is missing required boundary text: {required!r}"
            )
    for gate_id in EXPECTED_GATE_IDS:
        if f"**{gate_id} --" not in constitution:
            raise WorldEvidenceError(
                f"constitution does not define integration gate {gate_id}"
            )
    for required in (
        "non-qualifying for native OSTADIX Alpha",
        "cannot satisfy G0 through G13",
        "G12, G13, or the name **OSTADIX Alpha**",
    ):
        if required not in hosted:
            raise WorldEvidenceError(
                f"hosted reference profile is missing boundary text: {required!r}"
            )


def _registry_semantics_sha256(data: dict[str, Any]) -> str:
    projection = {
        "constitution_version": data["constitution_version"],
        "constitution": data["constitution"],
        "hosted_reference_profile": data["hosted_reference_profile"],
        "contract_schema": data["contract_schema"],
        "machine_contract_schema": data["machine_contract_schema"],
        "alpha_gate": data["alpha_gate"],
        "gate_count": data["gate_count"],
        "evidence_class": data["evidence_class"],
        "gate": [
            {
                key: value
                for key, value in gate.items()
                if key not in {"status", "evidence"}
            }
            for gate in data["gate"]
        ],
        "qualification_policy": {
            "required_claim_floors": {
                gate: sorted(claims)
                for gate, claims in sorted(REQUIRED_CLAIM_FLOORS.items())
            },
            "required_class_floors": {
                gate: sorted(classes)
                for gate, classes in sorted(REQUIRED_CLASS_FLOORS.items())
            },
            "one_of_class_floors": {
                gate: sorted(sorted(group) for group in groups)
                for gate, groups in sorted(ONE_OF_CLASS_FLOORS.items())
            },
            "nonclaim_floors": {
                evidence_class: list(fragments)
                for evidence_class, fragments in sorted(NONCLAIM_FLOORS.items())
            },
            "nonqualifying_classes": sorted(NONQUALIFYING_CLASSES),
            "legacy_active_schema2_ids": sorted(LEGACY_ACTIVE_SCHEMA2_IDS),
        },
    }
    encoded = json.dumps(
        projection,
        ensure_ascii=True,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("ascii")
    return hashlib.sha256(encoded).hexdigest()


def _validate_classes(raw_classes: Any) -> dict[str, str]:
    if not isinstance(raw_classes, list):
        raise WorldEvidenceError("manifest must contain [[evidence_class]] tables")
    seen: dict[str, str] = {}
    for index, raw in enumerate(raw_classes):
        location = f"evidence_class[{index}]"
        if not isinstance(raw, dict):
            raise WorldEvidenceError(f"{location} must be a TOML table")
        expected_keys = {"id", "scope", "description"}
        if set(raw) != expected_keys:
            raise WorldEvidenceError(f"{location} keys differ from schema")
        class_id = _require_string(raw["id"], f"{location}.id")
        if class_id in seen:
            raise WorldEvidenceError(f"duplicate evidence class {class_id}")
        expected_scope = EXPECTED_CLASS_SCOPES.get(class_id)
        if expected_scope is None:
            raise WorldEvidenceError(f"unknown evidence class {class_id}")
        scope = _require_string(raw["scope"], f"{location}.scope")
        if scope != expected_scope:
            raise WorldEvidenceError(
                f"{location}.scope must be {expected_scope!r}, got {scope!r}"
            )
        seen[class_id] = scope
        _require_string(raw["description"], f"{location}.description")
    expected = set(EXPECTED_CLASS_SCOPES)
    if set(seen) != expected:
        raise WorldEvidenceError(
            "evidence classes differ from schema; "
            f"missing={sorted(expected - set(seen))}, "
            f"unknown={sorted(set(seen) - expected)}"
        )
    return seen


def _validate_one_of_classes(
    value: Any, location: str, class_ids: set[str]
) -> list[set[str]]:
    if not isinstance(value, list):
        raise WorldEvidenceError(f"{location} must be a list of class groups")
    groups: list[set[str]] = []
    for index, raw_group in enumerate(value):
        group = set(
            _require_string_list(
                raw_group, f"{location}[{index}]", minimum=2
            )
        )
        unknown = group - class_ids
        if unknown:
            raise WorldEvidenceError(
                f"{location}[{index}] references unknown classes {sorted(unknown)}"
            )
        groups.append(group)
    frozen = [frozenset(group) for group in groups]
    if len(frozen) != len(set(frozen)):
        raise WorldEvidenceError(f"{location} contains a duplicate group")
    return groups


def validated_gates(
    data: dict[str, Any], root: Path = ROOT, *, definitions_only: bool = False
) -> list[dict[str, Any]]:
    expected_root_keys = {
        "schema_version",
        "constitution_version",
        "constitution",
        "hosted_reference_profile",
        "contract_schema",
        "machine_contract_schema",
        "alpha_gate",
        "gate_count",
        "evidence_class",
        "gate",
    }
    if set(data) != expected_root_keys:
        raise WorldEvidenceError(
            "manifest root keys differ from schema; "
            f"missing={sorted(expected_root_keys - set(data))}, "
            f"unknown={sorted(set(data) - expected_root_keys)}"
        )
    if type(data["schema_version"]) is not int or (
        data["schema_version"] != EXPECTED_SCHEMA_VERSION
    ):
        raise WorldEvidenceError(
            f"schema_version must be {EXPECTED_SCHEMA_VERSION}"
        )
    if type(data["constitution_version"]) is not int or (
        data["constitution_version"] != EXPECTED_CONSTITUTION_VERSION
    ):
        raise WorldEvidenceError(
            f"constitution_version must be {EXPECTED_CONSTITUTION_VERSION}"
        )
    if data["alpha_gate"] != "G13":
        raise WorldEvidenceError("alpha_gate must be G13")
    if type(data["gate_count"]) is not int or (
        data["gate_count"] != len(EXPECTED_GATE_IDS)
    ):
        raise WorldEvidenceError(
            f"gate_count must be {len(EXPECTED_GATE_IDS)}"
        )

    _validate_constitution(data, root)
    class_scopes = _validate_classes(data["evidence_class"])
    class_ids = set(class_scopes)
    _validate_world_contract(data, root, class_scopes)
    _validate_machine_contract(data, root)

    raw_gates = data["gate"]
    if not isinstance(raw_gates, list):
        raise WorldEvidenceError("manifest must contain [[gate]] tables")
    if len(raw_gates) != len(EXPECTED_GATE_IDS):
        raise WorldEvidenceError(
            f"manifest must contain {len(EXPECTED_GATE_IDS)} gates"
        )

    expected_gate_keys = {
        "id",
        "title",
        "depends_on",
        "required_classes",
        "one_of_classes",
        "acceptance",
        "prohibited_substitutes",
    }
    gates: list[dict[str, Any]] = []
    for index, (expected_id, raw) in enumerate(zip(EXPECTED_GATE_IDS, raw_gates)):
        location = f"gate[{index}]"
        if not isinstance(raw, dict) or set(raw) != expected_gate_keys:
            raise WorldEvidenceError(f"{location} keys differ from schema")
        gate_id = _require_string(raw["id"], f"{location}.id")
        if gate_id != expected_id:
            raise WorldEvidenceError(
                f"{location}.id must be {expected_id}, got {gate_id}"
            )
        dependencies = _require_string_list(
            raw["depends_on"], f"{location}.depends_on"
        )
        if tuple(dependencies) != EXPECTED_DEPENDENCIES[gate_id]:
            raise WorldEvidenceError(
                f"{location}.depends_on must be "
                f"{list(EXPECTED_DEPENDENCIES[gate_id])}"
            )
        required_classes = set(
            _require_string_list(
                raw["required_classes"],
                f"{location}.required_classes",
                minimum=1,
            )
        )
        unknown = required_classes - class_ids
        if unknown:
            raise WorldEvidenceError(
                f"{location}.required_classes references {sorted(unknown)}"
            )
        missing_floor = REQUIRED_CLASS_FLOORS[gate_id] - required_classes
        if missing_floor:
            raise WorldEvidenceError(
                f"{location}.required_classes weakens qualification; "
                f"missing={sorted(missing_floor)}"
            )
        one_of_classes = _validate_one_of_classes(
            raw["one_of_classes"], f"{location}.one_of_classes", class_ids
        )
        actual_one_of = {frozenset(group) for group in one_of_classes}
        missing_one_of = ONE_OF_CLASS_FLOORS.get(gate_id, set()) - actual_one_of
        if missing_one_of:
            raise WorldEvidenceError(
                f"{location}.one_of_classes weakens hardware qualification"
            )
        qualifying_classes = required_classes | set().union(*one_of_classes)
        forbidden = qualifying_classes & NONQUALIFYING_CLASSES
        if forbidden:
            raise WorldEvidenceError(
                f"{location} treats nonqualifying classes as qualifying: "
                f"{sorted(forbidden)}"
            )
        gates.append(
            {
                "id": gate_id,
                "title": _require_string(raw["title"], f"{location}.title"),
                "depends_on": dependencies,
                "required_classes": required_classes,
                "one_of_classes": one_of_classes,
                "acceptance": _require_string(
                    raw["acceptance"], f"{location}.acceptance"
                ),
                "prohibited_substitutes": _require_string_list(
                    raw["prohibited_substitutes"],
                    f"{location}.prohibited_substitutes",
                    minimum=1,
                ),
                "required_claims": REQUIRED_CLAIM_FLOORS[gate_id],
            }
        )
    actual_semantics = _registry_semantics_sha256(data)
    if actual_semantics != EXPECTED_REGISTRY_SEMANTICS_SHA256:
        raise WorldEvidenceError(
            "registry semantics drifted without a constitution/schema version update; "
            f"got {actual_semantics}"
        )
    if definitions_only:
        for gate in gates:
            gate.update(
                {
                    "status": "defined",
                    "evidence": [],
                    "derived_claims": set(),
                    "not_established": sorted(gate["required_claims"]),
                }
            )
        return gates

    active_evidence, _events = _active_evidence_ledger(
        root, class_ids, actual_semantics
    )
    by_gate: dict[str, list[dict[str, Any]]] = {
        gate_id: [] for gate_id in EXPECTED_GATE_IDS
    }
    for attestation in active_evidence:
        by_gate[attestation["gate"]].append(attestation)

    statuses: dict[str, str] = {}
    for gate in gates:
        attestations = by_gate[gate["id"]]
        observed_classes = {item["class"] for item in attestations}
        derived_claims = set().union(
            *(item["derived_claims"] for item in attestations)
        ) if attestations else set()
        classes_satisfied = gate["required_classes"] <= observed_classes
        alternatives_satisfied = all(
            alternatives & observed_classes
            for alternatives in gate["one_of_classes"]
        )
        claims_satisfied = gate["required_claims"] <= derived_claims
        dependencies_satisfied = all(
            statuses.get(dependency) == "passed"
            for dependency in gate["depends_on"]
        )
        status = (
            "passed"
            if classes_satisfied
            and alternatives_satisfied
            and claims_satisfied
            and dependencies_satisfied
            else "defined"
        )
        statuses[gate["id"]] = status
        gate.update(
            {
                "status": status,
                "evidence": attestations,
                "derived_claims": derived_claims,
                "not_established": sorted(gate["required_claims"] - derived_claims),
            }
        )
    return gates


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="validate the OSTADIX Alpha G0-G13 registry"
    )
    parser.add_argument(
        "--manifest",
        type=Path,
        default=MANIFEST,
        help="registry to validate (paths inside it remain repository-relative)",
    )
    parser.add_argument(
        "--definitions-only",
        action="store_true",
        help="validate the frozen contract and gate definitions without opening attestations",
    )
    parser.add_argument("--quiet", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        gates = validated_gates(
            load_manifest(args.manifest),
            ROOT,
            definitions_only=args.definitions_only,
        )
    except WorldEvidenceError as error:
        print(f"OSTADIX Alpha evidence error: {error}", file=sys.stderr)
        return 1
    if not args.quiet:
        passed = sum(gate["status"] == "passed" for gate in gates)
        alpha = next(gate for gate in gates if gate["id"] == "G13")
        print(
            "OSTADIX Alpha gate registry: "
            f"{len(gates)}/{len(gates)} gates defined, {passed} passed; "
            f"G13 {alpha['status'].upper()} (registry schema v4 derived ledger view)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
