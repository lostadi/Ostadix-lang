#!/usr/bin/env python3
"""Checksum-pinned, bounded QEMU boots of upstream foreign kernels.

This is a host-side substrate lab.  It deliberately does not participate in
the O-core release gate registry and does not claim KernelWorld governance.
"""

from __future__ import annotations

import argparse
import gzip
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import lzma
import math
import os
from pathlib import Path, PurePosixPath
import re
import selectors
import secrets
import shutil
import signal
import stat
import subprocess
import time
import tomllib
from typing import Any, BinaryIO, Iterable
import urllib.parse
import urllib.request


PROJECT_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = PROJECT_ROOT / "evidence" / "foreign_kernel_lab.toml"
MANIFEST_SCHEMA = "ostadix.foreign-kernel-lab/v1"
OBSERVATION_SCHEMA = "ostadix.foreign-kernel-boot-observation/v1"
CLAIM_CLASS = "qemu_tcg_upstream_foreign_kernel_boot"
ID_PATTERN = re.compile(r"[a-z0-9][a-z0-9._-]*\Z")
SHA256_PATTERN = re.compile(r"[0-9a-f]{64}\Z")
QEMU_VERSION_PATTERN = re.compile(
    r"\AQEMU emulator version [0-9]+(?:\.[0-9]+){1,3}(?:\s|\Z)"
)
PLACEHOLDER_PATTERN = re.compile(r"\{(artifact|firmware):([a-z0-9][a-z0-9._-]*)\}")
ANSI_PATTERN = re.compile(
    r"\x1b(?:\][^\x07]*(?:\x07|\x1b\\)|\[[0-?]*[ -/]*[@-~]|[@-_])"
)
MAX_ARTIFACT_BYTES = 8 * 1024 * 1024 * 1024
MAX_TIMEOUT_SECONDS = 300.0
MAX_CAPTURE_BYTES = 8 * 1024 * 1024
DOWNLOAD_CHUNK_BYTES = 1024 * 1024
QEMU_VERSION_CAPTURE_BYTES = 4096
QEMU_VERSION_TIMEOUT_SECONDS = 5.0
TOOL_DIAGNOSTIC_BYTES = 262_144
ZSTD_DIAGNOSTIC_BYTES = 65_536
EXTRACTION_TIMEOUT_SECONDS = 180.0
SOURCE_RELEASE_SCHEMA = "ostadix-source-release-v1"
SOURCE_RELEASE_MANIFEST = "SOURCE-MANIFEST.json"
SOURCE_RELEASE_CHECKSUMS = "SHA256SUMS"
MAX_SOURCE_MANIFEST_BYTES = 16 * 1024 * 1024
MAX_SOURCE_FILES = 20_000


class LabError(RuntimeError):
    """A fail-closed manifest, artifact, or boot error."""


@dataclass(frozen=True)
class FileIdentity:
    size_bytes: int
    sha256: str


@dataclass(frozen=True)
class PinnedInput:
    source_path: Path
    identity: FileIdentity
    descriptor: int
    child_path: str


@dataclass
class OpenDirectory:
    path: Path
    descriptor: int

    def __enter__(self) -> "OpenDirectory":
        return self

    def __exit__(self, *_arguments: object) -> None:
        os.close(self.descriptor)


@dataclass(frozen=True)
class ArtifactMember:
    id: str
    path: str
    filename: str
    size_bytes: int
    sha256: str


@dataclass(frozen=True)
class Artifact:
    id: str
    filename: str
    url: str
    size_bytes: int
    sha256: str
    integrity: str
    checksum_url: str | None = None
    unpack: str | None = None
    expanded_id: str | None = None
    expanded_filename: str | None = None
    expanded_size_bytes: int | None = None
    expanded_sha256: str | None = None
    members: tuple[ArtifactMember, ...] = ()


@dataclass(frozen=True)
class Firmware:
    id: str
    candidates: tuple[str, ...]


@dataclass(frozen=True)
class ConsoleAction:
    trigger: str
    commands: tuple[str, ...]


@dataclass(frozen=True)
class Guest:
    id: str
    family: str
    version: str
    architecture: str
    qemu_profile: str
    cache_dir: str
    qemu_executable: str
    timeout_seconds: float
    post_completion_seconds: float
    max_capture_bytes: int
    qemu_args: tuple[str, ...]
    required_markers: tuple[str, ...]
    unique_markers: tuple[str, ...]
    forbidden_markers: tuple[str, ...]
    console_actions: tuple[ConsoleAction, ...]
    claim: str
    nonclaims: tuple[str, ...]
    artifacts: tuple[Artifact, ...]


@dataclass(frozen=True)
class Manifest:
    path: Path
    identity: FileIdentity
    schema: str
    claim_class: str
    claims: tuple[str, ...]
    nonclaims: tuple[str, ...]
    firmware: dict[str, Firmware]
    guests: tuple[Guest, ...]


def _require_keys(value: dict[str, Any], allowed: set[str], context: str) -> None:
    unknown = sorted(set(value) - allowed)
    if unknown:
        raise LabError(f"{context} has unknown fields: {unknown}")


def _string(value: Any, context: str, *, maximum: int = 4096) -> str:
    if not isinstance(value, str) or not value or len(value) > maximum:
        raise LabError(f"{context} must be a non-empty string of at most {maximum} bytes")
    return value


def _string_list(
    value: Any,
    context: str,
    *,
    minimum: int = 0,
    maximum: int = 64,
    unique: bool = True,
) -> tuple[str, ...]:
    if not isinstance(value, list) or not minimum <= len(value) <= maximum:
        raise LabError(f"{context} must contain {minimum}..{maximum} strings")
    result = tuple(_string(item, f"{context} item", maximum=1024) for item in value)
    if unique and len(set(result)) != len(result):
        raise LabError(f"{context} must not contain duplicates")
    return result


