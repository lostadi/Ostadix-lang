#!/usr/bin/env python3
"""Validate the Ostadix World Alpha G0-G13 qualification registry.

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
import shlex
import subprocess
import sys
import tomllib
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "evidence/world_alpha_gates.toml"

EXPECTED_SCHEMA_VERSION = 3
EXPECTED_CONSTITUTION_VERSION = 2
EXPECTED_CONSTITUTION_SHA256 = "2a56a9b54297c9b6190505055bad3f2e8760a501498b1a55da72a0fd4d298643"
EXPECTED_HOSTED_PROFILE_SHA256 = "4d4681039ff8a9d1c92509356f7ee76444b133b9ee3e026d08b7b815e723777f"
EXPECTED_REGISTRY_SEMANTICS_SHA256 = (
    "325c1ca4443481596491b8147361c6dcd5f2382fde65e0cc5c1aabd07f1cb6eb"
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
    "G3": {"aarch64.smp_execution", "lifecycle.multicore_linearization"},
    "G4": {"transport.native_authenticated", "transport.three_physical_nodes"},
    "G5": {"governor.authoritative_log", "governor.partition_fencing"},
    "G6": {"worldfs.live_multinode_namespace", "worldfs.stale_fid_rejected"},
    "G7": {
        "foreign_kernel.booted",
        "foreign_kernel.userspace_reached",
        "foreign_kernel.fresh_challenge_answered",
        "kernel_world.revocation_complete",
        "kernel_world.pages_reclaimed",
    },
    "G8": {"driver.physical_device_service", "driver.revocation_complete"},
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
)

CLAIM_RULE_POLICY_SHA256 = hashlib.sha256(
    json.dumps(CLAIM_RULES, ensure_ascii=True, separators=(",", ":")).encode("ascii")
).hexdigest()

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


class WorldEvidenceError(RuntimeError):
    """The World Alpha qualification registry is malformed or overclaims."""


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
    candidate = root.joinpath(*path.parts)
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
    _, contract_path = _repo_file(
        root, data["contract_schema"], "manifest.contract_schema"
    )
    contract = _strict_toml_file(contract_path, data["contract_schema"])
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
        or contract["constitution_version"] != EXPECTED_CONSTITUTION_VERSION
    ):
        raise WorldEvidenceError(
            f"World contract constitution_version must be {EXPECTED_CONSTITUTION_VERSION}"
        )
    if contract["constitution"] != data["constitution"]:
        raise WorldEvidenceError("World contract constitution reference differs from registry")
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
        root, contract["constitution"], "World contract constitution"
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


def _parse_observations(transcript: str, location: str) -> list[dict[str, str]]:
    observations: list[dict[str, str]] = []
    for line_number, line in enumerate(transcript.splitlines(), 1):
        if not line.startswith("@evidence "):
            continue
        try:
            tokens = shlex.split(line[len("@evidence ") :])
        except ValueError as error:
            raise WorldEvidenceError(
                f"{location}:{line_number} contains malformed evidence fields"
            ) from error
        fields: dict[str, str] = {}
        for token in tokens:
            if "=" not in token:
                raise WorldEvidenceError(
                    f"{location}:{line_number} evidence token lacks '='"
                )
            key, value = token.split("=", 1)
            if not key or not value or key in fields:
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


def _derive_claims(transcript: str, location: str) -> set[str]:
    observations = _parse_observations(transcript, location)
    claims: set[str] = set()
    for claim, requirements in CLAIM_RULES:
        if all(
            any(
                observation.get("event") == event
                and all(observation.get(key) == value for key, value in fields)
                for observation in observations
            )
            for event, fields in requirements
        ):
            claims.add(claim)
    transcript_lines = set(transcript.splitlines())
    for claim, markers in LEGACY_CLAIM_MARKERS.items():
        if all(marker in transcript_lines for marker in markers):
            claims.add(claim)
    return claims


def _validator_sha256(root: Path) -> str:
    return hashlib.sha256(
        (root / "scripts/world_alpha_evidence.py").read_bytes()
    ).hexdigest()


def _source_digest_matches(
    root: Path, source_path: Path, source_text: str, source_commit: str, digest: str
) -> bool:
    if hashlib.sha256(source_path.read_bytes()).hexdigest() == digest:
        return True
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "show", f"{source_commit}:{source_text}"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
    except OSError:
        return False
    return result.returncode == 0 and hashlib.sha256(result.stdout).hexdigest() == digest


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
    if type(schema_version) is not int or schema_version not in {1, 2}:
        raise WorldEvidenceError(
            f"attestation {path_text} schema_version must be 1 or 2"
        )
    version_keys = (
        {"claims"}
        if schema_version == 1
        else {
            "derived_claims",
            "validator_sha256",
            "claim_rule_policy_sha256",
            "registry_semantics_sha256",
        }
    )
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
    derived_claims = _derive_claims(transcript, transcript_text)
    if schema_version == 2:
        recorded_claims = _require_string_list(
            attestation["derived_claims"],
            f"{path_text}.derived_claims",
            minimum=1,
        )
        if recorded_claims != sorted(set(recorded_claims)):
            raise WorldEvidenceError(
                f"{path_text}.derived_claims must be sorted and unique"
            )
        if set(recorded_claims) != derived_claims:
            raise WorldEvidenceError(
                f"{path_text}.derived_claims differs from validator output"
            )
        if _require_sha256(
            attestation["validator_sha256"], f"{path_text}.validator_sha256"
        ) != _validator_sha256(root):
            raise WorldEvidenceError(
                f"{path_text}.validator_sha256 does not pin this validator"
            )
        if _require_sha256(
            attestation["claim_rule_policy_sha256"],
            f"{path_text}.claim_rule_policy_sha256",
        ) != CLAIM_RULE_POLICY_SHA256:
            raise WorldEvidenceError(
                f"{path_text}.claim_rule_policy_sha256 differs from claim rules"
            )
        if _require_sha256(
            attestation["registry_semantics_sha256"],
            f"{path_text}.registry_semantics_sha256",
        ) != registry_semantics_sha256:
            raise WorldEvidenceError(
                f"{path_text}.registry_semantics_sha256 differs from gate definitions"
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
    for index, source in enumerate(sources):
        owner = f"{path_text}.source[{index}]"
        if not isinstance(source, dict) or set(source) != {"path", "sha256"}:
            raise WorldEvidenceError(f"{owner} keys differ from schema")
        source_text, source_path = _repo_file(root, source["path"], f"{owner}.path")
        if source_text in seen_sources:
            raise WorldEvidenceError(f"{path_text}.source contains a duplicate path")
        seen_sources.add(source_text)
        digest = _require_sha256(source["sha256"], f"{owner}.sha256")
        # Schema-v1 records predate the append-only ledger and used hashes of a
        # content-addressed working tree that may never have been committed.
        # Preserve those immutable records even when their historical bytes are
        # no longer locally reconstructible.  Schema v2 requires live or Git
        # object verification for every source digest.
        if schema_version == 2 and not _source_digest_matches(
            root, source_path, source_text, source_commit, digest
        ):
            raise WorldEvidenceError(f"{owner} digest does not match {source_text}")

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
        _require_string(artifact["kind"], f"{owner}.kind")
        digest = _require_sha256(artifact["sha256"], f"{owner}.sha256")
        if type(artifact["retained"]) is not bool:
            raise WorldEvidenceError(f"{owner}.retained must be boolean")
        if artifact["retained"]:
            artifact_text, artifact_path = _repo_file(
                root, artifact["path"], f"{owner}.path"
            )
            if hashlib.sha256(artifact_path.read_bytes()).hexdigest() != digest:
                raise WorldEvidenceError(
                    f"{owner} digest does not match retained {artifact_text}"
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
    return {
        "id": attestation_id,
        "path": path_text,
        "gate": gate_id,
        "class": evidence_class,
        "derived_claims": derived_claims,
        "schema_version": schema_version,
    }


def _validate_evidence_event(root: Path, path: Path) -> dict[str, Any]:
    path_text = path.relative_to(root).as_posix()
    event = _strict_toml_file(path, path_text)
    expected_keys = {
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
    if set(event) != expected_keys:
        raise WorldEvidenceError(f"evidence event {path_text} keys differ from schema")
    if type(event["schema_version"]) is not int or event["schema_version"] != 1:
        raise WorldEvidenceError(f"evidence event {path_text} schema_version must be 1")
    event_id = _require_string(event["id"], f"{path_text}.id")
    subject = _require_string(event["subject"], f"{path_text}.subject")
    for field, value in (("id", event_id), ("subject", subject)):
        if ATTESTATION_ID.fullmatch(value) is None:
            raise WorldEvidenceError(f"{path_text}.{field} is not normalized")
    kind = _require_string(event["event"], f"{path_text}.event")
    if kind not in {"supersede", "retract"}:
        raise WorldEvidenceError(f"{path_text}.event must be supersede or retract")
    replacement = event["replacement"]
    if not isinstance(replacement, str):
        raise WorldEvidenceError(f"{path_text}.replacement must be a string")
    if kind == "supersede":
        replacement = _require_string(replacement, f"{path_text}.replacement")
        if ATTESTATION_ID.fullmatch(replacement) is None:
            raise WorldEvidenceError(f"{path_text}.replacement is not normalized")
        if replacement == subject:
            raise WorldEvidenceError(f"{path_text} cannot supersede an attestation with itself")
    elif replacement != "":
        raise WorldEvidenceError(f"{path_text}.replacement must be empty for retraction")
    _require_string(event["reason_code"], f"{path_text}.reason_code")
    _require_string(event["reason"], f"{path_text}.reason")
    source_commit = _require_string(event["source_commit"], f"{path_text}.source_commit")
    if HEX_COMMIT.fullmatch(source_commit) is None:
        raise WorldEvidenceError(f"{path_text}.source_commit must be a Git object ID")
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
    event_by_subject: dict[str, dict[str, Any]] = {}
    for event in events:
        subject = event["subject"]
        if subject not in attestations_by_id:
            raise WorldEvidenceError(
                f"{event['path']} references missing subject {subject}"
            )
        if subject in event_by_subject:
            raise WorldEvidenceError(
                f"attestation {subject} has multiple competing ledger events"
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
        event_by_subject[subject] = event

    # Every successor chain must terminate, and no attestation may be the
    # replacement of two unrelated active histories.
    replacement_predecessors: dict[str, str] = {}
    for subject, event in event_by_subject.items():
        replacement = event["replacement"]
        if replacement:
            previous = replacement_predecessors.setdefault(replacement, subject)
            if previous != subject:
                raise WorldEvidenceError(
                    f"replacement {replacement} has competing predecessors"
                )
        seen: set[str] = set()
        current = subject
        while current in event_by_subject:
            if current in seen:
                raise WorldEvidenceError("evidence supersession graph contains a cycle")
            seen.add(current)
            current = event_by_subject[current]["replacement"]
            if not current:
                break

    inactive_ids = set(event_by_subject)
    active = [item for item in attestations if item["id"] not in inactive_ids]
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
        "normative native Alpha constitution",
        "They do not satisfy the native release gates in this roadmap.",
        "# 21. Integration gate ladder",
        "G13 -- eight-node World Alpha",
        "# 28. Alpha non-claims",
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
        "non-qualifying for native Ostadix",
        "cannot satisfy G0 through G13",
        "G12, G13, or the name **Ostadix World Alpha**",
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
        description="validate the Ostadix World Alpha G0-G13 registry"
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
        print(f"World Alpha evidence error: {error}", file=sys.stderr)
        return 1
    if not args.quiet:
        passed = sum(gate["status"] == "passed" for gate in gates)
        alpha = next(gate for gate in gates if gate["id"] == "G13")
        print(
            "World Alpha gate registry: "
            f"{len(gates)}/{len(gates)} gates defined, {passed} passed; "
            f"G13 {alpha['status'].upper()} (schema v3 derived ledger view)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
