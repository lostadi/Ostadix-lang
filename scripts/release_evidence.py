#!/usr/bin/env python3
"""Validate and project O-core's required release-evidence gates.

The authoritative data lives in evidence/gates.toml.  This tool deliberately
uses only the Python standard library so the release-claim and QEMU CI jobs can
run it before building the repository.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import subprocess
import sys
import tomllib
from typing import Any, Callable


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "evidence/gates.toml"
EXPECTED_SCHEMA = 2
EXPECTED_REQUIRED_GATE_COUNT = 26
EXPECTED_SUPPLEMENTAL_GATE_COUNT = 1
ALLOWED_EVIDENCE_CLASSES = {
    "portable_tcg",
    "qemu_tcg_aarch64",
    "hardware_kvm",
}

COMMON_REQUIRED_TOOLS = {
    "bash",
    "cargo",
    "rustc",
    "clang",
    "lld",
    "python3",
}
CLASS_REQUIRED_TOOLS = {
    "portable_tcg": {"qemu-system-x86_64"},
    "qemu_tcg_aarch64": {"qemu-system-aarch64"},
    "hardware_kvm": {"qemu-system-x86_64"},
}

G2_AARCH64_GATE_ID = "world-g2-aarch64-native"
G2_AARCH64_SCRIPT = "ocore/kernel/smoke-aarch64-g2-qemu.sh"
G2_AARCH64_REQUIRED_TOOLS = COMMON_REQUIRED_TOOLS | {
    "cmp",
    "git",
    "qemu-system-aarch64",
    "shasum",
}
G2_AARCH64_POSITIVE_CLAIMS = (
    "One O-core kernel compiled for AArch64 retains EL2, enters host EL1, "
    "completes one domain-separated HVC return with register and stack integrity, "
    "and in one live QEMU TCG run executes native EL0 process, IPC, capability, "
    "lifecycle, stale-generation, reclamation, and bounded post-lifecycle "
    "counter-progress checks",
)
G2_AARCH64_NONCLAIMS = (
    "This single-vCPU QEMU TCG gate is not physical AArch64, KVM/SVM, SMP, or "
    "G3 evidence",
    "It does not boot Linux or Plan 9 and does not establish a general foreign ABI",
    "It provides no PCI or physical-device assignment, DMA isolation, or "
    "IOMMU/SMMU evidence",
)
G2_AARCH64_EXPECTED_MARKERS = (
    "G2 AArch64 ocorec object: PASS",
    "G2 AArch64 resident EL2 HVC round-trip: PASS",
    "G2 AArch64 EL0 process lifecycle: PASS",
    "G2 AArch64 IPC capability lifecycle: PASS",
    "G2 AArch64 post-lifecycle counter progress: PASS",
    "G2 AArch64 native compiler QEMU smoke: PASS",
)

README_BEGIN = "<!-- BEGIN GENERATED: REQUIRED_QEMU_EVIDENCE -->"
README_END = "<!-- END GENERATED: REQUIRED_QEMU_EVIDENCE -->"
CHECKLIST_BEGIN = "<!-- BEGIN GENERATED: REQUIRED_QEMU_EVIDENCE_CHECKLIST -->"
CHECKLIST_END = "<!-- END GENERATED: REQUIRED_QEMU_EVIDENCE_CHECKLIST -->"
CI_BEGIN = "      # BEGIN GENERATED: REQUIRED_QEMU_EVIDENCE_CI"
CI_END = "      # END GENERATED: REQUIRED_QEMU_EVIDENCE_CI"
DEVELOPMENT_BEGIN = "<!-- BEGIN GENERATED: REQUIRED_QEMU_EVIDENCE_DEVELOPMENT -->"
DEVELOPMENT_END = "<!-- END GENERATED: REQUIRED_QEMU_EVIDENCE_DEVELOPMENT -->"


class EvidenceError(RuntimeError):
    """A release-evidence schema or projection error."""


def load_manifest() -> dict[str, Any]:
    try:
        with MANIFEST.open("rb") as handle:
            data = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise EvidenceError(f"cannot read {MANIFEST.relative_to(ROOT)}: {error}") from error
    if not isinstance(data, dict):
        raise EvidenceError("manifest root must be a TOML table")
    return data


def require_string(value: Any, location: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise EvidenceError(f"{location} must be a non-empty string")
    if value != value.strip():
        raise EvidenceError(f"{location} must not have leading or trailing whitespace")
    return value


def require_string_list(value: Any, location: str, minimum: int = 1) -> list[str]:
    if not isinstance(value, list) or len(value) < minimum:
        raise EvidenceError(f"{location} must contain at least {minimum} string(s)")
    result = [require_string(item, f"{location}[{index}]") for index, item in enumerate(value)]
    if len(result) != len(set(result)):
        raise EvidenceError(f"{location} contains a duplicate")
    return result


def validated_gates(data: dict[str, Any]) -> list[dict[str, Any]]:
    root_keys = {
        "schema_version",
        "required_gate_count",
        "supplemental_gate_count",
        "portable_command",
        "gate",
    }
    if set(data) != root_keys:
        raise EvidenceError(
            f"manifest root keys differ from schema; missing={sorted(root_keys - set(data))}, "
            f"unknown={sorted(set(data) - root_keys)}"
        )
    if data.get("schema_version") != EXPECTED_SCHEMA:
        raise EvidenceError(f"schema_version must be {EXPECTED_SCHEMA}")
    if data.get("required_gate_count") != EXPECTED_REQUIRED_GATE_COUNT:
        raise EvidenceError(
            f"required_gate_count must be {EXPECTED_REQUIRED_GATE_COUNT}, "
            f"got {data.get('required_gate_count')!r}"
        )
    if data.get("portable_command") != "./boot-and-test.sh smoke":
        raise EvidenceError("portable_command must be './boot-and-test.sh smoke'")
    supplemental_gate_count = data.get("supplemental_gate_count")
    if type(supplemental_gate_count) is not int or (
        supplemental_gate_count != EXPECTED_SUPPLEMENTAL_GATE_COUNT
    ):
        raise EvidenceError(
            f"supplemental_gate_count must be {EXPECTED_SUPPLEMENTAL_GATE_COUNT}, "
            f"got {supplemental_gate_count!r}"
        )

    raw_gates = data.get("gate")
    if not isinstance(raw_gates, list):
        raise EvidenceError("manifest must contain [[gate]] tables")
    expected_total = EXPECTED_REQUIRED_GATE_COUNT + supplemental_gate_count
    if len(raw_gates) != expected_total:
        raise EvidenceError(
            f"manifest must contain exactly {expected_total} total gates, "
            f"got {len(raw_gates)}"
        )

    ids: set[str] = set()
    scripts: set[str] = set()
    gates: list[dict[str, Any]] = []
    expected_keys = {
        "id",
        "required",
        "milestone",
        "script",
        "evidence_class",
        "required_tools",
        "positive_claims",
        "nonclaims",
        "expected_markers",
    }
    for index, raw_gate in enumerate(raw_gates):
        location = f"gate[{index}]"
        if not isinstance(raw_gate, dict):
            raise EvidenceError(f"{location} must be a TOML table")
        unknown = set(raw_gate) - expected_keys
        missing = expected_keys - set(raw_gate)
        if unknown or missing:
            raise EvidenceError(
                f"{location} keys differ from schema; missing={sorted(missing)}, "
                f"unknown={sorted(unknown)}"
            )

        gate_id = require_string(raw_gate["id"], f"{location}.id")
        if gate_id in ids:
            raise EvidenceError(f"duplicate gate id: {gate_id}")
        ids.add(gate_id)

        script = require_string(raw_gate["script"], f"{location}.script")
        script_path = Path(script)
        if script_path.is_absolute() or ".." in script_path.parts:
            raise EvidenceError(f"{location}.script must be a safe repository-relative path")
        if script in scripts:
            raise EvidenceError(f"duplicate gate script: {script}")
        scripts.add(script)
        if script_path.parent != Path("ocore/kernel") or script_path.suffix != ".sh":
            raise EvidenceError(f"{location}.script must name an ocore/kernel shell gate")

        evidence_class = require_string(
            raw_gate["evidence_class"], f"{location}.evidence_class"
        )
        if evidence_class not in ALLOWED_EVIDENCE_CLASSES:
            raise EvidenceError(
                f"{location}.evidence_class must be one of {sorted(ALLOWED_EVIDENCE_CLASSES)}"
            )
        required = raw_gate["required"]
        if not isinstance(required, bool):
            raise EvidenceError(f"{location}.required must be a boolean")

        gate = {
            "id": gate_id,
            "required": required,
            "milestone": require_string(raw_gate["milestone"], f"{location}.milestone"),
            "script": script,
            "evidence_class": evidence_class,
            "required_tools": require_string_list(
                raw_gate["required_tools"], f"{location}.required_tools"
            ),
            "positive_claims": require_string_list(
                raw_gate["positive_claims"], f"{location}.positive_claims"
            ),
            "nonclaims": require_string_list(raw_gate["nonclaims"], f"{location}.nonclaims"),
            "expected_markers": require_string_list(
                raw_gate["expected_markers"], f"{location}.expected_markers", minimum=2
            ),
        }
        if required and evidence_class == "hardware_kvm":
            raise EvidenceError(
                f"required gate {gate_id} is {evidence_class}; hardware-dependent gates "
                "must not enter the portable release aggregate"
            )
        required_tools = COMMON_REQUIRED_TOOLS | CLASS_REQUIRED_TOOLS[evidence_class]
        for tool in sorted(required_tools):
            if tool not in gate["required_tools"]:
                raise EvidenceError(
                    f"{location}.required_tools must include {tool!r} "
                    f"for evidence class {evidence_class!r}"
                )
        gates.append(gate)
    required = [gate for gate in gates if gate["required"]]
    supplemental = [gate for gate in gates if not gate["required"]]
    if len(required) != EXPECTED_REQUIRED_GATE_COUNT:
        raise EvidenceError(
            f"manifest must contain exactly {EXPECTED_REQUIRED_GATE_COUNT} required gates, "
            f"got {len(required)}"
        )
    if len(supplemental) != supplemental_gate_count:
        raise EvidenceError(
            f"manifest declares {supplemental_gate_count} supplemental gates, "
            f"got {len(supplemental)}"
        )
    if supplemental[0]["evidence_class"] != "hardware_kvm":
        raise EvidenceError("the supplemental evidence gate must be hardware_kvm")
    aarch64 = [
        gate for gate in required if gate["evidence_class"] == "qemu_tcg_aarch64"
    ]
    if len(aarch64) != 1:
        raise EvidenceError(
            "the portable release aggregate must contain exactly one "
            "qemu_tcg_aarch64 gate"
        )
    if aarch64[0]["id"] != G2_AARCH64_GATE_ID or aarch64[0]["script"] != (
        G2_AARCH64_SCRIPT
    ):
        raise EvidenceError(
            "the qemu_tcg_aarch64 gate must be world-g2-aarch64-native at "
            "ocore/kernel/smoke-aarch64-g2-qemu.sh"
        )
    if tuple(aarch64[0]["positive_claims"]) != G2_AARCH64_POSITIVE_CLAIMS:
        raise EvidenceError(
            "world-g2-aarch64-native positive claims differ from the bounded "
            "AArch64 QEMU/TCG contract"
        )
    if tuple(aarch64[0]["nonclaims"]) != G2_AARCH64_NONCLAIMS:
        raise EvidenceError(
            "world-g2-aarch64-native nonclaims must preserve the physical, "
            "virtualization, SMP, foreign-OS, ABI, and device-isolation boundaries"
        )
    if tuple(aarch64[0]["expected_markers"]) != G2_AARCH64_EXPECTED_MARKERS:
        raise EvidenceError(
            "world-g2-aarch64-native expected markers differ from the integrated "
            "AArch64 execution contract"
        )
    missing_g2_tools = G2_AARCH64_REQUIRED_TOOLS - set(aarch64[0]["required_tools"])
    if missing_g2_tools:
        raise EvidenceError(
            "world-g2-aarch64-native required_tools is missing "
            f"{sorted(missing_g2_tools)}"
        )
    return gates


def required_gates(gates: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [gate for gate in gates if gate["required"]]


def supplemental_gates(gates: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [gate for gate in gates if not gate["required"]]


def markdown_cell(value: str) -> str:
    return value.replace("|", "\\|").replace("\n", " ")


def joined_cell(values: list[str]) -> str:
    return "<br>".join(markdown_cell(value) for value in values)


def readme_projection(gates: list[dict[str, Any]]) -> str:
    required = required_gates(gates)
    supplemental = supplemental_gates(gates)
    lines = [
        README_BEGIN,
        f"The {len(required)} required portable QEMU release gates and {len(supplemental)}",
        "supplemental hardware-dependent gate are defined once in",
        "[`evidence/gates.toml`](evidence/gates.toml). The aggregate reads that manifest",
        "at runtime, selects only `required = true`, streams each gate's output, and",
        "requires every declared marker exactly once in that live transcript. This table",
        "is a checked projection.",
        "",
        "| Gate | Required | Milestone | Evidence | Establishes | Explicit non-claims |",
        "|------|----------|-----------|----------|-------------|---------------------|",
    ]
    for gate in gates:
        lines.append(
            f"| `{markdown_cell(gate['id'])}` | "
            f"{'yes' if gate['required'] else 'no'} | "
            f"{markdown_cell(gate['milestone'])} | "
            f"[{markdown_cell(gate['script'])}]({markdown_cell(gate['script'])}) "
            f"(`{markdown_cell(gate['evidence_class'])}`) | "
            f"{joined_cell(gate['positive_claims'])} | {joined_cell(gate['nonclaims'])} |"
        )
    lines.extend(
        [
            "",
            "Validate the schema, scripts, runtime transcript checks, projections, CI wiring,",
            "claim-guard wiring, and aggregate byte identity with:",
            "",
            "```bash",
            "python3 scripts/release_evidence.py validate",
            "./boot-and-test.sh smoke",
            "```",
            README_END,
        ]
    )
    return "\n".join(lines)


def checklist_projection(gates: list[dict[str, Any]]) -> str:
    required = required_gates(gates)
    supplemental = supplemental_gates(gates)
    lines = [
        CHECKLIST_BEGIN,
        f"The portable native release surface contains exactly **{len(required)}** required",
        "QEMU gates. `evidence/gates.toml` is authoritative; the aggregate, CI, this",
        "checklist, and the README status table are validated projections. After each",
        "successful gate, the aggregate requires every manifest marker exactly once in",
        "the captured live stdout/stderr transcript.",
        "",
        "```bash",
        "python3 scripts/release_evidence.py validate",
        "./boot-and-test.sh smoke",
        "```",
        "",
        "| Order | Gate | Milestone | Class | Script |",
        "|------:|------|-----------|-------|--------|",
    ]
    for index, gate in enumerate(required, start=1):
        lines.append(
            f"| {index} | `{markdown_cell(gate['id'])}` | "
            f"{markdown_cell(gate['milestone'])} | `{gate['evidence_class']}` | "
            f"`{markdown_cell(gate['script'])}` |"
        )
    lines.extend(
        [
            "",
            "Supplemental hardware evidence is validated by the same manifest but is not",
            "executed by the portable aggregate:",
            "",
            "| Gate | Milestone | Class | Script |",
            "|------|-----------|-------|--------|",
        ]
    )
    for gate in supplemental:
        lines.append(
            f"| `{markdown_cell(gate['id'])}` | {markdown_cell(gate['milestone'])} | "
            f"`{gate['evidence_class']}` | `{markdown_cell(gate['script'])}` |"
        )
    lines.extend(["", "Explicit supplemental non-claims:"])
    for gate in supplemental:
        lines.extend(f"- {claim}" for claim in gate["nonclaims"])
    lines.extend(["", CHECKLIST_END])
    return "\n".join(lines)


def ci_projection(_: list[dict[str, Any]]) -> str:
    return "\n".join(
        [
            CI_BEGIN,
            "      - name: Validate central release-evidence manifest",
            "        run: python3 scripts/release_evidence.py validate",
            "",
            "      - name: Run all required portable QEMU evidence gates",
            "        run: ./boot-and-test.sh smoke",
            CI_END,
        ]
    )


def development_projection(gates: list[dict[str, Any]]) -> str:
    required = required_gates(gates)
    supplemental = supplemental_gates(gates)
    supplemental_ids = ", ".join(f"`{gate['id']}`" for gate in supplemental)
    return "\n".join(
        [
            DEVELOPMENT_BEGIN,
            f"The aggregate executes all {len(required)} required portable QEMU gates in the",
            "order declared by `evidence/gates.toml`, streams their output, and requires",
            "every declared marker exactly once in each captured live transcript. The",
            "manifest also records each gate's milestone, tools, evidence class, positive",
            f"claims, and explicit non-claims. {supplemental_ids} is validated as",
            "supplemental hardware evidence rather than part of this portable release set.",
            DEVELOPMENT_END,
        ]
    )


Projection = tuple[Path, str, str, Callable[[list[dict[str, Any]]], str]]


def projections() -> list[Projection]:
    return [
        (ROOT / "README.md", README_BEGIN, README_END, readme_projection),
        (
            ROOT / "docs/RELEASE_CHECKLIST.md",
            CHECKLIST_BEGIN,
            CHECKLIST_END,
            checklist_projection,
        ),
        (ROOT / ".github/workflows/ci.yml", CI_BEGIN, CI_END, ci_projection),
        (
            ROOT / "DEVELOPMENT.md",
            DEVELOPMENT_BEGIN,
            DEVELOPMENT_END,
            development_projection,
        ),
    ]


def replace_projection(text: str, begin: str, end: str, generated: str, path: Path) -> str:
    if text.count(begin) != 1 or text.count(end) != 1:
        raise EvidenceError(
            f"{path.relative_to(ROOT)} must contain exactly one {begin!r}/{end!r} pair"
        )
    start = text.index(begin)
    finish = text.index(end, start) + len(end)
    if finish < start:
        raise EvidenceError(f"projection markers are reversed in {path.relative_to(ROOT)}")
    return text[:start] + generated + text[finish:]


def project(gates: list[dict[str, Any]], write: bool) -> None:
    drifted: list[str] = []
    for path, begin, end, render in projections():
        try:
            current = path.read_text(encoding="utf-8")
        except OSError as error:
            raise EvidenceError(f"cannot read {path.relative_to(ROOT)}: {error}") from error
        expected = replace_projection(current, begin, end, render(gates), path)
        if current == expected:
            continue
        relative = str(path.relative_to(ROOT))
        if write:
            path.write_text(expected, encoding="utf-8")
            print(f"updated {relative}")
        else:
            drifted.append(relative)
    if drifted:
        joined = ", ".join(drifted)
        raise EvidenceError(
            f"generated release-evidence projections drifted: {joined}; "
            "run 'python3 scripts/release_evidence.py project --write'"
        )


def validate_gate_scripts(gates: list[dict[str, Any]]) -> None:
    for gate in gates:
        relative = gate["script"]
        path = ROOT / relative
        if not path.is_file():
            raise EvidenceError(f"required gate script is missing: {relative}")
        if not os.access(path, os.X_OK):
            raise EvidenceError(f"required gate script is not executable: {relative}")
        result = subprocess.run(
            ["bash", "-n", str(path)],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip()
            raise EvidenceError(f"bash -n failed for {relative}: {detail}")


def verify_transcript(
    gates: list[dict[str, Any]], requested_script: str, transcript: bytes
) -> tuple[str, int]:
    """Require each marker for one manifest gate exactly once in live output."""

    script = requested_script[2:] if requested_script.startswith("./") else requested_script
    matches = [gate for gate in gates if gate["script"] == script]
    if len(matches) != 1:
        raise EvidenceError(f"transcript script is not a unique manifest gate: {requested_script!r}")
    gate = matches[0]
    wrong_counts: list[tuple[str, int]] = []
    for marker in gate["expected_markers"]:
        count = transcript.count(marker.encode("utf-8"))
        if count != 1:
            wrong_counts.append((marker, count))
    if wrong_counts:
        detail = ", ".join(f"{marker!r}: {count}" for marker, count in wrong_counts)
        raise EvidenceError(
            f"{script} transcript marker counts must each equal 1; observed {detail}"
        )
    return gate["id"], len(gate["expected_markers"])


def validate_wiring(gates: list[dict[str, Any]]) -> None:
    aggregate_paths = [ROOT / "boot-and-test.sh", ROOT / "okernel-multikernel/boot-and-test.sh"]
    aggregate_bytes = []
    required_fragments = [
        "python3 scripts/release_evidence.py validate",
        "python3 scripts/release_evidence.py list-scripts",
        "python3 scripts/release_evidence.py verify-transcript",
        "--package ostadix-api --lib",
        "ocore::driver::tests::ocore_object_is_byte_reproducible_across_source_directories",
        "test result: ok. 1 passed; 0 failed;",
    ]
    for path in aggregate_paths:
        if not path.is_file() or not os.access(path, os.X_OK):
            raise EvidenceError(f"aggregate is missing or not executable: {path.relative_to(ROOT)}")
        data = path.read_bytes()
        aggregate_bytes.append(data)
        source = data.decode("utf-8")
        for fragment in required_fragments:
            if source.count(fragment) != 1:
                raise EvidenceError(
                    f"{path.relative_to(ROOT)} must contain exactly one {fragment!r}"
                )
        result = subprocess.run(
            ["bash", "-n", str(path)],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip()
            raise EvidenceError(f"bash -n failed for {path.relative_to(ROOT)}: {detail}")
    if aggregate_bytes[0] != aggregate_bytes[1]:
        raise EvidenceError("the two boot-and-test.sh aggregates are not byte-identical")

    claim_guard = ROOT / "scripts/check_release_claims.sh"
    claim_guard_source = claim_guard.read_text(encoding="utf-8")
    fragment = "python3 scripts/release_evidence.py validate"
    if claim_guard_source.count(fragment) != 1:
        raise EvidenceError(f"scripts/check_release_claims.sh must invoke {fragment!r} exactly once")

    # The CI projection intentionally invokes only the manifest-driven aggregate;
    # a new hand-written gate step would recreate the drift this manifest removes.
    ci_source = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    direct_runs = [
        gate["script"]
        for gate in gates
        if f"run: ./{gate['script']}" in ci_source
    ]
    if direct_runs:
        raise EvidenceError(f"CI contains hand-written required-gate runs: {direct_runs!r}")


def validate(gates: list[dict[str, Any]]) -> None:
    validate_gate_scripts(gates)
    project(gates, write=False)
    validate_wiring(gates)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("validate", help="validate schema, gates, projections, and wiring")
    subparsers.add_parser("list-scripts", help="print required gate scripts in release order")
    transcript_parser = subparsers.add_parser(
        "verify-transcript",
        help="verify one gate's manifest markers against its captured runtime transcript",
    )
    transcript_parser.add_argument("--script", required=True, help="manifest gate script path")
    transcript_parser.add_argument(
        "--transcript", required=True, type=Path, help="captured combined stdout/stderr"
    )
    project_parser = subparsers.add_parser("project", help="check or update generated projections")
    project_parser.add_argument(
        "--write", action="store_true", help="rewrite marked projections instead of checking"
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        data = load_manifest()
        gates = validated_gates(data)
        required = required_gates(gates)
        if args.command == "validate":
            validate(gates)
            print(f"release evidence: PASS ({len(required)} required portable QEMU gates)")
        elif args.command == "list-scripts":
            for gate in required:
                print(gate["script"])
        elif args.command == "verify-transcript":
            try:
                transcript = args.transcript.read_bytes()
            except OSError as error:
                raise EvidenceError(
                    f"cannot read transcript {args.transcript}: {error}"
                ) from error
            gate_id, marker_count = verify_transcript(gates, args.script, transcript)
            print(
                f"release evidence transcript: PASS "
                f"({gate_id}, {marker_count} exact markers)"
            )
        elif args.command == "project":
            project(gates, write=args.write)
            action = "updated" if args.write else "current"
            print(f"release-evidence projections: {action}")
        else:  # argparse guarantees this is unreachable.
            raise AssertionError(args.command)
    except EvidenceError as error:
        print(f"release evidence: FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