def _positive_int(value: Any, context: str, *, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 0 < value <= maximum:
        raise LabError(f"{context} must be an integer within 1..{maximum}")
    return value


def _positive_float(value: Any, context: str, *, maximum: float) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise LabError(f"{context} must be numeric")
    converted = float(value)
    if not math.isfinite(converted) or not 0.0 < converted <= maximum:
        raise LabError(f"{context} must be finite and within 0..{maximum}")
    return converted


def _safe_id(value: Any, context: str) -> str:
    result = _string(value, context, maximum=128)
    if ID_PATTERN.fullmatch(result) is None:
        raise LabError(f"{context} is not a safe lowercase identifier")
    return result


def _digest(value: Any, context: str) -> str:
    result = _string(value, context, maximum=64)
    if SHA256_PATTERN.fullmatch(result) is None:
        raise LabError(f"{context} must be exactly 64 lowercase hexadecimal digits")
    return result


def _filename(value: Any, context: str) -> str:
    result = _string(value, context, maximum=255)
    if result in {".", ".."} or Path(result).name != result or "/" in result or "\\" in result:
        raise LabError(f"{context} must be one plain filename")
    return result


def _https_url(value: Any, context: str) -> str:
    result = _string(value, context)
    parsed = urllib.parse.urlsplit(result)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        raise LabError(f"{context} must be an HTTPS URL without embedded credentials")
    return result


def _iso_member_path(value: Any, context: str) -> str:
    result = _string(value, context, maximum=1024)
    path = PurePosixPath(result)
    if (
        not path.is_absolute()
        or result == "/"
        or ".." in path.parts
        or "." in path.parts
        or "\\" in result
        or "\x00" in result
        or str(path) != result
    ):
        raise LabError(f"{context} must be one canonical absolute ISO member path")
    return result


def hash_file(path: Path) -> FileIdentity:
    absolute = Path(os.path.abspath(path.expanduser()))
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(absolute, flags)
    except FileNotFoundError as error:
        raise LabError(f"missing artifact: {absolute}") from error
    except OSError as error:
        raise LabError(
            f"artifact is not an openable non-symlink regular file: {absolute}: {error}"
        ) from error
    try:
        return _hash_descriptor(descriptor)
    finally:
        os.close(descriptor)


def _hash_descriptor(descriptor: int) -> FileIdentity:
    before = os.fstat(descriptor)
    if not stat.S_ISREG(before.st_mode):
        raise LabError("opened input is not a regular file")
    digest = hashlib.sha256()
    size = 0
    offset = 0
    while chunk := os.pread(descriptor, DOWNLOAD_CHUNK_BYTES, offset):
        digest.update(chunk)
        size += len(chunk)
        offset += len(chunk)
    after = os.fstat(descriptor)
    stable_fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
    if any(getattr(before, field) != getattr(after, field) for field in stable_fields):
        raise LabError("opened input changed while it was being hashed")
    return FileIdentity(size_bytes=size, sha256=digest.hexdigest())


def _read_descriptor_exact(descriptor: int, size: int) -> bytes:
    payload = bytearray()
    offset = 0
    while offset < size:
        chunk = os.pread(descriptor, min(DOWNLOAD_CHUNK_BYTES, size - offset), offset)
        if not chunk:
            raise LabError("opened input ended before its admitted size")
        payload.extend(chunk)
        offset += len(chunk)
    return bytes(payload)


def _open_pinned_input(
    path: Path, expected: FileIdentity | None = None
) -> PinnedInput:
    absolute = Path(os.path.abspath(path.expanduser()))
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(absolute, flags)
    except OSError as error:
        raise LabError(f"cannot open admitted input {absolute}: {error}") from error
    try:
        identity = _hash_descriptor(descriptor)
        if expected is not None and identity != expected:
            raise LabError(
                f"artifact identity mismatch for {absolute}: expected size={expected.size_bytes} "
                f"sha256={expected.sha256}, observed size={identity.size_bytes} "
                f"sha256={identity.sha256}"
            )
        descriptor_root = "/proc/self/fd" if Path("/proc/self/fd").is_dir() else "/dev/fd"
        return PinnedInput(
            source_path=absolute,
            identity=identity,
            descriptor=descriptor,
            child_path=f"{descriptor_root}/{descriptor}",
        )
    except BaseException:
        os.close(descriptor)
        raise


def _open_pinned_input_at(
    directory: OpenDirectory,
    filename: str,
    expected: FileIdentity | None = None,
) -> PinnedInput:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(filename, flags, dir_fd=directory.descriptor)
    except OSError as error:
        raise LabError(
            f"cannot open admitted cache input {directory.path / filename}: {error}"
        ) from error
    try:
        identity = _hash_descriptor(descriptor)
        if expected is not None and identity != expected:
            raise LabError(
                f"artifact identity mismatch for {directory.path / filename}: "
                f"expected size={expected.size_bytes} sha256={expected.sha256}, "
                f"observed size={identity.size_bytes} sha256={identity.sha256}"
            )
        descriptor_root = "/proc/self/fd" if Path("/proc/self/fd").is_dir() else "/dev/fd"
        return PinnedInput(
            source_path=directory.path / filename,
            identity=identity,
            descriptor=descriptor,
            child_path=f"{descriptor_root}/{descriptor}",
        )
    except BaseException:
        os.close(descriptor)
        raise


def _parse_artifact(raw: Any, context: str) -> Artifact:
    if not isinstance(raw, dict):
        raise LabError(f"{context} must be a table")
    _require_keys(
        raw,
        {
            "id",
            "filename",
            "url",
            "size_bytes",
            "sha256",
            "integrity",
            "checksum_url",
            "unpack",
            "expanded_id",
            "expanded_filename",
            "expanded_size_bytes",
            "expanded_sha256",
            "members",
        },
        context,
    )
    checksum_url = raw.get("checksum_url")
    if checksum_url is not None:
        checksum_url = _https_url(checksum_url, f"{context}.checksum_url")
    unpack = raw.get("unpack")
    if unpack is not None and unpack not in {"gzip", "xz", "zstd"}:
        raise LabError(f"{context}.unpack supports only 'gzip', 'xz', or 'zstd'")
    expanded_values = (
        raw.get("expanded_id"),
        raw.get("expanded_filename"),
        raw.get("expanded_size_bytes"),
        raw.get("expanded_sha256"),
    )
    if unpack is None and any(value is not None for value in expanded_values):
        raise LabError(f"{context} has expanded fields without an unpack transform")
    if unpack is not None and any(value is None for value in expanded_values):
        raise LabError(f"{context} must completely describe the expanded artifact")
    members_raw = raw.get("members", [])
    if not isinstance(members_raw, list) or len(members_raw) > 8:
        raise LabError(f"{context}.members must contain at most 8 tables")
    if unpack is not None and members_raw:
        raise LabError(f"{context} cannot combine decompression with ISO member extraction")
    members: list[ArtifactMember] = []
    for index, member_raw in enumerate(members_raw):
        member_context = f"{context}.members[{index}]"
        if not isinstance(member_raw, dict):
            raise LabError(f"{member_context} must be a table")
        _require_keys(
            member_raw,
            {"id", "path", "filename", "size_bytes", "sha256"},
            member_context,
        )
        members.append(
            ArtifactMember(
                id=_safe_id(member_raw.get("id"), f"{member_context}.id"),
                path=_iso_member_path(
                    member_raw.get("path"), f"{member_context}.path"
                ),
                filename=_filename(
                    member_raw.get("filename"), f"{member_context}.filename"
                ),
                size_bytes=_positive_int(
                    member_raw.get("size_bytes"),
                    f"{member_context}.size_bytes",
                    maximum=MAX_ARTIFACT_BYTES,
                ),
                sha256=_digest(
                    member_raw.get("sha256"), f"{member_context}.sha256"
                ),
            )
        )
    return Artifact(
        id=_safe_id(raw.get("id"), f"{context}.id"),
        filename=_filename(raw.get("filename"), f"{context}.filename"),
        url=_https_url(raw.get("url"), f"{context}.url"),
        size_bytes=_positive_int(
            raw.get("size_bytes"), f"{context}.size_bytes", maximum=MAX_ARTIFACT_BYTES
        ),
        sha256=_digest(raw.get("sha256"), f"{context}.sha256"),
        integrity=_string(raw.get("integrity"), f"{context}.integrity", maximum=256),
        checksum_url=checksum_url,
        unpack=unpack,
        expanded_id=(
            _safe_id(raw.get("expanded_id"), f"{context}.expanded_id")
            if unpack is not None
            else None
        ),
        expanded_filename=(
            _filename(raw.get("expanded_filename"), f"{context}.expanded_filename")
            if unpack is not None
            else None
        ),
        expanded_size_bytes=(
            _positive_int(
                raw.get("expanded_size_bytes"),
                f"{context}.expanded_size_bytes",
                maximum=MAX_ARTIFACT_BYTES,
            )
            if unpack is not None
            else None
        ),
        expanded_sha256=(
            _digest(raw.get("expanded_sha256"), f"{context}.expanded_sha256")
            if unpack is not None
            else None
        ),
        members=tuple(members),
    )


def _validate_qemu_args(
    guest_id: str,
    architecture: str,
    profile: str,
    arguments: tuple[str, ...],
) -> None:
    value_options = {
        "-accel",
        "-append",
        "-bios",
        "-boot",
        "-cpu",
        "-device",
        "-display",
        "-drive",
        "-initrd",
        "-kernel",
        "-m",
        "-machine",
        "-monitor",
        "-nic",
        "-serial",
        "-smp",
    }
    flag_options = {"-no-reboot", "-nodefaults"}
    values: dict[str, list[str]] = {option: [] for option in value_options}
    flags: list[str] = []
    index = 0
    while index < len(arguments):
        option = arguments[index]
        if option in flag_options:
            flags.append(option)
            index += 1
            continue
        if option not in value_options:
            raise LabError(
                f"guest {guest_id} uses unsupported or unsafe QEMU option {option!r}"
            )
        if index + 1 >= len(arguments):
            raise LabError(f"guest {guest_id} QEMU option {option} lacks a value")
        values[option].append(arguments[index + 1])
        index += 2

    safe_singletons = {
        "-nic": "none",
        "-display": "none",
        "-serial": "stdio",
        "-monitor": "none",
    }
    invalid_pairs = {
        option: observed
        for option, expected in safe_singletons.items()
        if (observed := values[option]) != [expected]
    }
    if invalid_pairs:
        raise LabError(
            f"guest {guest_id} lacks exact safe headless QEMU pairs: {invalid_pairs}"
        )
    if values["-accel"]:
        raise LabError(f"guest {guest_id} must declare TCG only through -machine")
    if values["-smp"] != ["1"]:
        raise LabError(f"guest {guest_id} must remain single-vCPU")
    if len(values["-append"]) > 1:
        raise LabError(f"guest {guest_id} may declare at most one kernel command line")

    if profile == "aarch64-virt":
        if architecture != "aarch64":
            raise LabError(f"guest {guest_id} AArch64 profile requires aarch64")
        if flags != ["-no-reboot"]:
            raise LabError(f"guest {guest_id} must pass -no-reboot exactly once")
        if values["-machine"] != ["virt,gic-version=3,accel=tcg"]:
            raise LabError(
                f"guest {guest_id} must use the exact bounded AArch64 virt/TCG machine"
            )
        if values["-cpu"] not in (["cortex-a57"], ["cortex-a72"]):
            raise LabError(f"guest {guest_id} must use one admitted bounded TCG CPU")
        if values["-m"] not in (["512M"], ["1024M"]):
            raise LabError(f"guest {guest_id} memory must be exactly 512M or 1024M")
        if values["-boot"]:
            raise LabError(f"guest {guest_id} AArch64 profile does not admit -boot")
    elif profile == "x86_64-q35":
        if architecture != "x86_64":
            raise LabError(f"guest {guest_id} x86_64 profile requires x86_64")
        if flags not in (["-no-reboot"], ["-nodefaults", "-no-reboot"]):
            raise LabError(
                f"guest {guest_id} x86_64 profile requires -no-reboot and admits "
                "-nodefaults only when the guest does not require default VGA"
            )
        if values["-machine"] != ["q35,accel=tcg"]:
            raise LabError(
                f"guest {guest_id} must use the exact bounded x86_64 Q35/TCG machine"
            )
        if values["-cpu"] != ["qemu64"]:
            raise LabError(f"guest {guest_id} x86_64 profile must use qemu64")
        if values["-m"] not in (["512M"], ["1024M"], ["2048M"]):
            raise LabError(
                f"guest {guest_id} x86_64 memory must be 512M, 1024M, or 2048M"
            )
        if values["-bios"]:
            raise LabError(f"guest {guest_id} x86_64 profile does not admit -bios")
        if values["-boot"] not in (
            [],
            ["order=c,strict=on"],
            ["order=d,strict=on"],
        ):
            raise LabError(f"guest {guest_id} has an unsupported bounded boot order")
    else:
        raise LabError(f"guest {guest_id} uses unknown QEMU profile {profile!r}")

    for option in ("-kernel", "-initrd"):
        if len(values[option]) > 1 or any(
            PLACEHOLDER_PATTERN.fullmatch(value) is None
            or not value.startswith("{artifact:")
            for value in values[option]
        ):
            raise LabError(f"guest {guest_id} {option} must reference one admitted artifact")
    if len(values["-bios"]) > 1 or any(
        PLACEHOLDER_PATTERN.fullmatch(value) is None or not value.startswith("{firmware:")
        for value in values["-bios"]
    ):
        raise LabError(f"guest {guest_id} -bios must reference admitted firmware")
    if len(values["-drive"]) > 1:
        raise LabError(f"guest {guest_id} may declare at most one read-only drive")
    for drive in values["-drive"]:
        fields = drive.split(",")
        file_fields = [field.split("=", 1)[1] for field in fields if field.startswith("file=")]
        admitted_shapes = [
            {"if=none", "id=cd0", "media=cdrom", "readonly=on", "format=raw"},
        ]
        if profile == "x86_64-q35":
            admitted_shapes.append(
                {
                    "if=none",
                    "id=disk0",
                    "media=disk",
                    "readonly=on",
                    "snapshot=on",
                    "format=qcow2",
                }
            )
        observed_shape = set(fields) - {f"file={file_fields[0]}"} if file_fields else set()
        if (
            len(fields) not in {6, 7}
            or len(file_fields) != 1
            or PLACEHOLDER_PATTERN.fullmatch(file_fields[0]) is None
            or not file_fields[0].startswith("{artifact:")
            or observed_shape not in admitted_shapes
        ):
            raise LabError(
                f"guest {guest_id} drives must be raw, read-only admitted artifacts"
            )
    allowed_devices = {
        "virtio-scsi-pci,id=scsi0",
        "scsi-cd,drive=cd0,bus=scsi0.0",
        "ide-cd,drive=cd0,bus=ide.0",
        "virtio-blk-pci,drive=disk0",
    }
    if len(set(values["-device"])) != len(values["-device"]) or any(
        device not in allowed_devices for device in values["-device"]
    ):
        raise LabError(
            f"guest {guest_id} declares a device outside the foreign-lab allowlist"
        )
    if values["-drive"]:
        drive = values["-drive"][0]
        if "id=cd0" in drive:
            admitted_cd_devices = (
                ["ide-cd,drive=cd0,bus=ide.0"],
                ["virtio-scsi-pci,id=scsi0", "scsi-cd,drive=cd0,bus=scsi0.0"],
            )
            if values["-device"] not in admitted_cd_devices:
                raise LabError(
                    f"guest {guest_id} CD drive requires one exact admitted controller path"
                )
        elif values["-device"] != ["virtio-blk-pci,drive=disk0"]:
            raise LabError(
                f"guest {guest_id} disk drive requires the exact virtio-blk path"
            )
    elif values["-device"]:
        raise LabError(f"guest {guest_id} declares devices without an admitted drive")


def _parse_guest(raw: Any, firmware_ids: set[str], context: str) -> Guest:
    if not isinstance(raw, dict):
        raise LabError(f"{context} must be a table")
    _require_keys(
        raw,
        {
            "id",
            "family",
            "version",
            "architecture",
            "qemu_profile",
            "cache_dir",
            "qemu_executable",
            "timeout_seconds",
            "post_completion_seconds",
            "max_capture_bytes",
            "qemu_args",
            "required_markers",
            "unique_markers",
            "forbidden_markers",
            "console_actions",
            "claim",
            "nonclaims",
            "artifacts",
        },
        context,
    )
    guest_id = _safe_id(raw.get("id"), f"{context}.id")
    artifacts_raw = raw.get("artifacts")
    if not isinstance(artifacts_raw, list) or not artifacts_raw:
        raise LabError(f"{context}.artifacts must be a non-empty array")
    artifacts = tuple(
        _parse_artifact(value, f"{context}.artifacts[{index}]")
        for index, value in enumerate(artifacts_raw)
    )
    artifact_ids: set[str] = set()
    filenames: set[str] = set()
    for artifact in artifacts:
        published_ids = (
            (artifact.id,)
            + ((artifact.expanded_id,) if artifact.expanded_id else ())
            + tuple(member.id for member in artifact.members)
        )
        published_names = (artifact.filename,) + (
            (artifact.expanded_filename,) if artifact.expanded_filename else ()
        ) + tuple(member.filename for member in artifact.members)
        for published_id in published_ids:
            assert published_id is not None
            if published_id in artifact_ids:
                raise LabError(f"{context} repeats artifact id {published_id!r}")
            artifact_ids.add(published_id)
        for published_name in published_names:
            assert published_name is not None
            if published_name in filenames:
                raise LabError(f"{context} repeats artifact filename {published_name!r}")
            filenames.add(published_name)
    architecture = _safe_id(raw.get("architecture"), f"{context}.architecture")
    qemu_profile = _safe_id(raw.get("qemu_profile"), f"{context}.qemu_profile")
    qemu_args = _string_list(
        raw.get("qemu_args"),
        f"{context}.qemu_args",
        minimum=8,
        maximum=96,
        unique=False,
    )
    _validate_qemu_args(guest_id, architecture, qemu_profile, qemu_args)
    referenced_artifacts: set[str] = set()
    referenced_firmware: set[str] = set()
    for argument in qemu_args:
        matches = list(PLACEHOLDER_PATTERN.finditer(argument))
        scrubbed = PLACEHOLDER_PATTERN.sub("", argument)
        if "{" in scrubbed or "}" in scrubbed:
            raise LabError(f"{context}.qemu_args has an unknown placeholder in {argument!r}")
        for match in matches:
            kind, identifier = match.groups()
            if kind == "artifact":
                referenced_artifacts.add(identifier)
            else:
                referenced_firmware.add(identifier)
    unknown_artifacts = referenced_artifacts - artifact_ids
    unknown_firmware = referenced_firmware - firmware_ids
    if unknown_artifacts or unknown_firmware:
        raise LabError(
            f"{context} references unknown artifacts={sorted(unknown_artifacts)} "
            f"firmware={sorted(unknown_firmware)}"
        )
    if not referenced_artifacts:
        raise LabError(f"{context} must reference at least one verified artifact")
    required_markers = _string_list(
        raw.get("required_markers"), f"{context}.required_markers", minimum=2, maximum=24
    )
    forbidden_markers = _string_list(
        raw.get("forbidden_markers"), f"{context}.forbidden_markers", minimum=1, maximum=24
    )
    unique_markers = _string_list(
        raw.get("unique_markers", []), f"{context}.unique_markers", maximum=24
    )
    unknown_unique = set(unique_markers) - set(required_markers)
    if unknown_unique:
        raise LabError(
            f"{context}.unique_markers must be required markers: {sorted(unknown_unique)}"
        )
    overlap = set(required_markers) & set(forbidden_markers)
    if overlap:
        raise LabError(f"{context} markers overlap: {sorted(overlap)}")
    actions_raw = raw.get("console_actions", [])
    if not isinstance(actions_raw, list) or len(actions_raw) > 8:
        raise LabError(f"{context}.console_actions must contain at most 8 tables")
    console_actions: list[ConsoleAction] = []
    action_triggers: set[str] = set()
    for index, action_raw in enumerate(actions_raw):
        action_context = f"{context}.console_actions[{index}]"
        if not isinstance(action_raw, dict):
            raise LabError(f"{action_context} must be a table")
        _require_keys(action_raw, {"trigger", "commands"}, action_context)
        trigger = _string(
            action_raw.get("trigger"), f"{action_context}.trigger", maximum=512
        )
        if trigger not in required_markers:
            raise LabError(f"{action_context}.trigger must also be a required marker")
        if trigger in action_triggers:
            raise LabError(f"{context}.console_actions repeats trigger {trigger!r}")
        action_triggers.add(trigger)
        commands = _string_list(
            action_raw.get("commands"),
            f"{action_context}.commands",
            minimum=1,
            maximum=10,
            unique=False,
        )
        for command in commands:
            if any(not " " <= character <= "~" for character in command):
                raise LabError(
                    f"{action_context}.commands must contain printable ASCII only"
                )
        console_actions.append(ConsoleAction(trigger=trigger, commands=commands))
    action_positions = [required_markers.index(action.trigger) for action in console_actions]
    if action_positions != sorted(action_positions):
        raise LabError(
            f"{context}.console_actions must follow required_markers order"
        )
    qemu_executable = _string(raw.get("qemu_executable"), f"{context}.qemu_executable", maximum=128)
    if Path(qemu_executable).name != qemu_executable or not qemu_executable.startswith("qemu-system-"):
        raise LabError(f"{context}.qemu_executable must be a bare qemu-system-* name")
    expected_executable = {
        "aarch64-virt": "qemu-system-aarch64",
        "x86_64-q35": "qemu-system-x86_64",
    }[qemu_profile]
    if qemu_executable != expected_executable:
        raise LabError(
            f"{context}.qemu_executable must be {expected_executable!r} for {qemu_profile}"
        )
    return Guest(
        id=guest_id,
        family=_safe_id(raw.get("family"), f"{context}.family"),
        version=_string(raw.get("version"), f"{context}.version", maximum=256),
        architecture=architecture,
        qemu_profile=qemu_profile,
        cache_dir=_filename(raw.get("cache_dir"), f"{context}.cache_dir"),
        qemu_executable=qemu_executable,
        timeout_seconds=_positive_float(
            raw.get("timeout_seconds"), f"{context}.timeout_seconds", maximum=MAX_TIMEOUT_SECONDS
        ),
        post_completion_seconds=_positive_float(
            raw.get("post_completion_seconds"),
            f"{context}.post_completion_seconds",
            maximum=10.0,
        ),
        max_capture_bytes=_positive_int(
            raw.get("max_capture_bytes"),
            f"{context}.max_capture_bytes",
            maximum=MAX_CAPTURE_BYTES,
        ),
        qemu_args=qemu_args,
        required_markers=required_markers,
        unique_markers=unique_markers,
        forbidden_markers=forbidden_markers,
        console_actions=tuple(console_actions),
        claim=_string(raw.get("claim"), f"{context}.claim", maximum=1024),
        nonclaims=_string_list(raw.get("nonclaims"), f"{context}.nonclaims", minimum=1, maximum=24),
        artifacts=artifacts,
    )


def parse_manifest_data(raw: Any, path: Path, identity: FileIdentity) -> Manifest:
    if not isinstance(raw, dict):
        raise LabError("manifest root must be a table")
    _require_keys(raw, {"schema", "claim_class", "claims", "nonclaims", "firmware", "guests"}, "manifest")
    schema = _string(raw.get("schema"), "manifest.schema", maximum=128)
    if schema != MANIFEST_SCHEMA:
        raise LabError(f"unsupported manifest schema {schema!r}")
    claim_class = _string(raw.get("claim_class"), "manifest.claim_class", maximum=128)
    if claim_class != CLAIM_CLASS:
        raise LabError(f"manifest claim_class must remain {CLAIM_CLASS!r}")
    firmware_raw = raw.get("firmware", {})
    if not isinstance(firmware_raw, dict):
        raise LabError("manifest.firmware must be a table")
    firmware: dict[str, Firmware] = {}
    for identifier, value in firmware_raw.items():
        firmware_id = _safe_id(identifier, "firmware id")
        if not isinstance(value, dict):
            raise LabError(f"firmware.{firmware_id} must be a table")
        _require_keys(value, {"candidates"}, f"firmware.{firmware_id}")
        candidates = _string_list(
            value.get("candidates"), f"firmware.{firmware_id}.candidates", minimum=1, maximum=16
        )
        if any(not Path(candidate).is_absolute() for candidate in candidates):
            raise LabError(f"firmware.{firmware_id} candidates must be absolute paths")
        firmware[firmware_id] = Firmware(id=firmware_id, candidates=candidates)
    guests_raw = raw.get("guests")
    if not isinstance(guests_raw, list) or not guests_raw:
        raise LabError("manifest.guests must be a non-empty array")
    guests = tuple(
        _parse_guest(value, set(firmware), f"guests[{index}]")
        for index, value in enumerate(guests_raw)
    )
    guest_ids = [guest.id for guest in guests]
    if len(set(guest_ids)) != len(guest_ids):
        raise LabError("manifest guest ids must be unique")
    families = {guest.family for guest in guests}
    if "linux" not in families or len(families) < 2:
        raise LabError("manifest must retain Linux and at least one non-Linux kernel family")
    return Manifest(
        path=path,
        identity=identity,
        schema=schema,
        claim_class=claim_class,
        claims=_string_list(raw.get("claims"), "manifest.claims", minimum=1, maximum=16),
        nonclaims=_string_list(raw.get("nonclaims"), "manifest.nonclaims", minimum=1, maximum=24),
        firmware=firmware,
        guests=guests,
    )


def load_manifest(path: Path) -> Manifest:
    pinned = _open_pinned_input(path)
    try:
        with os.fdopen(os.dup(pinned.descriptor), "rb") as stream:
            raw = tomllib.load(stream)
        post_parse_identity = _hash_descriptor(pinned.descriptor)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise LabError(f"cannot load manifest {path}: {error}") from error
    finally:
        os.close(pinned.descriptor)
    if post_parse_identity != pinned.identity:
        raise LabError(f"manifest changed while it was parsed: {path}")
    return parse_manifest_data(raw, path.resolve(), pinned.identity)


def default_guest_root() -> Path:
    configured = os.environ.get("OSTADIX_GUESTS_DIR")
    if configured:
        return Path(configured).expanduser()
    data_root = os.environ.get("XDG_DATA_HOME")
    if data_root:
        return Path(data_root).expanduser() / "ostadix" / "guests"
    return Path.home() / ".local" / "share" / "ostadix" / "guests"


def _directory_open_flags() -> int:
    return (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )


def _open_directory_path(path: Path, *, create: bool) -> OpenDirectory:
    absolute = Path(os.path.abspath(path.expanduser()))
    if create:
        absolute.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        descriptor = os.open(absolute, _directory_open_flags())
    except OSError as error:
        raise LabError(
            f"directory must exist without a final symlink: {absolute}: {error}"
        ) from error
    metadata = os.fstat(descriptor)
    if not stat.S_ISDIR(metadata.st_mode):
        os.close(descriptor)
        raise LabError(f"path is not a directory: {absolute}")
    return OpenDirectory(path=absolute, descriptor=descriptor)


def _open_guest_cache(guest_root: Path, cache_dir: str, *, create: bool) -> OpenDirectory:
    with _open_directory_path(guest_root, create=create) as root:
        if create:
            try:
                os.mkdir(cache_dir, mode=0o700, dir_fd=root.descriptor)
            except FileExistsError:
                pass
        try:
            descriptor = os.open(
                cache_dir,
                _directory_open_flags(),
                dir_fd=root.descriptor,
            )
        except OSError as error:
            raise LabError(
                f"guest cache must be a non-symlink directory: "
                f"{root.path / cache_dir}: {error}"
            ) from error
        metadata = os.fstat(descriptor)
        if not stat.S_ISDIR(metadata.st_mode):
            os.close(descriptor)
            raise LabError(f"guest cache is not a directory: {root.path / cache_dir}")
        return OpenDirectory(path=root.path / cache_dir, descriptor=descriptor)


def _artifact_filenames(guest: Guest) -> dict[str, str]:
    result: dict[str, str] = {}
    for artifact in guest.artifacts:
        result[artifact.id] = artifact.filename
        if artifact.expanded_id is not None and artifact.expanded_filename is not None:
            result[artifact.expanded_id] = artifact.expanded_filename
        for member in artifact.members:
            result[member.id] = member.filename
    return result


def artifact_paths(guest: Guest, guest_root: Path) -> dict[str, Path]:
    with _open_guest_cache(guest_root, guest.cache_dir, create=False) as cache:
        return {
            identifier: cache.path / filename
            for identifier, filename in _artifact_filenames(guest).items()
        }


def expected_identities(guest: Guest) -> dict[str, FileIdentity]:
    result: dict[str, FileIdentity] = {}
    for artifact in guest.artifacts:
        result[artifact.id] = FileIdentity(artifact.size_bytes, artifact.sha256)
        if (
            artifact.expanded_id is not None
            and artifact.expanded_size_bytes is not None
            and artifact.expanded_sha256 is not None
        ):
            result[artifact.expanded_id] = FileIdentity(
                artifact.expanded_size_bytes, artifact.expanded_sha256
            )
        for member in artifact.members:
            result[member.id] = FileIdentity(member.size_bytes, member.sha256)
    return result


def verify_file(path: Path, expected: FileIdentity) -> FileIdentity:
    observed = hash_file(path)
    if observed != expected:
        raise LabError(
            f"artifact identity mismatch for {path}: expected size={expected.size_bytes} "
            f"sha256={expected.sha256}, observed size={observed.size_bytes} "
            f"sha256={observed.sha256}"
        )
    return observed


def verify_guest_artifacts(guest: Guest, guest_root: Path) -> dict[str, FileIdentity]:
    filenames = _artifact_filenames(guest)
    observed: dict[str, FileIdentity] = {}
    with _open_guest_cache(guest_root, guest.cache_dir, create=False) as cache:
        for identifier, expected in expected_identities(guest).items():
            pinned = _open_pinned_input_at(cache, filenames[identifier], expected)
            try:
                observed[identifier] = pinned.identity
            finally:
                os.close(pinned.descriptor)
    return observed


def _atomic_publish(
    temp_path: Path, destination: Path, expected: FileIdentity
) -> FileIdentity:
    try:
        os.link(temp_path, destination)
    except FileExistsError:
        return verify_file(destination, expected)
    finally:
        temp_path.unlink(missing_ok=True)
    return verify_file(destination, expected)


def _verify_directory_file(
    directory: OpenDirectory, filename: str, expected: FileIdentity
) -> FileIdentity:
    pinned = _open_pinned_input_at(directory, filename, expected)
    try:
        return pinned.identity
    finally:
        os.close(pinned.descriptor)


def _directory_entry_exists(directory: OpenDirectory, filename: str) -> bool:
    try:
        os.stat(filename, dir_fd=directory.descriptor, follow_symlinks=False)
    except FileNotFoundError:
        return False
    return True


def _atomic_publish_at(
    directory: OpenDirectory,
    temporary_name: str,
    destination_name: str,
    expected: FileIdentity,
) -> FileIdentity:
    try:
        os.link(
            temporary_name,
            destination_name,
            src_dir_fd=directory.descriptor,
            dst_dir_fd=directory.descriptor,
            follow_symlinks=False,
        )
    except FileExistsError:
        return _verify_directory_file(directory, destination_name, expected)
    finally:
        try:
            os.unlink(temporary_name, dir_fd=directory.descriptor)
        except FileNotFoundError:
            pass
    return _verify_directory_file(directory, destination_name, expected)


def _stream_to_directory(
    source: BinaryIO,
    directory: OpenDirectory,
    destination_name: str,
    expected: FileIdentity,
) -> FileIdentity:
    descriptor = -1
    temporary_name = ""
    for _attempt in range(32):
        temporary_name = f".{destination_name}.{os.getpid()}.{secrets.token_hex(8)}.part"
        try:
            descriptor = os.open(
                temporary_name,
                os.O_WRONLY
                | os.O_CREAT
                | os.O_EXCL
                | getattr(os, "O_CLOEXEC", 0)
                | getattr(os, "O_NOFOLLOW", 0),
                0o600,
                dir_fd=directory.descriptor,
            )
            break
        except FileExistsError:
            continue
    if descriptor < 0:
        raise LabError(f"cannot reserve a temporary artifact for {destination_name}")
    digest = hashlib.sha256()
    size = 0
    try:
        with os.fdopen(descriptor, "wb") as output:
            while chunk := source.read(DOWNLOAD_CHUNK_BYTES):
                size += len(chunk)
                if size > expected.size_bytes:
                    raise LabError(
                        f"artifact exceeded admitted size {expected.size_bytes}: "
                        f"{destination_name}"
                    )
                digest.update(chunk)
                output.write(chunk)
            output.flush()
            os.fsync(output.fileno())
        observed = FileIdentity(size, digest.hexdigest())
        if observed != expected:
            raise LabError(
                f"downloaded identity mismatch for {destination_name}: expected {expected}, "
                f"observed {observed}"
            )
        return _atomic_publish_at(
            directory, temporary_name, destination_name, expected
        )
    finally:
        try:
            os.unlink(temporary_name, dir_fd=directory.descriptor)
        except FileNotFoundError:
            pass


def _capture_process_output_bounded(
    process: subprocess.Popen[bytes],
    *,
    stdout_limit: int,
    stderr_limit: int,
    deadline: float,
    context: str,
) -> tuple[bytes, bytes, int]:
    """Capture both process pipes concurrently under hard byte/time bounds."""

    if process.stdout is None or process.stderr is None:
        raise LabError(f"{context} did not expose both diagnostic pipes")
    stdout = bytearray()
    stderr = bytearray()
    selector: selectors.BaseSelector | None = None
    try:
        selector = selectors.DefaultSelector()
        for stream, label in ((process.stdout, "stdout"), (process.stderr, "stderr")):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ, label)
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise LabError(f"{context} timed out")
            for key, _ in selector.select(timeout=min(0.05, remaining)):
                try:
                    chunk = os.read(key.fileobj.fileno(), 4096)
                except BlockingIOError:
                    continue
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                destination = stdout if key.data == "stdout" else stderr
                limit = stdout_limit if key.data == "stdout" else stderr_limit
                if _append_bounded(destination, chunk, limit):
                    raise LabError(f"{context} {key.data} exceeded {limit} bytes")
        remaining = deadline - time.monotonic()
        if remaining <= 0 and process.poll() is None:
            raise LabError(f"{context} timed out")
        try:
            return_code = process.wait(timeout=max(0.0, remaining))
        except subprocess.TimeoutExpired as error:
            raise LabError(f"{context} timed out") from error
        selector.close()
        selector = None
        return bytes(stdout), bytes(stderr), return_code
    except BaseException:
        try:
            _cleanup_process(process)
        except BaseException:
            pass
        raise
    finally:
        if selector is not None:
            try:
                selector.close()
            except BaseException:
                pass
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except BaseException:
                    pass


