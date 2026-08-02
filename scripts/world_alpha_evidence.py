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
import sys
import tomllib
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "evidence/world_alpha_gates.toml"

EXPECTED_SCHEMA_VERSION = 1
EXPECTED_CONSTITUTION_VERSION = 1
EXPECTED_CONSTITUTION_SHA256 = "d3db5a7d553bad43cbcba0ea7960a95d3cad26d8e830d4aea60b77b750235fe1"
EXPECTED_HOSTED_PROFILE_SHA256 = "a8ee89ddcb535b11ebafd9f88c9c2e9258b0a2314e718b1264635a94246daa49"
EXPECTED_REGISTRY_SEMANTICS_SHA256 = (
    "92c92ba5feb9523cdc0d89ab5629f69148d8750533ddf68f01751a1920892d15"
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
NONQUALIFYING_CLASSES = {"hosted_reference", "multinode_virtual"}


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


def _validate_classes(raw_classes: Any) -> set[str]:
    if not isinstance(raw_classes, list):
        raise WorldEvidenceError("manifest must contain [[evidence_class]] tables")
    seen: set[str] = set()
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
        seen.add(class_id)
        expected_scope = EXPECTED_CLASS_SCOPES.get(class_id)
        if expected_scope is None:
            raise WorldEvidenceError(f"unknown evidence class {class_id}")
        scope = _require_string(raw["scope"], f"{location}.scope")
        if scope != expected_scope:
            raise WorldEvidenceError(
                f"{location}.scope must be {expected_scope!r}, got {scope!r}"
            )
        _require_string(raw["description"], f"{location}.description")
    expected = set(EXPECTED_CLASS_SCOPES)
    if seen != expected:
        raise WorldEvidenceError(
            "evidence classes differ from schema; "
            f"missing={sorted(expected - seen)}, unknown={sorted(seen - expected)}"
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


def validated_gates(data: dict[str, Any], root: Path = ROOT) -> list[dict[str, Any]]:
    expected_root_keys = {
        "schema_version",
        "constitution_version",
        "constitution",
        "hosted_reference_profile",
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
    class_ids = _validate_classes(data["evidence_class"])

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
        "status",
        "depends_on",
        "required_classes",
        "one_of_classes",
        "acceptance",
        "prohibited_substitutes",
        "evidence",
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
        status = _require_string(raw["status"], f"{location}.status")
        if status != "defined":
            raise WorldEvidenceError(
                f"{location}.status must be defined; schema v1 is "
                "definition-only and cannot certify passed gates"
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
        if raw["evidence"] != []:
            raise WorldEvidenceError(
                f"{location}.evidence must be empty; schema v1 has no "
                "attestation format and cannot accept evidence records"
            )
        gates.append(
            {
                "id": gate_id,
                "title": _require_string(raw["title"], f"{location}.title"),
                "status": status,
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
                "evidence": [],
            }
        )
    actual_semantics = _registry_semantics_sha256(data)
    if actual_semantics != EXPECTED_REGISTRY_SEMANTICS_SHA256:
        raise WorldEvidenceError(
            "registry semantics drifted without a constitution/schema version update; "
            f"got {actual_semantics}"
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
    parser.add_argument("--quiet", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        gates = validated_gates(load_manifest(args.manifest), ROOT)
    except WorldEvidenceError as error:
        print(f"World Alpha evidence error: {error}", file=sys.stderr)
        return 1
    if not args.quiet:
        passed = sum(gate["status"] == "passed" for gate in gates)
        alpha = next(gate for gate in gates if gate["id"] == "G13")
        print(
            "World Alpha gate registry: "
            f"{len(gates)}/{len(gates)} gates defined, {passed} passed; "
            f"G13 {alpha['status'].upper()} (schema v1 definition-only)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
