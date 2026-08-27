#!/usr/bin/env python3
"""Build the lock for, inspect, and atomically publish OSTADIX capacity ISOs.

This module is deliberately Python-stdlib-only.  It treats a capacity ISO as a
closed, checksummed set of boot adapters: native Multiboot2/Linux entries and
Linux capacity-host entries which launch foreign guests under QEMU TCG.  ISO
inspection is descriptor-backed and streams all large payloads.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import struct
import sys
import tempfile
import tomllib
from typing import Any


PROFILE_SCHEMA = "ostadix.capacity-iso-profile/v1"
LOCK_SCHEMA = "ostadix.capacity-iso-lock/v1"
INSPECT_SCHEMA = "ostadix.capacity-iso/v1"
ARCHITECTURE = "x86_64"
VOLUME_ID = "OSTADIX_CAPACITY"
LOGICAL_BLOCK_SIZE = 2048
MIN_ISO_BYTES = 24 * LOGICAL_BLOCK_SIZE
MAX_ISO_BYTES = 16 * 1024 * 1024 * 1024
STREAM_CHUNK_BYTES = 4 * 1024 * 1024
MAX_VOLUME_DESCRIPTORS = 64
MAX_PROFILE_BYTES = 1024 * 1024
MAX_LOCK_BYTES = 4 * 1024 * 1024
MAX_DIRECTORY_BYTES = 16 * 1024 * 1024
MAX_EFI_FILE_BYTES = 64 * 1024 * 1024
MAX_SUSP_CONTINUATION_BYTES = 1024 * 1024
MAX_SUSP_ENTRIES = 256
MAX_ENTRIES = 64
MAX_ARTIFACTS = 256
MAX_ARGUMENTS = 128
MAX_ISO_PATH_BYTES = 1024
EL_TORITO_SYSTEM_ID = b"EL TORITO SPECIFICATION"
EFI_PLATFORM_ID = 0xEF
NO_EMULATION_MEDIA_TYPE = 0
LOCK_ISO_PATH = "/ostadix/capacity.lock.json"
GRUB_ISO_PATH = "/boot/grub/grub.cfg"

ADAPTERS = frozenset(
    {
        "multiboot2",
        "linux",
        "qemu-tcg-linux-direct",
        "qemu-tcg-qcow2",
        "qemu-tcg-raw-cd",
        "qemu-tcg-raw-cd-curses",
    }
)
QEMU_ADAPTERS = frozenset(adapter for adapter in ADAPTERS if adapter.startswith("qemu-tcg-"))
ARTIFACT_ROLES = frozenset(
    {
        "ocore-kernel",
        "linux-kernel",
        "linux-initrd",
        "guest-qcow2",
        "guest-raw-cd",
        "guest-rootfs",
        "userspace",
        "metadata",
        "signature",
    }
)

_IDENTIFIER = re.compile(r"[a-z0-9][a-z0-9._-]{0,63}\Z")
_ARGUMENT = re.compile(r"[A-Za-z0-9_./,:+%?=@-]{1,512}\Z")
_STAGE_COMPONENT = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,127}\Z")


class CapacityIsoError(ValueError):
    """The profile, stage, image, or publication violates the v1 contract."""


def _pairs_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise CapacityIsoError(f"duplicate JSON object key: {key!r}")
        result[key] = value
    return result


def _reject_json_constant(value: str) -> None:
    raise CapacityIsoError(f"non-finite JSON number is forbidden: {value}")


def _parse_json(raw: bytes, label: str) -> Any:
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise CapacityIsoError(f"{label} is not UTF-8") from error
    try:
        return json.loads(
            text,
            object_pairs_hook=_pairs_object,
            parse_constant=_reject_json_constant,
        )
    except CapacityIsoError:
        raise
    except (json.JSONDecodeError, RecursionError) as error:
        raise CapacityIsoError(f"{label} is not valid bounded JSON: {error}") from error


def canonical_json(value: Any) -> bytes:
    try:
        encoded = json.dumps(
            value,
            ensure_ascii=True,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("ascii") + b"\n"
    except (TypeError, ValueError, RecursionError) as error:
        raise CapacityIsoError(f"value cannot be represented as canonical JSON: {error}") from error
    return encoded


def _expect_mapping(value: Any, label: str) -> dict[str, Any]:
    if type(value) is not dict:
        raise CapacityIsoError(f"{label} must be an object")
    if any(type(key) is not str for key in value):
        raise CapacityIsoError(f"{label} contains a non-string key")
    return value


def _expect_list(value: Any, label: str, maximum: int) -> list[Any]:
    if type(value) is not list:
        raise CapacityIsoError(f"{label} must be an array")
    if len(value) > maximum:
        raise CapacityIsoError(f"{label} exceeds its {maximum}-item bound")
    return value


def _exact_fields(
    value: dict[str, Any], required: set[str], label: str, optional: set[str] | None = None
) -> None:
    optional = optional or set()
    missing = sorted(required - value.keys())
    unknown = sorted(value.keys() - required - optional)
    if missing:
        raise CapacityIsoError(f"{label} lacks required fields: {missing!r}")
    if unknown:
        raise CapacityIsoError(f"{label} contains unknown fields: {unknown!r}")


def _expect_string(value: Any, label: str, maximum: int = 4096) -> str:
    if type(value) is not str or not value or len(value.encode("utf-8")) > maximum:
        raise CapacityIsoError(f"{label} must be a non-empty string of at most {maximum} bytes")
    if "\x00" in value or any(ord(character) < 0x20 for character in value):
        raise CapacityIsoError(f"{label} contains a control character")
    return value


def _identifier(value: Any, label: str) -> str:
    text = _expect_string(value, label, 64)
    if not _IDENTIFIER.fullmatch(text):
        raise CapacityIsoError(f"{label} is not a canonical lowercase identifier")
    return text


def _title(value: Any, label: str) -> str:
    text = _expect_string(value, label, 160)
    if any(ord(character) > 0x7E for character in text) or any(
        character in text for character in ("'", "\\", "$", "{", "}")
    ):
        raise CapacityIsoError(f"{label} is not a safely renderable ASCII GRUB title")
    return text


def _hotkey(value: Any, label: str) -> str:
    text = _expect_string(value, label, 1)
    if len(text) != 1 or text not in "abcdefghijklmnopqrstuvwxyz0123456789":
        raise CapacityIsoError(f"{label} must be one lowercase ASCII letter or digit")
    return text


def _arguments(value: Any, label: str) -> list[str]:
    items = _expect_list(value, label, MAX_ARGUMENTS)
    result: list[str] = []
    for index, item in enumerate(items):
        argument = _expect_string(item, f"{label}[{index}]", 512)
        if not _ARGUMENT.fullmatch(argument):
            raise CapacityIsoError(f"{label}[{index}] is not a safe single kernel argument")
        if argument.startswith("ostadix.capacity="):
            raise CapacityIsoError(f"{label}[{index}] attempts to override the capacity selection")
        result.append(argument)
    return result


def _iso_path(value: Any, label: str) -> str:
    path = _expect_string(value, label, MAX_ISO_PATH_BYTES)
    if not path.startswith("/") or path.endswith("/") or "//" in path or "\\" in path:
        raise CapacityIsoError(f"{label} is not a canonical absolute ISO path")
    components = path[1:].split("/")
    if not components or any(component in ("", ".", "..") for component in components):
        raise CapacityIsoError(f"{label} contains an unsafe path component")
    for component in components:
        if not _STAGE_COMPONENT.fullmatch(component):
            raise CapacityIsoError(f"{label} contains a non-portable ISO path component")
    canonical = "/" + "/".join(component.lower() for component in components)
    if path != canonical:
        raise CapacityIsoError(f"{label} must be lowercase canonical form {canonical!r}")
    return path


def _stage_path(value: Any, label: str) -> str:
    path = _expect_string(value, label, 4096)
    if path.startswith("/") or path.endswith("/") or "//" in path or "\\" in path:
        raise CapacityIsoError(f"{label} is not a canonical relative stage path")
    components = path.split("/")
    if any(component in ("", ".", "..") or not _STAGE_COMPONENT.fullmatch(component) for component in components):
        raise CapacityIsoError(f"{label} contains an unsafe stage path component")
    return path


def _path_list(value: Any, label: str, maximum: int = MAX_ARTIFACTS) -> list[str]:
    items = _expect_list(value, label, maximum)
    result = [_iso_path(item, f"{label}[{index}]") for index, item in enumerate(items)]
    if len(set(result)) != len(result):
        raise CapacityIsoError(f"{label} contains duplicate ISO paths")
    return result


def _sha256_hex(value: Any, label: str) -> str:
    text = _expect_string(value, label, 64)
    if not re.fullmatch(r"[0-9a-f]{64}", text):
        raise CapacityIsoError(f"{label} is not a lowercase SHA-256 digest")
    return text


def _byte_count(value: Any, label: str, *, maximum: int = MAX_ISO_BYTES) -> int:
    if type(value) is not int or not 0 < value <= maximum:
        raise CapacityIsoError(f"{label} must be an integer in 1..{maximum}")
    return value


def _profile_artifacts(value: Any) -> list[dict[str, Any]]:
    items = _expect_list(value, "profile.artifacts", MAX_ARTIFACTS)
    if not items:
        raise CapacityIsoError("profile.artifacts must not be empty")
    result: list[dict[str, Any]] = []
    seen_iso: set[str] = set()
    seen_stage: set[str] = set()
    for index, item in enumerate(items):
        artifact = _expect_mapping(item, f"profile.artifacts[{index}]")
        _exact_fields(artifact, {"iso_path", "stage_path", "role"}, f"profile.artifacts[{index}]")
        iso_path = _iso_path(artifact["iso_path"], f"profile.artifacts[{index}].iso_path")
        stage_path = _stage_path(artifact["stage_path"], f"profile.artifacts[{index}].stage_path")
        role = _expect_string(artifact["role"], f"profile.artifacts[{index}].role", 32)
        if role not in ARTIFACT_ROLES:
            raise CapacityIsoError(f"profile.artifacts[{index}].role is unknown: {role!r}")
        if iso_path in (LOCK_ISO_PATH, GRUB_ISO_PATH):
            raise CapacityIsoError(f"profile artifact collides with reserved path {iso_path}")
        folded = iso_path.casefold()
        if folded in seen_iso:
            raise CapacityIsoError(f"duplicate profile artifact ISO path: {iso_path}")
        if stage_path in seen_stage:
            raise CapacityIsoError(f"duplicate profile artifact stage path: {stage_path}")
        seen_iso.add(folded)
        seen_stage.add(stage_path)
        result.append({"iso_path": iso_path, "stage_path": stage_path, "role": role})
    result.sort(key=lambda artifact: artifact["iso_path"])
    return result


def _entry(value: Any, label: str) -> dict[str, Any]:
    entry = _expect_mapping(value, label)
    common = {"id", "title", "hotkey", "adapter", "arguments"}
    if "adapter" not in entry:
        raise CapacityIsoError(f"{label} lacks required fields: ['adapter']")
    adapter = _expect_string(entry["adapter"], f"{label}.adapter", 32)
    if adapter not in ADAPTERS:
        raise CapacityIsoError(f"{label}.adapter is unknown: {adapter!r}")
    if adapter == "multiboot2":
        required = common | {"kernel_path"}
    elif adapter == "linux":
        required = common | {"kernel_path", "initrd_paths"}
    else:
        required = common | {
            "host_kernel_path",
            "host_initrd_path",
            "selection_id",
            "guest_artifact_paths",
        }
    _exact_fields(entry, required, label)
    normalized: dict[str, Any] = {
        "id": _identifier(entry["id"], f"{label}.id"),
        "title": _title(entry["title"], f"{label}.title"),
        "hotkey": _hotkey(entry["hotkey"], f"{label}.hotkey"),
        "adapter": adapter,
        "arguments": _arguments(entry["arguments"], f"{label}.arguments"),
    }
    if adapter == "multiboot2":
        normalized["kernel_path"] = _iso_path(entry["kernel_path"], f"{label}.kernel_path")
    elif adapter == "linux":
        normalized["kernel_path"] = _iso_path(entry["kernel_path"], f"{label}.kernel_path")
        initrds = _path_list(entry["initrd_paths"], f"{label}.initrd_paths", 16)
        if not initrds:
            raise CapacityIsoError(f"{label}.initrd_paths must not be empty")
        normalized["initrd_paths"] = initrds
    else:
        if "[virtualized/TCG]" not in normalized["title"]:
            raise CapacityIsoError(f"{label}.title must explicitly contain [virtualized/TCG]")
        normalized["host_kernel_path"] = _iso_path(
            entry["host_kernel_path"], f"{label}.host_kernel_path"
        )
        normalized["host_initrd_path"] = _iso_path(
            entry["host_initrd_path"], f"{label}.host_initrd_path"
        )
        normalized["selection_id"] = _identifier(entry["selection_id"], f"{label}.selection_id")
        guests = _path_list(
            entry["guest_artifact_paths"], f"{label}.guest_artifact_paths", MAX_ARTIFACTS
        )
        if not guests:
            raise CapacityIsoError(f"{label}.guest_artifact_paths must not be empty")
        if normalized["host_kernel_path"] in guests or normalized["host_initrd_path"] in guests:
            raise CapacityIsoError(f"{label} mixes capacity-host artifacts into guest closure")
        normalized["guest_artifact_paths"] = guests
    return normalized


def _validate_entries(
    entries_value: Any, artifacts: list[dict[str, Any]], default_entry_value: Any
) -> tuple[list[dict[str, Any]], str]:
    values = _expect_list(entries_value, "entries", MAX_ENTRIES)
    if not values:
        raise CapacityIsoError("entries must not be empty")
    entries = [_entry(value, f"entries[{index}]") for index, value in enumerate(values)]
    default_entry = _identifier(default_entry_value, "default_entry")
    ids: set[str] = set()
    hotkeys: set[str] = set()
    selections: set[str] = set()
    artifact_by_path = {artifact["iso_path"]: artifact for artifact in artifacts}
    referenced: set[str] = set()

    def require(path: str, roles: set[str], label: str) -> dict[str, Any]:
        artifact = artifact_by_path.get(path)
        if artifact is None:
            raise CapacityIsoError(f"{label} references undeclared artifact {path}")
        if artifact["role"] not in roles:
            raise CapacityIsoError(
                f"{label} references {path} with role {artifact['role']!r}; expected one of {sorted(roles)!r}"
            )
        referenced.add(path)
        return artifact

    for index, entry in enumerate(entries):
        label = f"entries[{index}]"
        if entry["id"] in ids:
            raise CapacityIsoError(f"duplicate entry id: {entry['id']}")
        if entry["hotkey"] in hotkeys:
            raise CapacityIsoError(f"duplicate entry hotkey: {entry['hotkey']}")
        ids.add(entry["id"])
        hotkeys.add(entry["hotkey"])
        adapter = entry["adapter"]
        if adapter == "multiboot2":
            require(entry["kernel_path"], {"ocore-kernel"}, f"{label}.kernel_path")
        elif adapter == "linux":
            require(entry["kernel_path"], {"linux-kernel"}, f"{label}.kernel_path")
            for path in entry["initrd_paths"]:
                require(path, {"linux-initrd"}, f"{label}.initrd_paths")
        else:
            require(entry["host_kernel_path"], {"linux-kernel"}, f"{label}.host_kernel_path")
            require(entry["host_initrd_path"], {"linux-initrd"}, f"{label}.host_initrd_path")
            selection = entry["selection_id"]
            if selection in selections:
                raise CapacityIsoError(f"duplicate QEMU capacity selection_id: {selection}")
            selections.add(selection)
            guest_roles: list[str] = []
            for path in entry["guest_artifact_paths"]:
                guest_roles.append(require(path, ARTIFACT_ROLES, f"{label}.guest_artifact_paths")["role"])
            if adapter == "qemu-tcg-linux-direct":
                if guest_roles.count("linux-kernel") != 1 or guest_roles.count("linux-initrd") < 1:
                    raise CapacityIsoError(
                        f"{label} requires exactly one guest Linux kernel and at least one guest initrd"
                    )
                if any(role in {"ocore-kernel", "guest-qcow2", "guest-raw-cd"} for role in guest_roles):
                    raise CapacityIsoError(f"{label} contains an incompatible guest artifact role")
            elif adapter == "qemu-tcg-qcow2":
                if guest_roles.count("guest-qcow2") != 1 or any(
                    role not in {"guest-qcow2", "metadata", "signature"} for role in guest_roles
                ):
                    raise CapacityIsoError(f"{label} requires exactly one QCOW2 guest artifact")
            else:
                if guest_roles.count("guest-raw-cd") != 1 or any(
                    role not in {"guest-raw-cd", "metadata", "signature"} for role in guest_roles
                ):
                    raise CapacityIsoError(f"{label} requires exactly one raw-CD guest artifact")
    if default_entry not in ids:
        raise CapacityIsoError("default_entry does not name an entry")
    unreferenced = sorted(set(artifact_by_path) - referenced)
    if unreferenced:
        raise CapacityIsoError(f"artifacts are outside the exact entry closure: {unreferenced!r}")
    return entries, default_entry


def render_grub(entries: list[dict[str, Any]], default_entry: str) -> bytes:
    """Render the only GRUB configuration admitted by lock schema v1."""

    lines = [
        "serial --unit=0 --speed=115200 --word=8 --parity=no --stop=1",
        "terminal_input console serial",
        "terminal_output console serial",
        "# Generated by ostadix_capacity_iso.py; do not edit.",
        "insmod part_gpt",
        "insmod fat",
        "insmod iso9660",
        "set timeout_style=menu",
        "set timeout=10",
        f"set default='{default_entry}'",
        "",
    ]
    for entry in entries:
        lines.append(
            f"menuentry '{entry['title']}' --id={entry['id']} --hotkey={entry['hotkey']} {{"
        )
        arguments = "" if not entry["arguments"] else " " + " ".join(entry["arguments"])
        if entry["adapter"] == "multiboot2":
            lines.append(f"    multiboot2 {entry['kernel_path']}{arguments}")
        elif entry["adapter"] == "linux":
            lines.append(f"    linux {entry['kernel_path']}{arguments}")
            lines.append("    initrd " + " ".join(entry["initrd_paths"]))
        else:
            selection = f"ostadix.capacity={entry['selection_id']}"
            host_arguments = " ".join([selection, *entry["arguments"]])
            lines.append(f"    linux {entry['host_kernel_path']} {host_arguments}")
            lines.append(f"    initrd {entry['host_initrd_path']}")
        lines.append("    boot")
        lines.append("}")
        lines.append("")
    return ("\n".join(lines).rstrip() + "\n").encode("ascii")


def _load_profile(path: Path) -> dict[str, Any]:
    raw = _read_small_path(path, MAX_PROFILE_BYTES, "capacity profile")
    if path.suffix.lower() == ".toml":
        try:
            parsed = tomllib.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
            raise CapacityIsoError(f"capacity profile is not valid TOML: {error}") from error
    elif path.suffix.lower() == ".json":
        parsed = _parse_json(raw, "capacity profile")
    else:
        raise CapacityIsoError("capacity profile must have a .json or .toml suffix")
    profile = _expect_mapping(parsed, "profile")
    _exact_fields(profile, {"schema", "architecture", "default_entry", "artifacts", "entries"}, "profile")
    if profile["schema"] != PROFILE_SCHEMA:
        raise CapacityIsoError(f"profile schema must be {PROFILE_SCHEMA!r}")
    if profile["architecture"] != ARCHITECTURE:
        raise CapacityIsoError(f"profile architecture must be {ARCHITECTURE!r}")
    artifacts = _profile_artifacts(profile["artifacts"])
    entries, default_entry = _validate_entries(profile["entries"], artifacts, profile["default_entry"])
    return {
        "schema": PROFILE_SCHEMA,
        "architecture": ARCHITECTURE,
        "default_entry": default_entry,
        "artifacts": artifacts,
        "entries": entries,
    }


def _open_pinned_regular(path: Path, label: str, *, readonly: bool = False) -> int:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
    if not hasattr(os, "O_NOFOLLOW"):
        raise CapacityIsoError("this host cannot pin files with O_NOFOLLOW")
    flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise CapacityIsoError(f"cannot open pinned {label}: {path}: {error}") from error
    try:
        state = os.fstat(descriptor)
        if not stat.S_ISREG(state.st_mode):
            raise CapacityIsoError(f"pinned {label} is not a regular file: {path}")
        if readonly and state.st_mode & 0o222:
            raise CapacityIsoError(f"pinned {label} has write-permission bits set: {path}")
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor


def _file_identity(value: os.stat_result) -> tuple[int, int, int, int, int]:
    return (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)


def _require_descriptor_identity(descriptor: int, expected: os.stat_result, label: str) -> None:
    try:
        current = os.fstat(descriptor)
    except OSError as error:
        raise CapacityIsoError(f"cannot recheck {label} descriptor: {error}") from error
    if _file_identity(current) != _file_identity(expected):
        raise CapacityIsoError(f"{label} changed while its descriptor was held")


def _require_path_identity(path: Path, expected: os.stat_result, label: str) -> None:
    try:
        current = os.stat(path, follow_symlinks=False)
    except OSError as error:
        raise CapacityIsoError(f"{label} path changed while held: {path}: {error}") from error
    if not stat.S_ISREG(current.st_mode) or _file_identity(current) != _file_identity(expected):
        raise CapacityIsoError(f"{label} path was replaced while held: {path}")


def _read_small_path(path: Path, maximum: int, label: str) -> bytes:
    descriptor = _open_pinned_regular(path, label)
    try:
        before = os.fstat(descriptor)
        if before.st_size <= 0 or before.st_size > maximum:
            raise CapacityIsoError(f"{label} size is outside 1..{maximum} bytes")
        raw = os.pread(descriptor, before.st_size, 0)
        if len(raw) != before.st_size or os.pread(descriptor, 1, before.st_size):
            raise CapacityIsoError(f"{label} changed length while read")
        _require_descriptor_identity(descriptor, before, label)
        _require_path_identity(path, before, label)
        return raw
    except OSError as error:
        raise CapacityIsoError(f"cannot read pinned {label}: {error}") from error
    finally:
        os.close(descriptor)


def _open_private_stage(path: Path) -> tuple[int, os.stat_result]:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_DIRECTORY", 0)
    if not hasattr(os, "O_NOFOLLOW"):
        raise CapacityIsoError("this host cannot pin a private stage with O_NOFOLLOW")
    flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise CapacityIsoError(f"cannot open private stage {path}: {error}") from error
    try:
        state = os.fstat(descriptor)
        if not stat.S_ISDIR(state.st_mode):
            raise CapacityIsoError(f"capacity stage is not a directory: {path}")
        if state.st_uid != os.geteuid() or state.st_mode & 0o077:
            raise CapacityIsoError("capacity stage must be owned by the caller and have mode 0700 or stricter")
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor, state


def _open_stage_artifact(stage_descriptor: int, relative: str) -> int:
    components = relative.split("/")
    current = os.dup(stage_descriptor)
    try:
        for component in components[:-1]:
            flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_DIRECTORY", 0)
            flags |= os.O_NOFOLLOW
            next_descriptor = os.open(component, flags, dir_fd=current)
            os.close(current)
            current = next_descriptor
            if not stat.S_ISDIR(os.fstat(current).st_mode):
                raise CapacityIsoError(f"stage component is not a directory: {relative}")
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | os.O_NOFOLLOW
        artifact_descriptor = os.open(components[-1], flags, dir_fd=current)
        state = os.fstat(artifact_descriptor)
        if not stat.S_ISREG(state.st_mode):
            os.close(artifact_descriptor)
            raise CapacityIsoError(f"stage artifact is not a regular file: {relative}")
        return artifact_descriptor
    except CapacityIsoError:
        raise
    except OSError as error:
        raise CapacityIsoError(f"cannot pin stage artifact {relative}: {error}") from error
    finally:
        os.close(current)


def _hash_descriptor(descriptor: int, size: int, label: str) -> str:
    digest = hashlib.sha256()
    offset = 0
    try:
        while offset < size:
            chunk = os.pread(descriptor, min(STREAM_CHUNK_BYTES, size - offset), offset)
            if not chunk:
                raise CapacityIsoError(f"{label} ended before its admitted size")
            digest.update(chunk)
            offset += len(chunk)
        if os.pread(descriptor, 1, size):
            raise CapacityIsoError(f"{label} grew beyond its admitted size")
    except OSError as error:
        raise CapacityIsoError(f"cannot stream {label}: {error}") from error
    return digest.hexdigest()


def _artifact_metadata(stage_descriptor: int, artifact: dict[str, Any]) -> dict[str, Any]:
    descriptor = _open_stage_artifact(stage_descriptor, artifact["stage_path"])
    try:
        before = os.fstat(descriptor)
        size = _byte_count(before.st_size, f"stage artifact {artifact['stage_path']} bytes")
        digest = _hash_descriptor(descriptor, size, f"stage artifact {artifact['stage_path']}")
        _require_descriptor_identity(descriptor, before, f"stage artifact {artifact['stage_path']}")
        return {
            "iso_path": artifact["iso_path"],
            "role": artifact["role"],
            "bytes": size,
            "sha256": digest,
        }
    finally:
        os.close(descriptor)


def _reject_output(path: Path, *, allow_regular: bool) -> None:
    try:
        state = os.stat(path, follow_symlinks=False)
    except FileNotFoundError:
        return
    except OSError as error:
        raise CapacityIsoError(f"cannot inspect output path {path}: {error}") from error
    if stat.S_ISLNK(state.st_mode) or not stat.S_ISREG(state.st_mode):
        raise CapacityIsoError(f"output path is a symlink or special file: {path}")
    if not allow_regular:
        raise CapacityIsoError(f"refusing to clobber existing output: {path}")


def _ensure_stage_directory(stage: Path, relative: str) -> Path:
    current = stage
    for component in relative.split("/"):
        current = current / component
        try:
            state = os.stat(current, follow_symlinks=False)
        except FileNotFoundError:
            try:
                current.mkdir(mode=0o700)
            except OSError as error:
                raise CapacityIsoError(f"cannot create stage directory {current}: {error}") from error
            state = os.stat(current, follow_symlinks=False)
        if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
            raise CapacityIsoError(f"stage output parent is not a non-symlink directory: {current}")
    return current


def _write_atomic(path: Path, data: bytes, *, replace_regular: bool) -> None:
    parent = path.parent
    try:
        parent_state = os.stat(parent, follow_symlinks=False)
    except OSError as error:
        raise CapacityIsoError(f"output parent is unavailable: {parent}: {error}") from error
    if stat.S_ISLNK(parent_state.st_mode) or not stat.S_ISDIR(parent_state.st_mode):
        raise CapacityIsoError(f"output parent is not a non-symlink directory: {parent}")
    _reject_output(path, allow_regular=replace_regular)
    descriptor = -1
    temporary = ""
    try:
        descriptor, temporary = tempfile.mkstemp(prefix=".ostadix-capacity.", suffix=".tmp", dir=parent)
        offset = 0
        while offset < len(data):
            written = os.write(descriptor, data[offset:])
            if written <= 0:
                raise CapacityIsoError(f"atomic output stopped accepting bytes: {path}")
            offset += written
        os.fchmod(descriptor, 0o444)
        os.fsync(descriptor)
        os.close(descriptor)
        descriptor = -1
        _reject_output(path, allow_regular=replace_regular)
        os.replace(temporary, path)
        temporary = ""
    except OSError as error:
        raise CapacityIsoError(f"cannot write atomic output {path}: {error}") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        if temporary:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass


def create_lock(stage: Path, profile_path: Path) -> dict[str, Any]:
    """Create canonical lock/config files at their fixed paths inside a private stage."""

    stage = Path(os.path.abspath(stage))
    profile_path = Path(os.path.abspath(profile_path))
    profile = _load_profile(profile_path)
    stage_descriptor, stage_state = _open_private_stage(stage)
    try:
        artifacts = [_artifact_metadata(stage_descriptor, artifact) for artifact in profile["artifacts"]]
        _require_descriptor_identity(stage_descriptor, stage_state, "capacity stage")
    finally:
        os.close(stage_descriptor)
    entries, default_entry = _validate_entries(profile["entries"], artifacts, profile["default_entry"])
    grub = render_grub(entries, default_entry)
    lock: dict[str, Any] = {
        "schema": LOCK_SCHEMA,
        "architecture": ARCHITECTURE,
        "volume_id": VOLUME_ID,
        "default_entry": default_entry,
        "artifacts": artifacts,
        "entries": entries,
        "grub": {
            "iso_path": GRUB_ISO_PATH,
            "bytes": len(grub),
            "sha256": hashlib.sha256(grub).hexdigest(),
        },
    }
    lock_bytes = canonical_json(lock)
    if len(lock_bytes) > MAX_LOCK_BYTES:
        raise CapacityIsoError("canonical capacity lock exceeds its size bound")
    grub_parent = _ensure_stage_directory(stage, "boot/grub")
    lock_parent = _ensure_stage_directory(stage, "ostadix")
    # Configuration is installed first; the lock is the final commit marker.
    _write_atomic(grub_parent / "grub.cfg", grub, replace_regular=True)
    _write_atomic(lock_parent / "capacity.lock.json", lock_bytes, replace_regular=True)
    return lock


class _DescriptorSource:
    """Bounded random and streaming access to one pinned regular descriptor."""

    def __init__(self, descriptor: int, size: int, label: str):
        self.descriptor = descriptor
        self.size = size
        self.label = label

    def read(self, offset: int, size: int, label: str, *, maximum: int = MAX_DIRECTORY_BYTES) -> bytes:
        if type(offset) is not int or type(size) is not int or offset < 0 or size < 0:
            raise CapacityIsoError(f"{label} has an invalid read range")
        if size > maximum:
            raise CapacityIsoError(f"{label} exceeds its {maximum}-byte materialization bound")
        if offset > self.size or size > self.size - offset:
            raise CapacityIsoError(f"{label} leaves {self.label}")
        chunks: list[bytes] = []
        cursor = offset
        remaining = size
        try:
            while remaining:
                chunk = os.pread(self.descriptor, min(remaining, STREAM_CHUNK_BYTES), cursor)
                if not chunk:
                    raise CapacityIsoError(f"{label} is truncated")
                chunks.append(chunk)
                cursor += len(chunk)
                remaining -= len(chunk)
        except OSError as error:
            raise CapacityIsoError(f"cannot read {label}: {error}") from error
        return b"".join(chunks)

    def hash_range(self, offset: int, size: int, label: str) -> str:
        if type(offset) is not int or type(size) is not int or offset < 0 or size < 0:
            raise CapacityIsoError(f"{label} has an invalid hash range")
        if offset > self.size or size > self.size - offset:
            raise CapacityIsoError(f"{label} leaves {self.label}")
        digest = hashlib.sha256()
        cursor = offset
        remaining = size
        try:
            while remaining:
                chunk = os.pread(self.descriptor, min(remaining, STREAM_CHUNK_BYTES), cursor)
                if not chunk:
                    raise CapacityIsoError(f"{label} is truncated while hashing")
                digest.update(chunk)
                cursor += len(chunk)
                remaining -= len(chunk)
        except OSError as error:
            raise CapacityIsoError(f"cannot hash {label}: {error}") from error
        return digest.hexdigest()


class _Extent:
    def __init__(self, source: _DescriptorSource, offset: int, size: int, label: str):
        if offset < 0 or size < 0 or offset > source.size or size > source.size - offset:
            raise CapacityIsoError(f"{label} extent leaves the ISO")
        self.source = source
        self.offset = offset
        self.size = size
        self.label = label

    def read(self, offset: int, size: int, *, maximum: int = MAX_DIRECTORY_BYTES) -> bytes:
        if offset < 0 or size < 0 or offset > self.size or size > self.size - offset:
            raise CapacityIsoError(f"read leaves {self.label} extent")
        return self.source.read(self.offset + offset, size, self.label, maximum=maximum)

    def sha256(self) -> str:
        return self.source.hash_range(self.offset, self.size, self.label)


def _u16_both(data: bytes, offset: int, label: str) -> int:
    if offset < 0 or offset + 4 > len(data):
        raise CapacityIsoError(f"{label} is truncated")
    little = int.from_bytes(data[offset : offset + 2], "little")
    big = int.from_bytes(data[offset + 2 : offset + 4], "big")
    if little != big:
        raise CapacityIsoError(f"{label} little- and big-endian forms differ")
    return little


def _u32_both(data: bytes, offset: int, label: str) -> int:
    if offset < 0 or offset + 8 > len(data):
        raise CapacityIsoError(f"{label} is truncated")
    little = int.from_bytes(data[offset : offset + 4], "little")
    big = int.from_bytes(data[offset + 4 : offset + 8], "big")
    if little != big:
        raise CapacityIsoError(f"{label} little- and big-endian forms differ")
    return little


def _directory_record(record: bytes, volume_bytes: int, label: str) -> dict[str, Any]:
    if len(record) < 34 or record[0] != len(record):
        raise CapacityIsoError(f"{label} has an invalid ISO9660 directory-record length")
    name_length = record[32]
    if 33 + name_length > len(record):
        raise CapacityIsoError(f"{label} has a truncated ISO9660 identifier")
    extent_lba = _u32_both(record, 2, f"{label} extent")
    size = _u32_both(record, 10, f"{label} size")
    flags = record[25]
    if flags & 0x80:
        raise CapacityIsoError(f"{label} uses unsupported ISO9660 multi-extent recording")
    start = extent_lba * LOGICAL_BLOCK_SIZE
    if extent_lba <= 0 or start > volume_bytes or size > volume_bytes - start:
        raise CapacityIsoError(f"{label} points outside the ISO9660 volume")
    return {
        "extent_lba": extent_lba,
        "offset": start,
        "size": size,
        "flags": flags,
        "name": record[33 : 33 + name_length],
        "record": record,
    }


def _normalized_iso_name(raw: bytes) -> str | None:
    if raw in (b"\x00", b"\x01"):
        return None
    try:
        value = raw.decode("ascii")
    except UnicodeDecodeError as error:
        raise CapacityIsoError("ISO9660 identifier is not ASCII") from error
    value = value.split(";", 1)[0].rstrip(".")
    if not value or "/" in value or "\x00" in value:
        raise CapacityIsoError("ISO9660 identifier is malformed")
    return value.upper()


def _system_use_bytes(record: bytes, label: str) -> bytes:
    if len(record) < 34:
        raise CapacityIsoError(f"{label} has a truncated ISO9660 directory record")
    name_length = record[32]
    start = 33 + name_length + (1 if name_length % 2 == 0 else 0)
    if start > len(record):
        raise CapacityIsoError(f"{label} has invalid ISO9660 system-use placement")
    return record[start:]


def _susp_entries(
    source: _DescriptorSource,
    record: bytes,
    volume_bytes: int,
    label: str,
) -> list[tuple[bytes, bytes]]:
    """Return bounded SUSP/Rock Ridge entries, following CE continuations."""

    pending = [_system_use_bytes(record, label)]
    continuations: set[tuple[int, int]] = set()
    result: list[tuple[bytes, bytes]] = []
    continuation_total = 0
    while pending:
        area = pending.pop(0)
        cursor = 0
        while cursor < len(area):
            if area[cursor:] == bytes(len(area) - cursor):
                break
            if cursor + 4 > len(area):
                raise CapacityIsoError(f"{label} has a truncated SUSP entry header")
            signature = area[cursor : cursor + 2]
            length = area[cursor + 2]
            version = area[cursor + 3]
            if length < 4 or cursor + length > len(area):
                raise CapacityIsoError(f"{label} has an invalid SUSP entry length")
            if version != 1:
                raise CapacityIsoError(f"{label} has an unsupported SUSP entry version")
            entry = area[cursor : cursor + length]
            result.append((signature, entry))
            if len(result) > MAX_SUSP_ENTRIES:
                raise CapacityIsoError(f"{label} exceeds the bounded SUSP entry count")
            if signature == b"CE":
                if length != 28:
                    raise CapacityIsoError(f"{label} has an invalid SUSP CE entry")
                block = _u32_both(entry, 4, f"{label} SUSP CE block")
                offset = _u32_both(entry, 12, f"{label} SUSP CE offset")
                size = _u32_both(entry, 20, f"{label} SUSP CE length")
                start = block * LOGICAL_BLOCK_SIZE + offset
                identity = (start, size)
                if (
                    size <= 0
                    or continuation_total + size > MAX_SUSP_CONTINUATION_BYTES
                    or start > volume_bytes
                    or size > volume_bytes - start
                    or identity in continuations
                ):
                    raise CapacityIsoError(f"{label} has an unsafe SUSP continuation area")
                continuations.add(identity)
                continuation_total += size
                pending.append(
                    source.read(
                        start,
                        size,
                        f"{label} SUSP continuation",
                        maximum=MAX_SUSP_CONTINUATION_BYTES,
                    )
                )
            cursor += length
            if signature == b"ST":
                break
    return result


def _rock_ridge_name(
    source: _DescriptorSource,
    record: bytes,
    volume_bytes: int,
    label: str,
) -> str | None:
    fragments: list[bytes] = []
    flags: list[int] = []
    for signature, entry in _susp_entries(source, record, volume_bytes, label):
        if signature != b"NM":
            continue
        if len(entry) < 5:
            raise CapacityIsoError(f"{label} has a truncated Rock Ridge NM entry")
        flag = entry[4]
        if flag & ~0x07 or flag & 0x06:
            raise CapacityIsoError(f"{label} has an unsupported Rock Ridge NM flag")
        fragments.append(entry[5:])
        flags.append(flag)
    if not fragments:
        return None
    if any(not flag & 0x01 for flag in flags[:-1]) or flags[-1] & 0x01:
        raise CapacityIsoError(f"{label} has an invalid continued Rock Ridge name")
    raw = b"".join(fragments)
    if not raw or len(raw) > 255:
        raise CapacityIsoError(f"{label} has an invalid Rock Ridge name length")
    try:
        name = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise CapacityIsoError(f"{label} Rock Ridge name is not UTF-8") from error
    if "/" in name or "\x00" in name or name in (".", "..") or any(
        ord(character) < 0x20 for character in name
    ):
        raise CapacityIsoError(f"{label} has an unsafe Rock Ridge name")
    return name


def _directory_entries(
    source: _DescriptorSource, directory: dict[str, Any], volume_bytes: int, label: str
) -> list[dict[str, Any]]:
    size = int(directory["size"])
    if size <= 0 or size > MAX_DIRECTORY_BYTES:
        raise CapacityIsoError(f"{label} directory size exceeds the bounded contract")
    content = source.read(int(directory["offset"]), size, label, maximum=MAX_DIRECTORY_BYTES)
    entries: list[dict[str, Any]] = []
    offset = 0
    seen: set[str] = set()
    while offset < len(content):
        record_length = content[offset]
        if record_length == 0:
            offset = ((offset // LOGICAL_BLOCK_SIZE) + 1) * LOGICAL_BLOCK_SIZE
            continue
        sector_remaining = LOGICAL_BLOCK_SIZE - (offset % LOGICAL_BLOCK_SIZE)
        if record_length > sector_remaining or offset + record_length > len(content):
            raise CapacityIsoError(f"{label} has a directory record crossing its block")
        entry = _directory_record(content[offset : offset + record_length], volume_bytes, label)
        normalized = _normalized_iso_name(entry["name"])
        if normalized is not None:
            rock_ridge = _rock_ridge_name(
                source,
                entry["record"],
                volume_bytes,
                f"{label}/{normalized}",
            )
            selected_name = rock_ridge if rock_ridge is not None else normalized
            folded = selected_name.casefold()
            if folded in seen:
                raise CapacityIsoError(
                    f"{label} contains duplicate effective ISO identifier {selected_name!r}"
                )
            seen.add(folded)
            entry["normalized_name"] = normalized
            entry["rock_ridge_name"] = rock_ridge
            entries.append(entry)
        offset += record_length
    return entries


def _find_iso_path(
    source: _DescriptorSource,
    root: dict[str, Any],
    logical_path: str,
    volume_bytes: int,
) -> dict[str, Any]:
    canonical = _iso_path(logical_path, "ISO lookup path")
    components = canonical[1:].split("/")
    current = root
    for index, component in enumerate(components):
        if not int(current["flags"]) & 0x02:
            raise CapacityIsoError(f"component before {component} in {canonical} is not a directory")
        entries = _directory_entries(
            source, current, volume_bytes, "/" + "/".join(components[:index])
        )
        matches = [
            entry
            for entry in entries
            if (
                entry["rock_ridge_name"] == component
                if entry["rock_ridge_name"] is not None
                else entry["normalized_name"] == component.upper()
            )
        ]
        if len(matches) != 1:
            raise CapacityIsoError(f"ISO path {canonical} has {len(matches)} matches for {component}")
        current = matches[0]
    if int(current["flags"]) & 0x02:
        raise CapacityIsoError(f"ISO path {canonical} is a directory")
    return current


def _el_torito_entries(catalog: bytes) -> list[tuple[int, bytes]]:
    if len(catalog) != LOGICAL_BLOCK_SIZE:
        raise CapacityIsoError("El Torito boot catalog is truncated")
    validation = catalog[:32]
    if validation[0] != 0x01 or validation[30:32] != b"\x55\xaa":
        raise CapacityIsoError("El Torito validation entry is malformed")
    if sum(struct.unpack("<16H", validation)) & 0xFFFF:
        raise CapacityIsoError("El Torito validation checksum is invalid")
    entries: list[tuple[int, bytes]] = [(validation[1], catalog[32:64])]
    index = 2
    while index < LOGICAL_BLOCK_SIZE // 32:
        entry = catalog[index * 32 : (index + 1) * 32]
        indicator = entry[0]
        if entry == bytes(32):
            break
        if indicator in (0x90, 0x91):
            platform = entry[1]
            count = int.from_bytes(entry[2:4], "little")
            if count <= 0 or index + count >= LOGICAL_BLOCK_SIZE // 32:
                raise CapacityIsoError("El Torito section has an invalid entry count")
            for section_index in range(index + 1, index + count + 1):
                selected = catalog[section_index * 32 : (section_index + 1) * 32]
                if selected[0] not in (0x00, 0x88):
                    raise CapacityIsoError("El Torito section has an invalid boot indicator")
                entries.append((platform, selected))
            index += count + 1
            if indicator == 0x91:
                break
            continue
        if indicator == 0x44:
            index += 1
            continue
        raise CapacityIsoError("El Torito catalog contains an unexpected entry")
    return entries


def _fat_geometry(image: _Extent) -> dict[str, int]:
    sector = image.read(0, 512, maximum=512)
    if sector[510:512] != b"\x55\xaa":
        raise CapacityIsoError("El Torito EFI image lacks a FAT boot signature")
    bytes_per_sector = int.from_bytes(sector[11:13], "little")
    sectors_per_cluster = sector[13]
    reserved_sectors = int.from_bytes(sector[14:16], "little")
    fat_count = sector[16]
    root_entries = int.from_bytes(sector[17:19], "little")
    total_sectors = int.from_bytes(sector[19:21], "little") or int.from_bytes(sector[32:36], "little")
    fat_sectors = int.from_bytes(sector[22:24], "little") or int.from_bytes(sector[36:40], "little")
    if bytes_per_sector not in (512, 1024, 2048, 4096):
        raise CapacityIsoError("El Torito EFI image has an unsupported FAT sector size")
    if (
        sectors_per_cluster == 0
        or sectors_per_cluster & (sectors_per_cluster - 1)
        or sectors_per_cluster > 128
        or reserved_sectors == 0
        or fat_count not in (1, 2)
        or fat_sectors == 0
        or total_sectors == 0
    ):
        raise CapacityIsoError("El Torito EFI image has invalid FAT geometry")
    image_bytes = total_sectors * bytes_per_sector
    if image_bytes > image.size:
        raise CapacityIsoError("El Torito EFI image is truncated")
    root_dir_sectors = (root_entries * 32 + bytes_per_sector - 1) // bytes_per_sector
    first_data_sector = reserved_sectors + fat_count * fat_sectors + root_dir_sectors
    if first_data_sector >= total_sectors:
        raise CapacityIsoError("El Torito EFI image has no FAT data region")
    cluster_count = (total_sectors - first_data_sector) // sectors_per_cluster
    if cluster_count < 1:
        raise CapacityIsoError("El Torito EFI image has no usable FAT clusters")
    fat_bits = 12 if cluster_count < 4085 else 16 if cluster_count < 65525 else 32
    root_cluster = int.from_bytes(sector[44:48], "little") if fat_bits == 32 else 0
    if fat_bits == 32 and root_cluster < 2:
        raise CapacityIsoError("El Torito EFI FAT32 root cluster is invalid")
    return {
        "bytes_per_sector": bytes_per_sector,
        "sectors_per_cluster": sectors_per_cluster,
        "reserved_sectors": reserved_sectors,
        "fat_count": fat_count,
        "root_entries": root_entries,
        "total_sectors": total_sectors,
        "fat_sectors": fat_sectors,
        "root_dir_sectors": root_dir_sectors,
        "first_data_sector": first_data_sector,
        "fat_bits": fat_bits,
        "root_cluster": root_cluster,
        "image_bytes": image_bytes,
    }


def _fat_next(image: _Extent, geometry: dict[str, int], cluster: int) -> int:
    fat_start = geometry["reserved_sectors"] * geometry["bytes_per_sector"]
    bits = geometry["fat_bits"]
    if bits == 12:
        offset = cluster + cluster // 2
        word = int.from_bytes(image.read(fat_start + offset, 2, maximum=2), "little")
        return (word >> 4 if cluster & 1 else word) & 0x0FFF
    if bits == 16:
        return int.from_bytes(image.read(fat_start + cluster * 2, 2, maximum=2), "little")
    return int.from_bytes(image.read(fat_start + cluster * 4, 4, maximum=4), "little") & 0x0FFFFFFF


def _fat_eoc(bits: int, cluster: int) -> bool:
    return cluster >= (0x0FF8 if bits == 12 else 0xFFF8 if bits == 16 else 0x0FFFFFF8)


def _fat_chain_bytes(
    image: _Extent, geometry: dict[str, int], first_cluster: int, maximum: int, label: str
) -> bytes:
    if first_cluster < 2:
        raise CapacityIsoError(f"{label} has an invalid first FAT cluster")
    cluster_bytes = geometry["sectors_per_cluster"] * geometry["bytes_per_sector"]
    first_data = geometry["first_data_sector"] * geometry["bytes_per_sector"]
    maximum_clusters = (geometry["total_sectors"] - geometry["first_data_sector"]) // geometry[
        "sectors_per_cluster"
    ]
    seen: set[int] = set()
    chunks: list[bytes] = []
    total = 0
    cluster = first_cluster
    while True:
        if cluster in seen or cluster < 2 or len(seen) >= maximum_clusters:
            raise CapacityIsoError(f"{label} FAT chain is cyclic or out of range")
        seen.add(cluster)
        start = first_data + (cluster - 2) * cluster_bytes
        if start > geometry["image_bytes"] or cluster_bytes > geometry["image_bytes"] - start:
            raise CapacityIsoError(f"{label} FAT cluster leaves the boot image")
        if total + cluster_bytes > maximum:
            raise CapacityIsoError(f"{label} exceeds its materialization bound")
        chunks.append(image.read(start, cluster_bytes, maximum=cluster_bytes))
        total += cluster_bytes
        next_cluster = _fat_next(image, geometry, cluster)
        if _fat_eoc(geometry["fat_bits"], next_cluster):
            break
        cluster = next_cluster
    return b"".join(chunks)


def _fat_directory(
    image: _Extent, geometry: dict[str, int], first_cluster: int | None
) -> list[dict[str, Any]]:
    if first_cluster is None:
        start_sector = geometry["reserved_sectors"] + geometry["fat_count"] * geometry["fat_sectors"]
        size = geometry["root_dir_sectors"] * geometry["bytes_per_sector"]
        if size > MAX_DIRECTORY_BYTES:
            raise CapacityIsoError("EFI FAT root directory exceeds its bound")
        content = image.read(start_sector * geometry["bytes_per_sector"], size, maximum=MAX_DIRECTORY_BYTES)
    else:
        content = _fat_chain_bytes(image, geometry, first_cluster, MAX_DIRECTORY_BYTES, "EFI directory")
    entries: list[dict[str, Any]] = []
    seen: set[str] = set()
    for offset in range(0, len(content), 32):
        record = content[offset : offset + 32]
        if len(record) < 32 or record[0] == 0x00:
            break
        if record[0] == 0xE5 or record[11] == 0x0F or record[11] & 0x08:
            continue
        try:
            stem = record[:8].decode("ascii").rstrip(" ")
            suffix = record[8:11].decode("ascii").rstrip(" ")
        except UnicodeDecodeError as error:
            raise CapacityIsoError("EFI FAT short name is not ASCII") from error
        name = (stem if not suffix else f"{stem}.{suffix}").upper()
        if name in seen:
            raise CapacityIsoError(f"EFI FAT directory contains duplicate entry {name}")
        seen.add(name)
        entries.append(
            {
                "name": name,
                "attributes": record[11],
                "first_cluster": (int.from_bytes(record[20:22], "little") << 16)
                | int.from_bytes(record[26:28], "little"),
                "size": int.from_bytes(record[28:32], "little"),
            }
        )
    return entries


def _find_fat_bootloader(image: _Extent, geometry: dict[str, int]) -> bytes:
    directory_cluster: int | None = geometry["root_cluster"] if geometry["fat_bits"] == 32 else None
    components = ("EFI", "BOOT", "BOOTX64.EFI")
    selected: dict[str, Any] | None = None
    for index, component in enumerate(components):
        matches = [entry for entry in _fat_directory(image, geometry, directory_cluster) if entry["name"] == component]
        if len(matches) != 1:
            raise CapacityIsoError(
                f"El Torito EFI image has {len(matches)} matches for /{'/'.join(components[: index + 1])}"
            )
        selected = matches[0]
        is_directory = bool(int(selected["attributes"]) & 0x10)
        if index < len(components) - 1:
            if not is_directory:
                raise CapacityIsoError(f"EFI FAT component {component} is not a directory")
            directory_cluster = int(selected["first_cluster"])
        elif is_directory:
            raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI is a directory")
    assert selected is not None
    size = int(selected["size"])
    if size <= 0 or size > MAX_EFI_FILE_BYTES:
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI size exceeds its bound")
    content = _fat_chain_bytes(
        image, geometry, int(selected["first_cluster"]), MAX_EFI_FILE_BYTES, "BOOTX64.EFI"
    )
    if size > len(content):
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI is truncated")
    return content[:size]


def _validate_x86_64_efi_application(bootloader: bytes) -> None:
    if len(bootloader) < 64 or bootloader[:2] != b"MZ":
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI lacks a DOS/PE header")
    pe_offset = int.from_bytes(bootloader[0x3C:0x40], "little")
    if pe_offset < 64 or pe_offset + 24 > len(bootloader):
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI has an invalid PE header offset")
    if bootloader[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI lacks a PE signature")
    if int.from_bytes(bootloader[pe_offset + 4 : pe_offset + 6], "little") != 0x8664:
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI is not x86_64")
    section_count = int.from_bytes(bootloader[pe_offset + 6 : pe_offset + 8], "little")
    optional_size = int.from_bytes(bootloader[pe_offset + 20 : pe_offset + 22], "little")
    characteristics = int.from_bytes(bootloader[pe_offset + 22 : pe_offset + 24], "little")
    if not 1 <= section_count <= 96 or not characteristics & 0x0002:
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI is not a bounded executable PE image")
    optional_offset = pe_offset + 24
    if optional_size < 112 or optional_offset + optional_size > len(bootloader):
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI has a truncated optional header")
    optional = bootloader[optional_offset : optional_offset + optional_size]
    if int.from_bytes(optional[:2], "little") != 0x20B:
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI is not PE32+")
    if int.from_bytes(optional[68:70], "little") != 10:
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI is not an EFI application")
    entry_rva = int.from_bytes(optional[16:20], "little")
    section_alignment = int.from_bytes(optional[32:36], "little")
    file_alignment = int.from_bytes(optional[36:40], "little")
    image_size = int.from_bytes(optional[56:60], "little")
    headers_size = int.from_bytes(optional[60:64], "little")
    if (
        entry_rva == 0
        or section_alignment == 0
        or section_alignment & (section_alignment - 1)
        or file_alignment == 0
        or file_alignment & (file_alignment - 1)
        or file_alignment > section_alignment
        or image_size == 0
    ):
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI has invalid PE geometry")
    table_offset = optional_offset + optional_size
    table_end = table_offset + section_count * 40
    if table_end > len(bootloader) or headers_size < table_end or headers_size > len(bootloader):
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI has a truncated PE section table")
    executable_entry = False
    for index in range(section_count):
        section = bootloader[table_offset + index * 40 : table_offset + (index + 1) * 40]
        virtual_size = int.from_bytes(section[8:12], "little")
        virtual_address = int.from_bytes(section[12:16], "little")
        raw_size = int.from_bytes(section[16:20], "little")
        raw_offset = int.from_bytes(section[20:24], "little")
        flags = int.from_bytes(section[36:40], "little")
        admitted = max(virtual_size, raw_size)
        if virtual_address > image_size or admitted > image_size - virtual_address:
            raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI has a section outside its image")
        if raw_size and (raw_offset > len(bootloader) or raw_size > len(bootloader) - raw_offset):
            raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI has a truncated section")
        if admitted and virtual_address <= entry_rva < virtual_address + admitted and flags & 0x20000000:
            relative = entry_rva - virtual_address
            executable_entry = executable_entry or (relative < raw_size and raw_offset + relative < len(bootloader))
    if not executable_entry:
        raise CapacityIsoError("EFI/BOOT/BOOTX64.EFI entry is not file-backed executable code")


def _valid_multiboot2_headers(prefix: bytes) -> list[int]:
    magic = 0xE85250D6
    scan_limit = min(len(prefix), 32768)
    valid: list[int] = []
    for offset in range(0, max(0, scan_limit - 15), 8):
        if int.from_bytes(prefix[offset : offset + 4], "little") != magic:
            continue
        architecture = int.from_bytes(prefix[offset + 4 : offset + 8], "little")
        header_length = int.from_bytes(prefix[offset + 8 : offset + 12], "little")
        checksum = int.from_bytes(prefix[offset + 12 : offset + 16], "little")
        if (
            architecture != 0
            or header_length < 24
            or header_length & 7
            or offset + header_length > scan_limit
            or (magic + architecture + header_length + checksum) & 0xFFFFFFFF
        ):
            continue
        cursor = offset + 16
        end = offset + header_length
        saw_end = False
        while cursor + 8 <= end:
            tag_type = int.from_bytes(prefix[cursor : cursor + 2], "little")
            tag_flags = int.from_bytes(prefix[cursor + 2 : cursor + 4], "little")
            tag_size = int.from_bytes(prefix[cursor + 4 : cursor + 8], "little")
            if tag_size < 8 or cursor + tag_size > end:
                break
            next_cursor = (cursor + tag_size + 7) & ~7
            if tag_type == 0:
                saw_end = tag_flags == 0 and tag_size == 8 and next_cursor == end
                cursor = next_cursor
                break
            cursor = next_cursor
        if saw_end and cursor == end:
            valid.append(offset)
    return valid


def _validate_x86_64_multiboot2_kernel(extent: _Extent, logical_path: str) -> None:
    if extent.size < 64:
        raise CapacityIsoError(f"{logical_path} is too small to be an ELF image")
    header = extent.read(0, min(extent.size, 32768), maximum=32768)
    if header[:4] != b"\x7fELF" or header[4:7] != b"\x02\x01\x01":
        raise CapacityIsoError(f"{logical_path} is not little-endian ELF64")
    if int.from_bytes(header[16:18], "little") != 2:
        raise CapacityIsoError(f"{logical_path} is not an executable ELF")
    if int.from_bytes(header[18:20], "little") != 62:
        raise CapacityIsoError(f"{logical_path} is not x86_64")
    if int.from_bytes(header[20:24], "little") != 1:
        raise CapacityIsoError(f"{logical_path} has an unsupported ELF version")
    entry = int.from_bytes(header[24:32], "little")
    program_offset = int.from_bytes(header[32:40], "little")
    header_size = int.from_bytes(header[52:54], "little")
    program_entry_size = int.from_bytes(header[54:56], "little")
    program_count = int.from_bytes(header[56:58], "little")
    if entry == 0 or header_size != 64 or program_entry_size != 56 or not 1 <= program_count <= 128:
        raise CapacityIsoError(f"{logical_path} has invalid ELF execution metadata")
    table_size = program_entry_size * program_count
    if program_offset < header_size or program_offset > extent.size or table_size > extent.size - program_offset:
        raise CapacityIsoError(f"{logical_path} has a truncated program-header table")
    programs = extent.read(program_offset, table_size, maximum=128 * 56)
    load_segments = 0
    executable_entry = False
    for index in range(program_count):
        program = programs[index * 56 : (index + 1) * 56]
        if int.from_bytes(program[:4], "little") != 1:
            continue
        load_segments += 1
        flags = int.from_bytes(program[4:8], "little")
        file_offset = int.from_bytes(program[8:16], "little")
        virtual_address = int.from_bytes(program[16:24], "little")
        file_size = int.from_bytes(program[32:40], "little")
        memory_size = int.from_bytes(program[40:48], "little")
        alignment = int.from_bytes(program[48:56], "little")
        if file_size > memory_size or file_offset > extent.size or file_size > extent.size - file_offset:
            raise CapacityIsoError(f"{logical_path} has a truncated PT_LOAD segment")
        if memory_size == 0 or (
            alignment not in (0, 1)
            and (alignment & (alignment - 1) or virtual_address % alignment != file_offset % alignment)
        ):
            raise CapacityIsoError(f"{logical_path} has invalid PT_LOAD geometry")
        if flags & 1 and virtual_address <= entry < virtual_address + file_size:
            executable_entry = True
    if load_segments == 0 or not executable_entry:
        raise CapacityIsoError(f"{logical_path} lacks file-backed executable PT_LOAD code")
    headers = _valid_multiboot2_headers(header)
    if len(headers) != 1:
        raise CapacityIsoError(f"{logical_path} has {len(headers)} valid Multiboot2 headers; expected one")


def _validate_linux_bzimage(extent: _Extent, logical_path: str) -> None:
    if extent.size < 0x240:
        raise CapacityIsoError(f"{logical_path} is too small to be a Linux bzImage")
    header = extent.read(0, 0x240, maximum=0x240)
    if header[0x1FE:0x200] != b"\x55\xaa" or header[0x202:0x206] != b"HdrS":
        raise CapacityIsoError(f"{logical_path} lacks the Linux boot protocol signature")
    version = int.from_bytes(header[0x206:0x208], "little")
    if version < 0x0200:
        raise CapacityIsoError(f"{logical_path} uses an unsupported Linux boot protocol")
    if not header[0x211] & 0x01:
        raise CapacityIsoError(f"{logical_path} is not marked as a loaded-high bzImage")
    xloadflags = int.from_bytes(header[0x236:0x238], "little")
    if not xloadflags & 0x0001:
        raise CapacityIsoError(f"{logical_path} is not an x86_64 Linux bzImage")
    setup_sectors = header[0x1F1] or 4
    if (setup_sectors + 1) * 512 >= extent.size:
        raise CapacityIsoError(f"{logical_path} has no protected-mode kernel payload")
    if int.from_bytes(header[0x1F4:0x1F8], "little") == 0:
        raise CapacityIsoError(f"{logical_path} has a zero Linux system size")


def _validate_qcow2(extent: _Extent, logical_path: str) -> None:
    if extent.size < 72:
        raise CapacityIsoError(f"{logical_path} is too small to be QCOW2")
    header = extent.read(0, min(extent.size, 104), maximum=104)
    if header[:4] != b"QFI\xfb":
        raise CapacityIsoError(f"{logical_path} lacks the QCOW2 magic")
    version = int.from_bytes(header[4:8], "big")
    cluster_bits = int.from_bytes(header[20:24], "big")
    virtual_size = int.from_bytes(header[24:32], "big")
    if version not in (2, 3) or not 9 <= cluster_bits <= 21 or virtual_size == 0:
        raise CapacityIsoError(f"{logical_path} has invalid QCOW2 geometry")


def _validate_raw_cd(extent: _Extent, logical_path: str) -> None:
    if extent.size < 17 * LOGICAL_BLOCK_SIZE:
        raise CapacityIsoError(f"{logical_path} is too small to be a raw CD image")
    descriptor = extent.read(16 * LOGICAL_BLOCK_SIZE, LOGICAL_BLOCK_SIZE, maximum=LOGICAL_BLOCK_SIZE)
    if descriptor[0] != 1 or descriptor[1:6] != b"CD001" or descriptor[6] != 1:
        raise CapacityIsoError(f"{logical_path} lacks an ISO9660 primary descriptor")


def _lock_artifacts(value: Any) -> list[dict[str, Any]]:
    items = _expect_list(value, "lock.artifacts", MAX_ARTIFACTS)
    if not items:
        raise CapacityIsoError("lock.artifacts must not be empty")
    result: list[dict[str, Any]] = []
    seen: set[str] = set()
    previous = ""
    for index, item in enumerate(items):
        artifact = _expect_mapping(item, f"lock.artifacts[{index}]")
        _exact_fields(artifact, {"iso_path", "role", "bytes", "sha256"}, f"lock.artifacts[{index}]")
        path = _iso_path(artifact["iso_path"], f"lock.artifacts[{index}].iso_path")
        role = _expect_string(artifact["role"], f"lock.artifacts[{index}].role", 32)
        if role not in ARTIFACT_ROLES:
            raise CapacityIsoError(f"lock.artifacts[{index}].role is unknown: {role!r}")
        if path in (LOCK_ISO_PATH, GRUB_ISO_PATH):
            raise CapacityIsoError(f"lock artifact collides with reserved path {path}")
        if path.casefold() in seen:
            raise CapacityIsoError(f"duplicate lock artifact ISO path: {path}")
        if previous and path <= previous:
            raise CapacityIsoError("lock.artifacts are not strictly sorted by iso_path")
        previous = path
        seen.add(path.casefold())
        result.append(
            {
                "iso_path": path,
                "role": role,
                "bytes": _byte_count(artifact["bytes"], f"lock.artifacts[{index}].bytes"),
                "sha256": _sha256_hex(artifact["sha256"], f"lock.artifacts[{index}].sha256"),
            }
        )
    return result


def _validate_lock(raw: bytes) -> dict[str, Any]:
    if not 0 < len(raw) <= MAX_LOCK_BYTES:
        raise CapacityIsoError("capacity lock size exceeds its bound")
    parsed = _parse_json(raw, "capacity lock")
    if canonical_json(parsed) != raw:
        raise CapacityIsoError("capacity lock is not exact canonical JSON")
    lock = _expect_mapping(parsed, "lock")
    _exact_fields(
        lock,
        {"schema", "architecture", "volume_id", "default_entry", "artifacts", "entries", "grub"},
        "lock",
    )
    if lock["schema"] != LOCK_SCHEMA:
        raise CapacityIsoError(f"lock schema must be {LOCK_SCHEMA!r}")
    if lock["architecture"] != ARCHITECTURE:
        raise CapacityIsoError(f"lock architecture must be {ARCHITECTURE!r}")
    if lock["volume_id"] != VOLUME_ID:
        raise CapacityIsoError(f"lock volume_id must be {VOLUME_ID!r}")
    artifacts = _lock_artifacts(lock["artifacts"])
    entries, default_entry = _validate_entries(lock["entries"], artifacts, lock["default_entry"])
    grub = _expect_mapping(lock["grub"], "lock.grub")
    _exact_fields(grub, {"iso_path", "bytes", "sha256"}, "lock.grub")
    grub_path = _iso_path(grub["iso_path"], "lock.grub.iso_path")
    if grub_path != GRUB_ISO_PATH:
        raise CapacityIsoError(f"lock.grub.iso_path must be {GRUB_ISO_PATH}")
    normalized: dict[str, Any] = {
        "schema": LOCK_SCHEMA,
        "architecture": ARCHITECTURE,
        "volume_id": VOLUME_ID,
        "default_entry": default_entry,
        "artifacts": artifacts,
        "entries": entries,
        "grub": {
            "iso_path": grub_path,
            "bytes": _byte_count(grub["bytes"], "lock.grub.bytes", maximum=MAX_LOCK_BYTES),
            "sha256": _sha256_hex(grub["sha256"], "lock.grub.sha256"),
        },
    }
    if canonical_json(normalized) != raw:
        raise CapacityIsoError("capacity lock contains values outside normalized schema v1")
    return normalized


def _parse_iso(source: _DescriptorSource) -> tuple[dict[str, Any], dict[str, Any]]:
    primary: bytes | None = None
    boot_records: list[bytes] = []
    terminated = False
    for index in range(MAX_VOLUME_DESCRIPTORS):
        lba = 16 + index
        descriptor = source.read(
            lba * LOGICAL_BLOCK_SIZE,
            LOGICAL_BLOCK_SIZE,
            "ISO volume descriptor",
            maximum=LOGICAL_BLOCK_SIZE,
        )
        if descriptor[1:6] != b"CD001" or descriptor[6] != 1:
            raise CapacityIsoError("ISO volume descriptor has an invalid identifier or version")
        kind = descriptor[0]
        if kind == 0 and descriptor[7:39].rstrip(b"\x00 ") == EL_TORITO_SYSTEM_ID:
            boot_records.append(descriptor)
        elif kind == 1:
            if primary is not None:
                raise CapacityIsoError("ISO contains more than one primary volume descriptor")
            primary = descriptor
        elif kind == 255:
            terminated = True
            break
    if not terminated:
        raise CapacityIsoError("ISO descriptor sequence lacks a terminator")
    if primary is None:
        raise CapacityIsoError("ISO lacks a primary volume descriptor")
    if len(boot_records) != 1:
        raise CapacityIsoError(f"ISO has {len(boot_records)} El Torito boot records; expected one")
    try:
        volume_id = primary[40:72].decode("ascii").rstrip(" ")
    except UnicodeDecodeError as error:
        raise CapacityIsoError("ISO volume identifier is not ASCII") from error
    if volume_id != VOLUME_ID:
        raise CapacityIsoError(f"ISO volume identifier is {volume_id!r}, expected {VOLUME_ID!r}")
    volume_blocks = _u32_both(primary, 80, "ISO volume-space size")
    block_size = _u16_both(primary, 128, "ISO logical-block size")
    if block_size != LOGICAL_BLOCK_SIZE:
        raise CapacityIsoError(f"ISO logical-block size is {block_size}, expected 2048")
    volume_bytes = volume_blocks * block_size
    if volume_bytes != source.size:
        raise CapacityIsoError(
            f"ISO byte length {source.size} differs from volume-space length {volume_bytes}"
        )
    root_length = primary[156]
    if root_length < 34 or 156 + root_length > len(primary):
        raise CapacityIsoError("ISO primary descriptor has an invalid root record")
    root = _directory_record(primary[156 : 156 + root_length], volume_bytes, "/")
    if not int(root["flags"]) & 0x02 or root["name"] != b"\x00":
        raise CapacityIsoError("ISO root record is not the canonical root directory")

    catalog_lba = int.from_bytes(boot_records[0][71:75], "little")
    if catalog_lba <= 0:
        raise CapacityIsoError("El Torito boot catalog LBA is invalid")
    catalog = source.read(
        catalog_lba * LOGICAL_BLOCK_SIZE,
        LOGICAL_BLOCK_SIZE,
        "El Torito boot catalog",
        maximum=LOGICAL_BLOCK_SIZE,
    )
    uefi_entries = [entry for platform, entry in _el_torito_entries(catalog) if platform == EFI_PLATFORM_ID]
    if len(uefi_entries) != 1:
        raise CapacityIsoError(f"El Torito catalog has {len(uefi_entries)} UEFI entries; expected one")
    uefi = uefi_entries[0]
    if uefi[0] != 0x88 or uefi[1] != NO_EMULATION_MEDIA_TYPE:
        raise CapacityIsoError("El Torito UEFI entry is not bootable no-emulation media")
    load_sectors = int.from_bytes(uefi[6:8], "little")
    image_lba = int.from_bytes(uefi[8:12], "little")
    if load_sectors <= 0 or image_lba <= 0:
        raise CapacityIsoError("El Torito UEFI entry has invalid load geometry")
    image_offset = image_lba * LOGICAL_BLOCK_SIZE
    if image_offset >= volume_bytes:
        raise CapacityIsoError("El Torito EFI image starts outside the ISO")
    provisional = _Extent(source, image_offset, volume_bytes - image_offset, "El Torito EFI image")
    geometry = _fat_geometry(provisional)
    image = _Extent(source, image_offset, geometry["image_bytes"], "El Torito EFI image")
    if load_sectors * 512 > image.size:
        raise CapacityIsoError("El Torito load-sector count exceeds the EFI image")
    bootloader = _find_fat_bootloader(image, geometry)
    _validate_x86_64_efi_application(bootloader)
    iso = {
        "logical_block_size": block_size,
        "volume_blocks": volume_blocks,
        "volume_id": volume_id,
        "volume_bytes": volume_bytes,
        "boot_catalog_lba": catalog_lba,
        "el_torito_platform_id": EFI_PLATFORM_ID,
        "el_torito_media_type": NO_EMULATION_MEDIA_TYPE,
        "el_torito_load_sectors": load_sectors,
        "efi_boot_image_lba": image_lba,
        "efi_boot_image_bytes": image.size,
        "efi_boot_image_sha256": image.sha256(),
        "efi_bootloader_path": "/EFI/BOOT/BOOTX64.EFI",
        "efi_bootloader_bytes": len(bootloader),
        "efi_bootloader_sha256": hashlib.sha256(bootloader).hexdigest(),
    }
    return root, iso


def _artifact_extents(
    source: _DescriptorSource,
    root: dict[str, Any],
    volume_bytes: int,
    lock: dict[str, Any],
) -> list[dict[str, Any]]:
    inspected: list[dict[str, Any]] = []
    ranges: list[tuple[int, int, str]] = []
    extent_by_path: dict[str, _Extent] = {}
    for artifact in lock["artifacts"]:
        path = artifact["iso_path"]
        record = _find_iso_path(source, root, path, volume_bytes)
        size = int(record["size"])
        if size != artifact["bytes"]:
            raise CapacityIsoError(
                f"artifact {path} has {size} ISO bytes, lock requires {artifact['bytes']}"
            )
        start = int(record["offset"])
        end = start + size
        for prior_start, prior_end, prior_path in ranges:
            if start < prior_end and prior_start < end:
                raise CapacityIsoError(f"artifact extents overlap: {prior_path} and {path}")
        ranges.append((start, end, path))
        extent = _Extent(source, start, size, path)
        digest = extent.sha256()
        if digest != artifact["sha256"]:
            raise CapacityIsoError(f"artifact {path} SHA-256 differs from capacity lock")
        extent_by_path[path] = extent
        inspected.append(
            {
                "iso_path": path,
                "role": artifact["role"],
                "bytes": size,
                "sha256": digest,
                "extent_lba": int(record["extent_lba"]),
            }
        )
    for artifact in lock["artifacts"]:
        path = artifact["iso_path"]
        extent = extent_by_path[path]
        role = artifact["role"]
        if role == "ocore-kernel":
            _validate_x86_64_multiboot2_kernel(extent, path)
        elif role == "linux-kernel":
            _validate_linux_bzimage(extent, path)
        elif role == "guest-qcow2":
            _validate_qcow2(extent, path)
        elif role in {"guest-raw-cd", "guest-rootfs"}:
            _validate_raw_cd(extent, path)
    return inspected


def inspect_descriptor(
    descriptor: int, label: str = "pinned capacity ISO", maximum: int = MAX_ISO_BYTES
) -> dict[str, Any]:
    """Inspect an already-open capacity ISO without materializing large payloads."""

    try:
        before = os.fstat(descriptor)
    except OSError as error:
        raise CapacityIsoError(f"cannot stat {label}: {error}") from error
    if not stat.S_ISREG(before.st_mode):
        raise CapacityIsoError(f"{label} descriptor is not a regular file")
    admitted_maximum = min(maximum, MAX_ISO_BYTES)
    if before.st_size < MIN_ISO_BYTES or before.st_size > admitted_maximum:
        raise CapacityIsoError(f"{label} size outside {MIN_ISO_BYTES}..{admitted_maximum} bytes")
    if before.st_size % LOGICAL_BLOCK_SIZE:
        raise CapacityIsoError("ISO length is not a multiple of 2048 bytes")
    source = _DescriptorSource(descriptor, before.st_size, label)
    root, iso = _parse_iso(source)
    lock_record = _find_iso_path(source, root, LOCK_ISO_PATH, iso["volume_bytes"])
    lock_size = int(lock_record["size"])
    if not 0 < lock_size <= MAX_LOCK_BYTES:
        raise CapacityIsoError("embedded capacity lock exceeds its size bound")
    lock_raw = source.read(int(lock_record["offset"]), lock_size, LOCK_ISO_PATH, maximum=MAX_LOCK_BYTES)
    lock = _validate_lock(lock_raw)

    config_record = _find_iso_path(source, root, GRUB_ISO_PATH, iso["volume_bytes"])
    config_size = int(config_record["size"])
    if config_size != lock["grub"]["bytes"]:
        raise CapacityIsoError("GRUB config byte count differs from capacity lock")
    if config_size > MAX_LOCK_BYTES:
        raise CapacityIsoError("GRUB config exceeds its size bound")
    config = source.read(int(config_record["offset"]), config_size, GRUB_ISO_PATH, maximum=MAX_LOCK_BYTES)
    config_digest = hashlib.sha256(config).hexdigest()
    if config_digest != lock["grub"]["sha256"]:
        raise CapacityIsoError("GRUB config SHA-256 differs from capacity lock")
    expected_config = render_grub(lock["entries"], lock["default_entry"])
    if config != expected_config:
        raise CapacityIsoError("GRUB config does not exactly correspond to typed lock entries")

    artifacts = _artifact_extents(source, root, iso["volume_bytes"], lock)
    image_digest = source.hash_range(0, source.size, label)
    _require_descriptor_identity(descriptor, before, label)
    return {
        "schema": INSPECT_SCHEMA,
        "architecture": ARCHITECTURE,
        "bytes": source.size,
        "sha256": image_digest,
        **{key: value for key, value in iso.items() if key != "volume_bytes"},
        "capacity_lock_path": LOCK_ISO_PATH,
        "capacity_lock_bytes": lock_size,
        "capacity_lock_sha256": hashlib.sha256(lock_raw).hexdigest(),
        "grub_config_path": GRUB_ISO_PATH,
        "grub_config_bytes": config_size,
        "grub_config_sha256": config_digest,
        "default_entry": lock["default_entry"],
        "entries": lock["entries"],
        "artifacts": artifacts,
    }


def inspect_path(path: Path, *, require_readonly: bool = False) -> dict[str, Any]:
    path = Path(os.path.abspath(path))
    descriptor = _open_pinned_regular(path, "capacity ISO", readonly=require_readonly)
    try:
        before = os.fstat(descriptor)
        result = inspect_descriptor(descriptor, str(path))
        _require_descriptor_identity(descriptor, before, "capacity ISO")
        _require_path_identity(path, before, "capacity ISO")
        return result
    finally:
        os.close(descriptor)


def _write_all(descriptor: int, data: bytes) -> None:
    offset = 0
    while offset < len(data):
        try:
            written = os.write(descriptor, data[offset:])
        except OSError as error:
            raise CapacityIsoError(f"cannot write private ISO output: {error}") from error
        if written <= 0:
            raise CapacityIsoError("private ISO output stopped accepting bytes")
        offset += written


def _copy_descriptor(source: int, output: int, size: int) -> str:
    digest = hashlib.sha256()
    offset = 0
    try:
        while offset < size:
            chunk = os.pread(source, min(STREAM_CHUNK_BYTES, size - offset), offset)
            if not chunk:
                raise CapacityIsoError("ISO source ended before its admitted publication size")
            _write_all(output, chunk)
            digest.update(chunk)
            offset += len(chunk)
        if os.pread(source, 1, size):
            raise CapacityIsoError("ISO source grew beyond its admitted publication size")
    except OSError as error:
        raise CapacityIsoError(f"cannot stream ISO publication: {error}") from error
    return digest.hexdigest()


def publish_path(source: Path, output: Path) -> dict[str, Any]:
    """Validate, stream-copy, and atomically no-clobber publish one read-only ISO."""

    source = Path(os.path.abspath(source))
    output = Path(os.path.abspath(output))
    if source == output:
        raise CapacityIsoError("capacity ISO source and output must be distinct")
    source_descriptor = _open_pinned_regular(source, "capacity ISO source")
    try:
        source_state = os.fstat(source_descriptor)
        metadata = inspect_descriptor(source_descriptor, str(source))
        _require_descriptor_identity(source_descriptor, source_state, "capacity ISO source")
        _require_path_identity(source, source_state, "capacity ISO source")
        parent = output.parent
        try:
            parent_state = os.stat(parent, follow_symlinks=False)
        except OSError as error:
            raise CapacityIsoError(f"capacity ISO output parent is unavailable: {parent}: {error}") from error
        if stat.S_ISLNK(parent_state.st_mode) or not stat.S_ISDIR(parent_state.st_mode):
            raise CapacityIsoError("capacity ISO output parent is not a non-symlink directory")
        _reject_output(output, allow_regular=False)
        temporary_descriptor = -1
        temporary = ""
        linked = False
        try:
            temporary_descriptor, temporary = tempfile.mkstemp(
                prefix=".ostadix-capacity-iso.", suffix=".tmp", dir=parent
            )
            copied = _copy_descriptor(source_descriptor, temporary_descriptor, source_state.st_size)
            if copied != metadata["sha256"]:
                raise CapacityIsoError("published copy digest differs from inspected source")
            _require_descriptor_identity(source_descriptor, source_state, "capacity ISO source")
            _require_path_identity(source, source_state, "capacity ISO source")
            os.fchmod(temporary_descriptor, 0o444)
            os.fsync(temporary_descriptor)
            private = inspect_descriptor(temporary_descriptor, "private capacity ISO output")
            if private != metadata:
                raise CapacityIsoError("private capacity ISO output differs from inspected source")
            os.close(temporary_descriptor)
            temporary_descriptor = -1
            _reject_output(output, allow_regular=False)
            try:
                os.link(temporary, output, follow_symlinks=False)
            except FileExistsError as error:
                raise CapacityIsoError(f"refusing to clobber existing output: {output}") from error
            linked = True
            os.unlink(temporary)
            temporary = ""
            try:
                parent_descriptor = os.open(parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
                try:
                    os.fsync(parent_descriptor)
                finally:
                    os.close(parent_descriptor)
            except OSError:
                pass
        except OSError as error:
            raise CapacityIsoError(f"cannot atomically publish capacity ISO: {error}") from error
        finally:
            if temporary_descriptor >= 0:
                os.close(temporary_descriptor)
            if temporary:
                try:
                    os.unlink(temporary)
                except FileNotFoundError:
                    pass
            if linked and not output.exists():
                raise CapacityIsoError("capacity ISO publication link disappeared")
    finally:
        os.close(source_descriptor)
    published = inspect_path(output, require_readonly=True)
    if published != metadata:
        raise CapacityIsoError("published capacity ISO differs from inspected source")
    return published


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Create, inspect, or atomically publish an OSTADIX absorbed-capacity ISO"
    )
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create-lock", help="create lock/config in a private stage")
    create.add_argument("--stage", required=True, type=Path)
    create.add_argument("--profile", required=True, type=Path)
    inspect = commands.add_parser("inspect", help="strictly inspect one capacity ISO")
    inspect.add_argument("path", type=Path)
    publish = commands.add_parser("publish", help="validate and no-clobber publish one capacity ISO")
    publish.add_argument("--source", required=True, type=Path)
    publish.add_argument("--output", required=True, type=Path)
    return parser


def main(argv: list[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        if arguments.command == "create-lock":
            result = create_lock(arguments.stage, arguments.profile)
        elif arguments.command == "inspect":
            result = inspect_path(arguments.path)
        else:
            result = publish_path(arguments.source, arguments.output)
    except CapacityIsoError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