def _stream_process_stdout_to_directory(
    process: subprocess.Popen[bytes],
    directory: OpenDirectory,
    destination_name: str,
    expected: FileIdentity,
    *,
    stderr_limit: int,
    deadline: float,
    context: str,
) -> FileIdentity:
    """Stream stdout to a private artifact while draining bounded stderr."""

    if process.stdout is None or process.stderr is None:
        raise LabError(f"{context} did not expose both output pipes")
    descriptor = -1
    temporary_name = ""
    for _attempt in range(32):
        temporary_name = (
            f".{destination_name}.{os.getpid()}.{secrets.token_hex(8)}.part"
        )
        try:
            descriptor = os.open(
                temporary_name,
                os.O_WRONLY
                | os.O_CREAT
                | os.O_EXCL
                | getattr(os, "O_CLOEXEC", 0)
                | getattr(os, "O_NOFOLLOW", 0),
                0o600,
                dir_fd=directory.descriptor,
            )
            break
        except FileExistsError:
            continue
    if descriptor < 0:
        raise LabError(f"cannot reserve a temporary artifact for {destination_name}")

    digest = hashlib.sha256()
    size = 0
    diagnostic = bytearray()
    selector: selectors.BaseSelector | None = None
    try:
        selector = selectors.DefaultSelector()
        for stream, label in ((process.stdout, "stdout"), (process.stderr, "stderr")):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ, label)
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise LabError(f"{context} timed out")
            for key, _ in selector.select(timeout=min(0.05, remaining)):
                try:
                    chunk = os.read(key.fileobj.fileno(), 65_536)
                except BlockingIOError:
                    continue
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                if key.data == "stderr":
                    if _append_bounded(diagnostic, chunk, stderr_limit):
                        raise LabError(
                            f"{context} stderr exceeded {stderr_limit} bytes"
                        )
                    continue
                size += len(chunk)
                if size > expected.size_bytes:
                    raise LabError(
                        f"artifact exceeded admitted size {expected.size_bytes}: "
                        f"{destination_name}"
                    )
                digest.update(chunk)
                view = memoryview(chunk)
                while view:
                    written = os.write(descriptor, view)
                    if written <= 0:
                        raise LabError(
                            f"short write while expanding {destination_name}"
                        )
                    view = view[written:]
        remaining = deadline - time.monotonic()
        if remaining <= 0 and process.poll() is None:
            raise LabError(f"{context} timed out")
        try:
            return_code = process.wait(timeout=max(0.0, remaining))
        except subprocess.TimeoutExpired as error:
            raise LabError(f"{context} timed out") from error
        if return_code != 0:
            rendered = diagnostic.decode("utf-8", "replace").strip()
            raise LabError(
                f"{context} failed with status {return_code}: {rendered}"
            )
        observed = FileIdentity(size, digest.hexdigest())
        if observed != expected:
            raise LabError(
                f"expanded identity mismatch for {destination_name}: "
                f"expected {expected}, observed {observed}"
            )
        os.fsync(descriptor)
        os.close(descriptor)
        descriptor = -1
        selector.close()
        selector = None
        return _atomic_publish_at(
            directory, temporary_name, destination_name, expected
        )
    except BaseException:
        try:
            _cleanup_process(process)
        except BaseException:
            pass
        raise
    finally:
        if selector is not None:
            try:
                selector.close()
            except BaseException:
                pass
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except BaseException:
                    pass
        if descriptor >= 0:
            try:
                os.close(descriptor)
            except OSError:
                pass
        try:
            os.unlink(temporary_name, dir_fd=directory.descriptor)
        except FileNotFoundError:
            pass


