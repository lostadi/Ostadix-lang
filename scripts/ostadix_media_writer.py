#!/usr/bin/env python3
"""Confirmation-gated writer for validated OSTADIX boot-media images."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import fcntl
import hashlib
import importlib.util
import json
import mmap
import os
from pathlib import Path
import plistlib
import platform
import re
import stat
import struct
import subprocess
import sys
import tempfile
from typing import Any, BinaryIO, Iterator
import uuid


# Exact maximum outer-container size admitted by ostadix.boot-media/v1:
# 512 MiB ESP plus the fixed 1 MiB head and 1 MiB tail geometry.
MAX_IMAGE_BYTES = 538_968_064
COPY_CHUNK = 4 * 1024 * 1024
BOOT_MEDIA_TOOL = Path(__file__).with_name("ostadix_boot_media.py")
TOKEN_DOMAIN = b"OSTADIX/MEDIA-WRITE-CONFIRM/V2\0"
WRITE_SCHEMA = "ostadix.media-write/v2"
LINUX_BLKGETSIZE64 = 0x80081272
DARWIN_DKIOCGETBLOCKSIZE = 0x40046418
DARWIN_DKIOCGETBLOCKCOUNT = 0x40086419


class WriterError(RuntimeError):
    pass


_BOOT_MEDIA_MODULE: Any | None = None


def _boot_media_module() -> Any:
    global _BOOT_MEDIA_MODULE
    if _BOOT_MEDIA_MODULE is not None:
        return _BOOT_MEDIA_MODULE
    if not BOOT_MEDIA_TOOL.is_file():
        raise WriterError(f"boot-media planner is missing: {BOOT_MEDIA_TOOL}")
    name = "_ostadix_boot_media_for_writer"
    specification = importlib.util.spec_from_file_location(name, BOOT_MEDIA_TOOL)
    if specification is None or specification.loader is None:
        raise WriterError(f"cannot load boot-media planner: {BOOT_MEDIA_TOOL}")
    module = importlib.util.module_from_spec(specification)
    sys.modules[name] = module
    try:
        specification.loader.exec_module(module)
    except Exception as error:
        sys.modules.pop(name, None)
        raise WriterError(f"cannot initialize boot-media planner: {error}") from error
    _BOOT_MEDIA_MODULE = module
    return module


@dataclass(frozen=True)
class DeviceInfo:
    path: str
    raw_path: str
    identity: str
    bytes: int
    model: str
    transport: str
    platform: str
    rdev: int

    def public(self) -> dict[str, object]:
        return {
            "path": self.path,
            "identity": self.identity,
            "bytes": self.bytes,
            "model": self.model,
            "transport": self.transport,
            "platform": self.platform,
            "device_number": f"{os.major(self.rdev)}:{os.minor(self.rdev)}",
        }


@dataclass(frozen=True)
class SourceIdentity:
    device: int
    inode: int
    bytes: int
    modified_ns: int
    changed_ns: int

    @classmethod
    def from_stat(cls, value: os.stat_result) -> "SourceIdentity":
        return cls(
            device=value.st_dev,
            inode=value.st_ino,
            bytes=value.st_size,
            modified_ns=value.st_mtime_ns,
            changed_ns=value.st_ctime_ns,
        )


@dataclass
class SourceSnapshot:
    """One validated, read-only image snapshot held through device mutation."""

    origin: Path
    stream: BinaryIO
    sha256: str
    bytes: int
    identity: SourceIdentity

    def assert_origin_unchanged(self) -> None:
        """Reject source growth, replacement, or content change before writing."""
        digest, identity = _hash_origin(self.origin, expected=self.identity)
        if digest != self.sha256 or identity != self.identity:
            raise WriterError("source image changed after its immutable snapshot was admitted")


@dataclass
class OpenMutationTarget:
    """One kernel object held from final admission through readback."""

    stream: BinaryIO
    rdev: int
    bytes: int


def _fd_capacity(descriptor: int, platform_name: str) -> int:
    try:
        if platform_name == "linux":
            raw = bytearray(8)
            fcntl.ioctl(descriptor, LINUX_BLKGETSIZE64, raw, True)
            capacity = struct.unpack("=Q", raw)[0]
        elif platform_name == "macos":
            raw_block_size = bytearray(4)
            raw_block_count = bytearray(8)
            fcntl.ioctl(descriptor, DARWIN_DKIOCGETBLOCKSIZE, raw_block_size, True)
            fcntl.ioctl(descriptor, DARWIN_DKIOCGETBLOCKCOUNT, raw_block_count, True)
            block_size = struct.unpack("=I", raw_block_size)[0]
            block_count = struct.unpack("=Q", raw_block_count)[0]
            capacity = block_size * block_count
        else:
            raise WriterError(f"held-device capacity is unsupported on {platform_name}")
    except OSError as error:
        raise WriterError(f"cannot derive capacity from held device descriptor: {error}") from error
    if capacity <= 0:
        raise WriterError("held device descriptor reported a non-positive capacity")
    return capacity


@contextmanager
def _open_mutation_target(device: DeviceInfo) -> Iterator[OpenMutationTarget]:
    flags = os.O_RDWR | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    if device.platform == "linux":
        if not hasattr(os, "O_EXCL"):
            raise WriterError("exclusive Linux device opening is unavailable")
        flags |= getattr(os, "O_EXCL")
    elif device.platform == "macos":
        if not hasattr(os, "O_EXLOCK"):
            raise WriterError("exclusive macOS device opening is unavailable")
        flags |= getattr(os, "O_EXLOCK") | getattr(os, "O_NONBLOCK", 0)
    try:
        descriptor = os.open(device.raw_path, flags)
    except PermissionError as error:
        raise WriterError(
            "permission denied opening target exclusively; rerun the exact confirmed "
            "command with appropriate local privilege"
        ) from error
    except OSError as error:
        raise WriterError(
            f"cannot open target once without following links and with exclusive access: {error}"
        ) from error
    transferred = False
    try:
        if device.platform == "macos" and hasattr(os, "O_EXLOCK"):
            current_flags = fcntl.fcntl(descriptor, fcntl.F_GETFL)
            fcntl.fcntl(
                descriptor,
                fcntl.F_SETFL,
                current_flags & ~getattr(os, "O_NONBLOCK", 0),
            )
        value = os.fstat(descriptor)
        expected_kind = (
            stat.S_ISCHR(value.st_mode)
            if device.platform == "macos"
            else stat.S_ISBLK(value.st_mode)
        )
        if not expected_kind:
            raise WriterError("opened mutation target is not the expected device-node type")
        if value.st_rdev != device.rdev:
            raise WriterError("opened mutation target has a different device identity")
        capacity = _fd_capacity(descriptor, device.platform)
        if capacity != device.bytes:
            raise WriterError(
                "opened mutation target capacity differs from admitted device capacity"
            )
        stream = os.fdopen(descriptor, "r+b", buffering=0)
        transferred = True
        with stream:
            yield OpenMutationTarget(stream=stream, rdev=value.st_rdev, bytes=capacity)
    finally:
        if not transferred:
            try:
                os.close(descriptor)
            except OSError:
                pass


def _run(command: list[str]) -> bytes:
    try:
        result = subprocess.run(command, check=False, capture_output=True)
    except OSError as error:
        raise WriterError(f"cannot execute {command[0]}: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise WriterError(f"{' '.join(command)} failed: {detail or result.returncode}")
    return result.stdout


def _open_origin(path: Path) -> BinaryIO:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise WriterError(
            f"cannot open source image without following links: {path}: {error}"
        ) from error
    return os.fdopen(descriptor, "rb")


def _checked_identity(value: os.stat_result, path: Path) -> SourceIdentity:
    if not stat.S_ISREG(value.st_mode):
        raise WriterError(f"not a regular image file: {path}")
    if value.st_size <= 0 or value.st_size > MAX_IMAGE_BYTES:
        raise WriterError(f"image size outside 1..{MAX_IMAGE_BYTES} bytes: {path}")
    return SourceIdentity.from_stat(value)


def _same_path_identity(path: Path, expected: SourceIdentity) -> None:
    try:
        current = os.stat(path, follow_symlinks=False)
    except OSError as error:
        raise WriterError(
            f"source image path disappeared or became unreadable: {path}: {error}"
        ) from error
    if not stat.S_ISREG(current.st_mode) or SourceIdentity.from_stat(current) != expected:
        raise WriterError("source image was replaced, resized, or modified during admission")


def _hash_exact(stream: BinaryIO, size: int, *, label: str) -> str:
    digest = hashlib.sha256()
    remaining = size
    while remaining:
        chunk = stream.read(min(COPY_CHUNK, remaining))
        if not chunk:
            raise WriterError(f"{label} ended before its admitted byte count")
        digest.update(chunk)
        remaining -= len(chunk)
    if stream.read(1):
        raise WriterError(f"{label} contains trailing bytes beyond its admitted byte count")
    return digest.hexdigest()


def _hash_origin(
    path: Path, *, expected: SourceIdentity | None = None
) -> tuple[str, SourceIdentity]:
    with _open_origin(path) as source:
        before = _checked_identity(os.fstat(source.fileno()), path)
        if expected is not None and before != expected:
            raise WriterError("source image was replaced, resized, or modified after admission")
        digest = _hash_exact(source, before.bytes, label="source image")
        after = _checked_identity(os.fstat(source.fileno()), path)
    if after != before:
        raise WriterError("source image changed while it was being verified")
    _same_path_identity(path, before)
    return digest, before


def _inspect_boot_media_snapshot(path: Path, digest: str, size: int) -> None:
    if not BOOT_MEDIA_TOOL.is_file():
        raise WriterError(f"boot-media inspector is missing: {BOOT_MEDIA_TOOL}")
    raw = _run([sys.executable, str(BOOT_MEDIA_TOOL), "inspect", str(path)])
    try:
        metadata = json.loads(raw)
    except json.JSONDecodeError as error:
        raise WriterError(f"boot-media inspector returned invalid JSON: {error}") from error
    if metadata.get("schema") != "ostadix.boot-media/v1":
        raise WriterError("image is not bounded OSTADIX boot-media v1")
    if metadata.get("sha256") != digest or metadata.get("bytes") != size:
        raise WriterError("immutable image snapshot disagrees with boot-media inspection")


def _target_plan(snapshot: SourceSnapshot, target_bytes: int) -> Any:
    """Plan against the held immutable source without target-sized allocation."""
    module = _boot_media_module()
    try:
        try:
            descriptor = snapshot.stream.fileno()
        except (AttributeError, OSError):
            snapshot.stream.seek(0)
            source: object = snapshot.stream.read(snapshot.bytes)
            if len(source) != snapshot.bytes or snapshot.stream.read(1):
                raise WriterError("held source snapshot changed during target planning")
            plan = module.plan_target_image(source, target_bytes)
        else:
            with mmap.mmap(descriptor, 0, access=mmap.ACCESS_READ) as source_map:
                plan = module.plan_target_image(source_map, target_bytes)
    except module.MediaError as error:
        raise WriterError(str(error)) from error
    if plan.source_sha256 != snapshot.sha256 or plan.source_bytes != snapshot.bytes:
        raise WriterError("target plan disagrees with the admitted source snapshot")
    if plan.target_bytes != target_bytes:
        raise WriterError("target planner returned a different capacity")
    return plan


@contextmanager
def _validated_source_snapshot(path: Path) -> Iterator[SourceSnapshot]:
    """Capture and validate one private snapshot before any device mutation.

    The source pathname is opened without following symlinks. Exactly its
    initial regular-file length is copied into a private file; early EOF,
    growth, replacement, or metadata/content change fails closed. All later
    writes consume the held read-only descriptor, never the original path.
    """
    origin = Path(os.path.abspath(os.fspath(path)))
    with tempfile.TemporaryDirectory(prefix="ostadix-media-source-") as directory:
        snapshot_path = Path(directory) / "admitted.img"
        try:
            with _open_origin(origin) as source, snapshot_path.open("x+b") as target:
                before = _checked_identity(os.fstat(source.fileno()), origin)
                digest = hashlib.sha256()
                remaining = before.bytes
                while remaining:
                    chunk = source.read(min(COPY_CHUNK, remaining))
                    if not chunk:
                        raise WriterError(
                            "source image ended while its snapshot was captured"
                        )
                    _write_all(target, chunk)
                    digest.update(chunk)
                    remaining -= len(chunk)
                if source.read(1):
                    raise WriterError("source image grew while its snapshot was captured")
                after = _checked_identity(os.fstat(source.fileno()), origin)
                if after != before:
                    raise WriterError("source image changed while its snapshot was captured")
                _same_path_identity(origin, before)
                target.flush()
                os.fsync(target.fileno())
            os.chmod(snapshot_path, 0o400)
        except PermissionError as error:
            raise WriterError(
                f"permission denied while capturing source image: {origin}"
            ) from error
        except OSError as error:
            raise WriterError(
                f"cannot capture immutable source image snapshot: {error}"
            ) from error

        captured_digest = digest.hexdigest()
        _inspect_boot_media_snapshot(snapshot_path, captured_digest, before.bytes)
        try:
            with snapshot_path.open("rb") as verification_stream:
                verified_digest = _hash_exact(
                    verification_stream,
                    before.bytes,
                    label="immutable image snapshot",
                )
        except OSError as error:
            raise WriterError(
                f"cannot verify immutable source image snapshot: {error}"
            ) from error
        if verified_digest != captured_digest:
            raise WriterError("immutable image snapshot changed during validation")

        try:
            held_stream = snapshot_path.open("rb")
        except OSError as error:
            raise WriterError(
                f"cannot hold immutable source image snapshot: {error}"
            ) from error
        with held_stream:
            snapshot = SourceSnapshot(
                origin=origin,
                stream=held_stream,
                sha256=captured_digest,
                bytes=before.bytes,
                identity=before,
            )
            snapshot.assert_origin_unchanged()
            yield snapshot


def _device_node_rdev(path: str, *, platform_name: str) -> int:
    try:
        value = os.stat(path, follow_symlinks=False)
    except OSError as error:
        raise WriterError(f"cannot inspect device node {path}: {error}") from error
    expected = stat.S_ISCHR(value.st_mode) if platform_name == "macos" else stat.S_ISBLK(
        value.st_mode
    )
    if not expected:
        kind = "character" if platform_name == "macos" else "block"
        raise WriterError(f"target node is not a {kind} device: {path}")
    return value.st_rdev


def _macos_stable_identity(info: dict[str, Any]) -> str:
    for key in ("SerialNumber", "DeviceSerialNumber", "MediaUUID"):
        value = str(info.get(key) or "").strip()
        if value:
            return f"{key.lower()}:{value}"
    raise WriterError(
        "diskutil did not provide a stable device serial or media UUID; "
        "USB DeviceTreePath is port topology and is not accepted as device identity"
    )


def _macos_device(path: str) -> DeviceInfo:
    if not re.fullmatch(r"/dev/(?:r)?disk[0-9]+", path):
        raise WriterError("macOS target must be a whole /dev/diskN or /dev/rdiskN device")
    canonical = "/dev/" + Path(path).name.removeprefix("r")
    info = plistlib.loads(_run(["diskutil", "info", "-plist", canonical]))
    if not info.get("Whole"):
        raise WriterError("target is not a whole disk")
    if info.get("Internal") is not False:
        raise WriterError("target must be reported as an external disk")
    if info.get("Writable") is not True or info.get("ReadOnlyMedia") is True:
        raise WriterError("target is not writable")
    identifier = str(info.get("DeviceIdentifier", ""))
    size = int(info.get("TotalSize", 0))
    if not identifier or size <= 0:
        raise WriterError("diskutil did not provide a device identifier and size")
    identity = _macos_stable_identity(info)
    root = plistlib.loads(_run(["diskutil", "info", "-plist", "/"]))
    if root.get("ParentWholeDisk") == identifier or root.get("PartOfWhole") == identifier:
        raise WriterError("refusing the disk that contains the active root filesystem")
    raw_path = f"/dev/r{identifier}"
    return DeviceInfo(
        path=f"/dev/{identifier}",
        raw_path=raw_path,
        identity=identity,
        bytes=size,
        model=str(info.get("MediaName") or info.get("DeviceModel") or "unknown"),
        transport=str(info.get("BusProtocol") or "unknown"),
        platform="macos",
        rdev=_device_node_rdev(raw_path, platform_name="macos"),
    )


def _linux_inventory() -> dict[str, Any]:
    raw = _run(
        [
            "lsblk",
            "--json",
            "--bytes",
            "--output",
            "NAME,PATH,TYPE,SIZE,RO,RM,MODEL,SERIAL,WWN,MOUNTPOINTS,PKNAME,TRAN",
        ]
    )
    try:
        return json.loads(raw)
    except json.JSONDecodeError as error:
        raise WriterError(f"lsblk returned invalid JSON: {error}") from error


def _flatten_linux(nodes: list[dict[str, Any]]) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    for node in nodes:
        result.append(node)
        result.extend(_flatten_linux(node.get("children") or []))
    return result


def _linux_flag_is_true(value: Any) -> bool:
    if value is True or value == 1:
        return True
    if isinstance(value, str):
        return value.strip().lower() in {"1", "true", "yes"}
    return False


def _linux_has_mountpoint(node: dict[str, Any]) -> bool:
    values = node.get("mountpoints")
    if values is None:
        return False
    if not isinstance(values, list):
        values = [values]
    return any(value is not None and str(value).strip() for value in values)


def _linux_device(path: str) -> DeviceInfo:
    if not re.fullmatch(r"/dev/[A-Za-z0-9._+-]+", path):
        raise WriterError("Linux target must be one exact whole /dev device")
    inventory = _linux_inventory()
    nodes = _flatten_linux(inventory.get("blockdevices") or [])
    matches = [node for node in nodes if node.get("path") == path]
    if len(matches) != 1:
        raise WriterError("target is absent or ambiguous in lsblk inventory")
    node = matches[0]
    if node.get("type") != "disk" or _linux_flag_is_true(node.get("ro")):
        raise WriterError("target must be one writable whole disk")
    nodes_by_name = {
        str(candidate.get("name")): candidate
        for candidate in nodes
        if candidate.get("name")
    }

    def descends_from_disk(candidate: dict[str, Any]) -> bool:
        parent = str(candidate.get("pkname") or "")
        visited: set[str] = set()
        while parent and parent not in visited:
            if parent == node.get("name"):
                return True
            visited.add(parent)
            parent_node = nodes_by_name.get(parent)
            if parent_node is None:
                return False
            parent = str(parent_node.get("pkname") or "")
        return False

    for candidate in nodes:
        if (candidate is node or descends_from_disk(candidate)) and _linux_has_mountpoint(
            candidate
        ):
            raise WriterError(
                "target or one of its descendants is mounted; unmount it first"
            )
    root_source = _run(["findmnt", "--noheadings", "--output", "SOURCE", "/"]).decode().strip()
    root_nodes = [candidate for candidate in nodes if candidate.get("path") == root_source]
    if root_nodes:
        root = root_nodes[0]
        if root is node or descends_from_disk(root):
            raise WriterError("refusing the disk that contains the active root filesystem")
    transport = str(node.get("tran") or "unknown").strip().lower()
    if not _linux_flag_is_true(node.get("rm")) and transport != "usb":
        raise WriterError(
            "target must be reported removable (RM=true) or use external USB transport"
        )
    size = int(node.get("size") or 0)
    if size <= 0:
        raise WriterError("lsblk did not provide a positive device size")
    serial = str(node.get("serial") or "").strip()
    wwn = str(node.get("wwn") or "").strip()
    identity = f"serial:{serial}" if serial else (f"wwn:{wwn}" if wwn else "")
    if not identity:
        raise WriterError("lsblk did not provide a stable SERIAL or WWN device identity")
    return DeviceInfo(
        path=path,
        raw_path=path,
        identity=identity,
        bytes=size,
        model=str(node.get("model") or "unknown").strip(),
        transport=transport,
        platform="linux",
        rdev=_device_node_rdev(path, platform_name="linux"),
    )


def inspect_device(path: str, system: str | None = None) -> DeviceInfo:
    system = system or platform.system()
    if system == "Darwin":
        return _macos_device(path)
    if system == "Linux":
        return _linux_device(path)
    raise WriterError(f"physical media writing is unsupported on {system}")


def _require_exact_keys(value: dict[str, object], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        raise WriterError(f"{label} keys differ (missing={missing}, extra={extra})")


def _require_int(value: object, label: str) -> int:
    if type(value) is not int:
        raise WriterError(f"{label} must be an integer")
    return value


def _require_string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise WriterError(f"{label} must be a non-empty string")
    return value


def _require_sha256(value: object, label: str) -> str:
    result = _require_string(value, label)
    if not re.fullmatch(r"[0-9a-f]{64}", result):
        raise WriterError(f"{label} must be one lowercase SHA-256")
    return result


def confirmation_token_from_public(
    device: dict[str, object], target_plan: dict[str, object]
) -> str:
    """Validate canonical public evidence and recompute its confirmation token."""
    device_keys = {
        "path",
        "identity",
        "bytes",
        "model",
        "transport",
        "platform",
        "device_number",
    }
    _require_exact_keys(device, device_keys, "public device")
    platform_name = _require_string(device["platform"], "device.platform")
    if platform_name not in {"linux", "macos"}:
        raise WriterError("device.platform must be linux or macos")
    device_path = _require_string(device["path"], "device.path")
    if not device_path.startswith("/dev/"):
        raise WriterError("device.path must be an absolute /dev path")
    for field in ("identity", "model", "transport"):
        _require_string(device[field], f"device.{field}")
    if not re.fullmatch(r"[0-9]+:[0-9]+", _require_string(
        device["device_number"], "device.device_number"
    )):
        raise WriterError("device.device_number must use major:minor form")
    device_bytes = _require_int(device["bytes"], "device.bytes")

    plan_keys = {
        "schema",
        "source_sha256",
        "source_bytes",
        "target_bytes",
        "target_plan_sha256",
        "target_image_sha256",
        "disk_guid",
        "partition_guid",
        "esp_sha256",
        "esp_bytes",
        "esp_first_lba",
        "esp_last_lba",
        "sector_size",
        "target_last_usable_lba",
        "target_backup_entries_lba",
        "target_backup_header_lba",
        "extents",
        "unwritten_policy",
        "unwritten_ranges",
    }
    _require_exact_keys(target_plan, plan_keys, "public target plan")
    module = _boot_media_module()
    if target_plan["schema"] != module.TARGET_PLAN_SCHEMA:
        raise WriterError("target plan schema is unsupported")
    if target_plan["unwritten_policy"] != module.UNWRITTEN_POLICY:
        raise WriterError("target plan unwritten-byte policy is unsupported")
    _require_sha256(target_plan["source_sha256"], "target_plan.source_sha256")
    _require_sha256(target_plan["esp_sha256"], "target_plan.esp_sha256")
    plan_sha256 = _require_sha256(
        target_plan["target_plan_sha256"], "target_plan.target_plan_sha256"
    )
    target_image_sha256 = target_plan["target_image_sha256"]
    if target_image_sha256 is not None:
        _require_sha256(target_image_sha256, "target_plan.target_image_sha256")
    source_bytes = _require_int(target_plan["source_bytes"], "target_plan.source_bytes")
    target_bytes = _require_int(target_plan["target_bytes"], "target_plan.target_bytes")
    sector_size = _require_int(target_plan["sector_size"], "target_plan.sector_size")
    if sector_size != module.SECTOR_SIZE:
        raise WriterError("target plan sector size is unsupported")
    if (
        source_bytes <= 0
        or target_bytes < source_bytes
        or target_bytes > module.MAX_TARGET_BYTES
        or target_bytes % sector_size != 0
        or device_bytes != target_bytes
    ):
        raise WriterError("device and target-plan capacities are inconsistent")
    for field in ("disk_guid", "partition_guid"):
        try:
            uuid.UUID(_require_string(target_plan[field], f"target_plan.{field}"))
        except ValueError as error:
            raise WriterError(f"target_plan.{field} is not a UUID") from error
    esp_bytes = _require_int(target_plan["esp_bytes"], "target_plan.esp_bytes")
    esp_first = _require_int(target_plan["esp_first_lba"], "target_plan.esp_first_lba")
    esp_last = _require_int(target_plan["esp_last_lba"], "target_plan.esp_last_lba")
    backup_header = _require_int(
        target_plan["target_backup_header_lba"],
        "target_plan.target_backup_header_lba",
    )
    backup_entries = _require_int(
        target_plan["target_backup_entries_lba"],
        "target_plan.target_backup_entries_lba",
    )
    last_usable = _require_int(
        target_plan["target_last_usable_lba"],
        "target_plan.target_last_usable_lba",
    )
    if (
        esp_first != module.ESP_START_LBA
        or esp_bytes != (esp_last - esp_first + 1) * sector_size
        or backup_header != target_bytes // sector_size - 1
        or backup_entries != backup_header - module.PARTITION_TABLE_SECTORS
        or last_usable != backup_entries - 1
        or esp_last > last_usable
    ):
        raise WriterError("target plan GPT/ESP geometry is inconsistent")

    raw_extents = target_plan["extents"]
    if not isinstance(raw_extents, list) or not raw_extents:
        raise WriterError("target_plan.extents must be a non-empty list")
    cursor = 0
    computed_unwritten: list[dict[str, int]] = []
    kinds: list[str] = []
    for index, raw_extent in enumerate(raw_extents):
        if not isinstance(raw_extent, dict):
            raise WriterError(f"target_plan.extents[{index}] must be an object")
        source_backed = "source_offset" in raw_extent
        extent_keys = {"kind", "target_offset", "bytes", "sha256"}
        extent_keys.add("source_offset" if source_backed else "generated")
        _require_exact_keys(raw_extent, extent_keys, f"target_plan.extents[{index}]")
        kind = _require_string(raw_extent["kind"], f"target_plan.extents[{index}].kind")
        offset = _require_int(
            raw_extent["target_offset"], f"target_plan.extents[{index}].target_offset"
        )
        size = _require_int(raw_extent["bytes"], f"target_plan.extents[{index}].bytes")
        _require_sha256(raw_extent["sha256"], f"target_plan.extents[{index}].sha256")
        if offset < cursor or size <= 0 or offset % sector_size or size % sector_size:
            raise WriterError("target plan extents overlap or are not sector aligned")
        if offset > cursor:
            computed_unwritten.append({"offset": cursor, "bytes": offset - cursor})
        if offset + size > target_bytes:
            raise WriterError("target plan extent exceeds target capacity")
        if source_backed:
            source_offset = _require_int(
                raw_extent["source_offset"],
                f"target_plan.extents[{index}].source_offset",
            )
            if source_offset < 0 or source_offset + size > source_bytes:
                raise WriterError("source-backed target extent exceeds source image")
        elif raw_extent["generated"] is not True:
            raise WriterError("generated target extent marker must be true")
        cursor = offset + size
        kinds.append(kind)
    if cursor < target_bytes:
        computed_unwritten.append({"offset": cursor, "bytes": target_bytes - cursor})
    if kinds[0] != "primary-gpt" or kinds[-1] != "target-backup-gpt":
        raise WriterError("target plan is missing authoritative GPT extents")
    raw_unwritten = target_plan["unwritten_ranges"]
    if raw_unwritten != computed_unwritten:
        raise WriterError("target plan unwritten ranges are incomplete or non-canonical")
    if raw_unwritten and target_image_sha256 is not None:
        raise WriterError("sparse target plan cannot claim a whole-target image SHA-256")

    commitment = dict(target_plan)
    commitment.pop("target_plan_sha256")
    if module._target_plan_commitment(commitment) != plan_sha256:
        raise WriterError("target plan SHA-256 does not match its canonical content")
    payload = json.dumps(
        {"device": device, "target_plan": target_plan},
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    suffix = hashlib.sha256(TOKEN_DOMAIN + payload).hexdigest()[:32].upper()
    return f"OSTADIX-WRITE-{suffix}"


def confirmation_token(device: DeviceInfo, plan: Any) -> str:
    return confirmation_token_from_public(device.public(), plan.public())


def prepare(image: Path, device_path: str) -> tuple[DeviceInfo, Any, str]:
    with _validated_source_snapshot(image) as snapshot:
        device = inspect_device(device_path)
        plan = _target_plan(snapshot, device.bytes)
        return (
            device,
            plan,
            confirmation_token(device, plan),
        )


def _write_all(target: BinaryIO, chunk: bytes) -> None:
    view = memoryview(chunk)
    while view:
        written = target.write(view)
        if written is None or written <= 0:
            raise WriterError("target stopped accepting bytes before the image was complete")
        view = view[written:]


def _hash_source_range(snapshot: SourceSnapshot, offset: int, size: int) -> str:
    snapshot.stream.seek(offset)
    digest = hashlib.sha256()
    remaining = size
    while remaining:
        chunk = snapshot.stream.read(min(COPY_CHUNK, remaining))
        if not chunk:
            raise WriterError("held source snapshot ended inside a planned extent")
        digest.update(chunk)
        remaining -= len(chunk)
    return digest.hexdigest()


def _extent_chunks(snapshot: SourceSnapshot, extent: Any) -> Iterator[bytes]:
    if extent.source_offset is not None:
        snapshot.stream.seek(extent.source_offset)
        remaining = extent.bytes
        while remaining:
            chunk = snapshot.stream.read(min(COPY_CHUNK, remaining))
            if not chunk:
                raise WriterError(f"planned extent {extent.kind!r} ended inside source image")
            remaining -= len(chunk)
            yield chunk
        return
    if extent.data is None or len(extent.data) != extent.bytes:
        raise WriterError(f"generated planned extent {extent.kind!r} is unavailable")
    offset = 0
    while offset < extent.bytes:
        chunk = extent.data[offset : offset + COPY_CHUNK]
        offset += len(chunk)
        yield chunk


def _verify_plan_content(snapshot: SourceSnapshot, plan: Any) -> None:
    if plan.source_sha256 != snapshot.sha256 or plan.source_bytes != snapshot.bytes:
        raise WriterError("target plan no longer matches the held source image")
    for extent in plan.extents:
        if extent.source_offset is not None:
            digest = _hash_source_range(snapshot, extent.source_offset, extent.bytes)
        elif extent.data is not None:
            digest = hashlib.sha256(extent.data).hexdigest()
        else:
            raise WriterError(f"planned extent {extent.kind!r} has no content")
        if digest != extent.sha256:
            raise WriterError(f"planned extent {extent.kind!r} digest changed before writing")


def _copy_and_verify(
    snapshot: SourceSnapshot,
    device: DeviceInfo,
    plan: Any,
    target: OpenMutationTarget,
) -> None:
    if (
        device.bytes != plan.target_bytes
        or target.bytes != device.bytes
        or target.rdev != device.rdev
    ):
        raise WriterError("held device identity or capacity differs from the admitted plan")
    snapshot.stream.seek(0)
    if (
        _hash_exact(
            snapshot.stream, snapshot.bytes, label="held immutable image snapshot"
        )
        != snapshot.sha256
    ):
        raise WriterError("held immutable image snapshot digest changed before writing")
    _verify_plan_content(snapshot, plan)
    try:
        for extent in plan.extents:
            target.stream.seek(extent.target_offset)
            written_digest = hashlib.sha256()
            written_bytes = 0
            for chunk in _extent_chunks(snapshot, extent):
                _write_all(target.stream, chunk)
                written_digest.update(chunk)
                written_bytes += len(chunk)
            if written_bytes != extent.bytes or written_digest.hexdigest() != extent.sha256:
                raise WriterError(
                    f"planned extent {extent.kind!r} changed during device write"
                )
        os.fsync(target.stream.fileno())
        for extent in plan.extents:
            target.stream.seek(extent.target_offset)
            digest = hashlib.sha256()
            remaining = extent.bytes
            while remaining:
                chunk = target.stream.read(min(COPY_CHUNK, remaining))
                if not chunk:
                    raise WriterError(
                        f"target ended while verifying extent {extent.kind!r}"
                    )
                digest.update(chunk)
                remaining -= len(chunk)
            if digest.hexdigest() != extent.sha256:
                raise WriterError(
                    f"post-write verification failed for extent {extent.kind!r}"
                )
    except OSError as error:
        raise WriterError(f"device write or verification failed: {error}") from error


def _emit(
    device: DeviceInfo,
    image: Path,
    plan: Any,
    token: str,
    *,
    written: bool,
) -> None:
    result = {
        "schema": WRITE_SCHEMA,
        "device": device.public(),
        "image": str(image.resolve()),
        "image_bytes": plan.source_bytes,
        "image_sha256": plan.source_sha256,
        "source_image_sha256": plan.source_sha256,
        "target_bytes": plan.target_bytes,
        "target_plan_sha256": plan.target_plan_sha256,
        "target_image_sha256": plan.target_image_sha256,
        "esp_sha256": plan.esp_sha256,
        "target_extents": [extent.public() for extent in plan.extents],
        "unwritten_policy": _boot_media_module().UNWRITTEN_POLICY,
        "unwritten_ranges": [
            {"offset": offset, "bytes": size}
            for offset, size in plan.unwritten_ranges
        ],
        "target_plan": plan.public(),
        "confirmation": token,
        "written": written,
    }
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("prepare", "write"):
        command = commands.add_parser(name)
        command.add_argument("--image", type=Path, required=True)
        command.add_argument("--device", required=True)
        if name == "write":
            command.add_argument("--confirm", required=True)
    args = parser.parse_args(argv)
    try:
        with _validated_source_snapshot(args.image) as snapshot:
            device = inspect_device(args.device)
            plan = _target_plan(snapshot, device.bytes)
            token = confirmation_token(device, plan)
            if args.command == "prepare":
                _emit(
                    device,
                    args.image,
                    plan,
                    token,
                    written=False,
                )
                return 0
            if args.confirm != token:
                raise WriterError(
                    "confirmation mismatch; run prepare again and supply its exact token"
                )

            # The write invocation captures one validated snapshot and keeps
            # its read-only descriptor alive through verification. Re-probe
            # the device and re-hash the origin before the first state change.
            current = inspect_device(args.device)
            if current != device:
                raise WriterError("device changed after confirmation")
            snapshot.assert_origin_unchanged()
            if current.platform == "macos":
                _run(["diskutil", "unmountDisk", current.path])

            # Open exactly once, validate the held kernel object, then re-probe
            # the public path while that descriptor is held. A later /dev path
            # reassignment cannot redirect any mutation or readback operation.
            with _open_mutation_target(current) as target:
                final = inspect_device(args.device)
                if final != current:
                    raise WriterError("device changed while mutation target was held")
                if target.rdev != final.rdev or target.bytes != final.bytes:
                    raise WriterError("held device differs from final path-based probe")
                _same_path_identity(snapshot.origin, snapshot.identity)
                _copy_and_verify(snapshot, final, plan, target)
            _emit(
                final,
                args.image,
                plan,
                token,
                written=True,
            )
    except WriterError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
