#!/usr/bin/env python3
"""Fail-closed installer for one validated OSTADIX capacity ISO on macOS Ventoy."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import ctypes
from dataclasses import dataclass
import errno
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import stat
import subprocess
import sys
import time
from typing import Any, Iterator
import uuid


SCHEMA = "ostadix.ventoy-install/v1"
TOKEN_DOMAIN = b"OSTADIX/VENTOY-INSTALL-CONFIRM/V1\0"
TOKEN_PREFIX = "OSTADIX-VENTOY-"
CAPACITY_ISO_TOOL = Path(__file__).with_name("ostadix_capacity_iso.py")
DISKUTIL = "/usr/sbin/diskutil"
COPY_CHUNK = 4 * 1024 * 1024
SPACE_MARGIN = 64 * 1024 * 1024
RENAME_EXCL = 0x00000004
EJECT_TIMEOUT_SECONDS = 5.0


class VentoyError(RuntimeError):
    pass


class CapacityValidationError(VentoyError):
    pass


class ExclusiveRenameUnsupported(VentoyError):
    """The admitted filesystem cannot provide a no-replace atomic rename."""


_CAPACITY_MODULE: Any | None = None


def _capacity_module() -> Any:
    global _CAPACITY_MODULE
    if _CAPACITY_MODULE is not None:
        return _CAPACITY_MODULE
    if not CAPACITY_ISO_TOOL.is_file():
        raise VentoyError(f"capacity ISO inspector is missing: {CAPACITY_ISO_TOOL}")
    name = "_ostadix_capacity_iso_for_ventoy"
    specification = importlib.util.spec_from_file_location(name, CAPACITY_ISO_TOOL)
    if specification is None or specification.loader is None:
        raise VentoyError(f"cannot load capacity ISO inspector: {CAPACITY_ISO_TOOL}")
    module = importlib.util.module_from_spec(specification)
    sys.modules[name] = module
    try:
        specification.loader.exec_module(module)
    except Exception as error:
        sys.modules.pop(name, None)
        raise VentoyError(f"cannot initialize capacity ISO inspector: {error}") from error
    _CAPACITY_MODULE = module
    return module


@dataclass(frozen=True)
class FileIdentity:
    device: int
    inode: int
    bytes: int
    modified_ns: int
    changed_ns: int

    @classmethod
    def from_stat(cls, value: os.stat_result) -> "FileIdentity":
        return cls(
            device=value.st_dev,
            inode=value.st_ino,
            bytes=value.st_size,
            modified_ns=value.st_mtime_ns,
            changed_ns=value.st_ctime_ns,
        )

    def public(self) -> dict[str, int]:
        return {
            "device": self.device,
            "inode": self.inode,
            "bytes": self.bytes,
            "modified_ns": self.modified_ns,
            "changed_ns": self.changed_ns,
        }


@dataclass
class SourceSnapshot:
    path: Path
    descriptor: int
    identity: FileIdentity
    metadata: dict[str, Any]

    def public(self) -> dict[str, Any]:
        return {
            "path": str(self.path),
            "identity": self.identity.public(),
            "schema": self.metadata["schema"],
            "architecture": self.metadata["architecture"],
            "bytes": self.metadata["bytes"],
            "sha256": self.metadata["sha256"],
            "capacity_lock_sha256": self.metadata["capacity_lock_sha256"],
            "default_entry": self.metadata["default_entry"],
            "entry_count": len(self.metadata["entries"]),
        }

    def assert_unchanged(self) -> None:
        current = os.fstat(self.descriptor)
        if FileIdentity.from_stat(current) != self.identity:
            raise VentoyError("source ISO changed while its descriptor was held")
        try:
            path_state = os.stat(self.path, follow_symlinks=False)
        except OSError as error:
            raise VentoyError(f"source ISO path became unavailable: {error}") from error
        if not stat.S_ISREG(path_state.st_mode) or FileIdentity.from_stat(path_state) != self.identity:
            raise VentoyError("source ISO was replaced, resized, or modified after admission")


@dataclass(frozen=True)
class VentoyMedium:
    whole_device: str
    whole_identifier: str
    whole_device_number: str
    whole_bytes: int
    model: str
    bus_protocol: str
    volume_device: str
    volume_identifier: str
    parent_whole_disk: str
    mountpoint: str
    volume_name: str
    filesystem: str
    volume_uuid: str
    partition_uuid: str
    volume_bytes: int
    mount_device: int
    mount_inode: int
    efi_identifier: str
    efi_volume_uuid: str
    efi_partition_uuid: str
    efi_bytes: int
    free_bytes: int
    allocation_block_bytes: int

    def public(self) -> dict[str, Any]:
        return {
            "whole_device": self.whole_device,
            "whole_identifier": self.whole_identifier,
            "whole_device_number": self.whole_device_number,
            "whole_bytes": self.whole_bytes,
            "model": self.model,
            "bus_protocol": self.bus_protocol,
            "volume_device": self.volume_device,
            "volume_identifier": self.volume_identifier,
            "parent_whole_disk": self.parent_whole_disk,
            "mountpoint": self.mountpoint,
            "volume_name": self.volume_name,
            "filesystem": self.filesystem,
            "volume_uuid": self.volume_uuid,
            "partition_uuid": self.partition_uuid,
            "volume_bytes": self.volume_bytes,
            "mount_device": self.mount_device,
            "mount_inode": self.mount_inode,
            "efi_identifier": self.efi_identifier,
            "efi_volume_uuid": self.efi_volume_uuid,
            "efi_partition_uuid": self.efi_partition_uuid,
            "efi_bytes": self.efi_bytes,
        }


def _run(command: list[str]) -> bytes:
    try:
        result = subprocess.run(command, check=False, capture_output=True)
    except OSError as error:
        raise VentoyError(f"cannot execute {command[0]}: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise VentoyError(f"{' '.join(command)} failed: {detail or result.returncode}")
    return result.stdout


def _diskutil_plist(arguments: list[str]) -> dict[str, Any]:
    raw = _run([DISKUTIL, *arguments])
    try:
        parsed = plistlib.loads(raw)
    except Exception as error:
        raise VentoyError(f"diskutil returned an invalid property list: {error}") from error
    if not isinstance(parsed, dict):
        raise VentoyError("diskutil property list is not a dictionary")
    return parsed


def _diskutil_info(target: str) -> dict[str, Any]:
    return _diskutil_plist(["info", "-plist", target])


def _diskutil_list(device: str) -> dict[str, Any]:
    return _diskutil_plist(["list", "-plist", device])


def _positive_int(value: Any, label: str) -> int:
    if type(value) is not int or value <= 0:
        raise VentoyError(f"{label} must be a positive integer")
    return value


def _uuid(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise VentoyError(f"{label} is missing")
    try:
        return str(uuid.UUID(value.strip()))
    except ValueError as error:
        raise VentoyError(f"{label} is not a UUID") from error


def _device_number(path: str) -> str:
    try:
        value = os.stat(path, follow_symlinks=False)
    except OSError as error:
        raise VentoyError(f"cannot inspect device node {path}: {error}") from error
    if not (stat.S_ISBLK(value.st_mode) or stat.S_ISCHR(value.st_mode)):
        raise VentoyError(f"target is not a device node: {path}")
    return f"{os.major(value.st_rdev)}:{os.minor(value.st_rdev)}"


def _mounted_directory(path: Path) -> os.stat_result:
    if not path.is_absolute():
        raise VentoyError("Ventoy volume path must be absolute")
    try:
        value = os.stat(path, follow_symlinks=False)
    except OSError as error:
        raise VentoyError(f"Ventoy mountpoint is unavailable: {path}: {error}") from error
    if stat.S_ISLNK(value.st_mode) or not stat.S_ISDIR(value.st_mode):
        raise VentoyError("Ventoy mountpoint is not a non-symlink directory")
    if os.path.realpath(path) != os.fspath(path):
        raise VentoyError("Ventoy mountpoint resolves through a symlink")
    return value


def _partition_records(inventory: dict[str, Any], whole_identifier: str) -> list[dict[str, Any]]:
    disks = inventory.get("AllDisksAndPartitions")
    if not isinstance(disks, list):
        raise VentoyError("diskutil list did not return disk records")
    matches = [entry for entry in disks if isinstance(entry, dict) and entry.get("DeviceIdentifier") == whole_identifier]
    if len(matches) != 1:
        raise VentoyError("whole disk is absent or ambiguous in diskutil list")
    partitions = matches[0].get("Partitions")
    if not isinstance(partitions, list):
        raise VentoyError("diskutil list did not return partition records")
    return [entry for entry in partitions if isinstance(entry, dict)]


def probe_ventoy(device: str, volume: Path, *, system: str | None = None) -> VentoyMedium:
    system = system or platform.system()
    if system != "Darwin":
        raise VentoyError(f"Ventoy installation is unsupported on {system}")
    if not re.fullmatch(r"/dev/disk[0-9]+", device):
        raise VentoyError("device must be one exact macOS whole /dev/diskN path")
    volume = Path(os.path.abspath(os.fspath(volume)))
    mount_state = _mounted_directory(volume)
    whole = _diskutil_info(device)
    expected_identifier = Path(device).name
    if whole.get("WholeDisk") is not True or whole.get("DeviceIdentifier") != expected_identifier:
        raise VentoyError("requested device is not the exact whole disk reported by diskutil")
    if whole.get("Internal") is not False or whole.get("OSInternalMedia") is not False:
        raise VentoyError("Ventoy whole disk must be external")
    if whole.get("VirtualOrPhysical") != "Physical":
        raise VentoyError("Ventoy whole disk must be physical")
    if whole.get("Removable") is not True or whole.get("RemovableMediaOrExternalDevice") is not True:
        raise VentoyError("Ventoy whole disk must be removable")
    if whole.get("Writable") is not True or whole.get("WritableMedia") is not True:
        raise VentoyError("Ventoy whole disk must be writable")
    if str(whole.get("BusProtocol") or "").upper() != "USB":
        raise VentoyError("Ventoy whole disk must use USB transport")
    if whole.get("Content") != "GUID_partition_scheme":
        raise VentoyError("Ventoy whole disk must use a GUID partition scheme")
    whole_bytes = _positive_int(whole.get("TotalSize"), "whole-disk size")
    root = _diskutil_info("/")
    root_parent = str(root.get("ParentWholeDisk") or root.get("PartOfWhole") or "")
    if not root_parent:
        raise VentoyError("diskutil did not identify the active root whole disk")
    if root_parent == expected_identifier:
        raise VentoyError("refusing the disk that contains the active root filesystem")

    mounted = _diskutil_info(os.fspath(volume))
    volume_identifier = str(mounted.get("DeviceIdentifier") or "")
    parent = str(mounted.get("ParentWholeDisk") or "")
    if mounted.get("WholeDisk") is not False or not volume_identifier:
        raise VentoyError("Ventoy mountpoint is not one exact partition")
    if parent != expected_identifier:
        raise VentoyError("Ventoy mountpoint does not belong to the requested whole disk")
    if mounted.get("MountPoint") != os.fspath(volume):
        raise VentoyError("diskutil mountpoint differs from the requested path")
    if mounted.get("VolumeName") != "Ventoy":
        raise VentoyError("data volume must be named Ventoy")
    filesystem = str(mounted.get("FilesystemType") or "").lower()
    if filesystem != "exfat":
        raise VentoyError("macOS Ventoy installation requires a writable ExFAT data volume")
    if mounted.get("Internal") is not False or mounted.get("Removable") is not True:
        raise VentoyError("Ventoy data partition must be external and removable")
    if mounted.get("RemovableMediaOrExternalDevice") is not True:
        raise VentoyError("Ventoy data partition is not reported as external media")
    if mounted.get("Writable") is not True or mounted.get("WritableMedia") is not True or mounted.get("WritableVolume") is not True:
        raise VentoyError("Ventoy data partition must be writable")
    if str(mounted.get("BusProtocol") or "").upper() != "USB":
        raise VentoyError("Ventoy data partition must use USB transport")
    if mounted.get("Bootable") is not True:
        raise VentoyError("Ventoy data partition is not reported bootable")
    volume_uuid = _uuid(mounted.get("VolumeUUID"), "Ventoy volume UUID")
    partition_uuid = _uuid(mounted.get("DiskUUID"), "Ventoy partition UUID")
    volume_bytes = _positive_int(mounted.get("TotalSize"), "Ventoy volume size")
    free_plist = _positive_int(mounted.get("FreeSpace"), "Ventoy free space")
    allocation = _positive_int(mounted.get("VolumeAllocationBlockSize"), "Ventoy allocation block")

    inventory = _diskutil_list(device)
    partitions = _partition_records(inventory, expected_identifier)
    data_matches = [entry for entry in partitions if entry.get("DeviceIdentifier") == volume_identifier]
    if len(data_matches) != 1:
        raise VentoyError("Ventoy data partition is absent or ambiguous in partition inventory")
    data = data_matches[0]
    if _uuid(data.get("VolumeUUID"), "listed Ventoy volume UUID") != volume_uuid or _uuid(data.get("DiskUUID"), "listed Ventoy partition UUID") != partition_uuid:
        raise VentoyError("Ventoy data partition identity differs across diskutil probes")
    efi_matches = [entry for entry in partitions if entry.get("VolumeName") == "VTOYEFI"]
    if len(efi_matches) != 1:
        raise VentoyError("requested disk does not have one VTOYEFI sibling partition")
    efi = efi_matches[0]
    efi_identifier = str(efi.get("DeviceIdentifier") or "")
    if not efi_identifier or efi_identifier == volume_identifier:
        raise VentoyError("VTOYEFI sibling partition identity is invalid")
    efi_bytes = _positive_int(efi.get("Size"), "VTOYEFI partition size")
    if not 16 * 1024 * 1024 <= efi_bytes <= 128 * 1024 * 1024:
        raise VentoyError("VTOYEFI partition size is outside the admitted range")

    try:
        filesystem_state = os.statvfs(volume)
    except OSError as error:
        raise VentoyError(f"cannot inspect Ventoy free space: {error}") from error
    free_stat = filesystem_state.f_bavail * filesystem_state.f_frsize
    if free_stat <= 0:
        raise VentoyError("Ventoy filesystem reported no available space")
    return VentoyMedium(
        whole_device=device,
        whole_identifier=expected_identifier,
        whole_device_number=_device_number(device),
        whole_bytes=whole_bytes,
        model=str(whole.get("MediaName") or whole.get("IORegistryEntryName") or "unknown"),
        bus_protocol="USB",
        volume_device=f"/dev/{volume_identifier}",
        volume_identifier=volume_identifier,
        parent_whole_disk=parent,
        mountpoint=os.fspath(volume),
        volume_name="Ventoy",
        filesystem=filesystem,
        volume_uuid=volume_uuid,
        partition_uuid=partition_uuid,
        volume_bytes=volume_bytes,
        mount_device=mount_state.st_dev,
        mount_inode=mount_state.st_ino,
        efi_identifier=efi_identifier,
        efi_volume_uuid=_uuid(efi.get("VolumeUUID"), "VTOYEFI volume UUID"),
        efi_partition_uuid=_uuid(efi.get("DiskUUID"), "VTOYEFI partition UUID"),
        efi_bytes=efi_bytes,
        free_bytes=min(free_plist, free_stat),
        allocation_block_bytes=allocation,
    )


def _inspect_capacity(descriptor: int, label: str) -> dict[str, Any]:
    module = _capacity_module()
    try:
        metadata = module.inspect_descriptor(descriptor, label)
    except module.CapacityIsoError as error:
        raise CapacityValidationError(f"{label} is not a valid OSTADIX capacity ISO: {error}") from error
    if not isinstance(metadata, dict):
        raise CapacityValidationError(f"{label} inspector returned invalid metadata")
    required = {"schema", "architecture", "bytes", "sha256", "capacity_lock_sha256", "default_entry", "entries"}
    if not required.issubset(metadata):
        raise CapacityValidationError(f"{label} inspection metadata is incomplete")
    if metadata["schema"] != "ostadix.capacity-iso/v1" or metadata["architecture"] != "x86_64":
        raise CapacityValidationError(f"{label} is not an x86_64 OSTADIX capacity ISO")
    entries = metadata.get("entries")
    artifacts = metadata.get("artifacts")
    if (
        not isinstance(entries, list)
        or len(entries) != 1
        or not isinstance(entries[0], dict)
        or entries[0].get("id") != "hosted"
    ):
        raise CapacityValidationError(
            f"{label} is not the single-entry physical Hosted Live ISO"
        )
    entry = entries[0]
    if (
        entry.get("adapter") != "linux-selection"
        or entry.get("kernel_path") != "/boot/hosted/vmlinuz-lts"
        or entry.get("initrd_paths") != ["/boot/hosted/initramfs.cpio.gz"]
        or entry.get("selection_id") != "hosted"
        or entry.get("arguments") != [
            "console=ttyS0,115200n8",
            "console=tty0",
            "rdinit=/init",
            "panic=0",
            "loglevel=7",
            "ignore_loglevel",
        ]
    ):
        raise CapacityValidationError(f"{label} has the wrong physical Hosted Live boot profile")
    artifact_closure = {
        (artifact.get("iso_path"), artifact.get("role"))
        for artifact in artifacts or []
        if isinstance(artifact, dict)
    }
    if artifact_closure != {
        ("/boot/hosted/vmlinuz-lts", "linux-kernel"),
        ("/boot/hosted/initramfs.cpio.gz", "linux-initrd"),
    }:
        raise CapacityValidationError(f"{label} has the wrong physical Hosted Live artifact closure")
    return metadata


@contextmanager
def _source_snapshot(path: Path) -> Iterator[SourceSnapshot]:
    path = Path(os.path.abspath(os.fspath(path)))
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise VentoyError(f"cannot open source ISO without following links: {path}: {error}") from error
    try:
        state = os.fstat(descriptor)
        if not stat.S_ISREG(state.st_mode) or state.st_size <= 0:
            raise VentoyError(f"source ISO is not a non-empty regular file: {path}")
        identity = FileIdentity.from_stat(state)
        metadata = _inspect_capacity(descriptor, str(path))
        if metadata["bytes"] != identity.bytes:
            raise VentoyError("source ISO byte count differs from capacity inspection")
        snapshot = SourceSnapshot(path=path, descriptor=descriptor, identity=identity, metadata=metadata)
        snapshot.assert_unchanged()
        yield snapshot
        snapshot.assert_unchanged()
    finally:
        os.close(descriptor)


@contextmanager
def _volume_directory(medium: VentoyMedium) -> Iterator[int]:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_DIRECTORY", 0)
    try:
        descriptor = os.open(medium.mountpoint, flags)
    except OSError as error:
        raise VentoyError(f"cannot pin Ventoy mountpoint: {error}") from error
    try:
        value = os.fstat(descriptor)
        if not stat.S_ISDIR(value.st_mode) or value.st_dev != medium.mount_device or value.st_ino != medium.mount_inode:
            raise VentoyError("pinned Ventoy directory differs from admitted mountpoint")
        yield descriptor
        value = os.fstat(descriptor)
        if value.st_dev != medium.mount_device or value.st_ino != medium.mount_inode:
            raise VentoyError("Ventoy directory identity changed while held")
    finally:
        os.close(descriptor)


def _validate_name(name: str) -> str:
    if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,126}\.iso", name, re.IGNORECASE):
        raise VentoyError("destination name must be one bounded .iso basename")
    if name in {".", ".."} or "/" in name or "\\" in name:
        raise VentoyError("destination name must not contain path traversal")
    if not name.endswith("_VTGRUB2.iso"):
        raise VentoyError("destination name must end with _VTGRUB2.iso to force Ventoy GRUB2 mode")
    return name


def _hash_descriptor(descriptor: int, size: int, label: str) -> str:
    digest = hashlib.sha256()
    offset = 0
    try:
        while offset < size:
            chunk = os.pread(descriptor, min(COPY_CHUNK, size - offset), offset)
            if not chunk:
                raise VentoyError(f"{label} ended before its admitted byte count")
            digest.update(chunk)
            offset += len(chunk)
        if os.pread(descriptor, 1, size):
            raise VentoyError(f"{label} grew beyond its admitted byte count")
    except OSError as error:
        raise VentoyError(f"cannot hash {label}: {error}") from error
    return digest.hexdigest()


def _entry_identity(directory: int, name: str) -> FileIdentity:
    try:
        value = os.stat(name, dir_fd=directory, follow_symlinks=False)
    except OSError as error:
        raise VentoyError(f"cannot recheck Ventoy destination {name}: {error}") from error
    if not stat.S_ISREG(value.st_mode):
        raise VentoyError("Ventoy destination is not a regular file")
    return FileIdentity.from_stat(value)


def _destination_state(directory: int, name: str, source: SourceSnapshot) -> dict[str, Any]:
    try:
        value = os.stat(name, dir_fd=directory, follow_symlinks=False)
    except FileNotFoundError:
        return {"basename": name, "state": "absent", "existing_bytes": None, "existing_sha256": None}
    except OSError as error:
        raise VentoyError(f"cannot inspect Ventoy destination {name}: {error}") from error
    if stat.S_ISLNK(value.st_mode) or not stat.S_ISREG(value.st_mode):
        raise VentoyError("Ventoy destination is a symlink or special file")
    identity = FileIdentity.from_stat(value)
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(name, flags, dir_fd=directory)
    except OSError as error:
        raise VentoyError(f"cannot pin Ventoy destination {name}: {error}") from error
    try:
        if FileIdentity.from_stat(os.fstat(descriptor)) != identity:
            raise VentoyError("Ventoy destination changed while it was opened")
        try:
            metadata = _inspect_capacity(descriptor, f"Ventoy destination {name}")
            digest = metadata["sha256"]
        except CapacityValidationError:
            metadata = None
            digest = _hash_descriptor(descriptor, identity.bytes, "Ventoy destination")
        if _entry_identity(directory, name) != identity or FileIdentity.from_stat(os.fstat(descriptor)) != identity:
            raise VentoyError("Ventoy destination changed during verification")
    finally:
        os.close(descriptor)
    identical = metadata == source.metadata and identity.bytes == source.identity.bytes and digest == source.metadata["sha256"]
    return {
        "basename": name,
        "state": "identical" if identical else "divergent",
        "existing_bytes": identity.bytes,
        "existing_sha256": digest,
    }


def _commitment(source: SourceSnapshot, medium: VentoyMedium, destination: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema": SCHEMA,
        "operation": "install-if-absent-or-identical",
        "publication_policy": "atomic-exclusive-rename-or-verified-exclusive-copy",
        "source": source.public(),
        "medium": medium.public(),
        "destination": destination,
    }


def confirmation_token(commitment: dict[str, Any]) -> str:
    payload = json.dumps(commitment, sort_keys=True, separators=(",", ":")).encode("utf-8")
    suffix = hashlib.sha256(TOKEN_DOMAIN + payload).hexdigest()[:32].upper()
    return TOKEN_PREFIX + suffix


@contextmanager
def _admission(iso: Path, device: str, volume: Path, name: str) -> Iterator[tuple[SourceSnapshot, VentoyMedium, int, dict[str, Any]]]:
    name = _validate_name(name)
    with _source_snapshot(iso) as source:
        medium = probe_ventoy(device, volume)
        with _volume_directory(medium) as directory:
            destination = _destination_state(directory, name, source)
            commitment = _commitment(source, medium, destination)
            required = (source.identity.bytes * 2) + max(
                SPACE_MARGIN, medium.allocation_block_bytes * 16
            )
            status = {
                "absent": "ready-to-install",
                "identical": "already-current",
                "divergent": "refuse-divergent",
            }[destination["state"]]
            record = {
                **commitment,
                "status": status,
                "required_free_bytes": required,
                "observed_free_bytes": medium.free_bytes,
                "confirmation": confirmation_token(commitment),
                "written": False,
                "verified": destination["state"] == "identical",
                "ejected": False,
            }
            yield source, medium, directory, record


def prepare(iso: Path, device: str, volume: Path, name: str) -> dict[str, Any]:
    with _admission(iso, device, volume, name) as (_source, _medium, _directory, record):
        return {**record, "action": "prepare"}


@contextmanager
def _exclusive_volume_lock(directory: int) -> Iterator[None]:
    try:
        fcntl.flock(directory, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as error:
        raise VentoyError(f"another operation holds the Ventoy volume lock: {error}") from error
    try:
        yield
    finally:
        fcntl.flock(directory, fcntl.LOCK_UN)


def _write_all(descriptor: int, data: bytes) -> None:
    view = memoryview(data)
    while view:
        try:
            written = os.write(descriptor, view)
        except OSError as error:
            raise VentoyError(f"Ventoy temporary file stopped accepting bytes: {error}") from error
        if written <= 0:
            raise VentoyError("Ventoy temporary file stopped accepting bytes")
        view = view[written:]


def _copy_source(source: SourceSnapshot, output: int) -> str:
    digest = hashlib.sha256()
    offset = 0
    while offset < source.identity.bytes:
        try:
            chunk = os.pread(source.descriptor, min(COPY_CHUNK, source.identity.bytes - offset), offset)
        except OSError as error:
            raise VentoyError(f"cannot read held source ISO: {error}") from error
        if not chunk:
            raise VentoyError("held source ISO ended during Ventoy copy")
        _write_all(output, chunk)
        digest.update(chunk)
        offset += len(chunk)
    if os.pread(source.descriptor, 1, source.identity.bytes):
        raise VentoyError("held source ISO grew during Ventoy copy")
    return digest.hexdigest()


def _full_sync(descriptor: int) -> None:
    try:
        os.fsync(descriptor)
        full = getattr(fcntl, "F_FULLFSYNC", None)
        if full is None:
            raise VentoyError("Darwin F_FULLFSYNC is unavailable")
        fcntl.fcntl(descriptor, full)
    except VentoyError:
        raise
    except OSError as error:
        raise VentoyError(f"cannot flush Ventoy bytes to physical media: {error}") from error


def _sync_directory(directory: int) -> None:
    try:
        os.fsync(directory)
    except OSError as error:
        if error.errno not in {errno.EBADF, errno.EINVAL, errno.ENOTSUP}:
            raise VentoyError(f"cannot flush Ventoy directory metadata: {error}") from error


def _rename_exclusive(directory: int, source: str, destination: str) -> None:
    if platform.system() != "Darwin":
        raise VentoyError("atomic Ventoy publication requires Darwin renameatx_np")
    libc = ctypes.CDLL(None, use_errno=True)
    rename = getattr(libc, "renameatx_np", None)
    if rename is None:
        raise VentoyError("Darwin renameatx_np is unavailable")
    rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int
    result = rename(directory, os.fsencode(source), directory, os.fsencode(destination), RENAME_EXCL)
    if result != 0:
        code = ctypes.get_errno()
        if code == errno.EEXIST:
            raise VentoyError("Ventoy destination appeared before atomic publication")
        if code in {errno.ENOTSUP, getattr(errno, "EOPNOTSUPP", errno.ENOTSUP)}:
            raise ExclusiveRenameUnsupported(
                "Ventoy filesystem does not support atomic no-replace rename"
            )
        raise VentoyError(f"cannot atomically publish Ventoy ISO: {os.strerror(code)}")


def _same_medium(expected: VentoyMedium, device: str, volume: Path) -> None:
    current = probe_ventoy(device, volume)
    if current.public() != expected.public():
        raise VentoyError("Ventoy disk, partition, or mount identity changed after admission")


def _copy_descriptor(source: int, size: int, output: int, label: str) -> str:
    digest = hashlib.sha256()
    offset = 0
    while offset < size:
        try:
            chunk = os.pread(source, min(COPY_CHUNK, size - offset), offset)
        except OSError as error:
            raise VentoyError(f"cannot read {label}: {error}") from error
        if not chunk:
            raise VentoyError(f"{label} ended during exclusive publication copy")
        _write_all(output, chunk)
        digest.update(chunk)
        offset += len(chunk)
    try:
        if os.pread(source, 1, size):
            raise VentoyError(f"{label} grew during exclusive publication copy")
    except OSError as error:
        raise VentoyError(f"cannot recheck {label}: {error}") from error
    return digest.hexdigest()


def _copy_verified_exclusive(
    source: SourceSnapshot,
    private: int,
    directory: int,
    name: str,
) -> dict[str, Any]:
    """Publish through O_EXCL when ExFAT cannot no-replace-rename a private copy.

    The final basename is visible while its second verified copy is written, but
    O_EXCL preserves the no-overwrite contract. Any failed owned copy is removed
    only while its exact inode identity is still pinned.
    """

    flags = (
        os.O_RDWR
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    descriptor = -1
    created: FileIdentity | None = None
    succeeded = False
    try:
        try:
            descriptor = os.open(name, flags, 0o600, dir_fd=directory)
        except FileExistsError as error:
            raise VentoyError(
                "Ventoy destination appeared before exclusive publication copy"
            ) from error
        except OSError as error:
            raise VentoyError(
                f"cannot create exclusive Ventoy destination: {error}"
            ) from error
        created = FileIdentity.from_stat(os.fstat(descriptor))
        copied = _copy_descriptor(
            private, source.identity.bytes, descriptor, "verified private Ventoy ISO"
        )
        if copied != source.metadata["sha256"]:
            raise VentoyError(
                "exclusive Ventoy publication copy differs from admitted source ISO"
            )
        _full_sync(descriptor)
        final = _inspect_capacity(descriptor, f"exclusive Ventoy ISO {name}")
        if final != source.metadata:
            raise VentoyError(
                "exclusive Ventoy publication copy differs from verified private ISO"
            )
        current = _entry_identity(directory, name)
        held = FileIdentity.from_stat(os.fstat(descriptor))
        if current != held:
            raise VentoyError(
                "exclusive Ventoy destination changed during publication copy"
            )
        succeeded = True
        return final
    except BaseException as error:
        if created is not None:
            try:
                current_stat = os.stat(name, dir_fd=directory, follow_symlinks=False)
            except FileNotFoundError:
                current_stat = None
            except OSError as cleanup_error:
                raise VentoyError(
                    f"cannot inspect failed exclusive Ventoy copy for cleanup: {cleanup_error}"
                ) from error
            if current_stat is not None:
                if (
                    current_stat.st_dev != created.device
                    or current_stat.st_ino != created.inode
                ):
                    raise VentoyError(
                        "failed exclusive Ventoy copy changed identity; refusing unsafe cleanup"
                    ) from error
                try:
                    os.unlink(name, dir_fd=directory)
                except OSError as cleanup_error:
                    raise VentoyError(
                        f"cannot remove failed exclusive Ventoy copy: {cleanup_error}"
                    ) from error
        raise
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        if succeeded:
            _sync_directory(directory)


def _copy_absent(source: SourceSnapshot, medium: VentoyMedium, directory: int, name: str) -> dict[str, Any]:
    temporary = f".ostadix-ventoy-{source.metadata['sha256'][:16]}-{uuid.uuid4().hex}.part"
    flags = os.O_RDWR | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = -1
    published = False
    try:
        descriptor = os.open(temporary, flags, 0o600, dir_fd=directory)
        copied = _copy_source(source, descriptor)
        if copied != source.metadata["sha256"]:
            raise VentoyError("Ventoy copy digest differs from admitted source ISO")
        _full_sync(descriptor)
        private = _inspect_capacity(descriptor, "private Ventoy ISO copy")
        if private != source.metadata:
            raise VentoyError("private Ventoy ISO copy differs from admitted source")
        source.assert_unchanged()
        _same_medium(medium, medium.whole_device, Path(medium.mountpoint))
        if _destination_state(directory, name, source)["state"] != "absent":
            raise VentoyError("Ventoy destination changed before atomic publication")
        try:
            _rename_exclusive(directory, temporary, name)
        except ExclusiveRenameUnsupported:
            final = _copy_verified_exclusive(source, descriptor, directory, name)
            source.assert_unchanged()
            _same_medium(medium, medium.whole_device, Path(medium.mountpoint))
            return final
        published = True
        _full_sync(descriptor)
        _sync_directory(directory)
        final_identity = _entry_identity(directory, name)
        if final_identity != FileIdentity.from_stat(os.fstat(descriptor)):
            raise VentoyError("published Ventoy path does not identify the verified temporary file")
        final = _inspect_capacity(descriptor, f"published Ventoy ISO {name}")
        if final != source.metadata:
            raise VentoyError("published Ventoy ISO differs from admitted source")
        source.assert_unchanged()
        _same_medium(medium, medium.whole_device, Path(medium.mountpoint))
        return final
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        if not published:
            try:
                os.unlink(temporary, dir_fd=directory)
            except FileNotFoundError:
                pass
            except OSError:
                pass


def install(iso: Path, device: str, volume: Path, name: str, confirm: str) -> tuple[dict[str, Any], VentoyMedium]:
    with _admission(iso, device, volume, name) as (source, medium, directory, record):
        if confirm != record["confirmation"]:
            raise VentoyError("confirmation mismatch; run prepare again and supply its exact token")
        state = record["destination"]["state"]
        if state == "divergent":
            raise VentoyError("refusing to overwrite divergent Ventoy destination")
        if state == "identical":
            return ({**record, "action": "install", "status": "already-current", "verified": True}, medium)
        if medium.free_bytes < record["required_free_bytes"]:
            raise VentoyError(
                "Ventoy volume does not have enough free space for verified private "
                "and exclusive publication copies"
            )
        with _exclusive_volume_lock(directory):
            source.assert_unchanged()
            _same_medium(medium, device, volume)
            if _destination_state(directory, name, source)["state"] != "absent":
                raise VentoyError("Ventoy destination changed after confirmation")
            _copy_absent(source, medium, directory, name)
        destination = _destination_state(directory, name, source)
        if destination["state"] != "identical":
            raise VentoyError("final Ventoy destination did not verify as identical")
        return (
            {
                **record,
                "action": "install",
                "status": "installed",
                "destination": destination,
                "written": True,
                "verified": True,
            },
            medium,
        )


def verify(iso: Path, device: str, volume: Path, name: str) -> dict[str, Any]:
    with _admission(iso, device, volume, name) as (_source, _medium, _directory, record):
        if record["destination"]["state"] != "identical":
            raise VentoyError("Ventoy destination does not match the admitted source ISO")
        return {**record, "action": "verify", "status": "verified", "verified": True}


def _diskutil_target_exists(target: str) -> bool:
    try:
        result = subprocess.run([DISKUTIL, "info", "-plist", target], check=False, capture_output=True)
    except OSError as error:
        raise VentoyError(f"cannot recheck ejected device: {error}") from error
    return result.returncode == 0


def eject(medium: VentoyMedium) -> None:
    _run([DISKUTIL, "eject", medium.whole_device])
    deadline = time.monotonic() + EJECT_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if not _diskutil_target_exists(medium.whole_device) and not _diskutil_target_exists(medium.mountpoint):
            return
        time.sleep(0.1)
    raise VentoyError("diskutil reported success but the Ventoy disk or mountpoint remains present")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for command_name in ("prepare", "install", "verify"):
        command = commands.add_parser(command_name)
        command.add_argument("--iso", required=True, type=Path)
        command.add_argument("--device", required=True)
        command.add_argument("--volume", required=True, type=Path)
        command.add_argument("--name", required=True)
        if command_name == "install":
            command.add_argument("--confirm", required=True)
            command.add_argument("--eject", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        if arguments.command == "prepare":
            record = prepare(arguments.iso, arguments.device, arguments.volume, arguments.name)
        elif arguments.command == "verify":
            record = verify(arguments.iso, arguments.device, arguments.volume, arguments.name)
        else:
            record, medium = install(
                arguments.iso,
                arguments.device,
                arguments.volume,
                arguments.name,
                arguments.confirm,
            )
            if arguments.eject:
                try:
                    eject(medium)
                except VentoyError:
                    record["ejected"] = False
                    print(json.dumps(record, indent=2, sort_keys=True))
                    raise
                record["ejected"] = True
        print(json.dumps(record, indent=2, sort_keys=True))
        return 0
    except VentoyError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