def _expand_zstd_at(
    source: PinnedInput,
    cache: OpenDirectory,
    destination_name: str,
    expected: FileIdentity,
) -> FileIdentity:
    """Expand one pinned zstd frame while bounding and hashing its output."""

    try:
        from compression import zstd  # type: ignore[attr-defined]
    except ImportError:
        zstd_path = shutil.which("zstd")
        if zstd_path is None:
            raise LabError(
                "zstd expansion requires Python 3.14 compression.zstd or the zstd CLI"
            )
        with os.fdopen(os.dup(source.descriptor), "rb") as compressed:
            deadline = time.monotonic() + EXTRACTION_TIMEOUT_SECONDS
            process = subprocess.Popen(
                [zstd_path, "--decompress", "--stdout", "--quiet"],
                stdin=compressed,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
            return _stream_process_stdout_to_directory(
                process,
                cache,
                destination_name,
                expected,
                stderr_limit=ZSTD_DIAGNOSTIC_BYTES,
                deadline=deadline,
                context="zstd expansion",
            )

    try:
        with os.fdopen(os.dup(source.descriptor), "rb") as compressed:
            with zstd.ZstdFile(compressed, "rb") as decompressed:
                return _stream_to_directory(
                    decompressed, cache, destination_name, expected
                )
    except (OSError, zstd.ZstdError) as error:
        raise LabError(f"cannot expand zstd artifact: {error}") from error


def _extract_iso_member_at(
    source: PinnedInput,
    member: ArtifactMember,
    cache: OpenDirectory,
) -> FileIdentity:
    expected = FileIdentity(member.size_bytes, member.sha256)
    if _directory_entry_exists(cache, member.filename):
        return _verify_directory_file(cache, member.filename, expected)
    xorriso = shutil.which("xorriso")
    if xorriso is None:
        raise LabError("ISO member extraction requires the xorriso executable")

    directory_name = ""
    directory: OpenDirectory | None = None
    for _attempt in range(32):
        directory_name = f".iso-extract.{os.getpid()}.{secrets.token_hex(8)}"
        try:
            os.mkdir(directory_name, mode=0o700, dir_fd=cache.descriptor)
            descriptor = os.open(
                directory_name,
                _directory_open_flags(),
                dir_fd=cache.descriptor,
            )
            directory = OpenDirectory(
                path=cache.path / directory_name,
                descriptor=descriptor,
            )
            break
        except FileExistsError:
            continue
    if directory is None:
        raise LabError(f"cannot reserve a private extraction directory for {member.filename}")

    destination = (
        f"/proc/self/fd/{directory.descriptor}/{member.filename}"
        if Path("/proc/self/fd").is_dir()
        else str(directory.path / member.filename)
    )
    process: subprocess.Popen[bytes] | None = None
    try:
        deadline = time.monotonic() + EXTRACTION_TIMEOUT_SECONDS
        process = subprocess.Popen(
            [
                xorriso,
                "-abort_on",
                "FAILURE",
                "-osirrox",
                "on",
                "-indev",
                source.child_path,
                "-extract",
                member.path,
                destination,
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
            pass_fds=(source.descriptor, directory.descriptor),
        )
        stdout, stderr, return_code = _capture_process_output_bounded(
            process,
            stdout_limit=TOOL_DIAGNOSTIC_BYTES,
            stderr_limit=TOOL_DIAGNOSTIC_BYTES,
            deadline=deadline,
            context=f"xorriso extraction of {member.path}",
        )
        if return_code != 0:
            diagnostic = stderr.decode("utf-8", "replace").strip()
            raise LabError(
                f"xorriso failed extracting {member.path} with status "
                f"{return_code}: {diagnostic}"
            )
        pinned_member = _open_pinned_input_at(directory, member.filename, expected)
        try:
            try:
                os.link(
                    member.filename,
                    member.filename,
                    src_dir_fd=directory.descriptor,
                    dst_dir_fd=cache.descriptor,
                    follow_symlinks=False,
                )
            except FileExistsError:
                pass
        finally:
            os.close(pinned_member.descriptor)
        return _verify_directory_file(cache, member.filename, expected)
    finally:
        if process is not None and process.poll() is None:
            _cleanup_process(process)
        try:
            os.unlink(member.filename, dir_fd=directory.descriptor)
        except FileNotFoundError:
            pass
        os.close(directory.descriptor)
        try:
            os.rmdir(directory_name, dir_fd=cache.descriptor)
        except FileNotFoundError:
            pass


def _fetch_artifact_at(
    artifact: Artifact, cache: OpenDirectory
) -> list[tuple[str, Path, FileIdentity]]:
    source_path = cache.path / artifact.filename
    source_expected = FileIdentity(artifact.size_bytes, artifact.sha256)
    fetched: list[tuple[str, Path, FileIdentity]] = []
    if _directory_entry_exists(cache, artifact.filename):
        fetched.append(
            (
                artifact.id,
                source_path,
                _verify_directory_file(cache, artifact.filename, source_expected),
            )
        )
    else:
        request = urllib.request.Request(
            artifact.url,
            headers={"User-Agent": "Ostadix-foreign-kernel-lab/1"},
            method="GET",
        )
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                final_url = urllib.parse.urlsplit(response.geturl())
                if final_url.scheme != "https":
                    raise LabError(f"artifact redirect left HTTPS: {response.geturl()}")
                content_length = response.headers.get("Content-Length")
                if content_length is not None and int(content_length) != artifact.size_bytes:
                    raise LabError(
                        f"server length mismatch for {artifact.filename}: "
                        f"expected {artifact.size_bytes}, got {content_length}"
                    )
                observed = _stream_to_directory(
                    response, cache, artifact.filename, source_expected
                )
        except (OSError, ValueError) as error:
            raise LabError(f"failed to fetch {artifact.url}: {error}") from error
        fetched.append((artifact.id, source_path, observed))
    if artifact.unpack is not None:
        assert artifact.expanded_id is not None
        assert artifact.expanded_filename is not None
        assert artifact.expanded_size_bytes is not None
        assert artifact.expanded_sha256 is not None
        expanded_path = cache.path / artifact.expanded_filename
        expanded_expected = FileIdentity(
            artifact.expanded_size_bytes, artifact.expanded_sha256
        )
        if _directory_entry_exists(cache, artifact.expanded_filename):
            expanded_observed = _verify_directory_file(
                cache, artifact.expanded_filename, expanded_expected
            )
        else:
            pinned_source = _open_pinned_input_at(
                cache, artifact.filename, source_expected
            )
            try:
                if artifact.unpack == "zstd":
                    expanded_observed = _expand_zstd_at(
                        pinned_source,
                        cache,
                        artifact.expanded_filename,
                        expanded_expected,
                    )
                else:
                    with os.fdopen(os.dup(pinned_source.descriptor), "rb") as compressed:
                        decompressor: BinaryIO
                        if artifact.unpack == "gzip":
                            decompressor = gzip.GzipFile(fileobj=compressed, mode="rb")
                        else:
                            decompressor = lzma.LZMAFile(compressed, "rb")
                        with decompressor:
                            expanded_observed = _stream_to_directory(
                                decompressor,
                                cache,
                                artifact.expanded_filename,
                                expanded_expected,
                            )
            except (gzip.BadGzipFile, lzma.LZMAError, OSError) as error:
                raise LabError(f"cannot expand {source_path}: {error}") from error
            finally:
                os.close(pinned_source.descriptor)
        fetched.append((artifact.expanded_id, expanded_path, expanded_observed))
    if artifact.members:
        pinned_source = _open_pinned_input_at(cache, artifact.filename, source_expected)
        try:
            for member in artifact.members:
                observed_member = _extract_iso_member_at(
                    pinned_source, member, cache
                )
                fetched.append(
                    (member.id, cache.path / member.filename, observed_member)
                )
        finally:
            os.close(pinned_source.descriptor)
    return fetched


def fetch_artifact(artifact: Artifact, cache: Path) -> list[tuple[str, Path, FileIdentity]]:
    with _open_directory_path(cache, create=True) as opened_cache:
        return _fetch_artifact_at(artifact, opened_cache)


def fetch_guest(guest: Guest, guest_root: Path) -> list[tuple[str, Path, FileIdentity]]:
    fetched: list[tuple[str, Path, FileIdentity]] = []
    with _open_guest_cache(guest_root, guest.cache_dir, create=True) as cache:
        for artifact in guest.artifacts:
            fetched.extend(_fetch_artifact_at(artifact, cache))
    return fetched


def normalize_terminal(raw: bytes | bytearray | memoryview | str) -> str:
    text = (
        bytes(raw).decode("utf-8", "replace")
        if isinstance(raw, (bytes, bytearray, memoryview))
        else raw
    )
    text = ANSI_PATTERN.sub("", text)
    reduced: list[str] = []
    for character in text:
        if character == "\b":
            if reduced and reduced[-1] != "\n":
                reduced.pop()
            continue
        if character == "\r":
            reduced.append("\n")
        elif character == "\n" or character == "\t" or character >= " ":
            reduced.append(character)
    return "".join(reduced)


def validate_transcript(guest: Guest, transcript: str) -> dict[str, Any]:
    counts = {marker: transcript.count(marker) for marker in guest.required_markers}
    positions = {marker: transcript.find(marker) for marker in guest.required_markers}
    forbidden = {
        marker: transcript.count(marker)
        for marker in guest.forbidden_markers
        if marker in transcript
    }
    issues: list[str] = []
    missing = [marker for marker, count in counts.items() if count == 0]
    duplicated = [marker for marker in guest.unique_markers if counts[marker] > 1]
    if missing:
        issues.append(f"missing required markers: {missing}")
    if duplicated:
        issues.append(f"duplicated required markers: {duplicated}")
    if not missing:
        ordered_positions = [positions[marker] for marker in guest.required_markers]
        if ordered_positions != sorted(ordered_positions):
            issues.append("required markers are out of order")
    if forbidden:
        issues.append(f"forbidden markers observed: {sorted(forbidden)}")
    return {
        "counts": counts,
        "positions": positions,
        "forbidden_counts": forbidden,
        "issues": issues,
        "complete": not issues,
    }


def _resolve_firmware(
    manifest: Manifest, overrides: dict[str, Path], required_ids: set[str]
) -> tuple[dict[str, Path], dict[str, FileIdentity]]:
    paths: dict[str, Path] = {}
    identities: dict[str, FileIdentity] = {}
    for identifier in sorted(required_ids):
        firmware = manifest.firmware[identifier]
        candidates: Iterable[Path]
        if identifier in overrides:
            candidates = (overrides[identifier],)
        else:
            environment_candidate = (
                os.environ.get("OSTADIX_AARCH64_UEFI")
                if identifier == "aarch64_uefi"
                else None
            )
            candidate_texts = (
                *((environment_candidate,) if environment_candidate else ()),
                *firmware.candidates,
            )
            candidates = (Path(candidate) for candidate in candidate_texts)
        selected: Path | None = None
        for candidate in candidates:
            try:
                resolved = candidate.expanduser().resolve(strict=True)
            except (OSError, RuntimeError):
                continue
            if resolved.is_file():
                selected = resolved
                break
        if selected is None:
            raise LabError(
                f"no regular firmware file found for {identifier}; candidates={firmware.candidates}"
            )
        paths[identifier] = selected
        identities[identifier] = hash_file(selected)
    return paths, identities


def build_qemu_command(
    guest: Guest,
    qemu_path: str | Path,
    artifact_map: dict[str, str | Path],
    firmware_map: dict[str, str | Path],
) -> list[str]:
    replacements = {
        ("artifact", identifier): str(path)
        for identifier, path in artifact_map.items()
    }
    replacements.update(
        {("firmware", identifier): str(path) for identifier, path in firmware_map.items()}
    )
    command = [str(qemu_path)]
    for argument in guest.qemu_args:
        def replace(match: re.Match[str]) -> str:
            key = (match.group(1), match.group(2))
            try:
                return replacements[key]
            except KeyError as error:
                raise LabError(f"unresolved QEMU placeholder {match.group(0)}") from error

        command.append(PLACEHOLDER_PATTERN.sub(replace, argument))
    return command


def _process_group_exists(process_group: int) -> bool:
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _wait_for_process_group(
    process: subprocess.Popen[bytes], process_group: int, timeout_seconds: float
) -> bool:
    deadline = time.monotonic() + timeout_seconds
    while True:
        process.poll()
        if not _process_group_exists(process_group):
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.01)


def _cleanup_process(
    process: subprocess.Popen[bytes], timeout_seconds: float = 2.0
) -> tuple[str, int | None]:
    if os.name != "posix":
        if process.poll() is not None:
            return "already-exited", process.returncode
        try:
            process.terminate()
            process.wait(timeout=timeout_seconds)
            return "terminate", process.returncode
        except subprocess.TimeoutExpired:
            process.kill()
            try:
                process.wait(timeout=timeout_seconds)
            except subprocess.TimeoutExpired:
                return "kill-timeout", process.poll()
            return "kill", process.returncode

    process_group = process.pid
    if not _process_group_exists(process_group):
        if process.poll() is None:
            try:
                process.wait(timeout=0.05)
            except subprocess.TimeoutExpired:
                return "group-missing-leader-live", process.poll()
        return "already-exited", process.returncode
    try:
        os.killpg(process_group, signal.SIGTERM)
    except ProcessLookupError:
        return "exited-before-terminate", process.poll()
    except PermissionError:
        return "terminate-permission-denied", process.poll()
    if _wait_for_process_group(process, process_group, timeout_seconds):
        if process.poll() is None:
            try:
                process.wait(timeout=0.05)
            except subprocess.TimeoutExpired:
                pass
        return "terminate", process.poll()
    try:
        os.killpg(process_group, signal.SIGKILL)
    except ProcessLookupError:
        return "terminate", process.poll()
    except PermissionError:
        return "kill-permission-denied", process.poll()
    if not _wait_for_process_group(process, process_group, timeout_seconds):
        return "kill-timeout", process.poll()
    if process.poll() is None:
        try:
            process.wait(timeout=0.05)
        except subprocess.TimeoutExpired:
            pass
    return "kill", process.poll()


def _copy_pinned_executable(source: PinnedInput, destination: Path) -> PinnedInput:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    try:
        descriptor = os.open(destination, flags, 0o500)
    except OSError as error:
        raise LabError(f"cannot create private QEMU snapshot {destination}: {error}") from error
    try:
        offset = 0
        while chunk := os.pread(source.descriptor, DOWNLOAD_CHUNK_BYTES, offset):
            view = memoryview(chunk)
            while view:
                written = os.write(descriptor, view)
                if written <= 0:
                    raise LabError("short write while creating private QEMU snapshot")
                view = view[written:]
            offset += len(chunk)
        os.fchmod(descriptor, 0o500)
        os.fsync(descriptor)
    except BaseException:
        os.close(descriptor)
        destination.unlink(missing_ok=True)
        raise
    os.close(descriptor)
    try:
        return _open_pinned_input(destination, source.identity)
    except BaseException:
        destination.unlink(missing_ok=True)
        raise


def _pinned_path_matches(pinned: PinnedInput) -> bool:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(pinned.source_path, flags)
    except OSError:
        return False
    try:
        expected = os.fstat(pinned.descriptor)
        observed = os.fstat(descriptor)
        return (expected.st_dev, expected.st_ino) == (observed.st_dev, observed.st_ino)
    finally:
        os.close(descriptor)


def _bounded_version(
    executable: PinnedInput,
    launch_path: str,
    *,
    cwd: Path,
    environment: dict[str, str],
) -> str:
    deadline = time.monotonic() + QEMU_VERSION_TIMEOUT_SECONDS
    try:
        process = subprocess.Popen(
            [launch_path, "--version"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
            cwd=cwd,
            env=environment,
            pass_fds=(executable.descriptor,),
        )
    except OSError as error:
        raise LabError(f"cannot inspect QEMU version: {error}") from error
    stdout, stderr, return_code = _capture_process_output_bounded(
        process,
        stdout_limit=QEMU_VERSION_CAPTURE_BYTES,
        stderr_limit=QEMU_VERSION_CAPTURE_BYTES,
        deadline=deadline,
        context="QEMU version inspection",
    )
    if return_code != 0:
        diagnostic = (stdout + stderr).decode("utf-8", "replace").strip()
        raise LabError(
            f"cannot inspect QEMU version: executable exited {return_code}: {diagnostic}"
        )
    return (stdout + stderr).decode("utf-8", "replace").strip()


def _git_bytes(*arguments: str) -> bytes:
    try:
        return subprocess.run(
            ["git", "-C", str(PROJECT_ROOT), *arguments],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=10,
            check=True,
        ).stdout
    except (OSError, subprocess.SubprocessError) as error:
        raise LabError(f"cannot bind observation to repository state: {error}") from error


def _digest_frame(digest: Any, label: bytes, payload: bytes) -> None:
    digest.update(len(label).to_bytes(8, "big"))
    digest.update(label)
    digest.update(len(payload).to_bytes(8, "big"))
    digest.update(payload)


def _capture_git_state() -> dict[str, Any]:
    commit = _git_bytes("rev-parse", "--verify", "HEAD^{commit}").strip()
    status = _git_bytes(
        "status", "--porcelain=v1", "-z", "--untracked-files=all"
    )
    tracked_diff = _git_bytes("diff", "--binary", "--no-ext-diff", "HEAD", "--")
    untracked_paths = tuple(sorted(
        path
        for path in _git_bytes(
            "ls-files", "--others", "--exclude-standard", "-z"
        ).split(b"\0")
        if path
    ))
    untracked_digest = hashlib.sha256()
    for raw_path in untracked_paths:
        relative = Path(os.fsdecode(raw_path))
        candidate = PROJECT_ROOT / relative
        try:
            metadata = candidate.lstat()
        except OSError as error:
            raise LabError(f"cannot bind untracked source {relative}: {error}") from error
        if stat.S_ISREG(metadata.st_mode):
            identity = hash_file(candidate)
            payload = (
                f"regular\0{stat.S_IMODE(metadata.st_mode):o}\0"
                f"{identity.size_bytes}\0{identity.sha256}"
            ).encode("ascii")
        elif stat.S_ISLNK(metadata.st_mode):
            payload = b"symlink\0" + os.fsencode(os.readlink(candidate))
        else:
            raise LabError(f"untracked source is not a regular file or symlink: {relative}")
        _digest_frame(untracked_digest, raw_path, payload)
    return {
        "commit": commit,
        "status": status,
        "tracked_diff": tracked_diff,
        "untracked_paths": untracked_paths,
        "untracked_sha256": untracked_digest.hexdigest(),
    }


def _git_context() -> dict[str, Any]:
    top_level = Path(os.fsdecode(_git_bytes("rev-parse", "--show-toplevel").strip()))
    if top_level.resolve(strict=True) != PROJECT_ROOT.resolve(strict=True):
        raise LabError(
            f"repository root mismatch: expected {PROJECT_ROOT}, observed {top_level}"
        )
    first = _capture_git_state()
    second = _capture_git_state()
    if first != second:
        raise LabError("repository state changed while provenance was captured")
    commit = os.fsdecode(second["commit"])
    if re.fullmatch(r"[0-9a-f]{40,64}", commit) is None:
        raise LabError(f"git returned an invalid source commit: {commit!r}")
    status = second["status"]
    tracked_diff = second["tracked_diff"]
    untracked_paths = second["untracked_paths"]

    status_sha256 = hashlib.sha256(status).hexdigest()
    tracked_diff_sha256 = hashlib.sha256(tracked_diff).hexdigest()
    untracked_sha256 = second["untracked_sha256"]
    state_digest = hashlib.sha256()
    _digest_frame(state_digest, b"source_commit", commit.encode("ascii"))
    _digest_frame(state_digest, b"status", status)
    _digest_frame(state_digest, b"tracked_diff", tracked_diff)
    _digest_frame(state_digest, b"untracked", bytes.fromhex(untracked_sha256))
    return {
        "provenance_kind": "git-worktree",
        "source_commit": commit,
        "worktree_dirty": bool(status),
        "status_porcelain_sha256": status_sha256,
        "tracked_diff_sha256": tracked_diff_sha256,
        "untracked_file_count": len(untracked_paths),
        "untracked_files_sha256": untracked_sha256,
        "working_tree_state_sha256": state_digest.hexdigest(),
    }


def _source_release_context() -> dict[str, Any]:
    manifest_path = PROJECT_ROOT / SOURCE_RELEASE_MANIFEST
    checksums_path = PROJECT_ROOT / SOURCE_RELEASE_CHECKSUMS
    pinned_manifest = _open_pinned_input(manifest_path)
    try:
        if pinned_manifest.identity.size_bytes > MAX_SOURCE_MANIFEST_BYTES:
            raise LabError("source-release manifest exceeds the bounded size limit")
        manifest_bytes = _read_descriptor_exact(
            pinned_manifest.descriptor, pinned_manifest.identity.size_bytes
        )
        try:
            manifest = json.loads(manifest_bytes.decode("ascii", "strict"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise LabError("source-release manifest is not canonical JSON") from error
        canonical = (
            json.dumps(
                manifest,
                ensure_ascii=True,
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n"
        ).encode("ascii")
        if canonical != manifest_bytes:
            raise LabError("source-release manifest is not canonical JSON")
        if not isinstance(manifest, dict) or set(manifest) != {
            "commit",
            "file_count",
            "files",
            "prefix",
            "schema",
        }:
            raise LabError("source-release manifest has an invalid root shape")
        if manifest["schema"] != SOURCE_RELEASE_SCHEMA:
            raise LabError("source-release manifest schema is unsupported")
        if (
            not isinstance(manifest["prefix"], str)
            or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,254}", manifest["prefix"])
            is None
        ):
            raise LabError("source-release manifest prefix is invalid")
        commit = manifest["commit"]
        if not isinstance(commit, str) or re.fullmatch(r"[0-9a-f]{40,64}", commit) is None:
            raise LabError("source-release manifest commit is invalid")
        raw_files = manifest["files"]
        if (
            not isinstance(raw_files, list)
            or not 0 < len(raw_files) <= MAX_SOURCE_FILES
            or isinstance(manifest["file_count"], bool)
            or not isinstance(manifest["file_count"], int)
            or manifest["file_count"] != len(raw_files)
        ):
            raise LabError("source-release manifest file count is invalid")

        checksum_lines: list[str] = []
        previous_path: bytes | None = None
        verified_files = hashlib.sha256()
        for index, item in enumerate(raw_files):
            if not isinstance(item, dict) or set(item) != {
                "mode",
                "path",
                "sha256",
                "size",
            }:
                raise LabError(f"source-release file record {index} is malformed")
            relative = item["path"]
            mode = item["mode"]
            digest = item["sha256"]
            size = item["size"]
            if (
                not isinstance(relative, str)
                or relative in {".", ".."}
                or not relative
                or len(relative) > 1024
                or any(ord(character) < 32 for character in relative)
            ):
                raise LabError(f"source-release file record {index} has an invalid path")
            posix = PurePosixPath(relative)
            if (
                posix.is_absolute()
                or ".." in posix.parts
                or "." in posix.parts
                or "\\" in relative
                or "\x00" in relative
            ):
                raise LabError(f"source-release file record {index} escapes its root")
            encoded_path = relative.encode("utf-8")
            if previous_path is not None and encoded_path <= previous_path:
                raise LabError("source-release file paths are not uniquely sorted")
            previous_path = encoded_path
            if mode not in {"100644", "100755"}:
                raise LabError(f"source-release file {relative} has an invalid mode")
            if not isinstance(digest, str) or SHA256_PATTERN.fullmatch(digest) is None:
                raise LabError(f"source-release file {relative} has an invalid digest")
            if (
                isinstance(size, bool)
                or not isinstance(size, int)
                or not 0 <= size <= MAX_ARTIFACT_BYTES
            ):
                raise LabError(f"source-release file {relative} has an invalid size")
            pinned = _open_pinned_input(
                PROJECT_ROOT / Path(*posix.parts), FileIdentity(size, digest)
            )
            try:
                executable = bool(os.fstat(pinned.descriptor).st_mode & 0o111)
                if executable != (mode == "100755"):
                    raise LabError(
                        f"source-release file {relative} mode differs from its manifest"
                    )
            finally:
                os.close(pinned.descriptor)
            checksum_lines.append(f"{digest}  {relative}")
            _digest_frame(
                verified_files,
                encoded_path,
                f"{mode}\0{size}\0{digest}".encode("ascii"),
            )

        checksum_lines.append(
            f"{pinned_manifest.identity.sha256}  {SOURCE_RELEASE_MANIFEST}"
        )
        expected_checksums = ("\n".join(checksum_lines) + "\n").encode("utf-8")
        pinned_checksums = _open_pinned_input(checksums_path)
        try:
            if pinned_checksums.identity.size_bytes > MAX_SOURCE_MANIFEST_BYTES:
                raise LabError("source-release checksums exceed the bounded size limit")
            observed_checksums = _read_descriptor_exact(
                pinned_checksums.descriptor, pinned_checksums.identity.size_bytes
            )
            if observed_checksums != expected_checksums:
                raise LabError("SHA256SUMS does not match the source-release payload")
            if _hash_descriptor(pinned_checksums.descriptor) != pinned_checksums.identity:
                raise LabError("source-release checksums changed during verification")
            checksums_identity = pinned_checksums.identity
        finally:
            os.close(pinned_checksums.descriptor)

        state_digest = hashlib.sha256()
        _digest_frame(state_digest, b"source_commit", commit.encode("ascii"))
        _digest_frame(
            state_digest,
            b"source_release_manifest",
            bytes.fromhex(pinned_manifest.identity.sha256),
        )
        _digest_frame(
            state_digest,
            b"source_release_checksums",
            bytes.fromhex(checksums_identity.sha256),
        )
        _digest_frame(
            state_digest,
            b"verified_files",
            verified_files.digest(),
        )
        if _hash_descriptor(pinned_manifest.descriptor) != pinned_manifest.identity:
            raise LabError("source-release manifest changed during verification")
        return {
            "provenance_kind": "source-release-manifest",
            "source_commit": commit,
            "worktree_dirty": False,
            "payload_scope": "source-release-declared-files",
            "untracked_files_audited": False,
            "working_tree_state_sha256": state_digest.hexdigest(),
            "source_release_manifest": {
                "path": str(manifest_path),
                "schema": SOURCE_RELEASE_SCHEMA,
                "prefix": manifest["prefix"],
                "file_count": len(raw_files),
                "size_bytes": pinned_manifest.identity.size_bytes,
                "sha256": pinned_manifest.identity.sha256,
            },
            "source_release_checksums": {
                "path": str(checksums_path),
                "size_bytes": checksums_identity.size_bytes,
                "sha256": checksums_identity.sha256,
            },
        }
    finally:
        os.close(pinned_manifest.descriptor)


def _repository_context() -> dict[str, Any]:
    try:
        (PROJECT_ROOT / ".git").lstat()
    except FileNotFoundError:
        if (PROJECT_ROOT / SOURCE_RELEASE_MANIFEST).is_file():
            return _source_release_context()
    except OSError as error:
        raise LabError(f"cannot inspect repository provenance: {error}") from error
    return _git_context()


def _append_bounded(destination: bytearray, chunk: bytes, limit: int) -> bool:
    remaining = max(0, limit - len(destination))
    destination.extend(chunk[:remaining])
    return len(chunk) > remaining


def _drain_nonblocking(
    source: BinaryIO, destination: bytearray, limit: int
) -> tuple[bool, bool]:
    overflow = False
    descriptor = source.fileno()
    os.set_blocking(descriptor, False)
    while True:
        try:
            chunk = os.read(descriptor, 4096)
        except BlockingIOError:
            return overflow, False
        if not chunk:
            return overflow, True
        if _append_bounded(destination, chunk, limit):
            return True, False


def run_guest(
    manifest: Manifest,
    guest: Guest,
    guest_root: Path,
    output_root: Path,
    *,
    qemu_override: Path | None = None,
    firmware_overrides: dict[str, Path] | None = None,
) -> dict[str, Any]:
    if os.name != "posix":
        raise LabError("foreign-kernel runs require POSIX inherited file descriptors")
    repository_context = _repository_context()
    harness_path = Path(__file__).resolve(strict=True)
    harness_identity = hash_file(harness_path)
    artifact_filenames = _artifact_filenames(guest)
    expected_artifacts = expected_identities(guest)
    required_firmware = {
        match.group(2)
        for argument in guest.qemu_args
        for match in PLACEHOLDER_PATTERN.finditer(argument)
        if match.group(1) == "firmware"
    }
    firmware_paths, firmware_identities = _resolve_firmware(
        manifest, firmware_overrides or {}, required_firmware
    )
    pinned_artifacts: dict[str, PinnedInput] = {}
    pinned_firmware: dict[str, PinnedInput] = {}
    all_inputs: list[PinnedInput] = []
    qemu_snapshot_path: Path | None = None
    input_dir: Path | None = None
    artifact_cache = _open_guest_cache(guest_root, guest.cache_dir, create=False)
    try:
        for identifier, expected in expected_artifacts.items():
            pinned = _open_pinned_input_at(
                artifact_cache, artifact_filenames[identifier], expected
            )
            pinned_artifacts[identifier] = pinned
            all_inputs.append(pinned)
        for identifier, expected in firmware_identities.items():
            pinned = _open_pinned_input(firmware_paths[identifier], expected)
            pinned_firmware[identifier] = pinned
            all_inputs.append(pinned)

        timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        run_dir = output_root / guest.id / f"{timestamp}-{os.getpid()}"
        run_dir.mkdir(parents=True, exist_ok=False, mode=0o700)
        run_dir.chmod(0o700)
        input_dir = run_dir / ".inputs"
        input_dir.mkdir(mode=0o700)
        input_dir.chmod(0o700)
        qemu_snapshot_path = input_dir / "qemu"
        qemu_home = run_dir / ".qemu-home"
        qemu_tmp = run_dir / ".qemu-tmp"
        qemu_home.mkdir(mode=0o700)
        qemu_tmp.mkdir(mode=0o700)
        qemu_environment = {
            "HOME": str(qemu_home),
            "TMPDIR": str(qemu_tmp),
            "TMP": str(qemu_tmp),
            "TEMP": str(qemu_tmp),
            "PATH": os.defpath,
            "LANG": "C",
            "LC_ALL": "C",
            "TZ": "UTC",
        }

        if qemu_override is None:
            discovered = shutil.which(guest.qemu_executable)
            if discovered is None:
                raise LabError(f"missing executable on PATH: {guest.qemu_executable}")
            qemu_path = Path(discovered).resolve(strict=True)
            executor_origin = "manifest-named-path-discovery"
            executor_claim_admitted = True
        else:
            qemu_path = qemu_override.expanduser().resolve(strict=True)
            # Overrides remain useful for harness tests and diagnostics, but an
            # arbitrary executable must not mint a QEMU/TCG evidence pass.
            executor_origin = "explicit-untrusted-override"
            executor_claim_admitted = False
        pinned_qemu_source = _open_pinned_input(qemu_path)
        try:
            if not os.fstat(pinned_qemu_source.descriptor).st_mode & 0o111:
                raise LabError(
                    f"QEMU path is not an executable regular file: {qemu_path}"
                )
        except BaseException:
            os.close(pinned_qemu_source.descriptor)
            raise
        all_inputs.append(pinned_qemu_source)
        qemu_identity = pinned_qemu_source.identity
        pinned_qemu = _copy_pinned_executable(
            pinned_qemu_source, qemu_snapshot_path
        )
        all_inputs.append(pinned_qemu)
        input_dir.chmod(0o500)
        linux_fd_exec = pinned_qemu.child_path.startswith("/proc/self/fd/")
        qemu_launch_path = (
            pinned_qemu.child_path if linux_fd_exec else str(qemu_snapshot_path)
        )
        qemu_transport = (
            "private-verified-copy-via-inherited-executable-fd"
            if linux_fd_exec
            else "private-verified-copy-with-inode-stability"
        )
        if not _pinned_path_matches(pinned_qemu):
            raise LabError("private QEMU snapshot path changed before version inspection")
        qemu_version = _bounded_version(
            pinned_qemu,
            qemu_launch_path,
            cwd=run_dir,
            environment=qemu_environment,
        )
        version_banner_admitted = QEMU_VERSION_PATTERN.match(qemu_version) is not None
        if executor_claim_admitted and not version_banner_admitted:
            raise LabError(
                "manifest-named QEMU emitted an unrecognized numeric version banner"
            )
        if not _pinned_path_matches(pinned_qemu):
            raise LabError("private QEMU snapshot path changed during version inspection")
        command = build_qemu_command(
            guest,
            qemu_launch_path,
            {
                identifier: pinned.child_path
                for identifier, pinned in pinned_artifacts.items()
            },
            {
                identifier: pinned.child_path
                for identifier, pinned in pinned_firmware.items()
            },
        )
        raw_path = run_dir / "serial.raw"
        normalized_path = run_dir / "serial.normalized.txt"
        stderr_path = run_dir / "qemu.stderr"
        observation_path = run_dir / "observation.json"
        stdout = bytearray()
        stderr = bytearray()
        launch_at = time.monotonic()
        deadline = launch_at + guest.timeout_seconds
        completion_at: float | None = None
        actions_sent_at: list[float] = []
        timed_out = False
        capture_overflow = False
        forbidden_seen = False
        if not _pinned_path_matches(pinned_qemu):
            raise LabError("private QEMU snapshot path changed before boot")
        process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE if guest.console_actions else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
            start_new_session=True,
            cwd=run_dir,
            env=qemu_environment,
            pass_fds=tuple(
                pinned.descriptor
                for pinned in (
                    *pinned_artifacts.values(),
                    *pinned_firmware.values(),
                    pinned_qemu,
                )
            ),
        )
        selector: selectors.BaseSelector | None = None
        try:
            if not linux_fd_exec and not _pinned_path_matches(pinned_qemu):
                raise LabError("private QEMU snapshot path changed during boot launch")
            assert process.stdout is not None
            assert process.stderr is not None
            selector = selectors.DefaultSelector()
            selector.register(process.stdout, selectors.EVENT_READ, "stdout")
            selector.register(process.stderr, selectors.EVENT_READ, "stderr")
            while True:
                now = time.monotonic()
                if now >= deadline and completion_at is None:
                    timed_out = True
                    break
                if (
                    completion_at is not None
                    and now - completion_at >= guest.post_completion_seconds
                ):
                    break
                if process.poll() is not None and not selector.get_map():
                    break
                timeout = max(0.0, min(0.05, deadline - now))
                for key, _ in selector.select(timeout=timeout):
                    chunk = os.read(key.fileobj.fileno(), 4096)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    destination = stdout if key.data == "stdout" else stderr
                    if _append_bounded(destination, chunk, guest.max_capture_bytes):
                        capture_overflow = True
                        break
                normalized = normalize_terminal(stdout)
                if (
                    len(actions_sent_at) < len(guest.console_actions)
                    and guest.console_actions[len(actions_sent_at)].trigger in normalized
                ):
                    action = guest.console_actions[len(actions_sent_at)]
                    assert process.stdin is not None
                    process.stdin.write(
                        ("\n".join(action.commands) + "\n").encode("ascii")
                    )
                    process.stdin.flush()
                    actions_sent_at.append(time.monotonic())
                validation = validate_transcript(guest, normalized)
                if validation["forbidden_counts"]:
                    forbidden_seen = True
                    break
                if validation["complete"] and completion_at is None:
                    completion_at = time.monotonic()
                if capture_overflow:
                    break
            selector.close()
            selector = None

            pre_cleanup_drain_eof: list[bool] = []
            for source, destination in (
                (process.stdout, stdout),
                (process.stderr, stderr),
            ):
                overflow, reached_eof = _drain_nonblocking(
                    source, destination, guest.max_capture_bytes
                )
                if overflow:
                    capture_overflow = True
                pre_cleanup_drain_eof.append(reached_eof)
            pre_cleanup_returncode = process.poll()
            pre_cleanup_stderr_size = len(stderr)
            cleanup_action, cleanup_returncode = _cleanup_process(process)
            cleanup_resolved = cleanup_action not in {
                "group-missing-leader-live",
                "kill-permission-denied",
                "kill-timeout",
                "terminate-permission-denied",
            } and not _process_group_exists(process.pid)
            drain_eof: list[bool] = []
            for source, destination in (
                (process.stdout, stdout),
                (process.stderr, stderr),
            ):
                overflow, reached_eof = _drain_nonblocking(
                    source, destination, guest.max_capture_bytes
                )
                if overflow:
                    capture_overflow = True
                drain_eof.append(reached_eof)
                source.close()
            drain_complete = all(drain_eof)
            if process.stdin is not None:
                process.stdin.close()
        except BaseException:
            try:
                _cleanup_process(process)
            except BaseException:
                pass
            if selector is not None:
                try:
                    selector.close()
                except BaseException:
                    pass
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream is not None:
                    try:
                        stream.close()
                    except BaseException:
                        pass
            raise
        observed_seconds = time.monotonic() - launch_at
        pre_cleanup_stderr = bytes(stderr[:pre_cleanup_stderr_size])
        pre_cleanup_stderr_sha256 = hashlib.sha256(pre_cleanup_stderr).hexdigest()
        cleanup_stderr = bytes(stderr)[pre_cleanup_stderr_size:]
        cleanup_stderr_size = len(cleanup_stderr)
        cleanup_stderr_sha256 = hashlib.sha256(cleanup_stderr).hexdigest()

        input_post_identities: dict[tuple[str, str], FileIdentity] = {}
        input_stability_issues: list[str] = []
        for kind, inputs in (("artifact", pinned_artifacts), ("firmware", pinned_firmware)):
            for identifier, pinned in inputs.items():
                try:
                    observed = _hash_descriptor(pinned.descriptor)
                except LabError as error:
                    input_stability_issues.append(f"{kind}:{identifier}: {error}")
                    continue
                input_post_identities[(kind, identifier)] = observed
                if observed != pinned.identity:
                    input_stability_issues.append(
                        f"{kind}:{identifier}: changed during QEMU execution"
                    )
        qemu_post_identity: FileIdentity | None = None
        try:
            qemu_post_identity = _hash_descriptor(pinned_qemu.descriptor)
        except LabError as error:
            input_stability_issues.append(f"qemu:executable: {error}")
        if qemu_post_identity is not None and qemu_post_identity != qemu_identity:
            input_stability_issues.append(
                "qemu:executable: changed during QEMU execution"
            )

        normalized = normalize_terminal(stdout)
        validation = validate_transcript(guest, normalized)
        exit_admissible = pre_cleanup_returncode in (None, 0)
        execution_success = bool(
            validation["complete"]
            and not timed_out
            and not capture_overflow
            and not forbidden_seen
            and exit_admissible
            and pre_cleanup_stderr_size == 0
            and cleanup_resolved
            and drain_complete
            and not input_stability_issues
            and len(actions_sent_at) == len(guest.console_actions)
        )
        claim_admissible = bool(
            execution_success
            and executor_claim_admitted
            and version_banner_admitted
        )
        status = (
            "passed"
            if claim_admissible
            else "synthetic-passed"
            if execution_success
            else "failed"
        )
        raw_path.write_bytes(bytes(stdout))
        normalized_path.write_text(normalized, encoding="utf-8")
        stderr_path.write_bytes(bytes(stderr))
        raw_identity = hash_file(raw_path)
        normalized_identity = hash_file(normalized_path)
        stderr_identity = hash_file(stderr_path)
        observation: dict[str, Any] = {
        "schema": OBSERVATION_SCHEMA,
        "status": status,
        "claim_admissible": claim_admissible,
        "claim_class": manifest.claim_class,
        "guest": {
            "id": guest.id,
            "family": guest.family,
            "version": guest.version,
            "architecture": guest.architecture,
            "qemu_profile": guest.qemu_profile,
            "claim": guest.claim,
            "nonclaims": list(guest.nonclaims),
        },
        "manifest": {
            "path": str(manifest.path),
            "size_bytes": manifest.identity.size_bytes,
            "sha256": manifest.identity.sha256,
        },
            "artifacts": {
                identifier: {
                    "path": str(pinned.source_path),
                    "transport": "inherited-read-only-fd",
                    "size_bytes": pinned.identity.size_bytes,
                    "sha256": pinned.identity.sha256,
                    "post_run_size_bytes": input_post_identities[
                        ("artifact", identifier)
                    ].size_bytes,
                    "post_run_sha256": input_post_identities[
                        ("artifact", identifier)
                    ].sha256,
                }
                for identifier, pinned in pinned_artifacts.items()
                if ("artifact", identifier) in input_post_identities
            },
            "firmware": {
                identifier: {
                    "path": str(pinned.source_path),
                    "transport": "inherited-read-only-fd",
                    "size_bytes": pinned.identity.size_bytes,
                    "sha256": pinned.identity.sha256,
                    "post_run_size_bytes": input_post_identities[
                        ("firmware", identifier)
                    ].size_bytes,
                    "post_run_sha256": input_post_identities[
                        ("firmware", identifier)
                    ].sha256,
                }
                for identifier, pinned in pinned_firmware.items()
                if ("firmware", identifier) in input_post_identities
            },
        "qemu": {
            "path": str(qemu_path),
            "executor_origin": executor_origin,
            "executor_claim_admitted": executor_claim_admitted,
            "version_banner_admitted": version_banner_admitted,
            "trust_policy": (
                "only the manifest-named executable discovered on PATH may establish "
                "the QEMU/TCG claim; explicit overrides are diagnostic"
            ),
            "transport": qemu_transport,
            "launch_path": qemu_launch_path,
            "size_bytes": qemu_identity.size_bytes,
            "sha256": qemu_identity.sha256,
            "post_run_size_bytes": (
                qemu_post_identity.size_bytes
                if qemu_post_identity is not None
                else None
            ),
            "post_run_sha256": (
                qemu_post_identity.sha256
                if qemu_post_identity is not None
                else None
            ),
            "version": qemu_version,
            "argv": command,
            "cwd": str(run_dir),
            "environment_policy": "private-home-tmp-minimal-fixed-locale-path",
            "host_privilege_boundary": "same-uid-no-qemu-containment-claim",
        },
        "transcript": {
            "raw_path": str(raw_path),
            "raw_size_bytes": raw_identity.size_bytes,
            "raw_sha256": raw_identity.sha256,
            "normalized_path": str(normalized_path),
            "normalized_size_bytes": normalized_identity.size_bytes,
            "normalized_sha256": normalized_identity.sha256,
            "stderr_path": str(stderr_path),
            "stderr_size_bytes": stderr_identity.size_bytes,
            "stderr_sha256": stderr_identity.sha256,
            "validation": validation,
        },
        "runtime": {
            "started_utc": timestamp,
            "observed_seconds": round(observed_seconds, 6),
            "timeout_seconds": guest.timeout_seconds,
            "completion_seen": completion_at is not None,
            "console_commands_sent": bool(actions_sent_at),
            "console_actions_total": len(guest.console_actions),
            "console_actions_sent": len(actions_sent_at),
            "timed_out": timed_out,
            "capture_overflow": capture_overflow,
            "execution_contract_complete": execution_success,
            "pre_cleanup_returncode": pre_cleanup_returncode,
            "pre_cleanup_drain_complete": all(pre_cleanup_drain_eof),
            "pre_cleanup_stderr_size": pre_cleanup_stderr_size,
            "pre_cleanup_stderr_sha256": pre_cleanup_stderr_sha256,
            "cleanup_stderr_size": cleanup_stderr_size,
            "cleanup_stderr_sha256": cleanup_stderr_sha256,
            "exit_admissible": exit_admissible,
            "cleanup_action": cleanup_action,
            "cleanup_returncode": cleanup_returncode,
            "cleanup_resolved": cleanup_resolved,
            "drain_complete": drain_complete,
            "input_stability_issues": input_stability_issues,
        },
            "repository": {
                **repository_context,
                "harness": {
                    "path": str(harness_path),
                    "size_bytes": harness_identity.size_bytes,
                    "sha256": harness_identity.sha256,
                },
            },
        "global_claims": list(manifest.claims),
        "global_nonclaims": list(manifest.nonclaims),
        }
        observation_path.write_text(
            json.dumps(observation, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        observation["observation_path"] = str(observation_path)
        return observation
    finally:
        for pinned in all_inputs:
            try:
                os.close(pinned.descriptor)
            except OSError:
                pass
        if input_dir is not None:
            try:
                input_dir.chmod(0o700)
            except OSError:
                pass
        if qemu_snapshot_path is not None:
            try:
                qemu_snapshot_path.unlink(missing_ok=True)
            except OSError:
                pass
        if input_dir is not None:
            try:
                input_dir.rmdir()
            except OSError:
                pass
        try:
            os.close(artifact_cache.descriptor)
        except OSError:
            pass


def _select_guests(manifest: Manifest, identifiers: list[str] | None) -> list[Guest]:
    by_id = {guest.id: guest for guest in manifest.guests}
    if not identifiers:
        return list(manifest.guests)
    unknown = sorted(set(identifiers) - set(by_id))
    if unknown:
        raise LabError(f"unknown guest ids: {unknown}")
    return [by_id[identifier] for identifier in identifiers]


def _firmware_overrides(values: list[str]) -> dict[str, Path]:
    result: dict[str, Path] = {}
    for value in values:
        identifier, separator, path = value.partition("=")
        if not separator or ID_PATTERN.fullmatch(identifier) is None or not path:
            raise LabError("--firmware must use ID=/absolute/path")
        if identifier in result:
            raise LabError(f"duplicate firmware override: {identifier}")
        candidate = Path(path)
        if not candidate.is_absolute():
            raise LabError("--firmware paths must be absolute")
        result[identifier] = candidate
    return result


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Fetch, verify, and boot checksum-pinned upstream kernels under QEMU TCG"
    )
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--guest-dir", type=Path, default=None)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("list", help="validate and list declared guests without mutation")
    for name in ("fetch", "verify"):
        command = subparsers.add_parser(name)
        command.add_argument("--guest", action="append", default=[])
    run = subparsers.add_parser("run")
    run.add_argument("guest")
    run.add_argument("--qemu", type=Path)
    run.add_argument("--firmware", action="append", default=[])
    run.add_argument("--output-dir", type=Path, default=PROJECT_ROOT / "target" / "foreign-kernel-lab")
    run_all = subparsers.add_parser("run-all")
    run_all.add_argument("--firmware", action="append", default=[])
    run_all.add_argument("--output-dir", type=Path, default=PROJECT_ROOT / "target" / "foreign-kernel-lab")
    return parser


def main(argv: list[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        manifest = load_manifest(arguments.manifest)
        guest_root = (arguments.guest_dir or default_guest_root()).expanduser().resolve()
        if arguments.command == "list":
            print(f"schema={manifest.schema}")
            print(f"claim_class={manifest.claim_class}")
            print(f"guest_root={guest_root}")
            for guest in manifest.guests:
                print(
                    f"guest={guest.id} family={guest.family} arch={guest.architecture} "
                    f"version={guest.version!r}"
                )
            return 0
        if arguments.command in {"fetch", "verify"}:
            selected = _select_guests(manifest, arguments.guest)
            for guest in selected:
                if arguments.command == "fetch":
                    for identifier, path, identity in fetch_guest(guest, guest_root):
                        print(
                            f"verified guest={guest.id} artifact={identifier} "
                            f"size={identity.size_bytes} sha256={identity.sha256} path={path}"
                        )
                else:
                    identities = verify_guest_artifacts(guest, guest_root)
                    for identifier, identity in identities.items():
                        print(
                            f"verified guest={guest.id} artifact={identifier} "
                            f"size={identity.size_bytes} sha256={identity.sha256}"
                        )
            return 0
        overrides = _firmware_overrides(arguments.firmware)
        unknown_overrides = sorted(set(overrides) - set(manifest.firmware))
        if unknown_overrides:
            raise LabError(f"unknown firmware override ids: {unknown_overrides}")
        selected = _select_guests(
            manifest, [arguments.guest] if arguments.command == "run" else None
        )
        failures = 0
        for guest in selected:
            observation = run_guest(
                manifest,
                guest,
                guest_root,
                arguments.output_dir.expanduser().resolve(),
                qemu_override=getattr(arguments, "qemu", None),
                firmware_overrides=overrides,
            )
            print(
                f"{observation['status'].upper()} guest={guest.id} "
                f"observation={observation['observation_path']}"
            )
            failures += observation["status"] != "passed"
        return 1 if failures else 0
    except (LabError, OSError) as error:
        print(f"foreign-kernel-lab: ERROR: {error}", file=os.sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
