#!/usr/bin/env python3
"""Prepare and validate authority-free OSTADIX physical-boot observations.

This tool does not promote an operator transcript into trusted attestation.  It
binds a fresh challenge embedded in one exact boot image to a bounded machine
profile and then checks a captured serial transcript against that intent.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import stat
import subprocess
import sys
import tempfile
from typing import Any, Sequence

import ostadix_boot_media as boot_media
import ostadix_media_writer as media_writer


MACHINE_SCHEMA = "ostadix.physical-machine-profile/v1"
INTENT_SCHEMA = "ostadix.physical-boot-intent/v1"
OBSERVATION_SCHEMA = "ostadix.physical-boot-observation/v1"
TRANSCRIPT_CHECK_SCHEMA = "ostadix.boot-transcript-check/v1"
CHALLENGE_PREFIX = b"ostadix.challenge="
CHALLENGE_RE = re.compile(rb"ostadix\.challenge=([0-9a-f]{64})")
SOURCE_COMMIT_RE = re.compile(rb"ostadix\.source_commit=([0-9a-f]{40})")
CPU_LINE_RE = re.compile(
    r"OSTADIX SMP CPU logical=([0-9]{1,3}) apic=([0-9]{1,10}) "
    r"stack=(0x[0-9a-f]{1,16}) online"
)
OPERATOR_ASSERTION = "I-OBSERVED-OSTADIX-ON-PHYSICAL-X86_64"
MAX_MACHINE_PROFILE_BYTES = 16 * 1024
MAX_TRANSCRIPT_BYTES = 4 * 1024 * 1024
MAX_CPUS = 256
MAX_TEXT_FIELD_BYTES = 256
MODE0_PROFILE = "mode0"
SMP4_PROFILE = "smp4"
MODE0_REQUIRED_MARKERS = (
    "O-core kernel: serial online",
    "BootInfoV1: malformed fixture rejected",
    "BootInfoV1: source pointer and temporary aperture released",
    "BootInfoV1: Multiboot2 normalized",
    "BootInfoV1: ACPI status valid",
    "BootInfoV1: EFI64 boot services exited",
    "page protections: W^X online",
    "page allocator: online",
    "BootInfoV1: firmware allocator window admitted",
    "CPL3 native[0]: online",
    "timer CPL3 return: online",
    "CPL3 heartbeat: online",
)
SMP4_REQUIRED_MARKERS = (
    "O-core kernel: serial online",
    "BootInfoV1: malformed fixture rejected",
    "BootInfoV1: Multiboot2 normalized",
    "BootInfoV1: ACPI status valid",
    "BootInfoV1: EFI64 boot services exited",
    "BootInfoV1: firmware allocator window admitted",
    "BootInfoV1: source pointer and temporary aperture released",
    "SMP boot inspection window closed: PASS",
    "SMP page protections: W^X online",
    "SMP firmware Multiboot2/ACPI handoff: PASS",
    "SMP low-memory trampoline admission: PASS",
    "SMP firmware MADT 4-CPU topology: PASS",
    "SMP timing source PIT: validated",
    "SMP x2APIC preparation: PASS",
    "SMP x2APIC INIT/SIPI: PASS",
    "SMP AP hardware identities unique: PASS",
    "SMP AP stacks isolated: PASS",
    "SMP BSP/AP barrier: 4 CPUs PASS",
    "SMP post-barrier timer: online",
    "SMP post-barrier heartbeat: online",
)
# Retain the historical name for callers that explicitly mean the mode-0
# profile. New code should select a named profile through transcript_profile().
BASE_REQUIRED_MARKERS = MODE0_REQUIRED_MARKERS
PROFILE_CPU_COUNTS = {MODE0_PROFILE: 1, SMP4_PROFILE: 4}
PROFILE_MARKERS = {
    MODE0_PROFILE: MODE0_REQUIRED_MARKERS,
    SMP4_PROFILE: SMP4_REQUIRED_MARKERS,
}
CONTRADICTION_PREFIXES = (
    "BootInfoV1: rejected",
    "BootInfoV1 rejection code:",
    "SMP probe: REJECT",
)
NONCLAIMS = (
    "This record is an operator-asserted authority-free observation, not an attestation.",
    "The machine profile, writer record, operator assertion, and serial transcript are self-reported and do not independently prove physical execution or a completed device write.",
    "The embedded challenge has no trusted freshness clock, one-shot registry, or replay prevention; copied artifacts can be replayed.",
    "Canonical record seals are unkeyed integrity hashes and provide no signer identity or authenticity.",
    "The embedded source commit binds a declared repository identity but does not prove a trusted build or toolchain.",
    "The record grants no capability, admission, release-gate credit, or hardware trust.",
)


class PhysicalEvidenceError(ValueError):
    """An intent, machine profile, image, or transcript is invalid."""


def transcript_profile(profile: str | None, expected_cpus: int) -> str:
    """Resolve one of the two v1 transcript contracts and reject ambiguity."""

    inferred = MODE0_PROFILE if expected_cpus == 1 else SMP4_PROFILE if expected_cpus == 4 else None
    if inferred is None:
        raise PhysicalEvidenceError("expected_cpus must be exactly 1 (mode0) or 4 (smp4)")
    selected = inferred if profile is None else profile
    if selected not in PROFILE_CPU_COUNTS:
        raise PhysicalEvidenceError(f"unknown transcript profile: {selected!r}")
    if PROFILE_CPU_COUNTS[selected] != expected_cpus:
        raise PhysicalEvidenceError(
            f"transcript profile {selected!r} requires exactly "
            f"{PROFILE_CPU_COUNTS[selected]} CPUs"
        )
    return selected


def _profile_causal_lines(profile: str, challenge: str, source_commit: str) -> list[str]:
    markers = list(PROFILE_MARKERS[profile])
    anchor = "BootInfoV1: firmware allocator window admitted"
    index = markers.index(anchor) + 1
    return [
        *markers[:index],
        f"OSTADIX boot challenge: {challenge}",
        f"OSTADIX source commit: {source_commit}",
        *markers[index:],
    ]


def _canonical_bytes(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("ascii")


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def _read_regular_nofollow(path: Path, maximum: int) -> bytes:
    try:
        mode = path.lstat().st_mode
    except OSError as error:
        raise PhysicalEvidenceError(f"cannot inspect {path}: {error}") from error
    if not stat.S_ISREG(mode):
        raise PhysicalEvidenceError(f"not a regular file: {path}")

    flags = os.O_RDONLY
    flags |= getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise PhysicalEvidenceError(f"cannot open {path}: {error}") from error
    try:
        before = os.fstat(descriptor)
        if before.st_size <= 0 or before.st_size > maximum:
            raise PhysicalEvidenceError(
                f"file size outside 1..{maximum} bytes: {path}"
            )
        chunks: list[bytes] = []
        remaining = before.st_size
        while remaining:
            chunk = os.read(descriptor, min(1024 * 1024, remaining))
            if not chunk:
                raise PhysicalEvidenceError(f"file was truncated while reading: {path}")
            chunks.append(chunk)
            remaining -= len(chunk)
        if os.read(descriptor, 1):
            raise PhysicalEvidenceError(f"file grew while reading: {path}")
        after = os.fstat(descriptor)
        if (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
        ):
            raise PhysicalEvidenceError(f"file identity changed while reading: {path}")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def _bounded_text(value: Any, field: str) -> str:
    if not isinstance(value, str) or value != value.strip() or not value:
        raise PhysicalEvidenceError(f"{field} must be a non-empty trimmed string")
    try:
        encoded = value.encode("ascii")
    except UnicodeEncodeError as error:
        raise PhysicalEvidenceError(f"{field} must be ASCII") from error
    if len(encoded) > MAX_TEXT_FIELD_BYTES or any(byte < 0x20 or byte > 0x7E for byte in encoded):
        raise PhysicalEvidenceError(f"{field} must be bounded printable ASCII")
    return value


def load_machine_profile(path: Path) -> dict[str, str]:
    raw = _read_regular_nofollow(path, MAX_MACHINE_PROFILE_BYTES)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise PhysicalEvidenceError(f"machine profile is not valid JSON: {error}") from error
    expected = {
        "schema",
        "architecture",
        "manufacturer",
        "model",
        "board",
        "cpu_model",
        "firmware",
        "serial_identity_sha256",
    }
    if not isinstance(value, dict) or set(value) != expected:
        actual = set(value) if isinstance(value, dict) else set()
        raise PhysicalEvidenceError(
            "machine profile keys differ from schema; "
            f"missing={sorted(expected - actual)}, unknown={sorted(actual - expected)}"
        )
    if value["schema"] != MACHINE_SCHEMA:
        raise PhysicalEvidenceError(f"machine profile schema must be {MACHINE_SCHEMA}")
    if value["architecture"] != "x86_64":
        raise PhysicalEvidenceError("physical workflow v1 admits only x86_64")
    for field in ("manufacturer", "model", "board", "cpu_model", "firmware"):
        _bounded_text(value[field], field)
    serial_digest = value["serial_identity_sha256"]
    if not isinstance(serial_digest, str) or not re.fullmatch(r"[0-9a-f]{64}", serial_digest):
        raise PhysicalEvidenceError("serial_identity_sha256 must be 64 lowercase hex digits")
    if serial_digest == "0" * 64:
        raise PhysicalEvidenceError("serial_identity_sha256 must not be all zero")
    return {field: str(value[field]) for field in sorted(expected)}


def inspect_challenged_image(
    path: Path,
) -> tuple[dict[str, object], str, str]:
    image = _read_regular_nofollow(path, boot_media.MAX_IMAGE_BYTES)
    try:
        metadata = boot_media.inspect_image(image)
    except boot_media.MediaError as error:
        raise PhysicalEvidenceError(f"boot image failed strict inspection: {error}") from error
    challenges = CHALLENGE_RE.findall(image)
    if len(challenges) != 1:
        raise PhysicalEvidenceError(
            "boot image must contain exactly one embedded ostadix.challenge token"
        )
    challenge = challenges[0].decode("ascii")
    if challenge == "0" * 64:
        raise PhysicalEvidenceError("embedded boot challenge must not be all zero")
    commits = SOURCE_COMMIT_RE.findall(image)
    if len(commits) != 1:
        raise PhysicalEvidenceError(
            "boot image must contain exactly one embedded ostadix.source_commit token"
        )
    source_commit = commits[0].decode("ascii")
    return metadata, challenge, source_commit


def load_media_write_binding(
    path: Path, image_path: Path
) -> tuple[dict[str, object], dict[str, object], str, str]:
    """Validate a successful writer-v2 record against the current source image."""

    record = _load_record(path, MAX_MACHINE_PROFILE_BYTES * 64)
    record_keys = {
        "schema",
        "device",
        "image",
        "image_bytes",
        "image_sha256",
        "source_image_sha256",
        "target_bytes",
        "target_plan_sha256",
        "target_image_sha256",
        "esp_sha256",
        "target_extents",
        "unwritten_policy",
        "unwritten_ranges",
        "target_plan",
        "confirmation",
        "written",
    }
    if set(record) != record_keys:
        raise PhysicalEvidenceError(
            "media-write record keys differ from canonical writer-v2 output"
        )
    if record.get("schema") != media_writer.WRITE_SCHEMA or record.get("written") is not True:
        raise PhysicalEvidenceError(
            "physical intent requires a successful ostadix.media-write/v2 record"
        )
    target_bytes = record.get("target_bytes")
    if type(target_bytes) is not int or target_bytes <= 0:
        raise PhysicalEvidenceError("media-write target_bytes is invalid")
    image = _read_regular_nofollow(image_path, boot_media.MAX_IMAGE_BYTES)
    try:
        plan = boot_media.plan_target_image(image, target_bytes)
        metadata = boot_media.inspect_image(image)
    except boot_media.MediaError as error:
        raise PhysicalEvidenceError(f"media-write source or target plan is invalid: {error}") from error
    expected_plan = plan.public()
    if record.get("target_plan") != expected_plan:
        raise PhysicalEvidenceError("media-write target plan differs from current image and capacity")
    expected_fields = {
        "image_bytes": plan.source_bytes,
        "image_sha256": plan.source_sha256,
        "source_image_sha256": plan.source_sha256,
        "target_bytes": plan.target_bytes,
        "target_plan_sha256": plan.target_plan_sha256,
        "target_image_sha256": plan.target_image_sha256,
        "esp_sha256": plan.esp_sha256,
        "target_extents": [extent.public() for extent in plan.extents],
        "unwritten_policy": boot_media.UNWRITTEN_POLICY,
        "unwritten_ranges": [
            {"offset": offset, "bytes": size}
            for offset, size in plan.unwritten_ranges
        ],
    }
    for field, expected in expected_fields.items():
        if record.get(field) != expected:
            raise PhysicalEvidenceError(f"media-write {field} differs from recomputed plan")
    device = record.get("device")
    if not isinstance(device, dict) or not device:
        raise PhysicalEvidenceError("media-write device identity is missing")
    if not isinstance(record.get("image"), str):
        raise PhysicalEvidenceError("media-write image path is malformed")
    try:
        recorded_image = Path(record["image"]).resolve(strict=True)
        supplied_image = image_path.resolve(strict=True)
    except OSError as error:
        raise PhysicalEvidenceError(f"cannot canonicalize media-write image path: {error}") from error
    if recorded_image != supplied_image:
        raise PhysicalEvidenceError(
            "media-write image path differs from the supplied challenged image"
        )
    try:
        expected_confirmation = media_writer.confirmation_token_from_public(
            device, expected_plan
        )
    except media_writer.WriterError as error:
        raise PhysicalEvidenceError(
            f"media-write public evidence is not canonical: {error}"
        ) from error
    if record.get("confirmation") != expected_confirmation:
        raise PhysicalEvidenceError("media-write confirmation does not match canonical evidence")
    challenges = CHALLENGE_RE.findall(image)
    commits = SOURCE_COMMIT_RE.findall(image)
    if len(challenges) != 1 or challenges[0] == b"0" * 64:
        raise PhysicalEvidenceError("boot image must contain exactly one nonzero embedded challenge")
    if len(commits) != 1:
        raise PhysicalEvidenceError("boot image must contain exactly one embedded source commit")
    binding = {
        "record_sha256": _sha256(_canonical_bytes(record)),
        "device": device,
        "target_bytes": plan.target_bytes,
        "target_plan_sha256": plan.target_plan_sha256,
        "target_image_sha256": plan.target_image_sha256,
        "target_plan": expected_plan,
    }
    return (
        metadata,
        binding,
        challenges[0].decode("ascii"),
        commits[0].decode("ascii"),
    )


def _seal(record: dict[str, object]) -> dict[str, object]:
    if "record_sha256" in record:
        raise PhysicalEvidenceError("record already contains record_sha256")
    result = dict(record)
    result["record_sha256"] = _sha256(_canonical_bytes(record))
    return result


def _verify_seal(record: dict[str, object], schema: str) -> None:
    if record.get("schema") != schema:
        raise PhysicalEvidenceError(f"record schema must be {schema}")
    digest = record.get("record_sha256")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise PhysicalEvidenceError("record_sha256 is malformed")
    unsigned = dict(record)
    del unsigned["record_sha256"]
    if _sha256(_canonical_bytes(unsigned)) != digest:
        raise PhysicalEvidenceError("record_sha256 does not match canonical record bytes")


def make_intent(
    *,
    image_path: Path,
    media_write_path: Path,
    machine_path: Path,
    expected_cpus: int,
    source_commit: str,
    created_utc: str,
    profile: str | None = None,
    extra_markers: Sequence[str] = (),
) -> dict[str, object]:
    selected_profile = transcript_profile(profile, expected_cpus)
    if not re.fullmatch(r"[0-9a-f]{40}", source_commit):
        raise PhysicalEvidenceError("source_commit must be a full lowercase Git object ID")
    metadata, media_write, challenge, embedded_source_commit = load_media_write_binding(
        media_write_path, image_path
    )
    if embedded_source_commit != source_commit:
        raise PhysicalEvidenceError(
            "embedded image source commit differs from the clean preparation repository HEAD"
        )
    machine = load_machine_profile(machine_path)
    markers = list(PROFILE_MARKERS[selected_profile])
    for marker in extra_markers:
        checked = _bounded_text(marker, "required marker")
        if checked in markers:
            raise PhysicalEvidenceError(f"duplicate required marker: {checked}")
        markers.append(checked)
    record: dict[str, object] = {
        "schema": INTENT_SCHEMA,
        "status": "prepared",
        "authority": "none",
        "created_utc": created_utc,
        "source_commit": source_commit,
        "challenge": challenge,
        "transcript_profile": selected_profile,
        "expected_cpu_count": expected_cpus,
        "image": {"path": str(image_path.resolve()), **metadata},
        "media_write": media_write,
        "machine": machine,
        "machine_sha256": _sha256(_canonical_bytes(machine)),
        "required_markers": markers,
        "nonclaims": list(NONCLAIMS),
    }
    return _seal(record)


def _load_record(path: Path, maximum: int) -> dict[str, object]:
    raw = _read_regular_nofollow(path, maximum)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise PhysicalEvidenceError(f"record is not valid JSON: {error}") from error
    if not isinstance(value, dict):
        raise PhysicalEvidenceError("record root must be an object")
    return value


def _exact_line_count(lines: Sequence[str], marker: str) -> int:
    return sum(line == marker for line in lines)


def _cpu_identities(lines: Sequence[str], expected_cpus: int) -> list[dict[str, object]]:
    matches = [CPU_LINE_RE.fullmatch(line) for line in lines]
    identities = [match for match in matches if match is not None]
    if expected_cpus == 1 and not identities:
        return []
    if len(identities) != expected_cpus:
        raise PhysicalEvidenceError(
            f"expected {expected_cpus} exact SMP CPU identity lines, found {len(identities)}"
        )
    try:
        records = [
            {
                "logical": int(match.group(1)),
                "apic": int(match.group(2)),
                "stack": match.group(3),
            }
            for match in identities
        ]
    except ValueError as error:
        raise PhysicalEvidenceError("SMP CPU identity contains an invalid integer") from error
    for record in records:
        logical_value = int(record["logical"])
        apic_value = int(record["apic"])
        stack_value = int(str(record["stack"]), 16)
        if logical_value >= MAX_CPUS or apic_value > 0xffff_ffff:
            raise PhysicalEvidenceError("SMP logical or APIC identity is outside v1 bounds")
        canonical_stack = (
            0 < stack_value <= 0x0000_7fff_ffff_ffff
            or 0xffff_8000_0000_0000 <= stack_value <= 0xffff_ffff_ffff_ffff
        )
        if not canonical_stack:
            raise PhysicalEvidenceError("SMP stack identity is not a canonical x86_64 address")
    logical = {int(record["logical"]) for record in records}
    apic = {int(record["apic"]) for record in records}
    stacks = {str(record["stack"]) for record in records}
    if logical != set(range(expected_cpus)):
        raise PhysicalEvidenceError("SMP logical CPU identities are not the exact expected range")
    if [int(record["logical"]) for record in records] != list(range(expected_cpus)):
        raise PhysicalEvidenceError("SMP logical CPU identities are not in causal order")
    if len(apic) != expected_cpus or len(stacks) != expected_cpus:
        raise PhysicalEvidenceError("SMP APIC or stack identities are not unique")
    for record in records:
        if int(str(record["stack"]), 16) % 16 != 0:
            raise PhysicalEvidenceError("SMP stack identity is not 16-byte aligned")
    return sorted(records, key=lambda item: int(item["logical"]))


def validate_transcript(
    *,
    transcript_path: Path,
    challenge: str,
    source_commit: str,
    expected_cpus: int,
    required_markers: Sequence[str],
    profile: str | None = None,
) -> tuple[dict[str, object], list[dict[str, object]]]:
    """Apply the exact transcript grammar shared by QEMU and physical records."""

    if not re.fullmatch(r"[0-9a-f]{64}", challenge) or challenge == "0" * 64:
        raise PhysicalEvidenceError("transcript challenge must be nonzero 64-hex")
    if not re.fullmatch(r"[0-9a-f]{40}", source_commit):
        raise PhysicalEvidenceError("transcript source commit must be 40 lowercase hex")
    selected_profile = transcript_profile(profile, expected_cpus)
    checked_markers: list[str] = []
    for marker in required_markers:
        checked = _bounded_text(marker, "required marker")
        if checked in checked_markers:
            raise PhysicalEvidenceError(f"duplicate required marker: {checked}")
        checked_markers.append(checked)
    if not checked_markers:
        raise PhysicalEvidenceError("at least one required transcript marker is required")

    transcript = _read_regular_nofollow(transcript_path, MAX_TRANSCRIPT_BYTES)
    if b"\0" in transcript:
        raise PhysicalEvidenceError("serial transcript contains a NUL byte")
    try:
        text = transcript.decode("ascii")
    except UnicodeDecodeError as error:
        raise PhysicalEvidenceError("serial transcript must be ASCII") from error
    lines = text.replace("\r\n", "\n").replace("\r", "\n").splitlines()
    challenge_line = f"OSTADIX boot challenge: {challenge}"
    if _exact_line_count(lines, challenge_line) != 1:
        raise PhysicalEvidenceError("serial transcript lacks one exact challenge line")
    source_line = f"OSTADIX source commit: {source_commit}"
    if _exact_line_count(lines, source_line) != 1:
        raise PhysicalEvidenceError("serial transcript lacks one exact source-commit line")
    for marker in checked_markers:
        if _exact_line_count(lines, marker) != 1:
            raise PhysicalEvidenceError(
                f"serial transcript marker count is not one: {marker!r}"
            )
    profile_markers = PROFILE_MARKERS[selected_profile]
    if checked_markers[: len(profile_markers)] != list(profile_markers):
        raise PhysicalEvidenceError(
            "required transcript markers do not begin with the selected profile contract"
        )
    for line in lines:
        if any(line.startswith(prefix) for prefix in CONTRADICTION_PREFIXES):
            raise PhysicalEvidenceError(f"serial transcript contains rejection: {line!r}")
    causal_lines = _profile_causal_lines(selected_profile, challenge, source_commit)
    causal_positions = [lines.index(line) for line in causal_lines]
    if causal_positions != sorted(causal_positions):
        raise PhysicalEvidenceError("serial transcript violates the profile's causal order")
    identities = _cpu_identities(lines, expected_cpus)
    if selected_profile == SMP4_PROFILE:
        cpu_indexes = [
            index for index, line in enumerate(lines) if CPU_LINE_RE.fullmatch(line) is not None
        ]
        stacks_index = lines.index("SMP AP stacks isolated: PASS")
        barrier_index = lines.index("SMP BSP/AP barrier: 4 CPUs PASS")
        if not cpu_indexes or not all(stacks_index < index < barrier_index for index in cpu_indexes):
            raise PhysicalEvidenceError(
                "SMP CPU identities do not occur between stack proof and barrier completion"
            )
    return (
        {
            "path": str(transcript_path.resolve()),
            "bytes": len(transcript),
            "sha256": _sha256(transcript),
        },
        identities,
    )


def make_observation(
    *,
    intent_path: Path,
    transcript_path: Path,
    image_override: Path | None,
    operator_assertion: str,
    created_utc: str,
) -> dict[str, object]:
    if operator_assertion != OPERATOR_ASSERTION:
        raise PhysicalEvidenceError(
            f"operator assertion must exactly equal {OPERATOR_ASSERTION!r}"
        )
    intent = _load_record(intent_path, MAX_MACHINE_PROFILE_BYTES * 8)
    _verify_seal(intent, INTENT_SCHEMA)
    if intent.get("status") != "prepared" or intent.get("authority") != "none":
        raise PhysicalEvidenceError("intent status or authority is invalid")

    image_info = intent.get("image")
    if not isinstance(image_info, dict) or not isinstance(image_info.get("path"), str):
        raise PhysicalEvidenceError("intent image binding is malformed")
    image_path = image_override if image_override is not None else Path(image_info["path"])
    media_write = intent.get("media_write")
    if not isinstance(media_write, dict):
        raise PhysicalEvidenceError("intent media_write binding is malformed")
    target_bytes = media_write.get("target_bytes")
    if type(target_bytes) is not int:
        raise PhysicalEvidenceError("intent media_write target_bytes is invalid")
    image = _read_regular_nofollow(image_path, boot_media.MAX_IMAGE_BYTES)
    try:
        metadata = boot_media.inspect_image(image)
        current_plan = boot_media.plan_target_image(image, target_bytes)
    except boot_media.MediaError as error:
        raise PhysicalEvidenceError(f"current image or target plan is invalid: {error}") from error
    challenges = CHALLENGE_RE.findall(image)
    commits = SOURCE_COMMIT_RE.findall(image)
    if len(challenges) != 1:
        raise PhysicalEvidenceError("current image lacks one exact challenge")
    if len(commits) != 1:
        raise PhysicalEvidenceError("current image lacks one exact source commit")
    challenge = challenges[0].decode("ascii")
    if challenge != intent.get("challenge"):
        raise PhysicalEvidenceError("current image challenge differs from intent")
    source_commit = commits[0].decode("ascii")
    if source_commit != intent.get("source_commit"):
        raise PhysicalEvidenceError("current image source commit differs from intent")
    for field in ("bytes", "sha256", "disk_guid", "partition_guid", "esp_sha256"):
        if metadata.get(field) != image_info.get(field):
            raise PhysicalEvidenceError(f"current image {field} differs from intent")
    if (
        current_plan.target_plan_sha256 != media_write.get("target_plan_sha256")
        or current_plan.public() != media_write.get("target_plan")
    ):
        raise PhysicalEvidenceError("current target plan differs from intent")

    required_markers = intent.get("required_markers")
    if not isinstance(required_markers, list) or not required_markers:
        raise PhysicalEvidenceError("intent required_markers is malformed")
    expected_cpus = intent.get("expected_cpu_count")
    if type(expected_cpus) is not int:
        raise PhysicalEvidenceError("intent expected_cpu_count is invalid")
    profile = intent.get("transcript_profile")
    if not isinstance(profile, str):
        raise PhysicalEvidenceError("intent transcript_profile is invalid")
    transcript_profile(profile, expected_cpus)
    if not all(isinstance(marker, str) for marker in required_markers):
        raise PhysicalEvidenceError("intent required_markers is malformed")
    transcript_record, cpu_identities = validate_transcript(
        transcript_path=transcript_path,
        challenge=challenge,
        source_commit=source_commit,
        expected_cpus=expected_cpus,
        required_markers=required_markers,
        profile=profile,
    )

    record: dict[str, object] = {
        "schema": OBSERVATION_SCHEMA,
        "status": "recorded",
        "authority": "none",
        "admission": "not-performed",
        "created_utc": created_utc,
        "intent_sha256": intent["record_sha256"],
        "source_commit": intent["source_commit"],
        "challenge": challenge,
        "image_sha256": metadata["sha256"],
        "machine_sha256": intent["machine_sha256"],
        "media_write_sha256": media_write["record_sha256"],
        "target_bytes": target_bytes,
        "target_plan_sha256": current_plan.target_plan_sha256,
        "target_image_sha256": current_plan.target_image_sha256,
        "transcript_profile": profile,
        "expected_cpu_count": expected_cpus,
        "observed_cpu_identities": cpu_identities,
        "required_markers": required_markers,
        "transcript": transcript_record,
        "operator_assertion": operator_assertion,
        "nonclaims": list(NONCLAIMS),
    }
    return _seal(record)


def _write_record(path: Path, record: dict[str, object]) -> None:
    """Publish one immutable record without replacing an earlier attempt."""

    data = _canonical_bytes(record)
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        try:
            os.link(temporary, path, follow_symlinks=False)
        except FileExistsError as error:
            raise PhysicalEvidenceError(f"refusing to replace existing evidence: {path}") from error
        directory_flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
        directory_fd = os.open(path.parent, directory_flags)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def _git_commit(root: Path) -> str:
    try:
        commit = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=root,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        ).stdout.strip()
        dirty = subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=root,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as error:
        raise PhysicalEvidenceError(f"cannot bind current Git identity: {error}") from error
    if dirty:
        raise PhysicalEvidenceError("physical intent requires a clean committed worktree")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise PhysicalEvidenceError("Git did not return a full lowercase commit ID")
    return commit


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Prepare or validate authority-free OSTADIX physical-boot observations"
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    challenge = subparsers.add_parser(
        "challenge", help="generate a caller-held random 256-bit boot challenge"
    )
    challenge.add_argument("--raw", action="store_true", help="print only the lowercase challenge")

    prepare = subparsers.add_parser("prepare", help="bind challenged media to a machine intent")
    prepare.add_argument("--image", required=True, type=Path)
    prepare.add_argument("--media-write", required=True, type=Path)
    prepare.add_argument("--machine", required=True, type=Path)
    prepare.add_argument("--expected-cpus", required=True, type=int)
    prepare.add_argument("--profile", choices=tuple(PROFILE_CPU_COUNTS))
    prepare.add_argument("--required-marker", action="append", default=[])
    prepare.add_argument("--output", required=True, type=Path)

    verify = subparsers.add_parser("verify", help="record a challenged serial observation")
    verify.add_argument("--intent", required=True, type=Path)
    verify.add_argument("--transcript", required=True, type=Path)
    verify.add_argument("--image", type=Path, help="override the intent's local image path")
    verify.add_argument("--assert-physical", required=True)
    verify.add_argument("--output", required=True, type=Path)

    check = subparsers.add_parser(
        "check-transcript",
        help="apply the shared exact transcript grammar without asserting a substrate",
    )
    check.add_argument("--transcript", required=True, type=Path)
    check.add_argument("--challenge", required=True)
    check.add_argument("--source-commit", required=True)
    check.add_argument("--expected-cpus", required=True, type=int)
    check.add_argument("--profile", choices=tuple(PROFILE_CPU_COUNTS))
    check.add_argument("--required-marker", action="append", default=[])
    check.add_argument("--context", choices=("qemu-tcg", "operator-captured"), required=True)
    check.add_argument("--output", type=Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "challenge":
            value = "0" * 64
            while value == "0" * 64:
                value = secrets.token_hex(32)
            print(value if args.raw else f"OSTADIX_BOOT_CHALLENGE={value}")
            return 0
        if args.command == "check-transcript":
            selected_profile = transcript_profile(args.profile, args.expected_cpus)
            markers = [*PROFILE_MARKERS[selected_profile], *args.required_marker]
            transcript_record, identities = validate_transcript(
                transcript_path=args.transcript,
                challenge=args.challenge,
                source_commit=args.source_commit,
                expected_cpus=args.expected_cpus,
                required_markers=markers,
                profile=selected_profile,
            )
            record = _seal(
                {
                    "schema": TRANSCRIPT_CHECK_SCHEMA,
                    "status": "matched",
                    "authority": "none",
                    "context": args.context,
                    "challenge": args.challenge,
                    "source_commit": args.source_commit,
                    "transcript_profile": selected_profile,
                    "expected_cpu_count": args.expected_cpus,
                    "observed_cpu_identities": identities,
                    "required_markers": markers,
                    "transcript": transcript_record,
                    "nonclaims": list(NONCLAIMS),
                }
            )
            if args.output is not None:
                _write_record(args.output, record)
            print(_canonical_bytes(record).decode("ascii"), end="")
            return 0
        root = Path(__file__).resolve().parents[1]
        if args.command == "prepare":
            record = make_intent(
                image_path=args.image,
                media_write_path=args.media_write,
                machine_path=args.machine,
                expected_cpus=args.expected_cpus,
                source_commit=_git_commit(root),
                created_utc=_utc_now(),
                profile=args.profile,
                extra_markers=args.required_marker,
            )
        else:
            record = make_observation(
                intent_path=args.intent,
                transcript_path=args.transcript,
                image_override=args.image,
                operator_assertion=args.assert_physical,
                created_utc=_utc_now(),
            )
        _write_record(args.output, record)
        print(_canonical_bytes(record).decode("ascii"), end="")
        return 0
    except (PhysicalEvidenceError, boot_media.MediaError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
