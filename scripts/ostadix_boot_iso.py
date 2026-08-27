#!/usr/bin/env python3
"""Strictly inspect and safely publish OSTADIX x86_64 UEFI ISO images.

The inspector is intentionally independent of GRUB, xorriso, QEMU, and
``file(1)``.  It validates the ISO9660 volume, the El Torito EFI no-emulation
entry, the embedded FAT EFI System Partition, BOOTX64.EFI, the x86_64 kernel,
and the GRUB configuration that selects that kernel.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import mmap
import os
from pathlib import Path
import re
import stat
import struct
import sys
import tempfile


SCHEMA = "ostadix.boot-iso/v1"
LOGICAL_BLOCK_SIZE = 2048
MIN_ISO_BYTES = 24 * LOGICAL_BLOCK_SIZE
# ISO9660's volume-space field is substantially larger than this bound, but the
# repository deliberately admits optical images only through 16 GiB.  Inspection
# maps the descriptor instead of materializing these bytes in the Python heap.
MAX_ISO_BYTES = 16 * 1024 * 1024 * 1024
COPY_CHUNK_BYTES = 4 * 1024 * 1024
MAX_VOLUME_DESCRIPTORS = 64
EL_TORITO_SYSTEM_ID = b"EL TORITO SPECIFICATION"
EFI_PLATFORM_ID = 0xEF
NO_EMULATION_MEDIA_TYPE = 0
VOLUME_ID = "OSTADIX"
KERNEL_PATH = ("BOOT", "KERNEL.ELF")
GRUB_CONFIG_PATH = ("BOOT", "GRUB", "GRUB.CFG")
EXPECTED_GRUB_CONFIG_SHA256 = (
    "365d4c87dad7d824cd9942534bfb45d213410363f1f8ffad853d1a4b506c89d8"
)
SMOKE_REQUIRED_MARKERS = (
    "O-core kernel: serial online",
    "page protections: W^X online",
    "CPL3 native[0]: online",
    "timer CPL3 return: online",
    "CPL3 heartbeat: online",
)
SMOKE_FORBIDDEN_FRAGMENTS = (
    "panic",
    "fatal",
    "triple fault",
    "m02 kernel fault",
    "m02 unexpected fault",
    "leaked",
)


class IsoError(ValueError):
    """The supplied image is malformed or outside the bounded ISO contract."""


def validate_smoke_output(output: str, sustained_liveness: bool) -> list[str]:
    """Return exact serial-contract violations for one bounded ISO boot."""

    lines = output.replace("\r\n", "\n").replace("\r", "\n").splitlines()
    issues: list[str] = []
    missing = [marker for marker in SMOKE_REQUIRED_MARKERS if marker not in lines]
    if missing:
        issues.append("missing=" + repr(missing))
    wrong_counts = [
        marker for marker in SMOKE_REQUIRED_MARKERS if lines.count(marker) != 1
    ]
    if wrong_counts:
        issues.append("wrong-marker-count=" + repr(wrong_counts))
    if not missing:
        positions = [lines.index(marker) for marker in SMOKE_REQUIRED_MARKERS]
        if positions != sorted(positions):
            issues.append("causal marker order")
    lowered = output.casefold()
    reached = [
        fragment
        for fragment in SMOKE_FORBIDDEN_FRAGMENTS
        if fragment in lowered
    ]
    if reached:
        issues.append("forbidden=" + repr(reached))
    if not sustained_liveness:
        issues.append("no bounded post-heartbeat liveness")
    return issues


def _open_pinned_regular(
    path: Path, *, nofollow: bool, require_no_write_bits: bool = False
) -> int:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
    if nofollow:
        if not hasattr(os, "O_NOFOLLOW"):
            raise IsoError("this host cannot pin an input with O_NOFOLLOW")
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        qualifier = " without following links" if nofollow else ""
        raise IsoError(f"cannot open pinned input{qualifier}: {path}: {error}") from error
    try:
        state = os.fstat(descriptor)
        if not stat.S_ISREG(state.st_mode):
            raise IsoError(f"pinned input is not a regular file: {path}")
        if require_no_write_bits and state.st_mode & 0o222:
            raise IsoError(f"pinned input has write-permission bits set: {path}")
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor


def _sha256(data: bytes | mmap.mmap) -> str:
    return hashlib.sha256(data).hexdigest()


def _file_identity(value: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        value.st_dev,
        value.st_ino,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def _require_descriptor_identity(
    descriptor: int, expected: os.stat_result, label: str
) -> os.stat_result:
    try:
        current = os.fstat(descriptor)
    except OSError as error:
        raise IsoError(f"cannot recheck {label} descriptor: {error}") from error
    if _file_identity(current) != _file_identity(expected):
        raise IsoError(f"{label} changed while its descriptor was held")
    return current


def _require_path_identity(path: Path, expected: os.stat_result, label: str) -> None:
    try:
        current = os.stat(path, follow_symlinks=False)
    except OSError as error:
        raise IsoError(f"{label} path changed while it was held: {path}: {error}") from error
    if not stat.S_ISREG(current.st_mode) or _file_identity(current) != _file_identity(
        expected
    ):
        raise IsoError(f"{label} path was replaced while it was held: {path}")


def _write_all(descriptor: int, data: bytes) -> None:
    offset = 0
    while offset < len(data):
        try:
            written = os.write(descriptor, data[offset:])
        except OSError as error:
            raise IsoError(f"cannot write private ISO output: {error}") from error
        if written <= 0:
            raise IsoError("private ISO output stopped accepting bytes")
        offset += written


def _stream_copy_descriptor(
    source_descriptor: int, output_descriptor: int, size: int
) -> str:
    digest = hashlib.sha256()
    offset = 0
    try:
        while offset < size:
            requested = min(COPY_CHUNK_BYTES, size - offset)
            chunk = os.pread(source_descriptor, requested, offset)
            if not chunk:
                raise IsoError("ISO source ended before its admitted size during publication")
            if len(chunk) > requested:
                raise IsoError("ISO source read exceeded its bounded publication chunk")
            _write_all(output_descriptor, chunk)
            digest.update(chunk)
            offset += len(chunk)
        if os.pread(source_descriptor, 1, size):
            raise IsoError("ISO source grew beyond its admitted size during publication")
    except IsoError:
        raise
    except OSError as error:
        raise IsoError(f"cannot stream ISO source during publication: {error}") from error
    return digest.hexdigest()


def _u16_both(data: bytes, offset: int, label: str) -> int:
    little = int.from_bytes(data[offset : offset + 2], "little")
    big = int.from_bytes(data[offset + 2 : offset + 4], "big")
    if little != big:
        raise IsoError(f"{label} little- and big-endian forms differ")
    return little


def _u32_both(data: bytes, offset: int, label: str) -> int:
    little = int.from_bytes(data[offset : offset + 4], "little")
    big = int.from_bytes(data[offset + 4 : offset + 8], "big")
    if little != big:
        raise IsoError(f"{label} little- and big-endian forms differ")
    return little


def _extent(
    data: bytes | mmap.mmap,
    lba: int,
    size: int,
    volume_bytes: int,
    label: str,
) -> bytes:
    if lba <= 0 or size < 0:
        raise IsoError(f"{label} has an invalid extent")
    start = lba * LOGICAL_BLOCK_SIZE
    end = start + size
    if start >= volume_bytes or end > volume_bytes or end > len(data):
        raise IsoError(f"{label} extent leaves the ISO9660 volume")
    return data[start:end]


def _directory_record(record: bytes, volume_bytes: int, label: str) -> dict[str, object]:
    if len(record) < 34 or record[0] != len(record):
        raise IsoError(f"{label} has an invalid ISO9660 directory-record length")
    name_length = record[32]
    if 33 + name_length > len(record):
        raise IsoError(f"{label} has a truncated ISO9660 identifier")
    extent_lba = _u32_both(record, 2, f"{label} extent")
    size = _u32_both(record, 10, f"{label} size")
    flags = record[25]
    if flags & 0x80:
        raise IsoError(f"{label} uses unsupported multi-extent recording")
    if extent_lba <= 0 or extent_lba * LOGICAL_BLOCK_SIZE + size > volume_bytes:
        raise IsoError(f"{label} points outside the ISO9660 volume")
    return {
        "extent_lba": extent_lba,
        "size": size,
        "flags": flags,
        "name": record[33 : 33 + name_length],
    }


def _normalized_iso_name(raw: bytes) -> str | None:
    if raw in (b"\x00", b"\x01"):
        return None
    try:
        value = raw.decode("ascii")
    except UnicodeDecodeError as error:
        raise IsoError("ISO9660 identifier is not ASCII") from error
    value = value.split(";", 1)[0].rstrip(".")
    if not value or "/" in value or "\x00" in value:
        raise IsoError("ISO9660 identifier is malformed")
    return value.upper()


def _directory_entries(
    data: bytes | mmap.mmap,
    directory: dict[str, object],
    volume_bytes: int,
    label: str,
) -> list[dict[str, object]]:
    content = _extent(
        data,
        int(directory["extent_lba"]),
        int(directory["size"]),
        volume_bytes,
        label,
    )
    entries: list[dict[str, object]] = []
    offset = 0
    while offset < len(content):
        record_length = content[offset]
        if record_length == 0:
            offset = ((offset // LOGICAL_BLOCK_SIZE) + 1) * LOGICAL_BLOCK_SIZE
            continue
        sector_remaining = LOGICAL_BLOCK_SIZE - (offset % LOGICAL_BLOCK_SIZE)
        if record_length > sector_remaining or offset + record_length > len(content):
            raise IsoError(f"{label} contains a directory record crossing its block")
        entry = _directory_record(
            content[offset : offset + record_length], volume_bytes, label
        )
        name = _normalized_iso_name(entry["name"])
        if name is not None:
            entry["normalized_name"] = name
            entries.append(entry)
        offset += record_length
    return entries


def _find_iso_path(
    data: bytes | mmap.mmap,
    root: dict[str, object],
    components: tuple[str, ...],
    volume_bytes: int,
) -> tuple[bytes, dict[str, object]]:
    current = root
    for index, component in enumerate(components):
        if not int(current["flags"]) & 0x02:
            raise IsoError(f"ISO path component before {component} is not a directory")
        matches = [
            entry
            for entry in _directory_entries(
                data, current, volume_bytes, "/" + "/".join(components[:index])
            )
            if entry["normalized_name"] == component.upper()
        ]
        if len(matches) != 1:
            raise IsoError(
                f"ISO path /{'/'.join(components)} has {len(matches)} matches for {component}"
            )
        current = matches[0]
    if int(current["flags"]) & 0x02:
        raise IsoError(f"ISO path /{'/'.join(components)} is a directory, not a file")
    content = _extent(
        data,
        int(current["extent_lba"]),
        int(current["size"]),
        volume_bytes,
        "/" + "/".join(components),
    )
    return content, current


def _fat_geometry(
    image: bytes, *, available_bytes: int | None = None
) -> dict[str, int]:
    if len(image) < 512 or image[510:512] != b"\x55\xaa":
        raise IsoError("El Torito EFI boot image lacks a FAT boot-sector signature")
    bytes_per_sector = int.from_bytes(image[11:13], "little")
    sectors_per_cluster = image[13]
    reserved_sectors = int.from_bytes(image[14:16], "little")
    fat_count = image[16]
    root_entries = int.from_bytes(image[17:19], "little")
    total_sectors = int.from_bytes(image[19:21], "little")
    if total_sectors == 0:
        total_sectors = int.from_bytes(image[32:36], "little")
    fat_sectors = int.from_bytes(image[22:24], "little")
    if fat_sectors == 0:
        fat_sectors = int.from_bytes(image[36:40], "little")
    if bytes_per_sector not in (512, 1024, 2048, 4096):
        raise IsoError("El Torito EFI boot image has an unsupported FAT sector size")
    if (
        sectors_per_cluster == 0
        or sectors_per_cluster & (sectors_per_cluster - 1)
        or sectors_per_cluster > 128
        or reserved_sectors == 0
        or fat_count not in (1, 2)
        or fat_sectors == 0
        or total_sectors == 0
    ):
        raise IsoError("El Torito EFI boot image has invalid FAT geometry")
    image_bytes = total_sectors * bytes_per_sector
    admitted_bytes = len(image) if available_bytes is None else available_bytes
    if image_bytes > admitted_bytes:
        raise IsoError("El Torito EFI boot image is truncated")
    root_dir_sectors = (root_entries * 32 + bytes_per_sector - 1) // bytes_per_sector
    first_data_sector = reserved_sectors + fat_count * fat_sectors + root_dir_sectors
    if first_data_sector >= total_sectors:
        raise IsoError("El Torito EFI boot image has no FAT data region")
    data_sectors = total_sectors - first_data_sector
    cluster_count = data_sectors // sectors_per_cluster
    if cluster_count < 1:
        raise IsoError("El Torito EFI boot image has no usable FAT clusters")
    if cluster_count < 4085:
        fat_bits = 12
    elif cluster_count < 65525:
        fat_bits = 16
    else:
        fat_bits = 32
    root_cluster = int.from_bytes(image[44:48], "little") if fat_bits == 32 else 0
    if fat_bits == 32 and root_cluster < 2:
        raise IsoError("El Torito EFI FAT32 image has an invalid root cluster")
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


def _fat_next(image: bytes, geometry: dict[str, int], cluster: int) -> int:
    fat_start = geometry["reserved_sectors"] * geometry["bytes_per_sector"]
    bits = geometry["fat_bits"]
    if bits == 12:
        offset = cluster + cluster // 2
        if fat_start + offset + 2 > len(image):
            raise IsoError("EFI FAT12 chain leaves the allocation table")
        value = int.from_bytes(image[fat_start + offset : fat_start + offset + 2], "little")
        return (value >> 4 if cluster & 1 else value) & 0x0FFF
    if bits == 16:
        offset = cluster * 2
        if fat_start + offset + 2 > len(image):
            raise IsoError("EFI FAT16 chain leaves the allocation table")
        return int.from_bytes(image[fat_start + offset : fat_start + offset + 2], "little")
    offset = cluster * 4
    if fat_start + offset + 4 > len(image):
        raise IsoError("EFI FAT32 chain leaves the allocation table")
    return int.from_bytes(image[fat_start + offset : fat_start + offset + 4], "little") & 0x0FFFFFFF


def _fat_eoc(geometry: dict[str, int], cluster: int) -> bool:
    if geometry["fat_bits"] == 12:
        return cluster >= 0x0FF8
    if geometry["fat_bits"] == 16:
        return cluster >= 0xFFF8
    return cluster >= 0x0FFFFFF8


def _fat_cluster_chain(
    image: bytes, geometry: dict[str, int], first_cluster: int
) -> bytes:
    if first_cluster < 2:
        raise IsoError("EFI FAT entry has an invalid first cluster")
    cluster_bytes = geometry["sectors_per_cluster"] * geometry["bytes_per_sector"]
    first_data = geometry["first_data_sector"] * geometry["bytes_per_sector"]
    maximum_clusters = (
        geometry["total_sectors"] - geometry["first_data_sector"]
    ) // geometry["sectors_per_cluster"]
    seen: set[int] = set()
    chunks: list[bytes] = []
    cluster = first_cluster
    while True:
        if cluster in seen or cluster < 2 or len(seen) >= maximum_clusters:
            raise IsoError("EFI FAT cluster chain is cyclic or out of range")
        seen.add(cluster)
        start = first_data + (cluster - 2) * cluster_bytes
        end = start + cluster_bytes
        if end > geometry["image_bytes"]:
            raise IsoError("EFI FAT cluster leaves the boot image")
        chunks.append(image[start:end])
        next_cluster = _fat_next(image, geometry, cluster)
        if _fat_eoc(geometry, next_cluster):
            break
        cluster = next_cluster
    return b"".join(chunks)


def _fat_directory(
    image: bytes, geometry: dict[str, int], first_cluster: int | None
) -> list[dict[str, object]]:
    if first_cluster is None:
        start_sector = (
            geometry["reserved_sectors"]
            + geometry["fat_count"] * geometry["fat_sectors"]
        )
        start = start_sector * geometry["bytes_per_sector"]
        size = geometry["root_dir_sectors"] * geometry["bytes_per_sector"]
        content = image[start : start + size]
    else:
        content = _fat_cluster_chain(image, geometry, first_cluster)
    entries: list[dict[str, object]] = []
    for offset in range(0, len(content), 32):
        record = content[offset : offset + 32]
        if len(record) < 32 or record[0] == 0x00:
            break
        if record[0] == 0xE5 or record[11] == 0x0F or record[11] & 0x08:
            continue
        try:
            stem = record[0:8].decode("ascii", "strict").rstrip(" ")
            suffix = record[8:11].decode("ascii", "strict").rstrip(" ")
        except UnicodeDecodeError as error:
            raise IsoError("EFI FAT short name is not ASCII") from error
        name = stem if not suffix else f"{stem}.{suffix}"
        high_cluster = int.from_bytes(record[20:22], "little")
        low_cluster = int.from_bytes(record[26:28], "little")
        entries.append(
            {
                "name": name.upper(),
                "attributes": record[11],
                "first_cluster": (high_cluster << 16) | low_cluster,
                "size": int.from_bytes(record[28:32], "little"),
            }
        )
    return entries


def _find_fat_bootloader(image: bytes, geometry: dict[str, int]) -> bytes:
    directory_cluster: int | None = (
        geometry["root_cluster"] if geometry["fat_bits"] == 32 else None
    )
    components = ("EFI", "BOOT", "BOOTX64.EFI")
    selected: dict[str, object] | None = None
    for index, component in enumerate(components):
        matches = [
            entry
            for entry in _fat_directory(image, geometry, directory_cluster)
            if entry["name"] == component
        ]
        if len(matches) != 1:
            raise IsoError(
                f"El Torito EFI image has {len(matches)} matches for /{'/'.join(components[: index + 1])}"
            )
        selected = matches[0]
        is_directory = bool(int(selected["attributes"]) & 0x10)
        if index < len(components) - 1:
            if not is_directory:
                raise IsoError(f"EFI FAT component {component} is not a directory")
            directory_cluster = int(selected["first_cluster"])
        elif is_directory:
            raise IsoError("EFI/BOOT/BOOTX64.EFI is a directory")
    assert selected is not None
    size = int(selected["size"])
    if size <= 0:
        raise IsoError("EFI/BOOT/BOOTX64.EFI is empty")
    content = _fat_cluster_chain(image, geometry, int(selected["first_cluster"]))
    if size > len(content):
        raise IsoError("EFI/BOOT/BOOTX64.EFI is truncated")
    return content[:size]


def _validate_x86_64_efi_application(bootloader: bytes) -> None:
    if len(bootloader) < 64 or bootloader[:2] != b"MZ":
        raise IsoError("EFI/BOOT/BOOTX64.EFI lacks a DOS/PE header")
    pe_offset = int.from_bytes(bootloader[0x3C:0x40], "little")
    if pe_offset < 64 or pe_offset + 24 > len(bootloader):
        raise IsoError("EFI/BOOT/BOOTX64.EFI has an invalid PE header offset")
    if bootloader[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise IsoError("EFI/BOOT/BOOTX64.EFI lacks a PE signature")
    machine = int.from_bytes(bootloader[pe_offset + 4 : pe_offset + 6], "little")
    if machine != 0x8664:
        raise IsoError("EFI/BOOT/BOOTX64.EFI is not an x86_64 PE image")
    section_count = int.from_bytes(
        bootloader[pe_offset + 6 : pe_offset + 8], "little"
    )
    if section_count <= 0 or section_count > 96:
        raise IsoError("EFI/BOOT/BOOTX64.EFI has an invalid PE section count")
    optional_size = int.from_bytes(
        bootloader[pe_offset + 20 : pe_offset + 22], "little"
    )
    characteristics = int.from_bytes(
        bootloader[pe_offset + 22 : pe_offset + 24], "little"
    )
    if not characteristics & 0x0002:
        raise IsoError("EFI/BOOT/BOOTX64.EFI is not marked executable")
    optional_offset = pe_offset + 24
    if optional_size < 112 or optional_offset + optional_size > len(bootloader):
        raise IsoError("EFI/BOOT/BOOTX64.EFI has a truncated optional header")
    optional = bootloader[optional_offset : optional_offset + optional_size]
    if int.from_bytes(optional[0:2], "little") != 0x20B:
        raise IsoError("EFI/BOOT/BOOTX64.EFI is not PE32+")
    if int.from_bytes(optional[68:70], "little") != 10:
        raise IsoError("EFI/BOOT/BOOTX64.EFI is not an EFI application")
    entry_rva = int.from_bytes(optional[16:20], "little")
    if entry_rva == 0:
        raise IsoError("EFI/BOOT/BOOTX64.EFI has a zero executable entry point")
    section_alignment = int.from_bytes(optional[32:36], "little")
    file_alignment = int.from_bytes(optional[36:40], "little")
    image_size = int.from_bytes(optional[56:60], "little")
    headers_size = int.from_bytes(optional[60:64], "little")
    if (
        section_alignment == 0
        or section_alignment & (section_alignment - 1)
        or file_alignment == 0
        or file_alignment & (file_alignment - 1)
        or file_alignment > section_alignment
        or image_size == 0
    ):
        raise IsoError("EFI/BOOT/BOOTX64.EFI has invalid PE image alignment")

    section_table_offset = optional_offset + optional_size
    section_table_end = section_table_offset + section_count * 40
    if (
        section_table_end > len(bootloader)
        or headers_size < section_table_end
        or headers_size > len(bootloader)
    ):
        raise IsoError("EFI/BOOT/BOOTX64.EFI has a truncated PE section table")
    executable_entry = False
    for index in range(section_count):
        offset = section_table_offset + index * 40
        section = bootloader[offset : offset + 40]
        virtual_size = int.from_bytes(section[8:12], "little")
        virtual_address = int.from_bytes(section[12:16], "little")
        raw_size = int.from_bytes(section[16:20], "little")
        raw_offset = int.from_bytes(section[20:24], "little")
        section_characteristics = int.from_bytes(section[36:40], "little")
        admitted_size = max(virtual_size, raw_size)
        if virtual_address + admitted_size > image_size:
            raise IsoError("EFI/BOOT/BOOTX64.EFI has a PE section outside its image")
        if raw_size and (
            raw_offset > len(bootloader)
            or raw_size > len(bootloader) - raw_offset
        ):
            raise IsoError("EFI/BOOT/BOOTX64.EFI has a truncated PE section")
        if (
            admitted_size
            and virtual_address <= entry_rva < virtual_address + admitted_size
            and section_characteristics & 0x20000000
        ):
            relative_entry = entry_rva - virtual_address
            executable_entry = executable_entry or (
                relative_entry < raw_size
                and raw_offset + relative_entry < len(bootloader)
            )
    if not executable_entry:
        raise IsoError(
            "EFI/BOOT/BOOTX64.EFI entry point is not in a file-backed executable section"
        )


def _valid_multiboot2_headers(kernel: bytes) -> list[int]:
    magic = 0xE85250D6
    scan_limit = min(len(kernel), 32768)
    valid: list[int] = []
    for offset in range(0, max(0, scan_limit - 15), 8):
        if int.from_bytes(kernel[offset : offset + 4], "little") != magic:
            continue
        architecture = int.from_bytes(kernel[offset + 4 : offset + 8], "little")
        header_length = int.from_bytes(kernel[offset + 8 : offset + 12], "little")
        checksum = int.from_bytes(kernel[offset + 12 : offset + 16], "little")
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
            tag_type = int.from_bytes(kernel[cursor : cursor + 2], "little")
            tag_flags = int.from_bytes(kernel[cursor + 2 : cursor + 4], "little")
            tag_size = int.from_bytes(kernel[cursor + 4 : cursor + 8], "little")
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


def _validate_x86_64_multiboot2_kernel(kernel: bytes) -> None:
    if len(kernel) < 64 or kernel[0:4] != b"\x7fELF":
        raise IsoError("/boot/kernel.elf is not an ELF image")
    if kernel[4:7] != b"\x02\x01\x01":
        raise IsoError("/boot/kernel.elf is not little-endian ELF64")
    if int.from_bytes(kernel[16:18], "little") != 2:
        raise IsoError("/boot/kernel.elf is not an executable ELF")
    if int.from_bytes(kernel[18:20], "little") != 62:
        raise IsoError("/boot/kernel.elf is not x86_64")
    if int.from_bytes(kernel[20:24], "little") != 1:
        raise IsoError("/boot/kernel.elf has an unsupported ELF version")
    entry = int.from_bytes(kernel[24:32], "little")
    if entry == 0:
        raise IsoError("/boot/kernel.elf has a zero entry point")
    program_offset = int.from_bytes(kernel[32:40], "little")
    header_size = int.from_bytes(kernel[52:54], "little")
    program_entry_size = int.from_bytes(kernel[54:56], "little")
    program_count = int.from_bytes(kernel[56:58], "little")
    if header_size != 64 or program_entry_size != 56 or not 1 <= program_count <= 128:
        raise IsoError("/boot/kernel.elf has an invalid ELF program-header table")
    program_end = program_offset + program_entry_size * program_count
    if program_offset < header_size or program_end > len(kernel):
        raise IsoError("/boot/kernel.elf has a truncated ELF program-header table")

    load_segments = 0
    executable_entry = False
    for index in range(program_count):
        offset = program_offset + index * program_entry_size
        program = kernel[offset : offset + program_entry_size]
        if int.from_bytes(program[0:4], "little") != 1:
            continue
        load_segments += 1
        flags = int.from_bytes(program[4:8], "little")
        file_offset = int.from_bytes(program[8:16], "little")
        virtual_address = int.from_bytes(program[16:24], "little")
        file_size = int.from_bytes(program[32:40], "little")
        memory_size = int.from_bytes(program[40:48], "little")
        alignment = int.from_bytes(program[48:56], "little")
        if (
            file_size > memory_size
            or file_offset > len(kernel)
            or file_size > len(kernel) - file_offset
        ):
            raise IsoError("/boot/kernel.elf has a truncated PT_LOAD segment")
        if memory_size == 0:
            raise IsoError("/boot/kernel.elf has an empty PT_LOAD segment")
        if alignment not in (0, 1) and (
            alignment & (alignment - 1)
            or virtual_address % alignment != file_offset % alignment
        ):
            raise IsoError("/boot/kernel.elf has invalid PT_LOAD alignment")
        if (
            flags & 0x1
            and virtual_address <= entry < virtual_address + file_size
        ):
            executable_entry = True
    if load_segments == 0:
        raise IsoError("/boot/kernel.elf has no PT_LOAD segments")
    if not executable_entry:
        raise IsoError("/boot/kernel.elf entry point is not file-backed executable code")

    multiboot_headers = _valid_multiboot2_headers(kernel)
    if len(multiboot_headers) != 1:
        raise IsoError(
            f"/boot/kernel.elf has {len(multiboot_headers)} valid Multiboot2 "
            "headers; expected one"
        )


def _el_torito_entries(catalog: bytes) -> list[tuple[int, bytes]]:
    if len(catalog) != LOGICAL_BLOCK_SIZE:
        raise IsoError("El Torito boot catalog is truncated")
    validation = catalog[0:32]
    if validation[0] != 0x01 or validation[30:32] != b"\x55\xaa":
        raise IsoError("El Torito validation entry is malformed")
    checksum = sum(struct.unpack("<16H", validation)) & 0xFFFF
    if checksum != 0:
        raise IsoError("El Torito validation checksum is invalid")
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
                raise IsoError("El Torito section header has an invalid entry count")
            for section_index in range(index + 1, index + 1 + count):
                section_entry = catalog[
                    section_index * 32 : (section_index + 1) * 32
                ]
                if section_entry[0] not in (0x00, 0x88):
                    raise IsoError("El Torito section contains an invalid boot indicator")
                entries.append((platform, section_entry))
            index += 1 + count
            if indicator == 0x91:
                break
            continue
        if indicator == 0x44:
            index += 1
            continue
        raise IsoError("El Torito catalog contains an unexpected entry")
    return entries


def inspect_image(data: bytes | mmap.mmap) -> dict[str, object]:
    if len(data) < MIN_ISO_BYTES or len(data) > MAX_ISO_BYTES:
        raise IsoError(f"ISO size outside {MIN_ISO_BYTES}..{MAX_ISO_BYTES} bytes")
    if len(data) % LOGICAL_BLOCK_SIZE:
        raise IsoError("ISO length is not a multiple of 2048 bytes")

    primary: bytes | None = None
    boot_records: list[bytes] = []
    terminated = False
    for index in range(MAX_VOLUME_DESCRIPTORS):
        lba = 16 + index
        descriptor = data[
            lba * LOGICAL_BLOCK_SIZE : (lba + 1) * LOGICAL_BLOCK_SIZE
        ]
        if len(descriptor) != LOGICAL_BLOCK_SIZE:
            raise IsoError("ISO volume-descriptor sequence is truncated")
        if descriptor[1:6] != b"CD001" or descriptor[6] != 1:
            raise IsoError("ISO volume descriptor has an invalid identifier or version")
        kind = descriptor[0]
        if kind == 0 and descriptor[7:39].rstrip(b"\x00 ") == EL_TORITO_SYSTEM_ID:
            boot_records.append(descriptor)
        elif kind == 1:
            if primary is not None:
                raise IsoError("ISO contains more than one primary volume descriptor")
            primary = descriptor
        elif kind == 255:
            terminated = True
            break
    if not terminated:
        raise IsoError("ISO volume-descriptor sequence lacks a terminator")
    if primary is None:
        raise IsoError("ISO lacks a primary volume descriptor")
    if len(boot_records) != 1:
        raise IsoError(f"ISO has {len(boot_records)} El Torito boot records; expected one")

    try:
        volume_id = primary[40:72].decode("ascii", "strict").rstrip(" ")
    except UnicodeDecodeError as error:
        raise IsoError("ISO volume identifier is not ASCII") from error
    if volume_id != VOLUME_ID:
        raise IsoError(f"ISO volume identifier is {volume_id!r}, expected {VOLUME_ID!r}")
    volume_blocks = _u32_both(primary, 80, "ISO volume-space size")
    block_size = _u16_both(primary, 128, "ISO logical-block size")
    if block_size != LOGICAL_BLOCK_SIZE:
        raise IsoError(f"ISO logical-block size is {block_size}, expected 2048")
    volume_bytes = volume_blocks * block_size
    if volume_bytes != len(data):
        raise IsoError(
            f"ISO byte length {len(data)} differs from volume-space length {volume_bytes}"
        )
    root_length = primary[156]
    if root_length < 34 or 156 + root_length > len(primary):
        raise IsoError("ISO primary volume descriptor has an invalid root record")
    root = _directory_record(primary[156 : 156 + root_length], volume_bytes, "/")
    if not int(root["flags"]) & 0x02 or root["name"] != b"\x00":
        raise IsoError("ISO root record is not the canonical root directory")

    boot_catalog_lba = int.from_bytes(boot_records[0][71:75], "little")
    catalog = _extent(
        data,
        boot_catalog_lba,
        LOGICAL_BLOCK_SIZE,
        volume_bytes,
        "El Torito boot catalog",
    )
    uefi_entries = [
        entry for platform, entry in _el_torito_entries(catalog) if platform == EFI_PLATFORM_ID
    ]
    if len(uefi_entries) != 1:
        raise IsoError(f"El Torito catalog has {len(uefi_entries)} UEFI entries; expected one")
    uefi = uefi_entries[0]
    if uefi[0] != 0x88:
        raise IsoError("El Torito UEFI entry is not bootable")
    if uefi[1] != NO_EMULATION_MEDIA_TYPE:
        raise IsoError("El Torito UEFI entry is not no-emulation media")
    boot_load_sectors = int.from_bytes(uefi[6:8], "little")
    boot_image_lba = int.from_bytes(uefi[8:12], "little")
    if boot_load_sectors <= 0:
        raise IsoError("El Torito UEFI entry has a zero load-sector count")
    boot_image_start = boot_image_lba * LOGICAL_BLOCK_SIZE
    boot_image_available = volume_bytes - boot_image_start
    boot_sector = _extent(
        data,
        boot_image_lba,
        512,
        volume_bytes,
        "El Torito EFI boot sector",
    )
    geometry = _fat_geometry(boot_sector, available_bytes=boot_image_available)
    boot_image_bytes = geometry["image_bytes"]
    if boot_load_sectors * 512 > boot_image_bytes:
        raise IsoError("El Torito load-sector count exceeds the EFI boot image")
    boot_image = _extent(
        data,
        boot_image_lba,
        boot_image_bytes,
        volume_bytes,
        "El Torito EFI boot image",
    )
    bootloader = _find_fat_bootloader(boot_image, geometry)
    _validate_x86_64_efi_application(bootloader)

    kernel, _kernel_record = _find_iso_path(data, root, KERNEL_PATH, volume_bytes)
    _validate_x86_64_multiboot2_kernel(kernel)

    grub_config, _config_record = _find_iso_path(
        data, root, GRUB_CONFIG_PATH, volume_bytes
    )
    try:
        config_text = grub_config.decode("utf-8")
    except UnicodeDecodeError as error:
        raise IsoError("/boot/grub/grub.cfg is not UTF-8") from error
    if "\x00" in config_text or re.search(r"(?m)^\s*search\b", config_text):
        raise IsoError("ISO GRUB configuration must not search for a filesystem UUID")
    if config_text.count("@"):
        raise IsoError("ISO GRUB configuration contains an unresolved template marker")
    multiboot_lines = re.findall(
        r"(?m)^\s*multiboot2\s+/boot/kernel\.elf\s*$", config_text
    )
    if len(multiboot_lines) != 1:
        raise IsoError(
            "ISO GRUB configuration must contain exactly one argument-free "
            "multiboot2 /boot/kernel.elf command"
        )
    if _sha256(grub_config) != EXPECTED_GRUB_CONFIG_SHA256:
        raise IsoError(
            "ISO GRUB configuration does not match the exact committed grub-iso.cfg"
        )

    return {
        "schema": SCHEMA,
        "bytes": len(data),
        "sha256": _sha256(data),
        "logical_block_size": block_size,
        "volume_blocks": volume_blocks,
        "volume_id": volume_id,
        "boot_catalog_lba": boot_catalog_lba,
        "el_torito_platform_id": EFI_PLATFORM_ID,
        "el_torito_media_type": NO_EMULATION_MEDIA_TYPE,
        "el_torito_load_sectors": boot_load_sectors,
        "efi_boot_image_lba": boot_image_lba,
        "efi_boot_image_bytes": boot_image_bytes,
        "efi_boot_image_sha256": _sha256(boot_image),
        "efi_bootloader_path": "/EFI/BOOT/BOOTX64.EFI",
        "efi_bootloader_bytes": len(bootloader),
        "efi_bootloader_sha256": _sha256(bootloader),
        "kernel_path": "/boot/kernel.elf",
        "kernel_bytes": len(kernel),
        "kernel_sha256": _sha256(kernel),
        "grub_config_path": "/boot/grub/grub.cfg",
        "grub_config_bytes": len(grub_config),
        "grub_config_sha256": _sha256(grub_config),
    }


def inspect_path(path: Path) -> dict[str, object]:
    descriptor = _open_pinned_regular(path, nofollow=True)
    try:
        before = os.fstat(descriptor)
        result = inspect_descriptor(descriptor, str(path))
        _require_path_identity(path, before, "ISO input")
        _require_descriptor_identity(descriptor, before, "ISO input")
        return result
    finally:
        os.close(descriptor)


def inspect_descriptor(
    descriptor: int, label: str = "pinned ISO", maximum: int = MAX_ISO_BYTES
) -> dict[str, object]:
    """Inspect one already-open regular ISO without resolving its pathname again."""

    mapping: mmap.mmap | None = None
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise IsoError(f"{label} descriptor is not a regular file")
        admitted_maximum = min(maximum, MAX_ISO_BYTES)
        if before.st_size < MIN_ISO_BYTES or before.st_size > admitted_maximum:
            raise IsoError(
                f"{label} size outside {MIN_ISO_BYTES}..{admitted_maximum} bytes"
            )
        mapping = mmap.mmap(descriptor, before.st_size, access=mmap.ACCESS_READ)
        result = inspect_image(mapping)
        after = _require_descriptor_identity(descriptor, before, label)
    except IsoError:
        raise
    except (OSError, ValueError) as error:
        raise IsoError(f"cannot map or read {label} descriptor: {error}") from error
    finally:
        if mapping is not None:
            mapping.close()
    if _file_identity(before) != _file_identity(after):
        raise IsoError(f"{label} changed while its descriptor was read")
    return result


def _reject_output_link_or_special(path: Path) -> None:
    try:
        current = os.stat(path, follow_symlinks=False)
    except FileNotFoundError:
        return
    except OSError as error:
        raise IsoError(f"cannot inspect ISO output path {path}: {error}") from error
    if stat.S_ISLNK(current.st_mode):
        raise IsoError(f"refusing ISO output symlink: {path}")
    if not stat.S_ISREG(current.st_mode):
        raise IsoError(f"ISO output exists and is not a regular file: {path}")


def publish_path(source: Path, output: Path) -> dict[str, object]:
    output = Path(os.path.abspath(output))
    source_absolute = Path(os.path.abspath(source))
    if source_absolute == output:
        raise IsoError("ISO publish source and output must be distinct paths")
    source_descriptor = _open_pinned_regular(source_absolute, nofollow=True)
    try:
        source_state = os.fstat(source_descriptor)
        metadata = inspect_descriptor(source_descriptor, str(source_absolute))
        _require_path_identity(source_absolute, source_state, "ISO source")
        _require_descriptor_identity(source_descriptor, source_state, "ISO source")

        return _publish_descriptor(
            source_absolute,
            source_descriptor,
            source_state,
            output,
            metadata,
        )
    finally:
        os.close(source_descriptor)


def _publish_descriptor(
    source: Path,
    source_descriptor: int,
    source_state: os.stat_result,
    output: Path,
    metadata: dict[str, object],
) -> dict[str, object]:
    parent = output.parent
    try:
        parent_state = os.stat(parent, follow_symlinks=False)
    except OSError as error:
        raise IsoError(f"ISO output parent is unavailable: {parent}: {error}") from error
    if stat.S_ISLNK(parent_state.st_mode) or not stat.S_ISDIR(parent_state.st_mode):
        raise IsoError(f"ISO output parent is not a non-symlink directory: {parent}")
    _reject_output_link_or_special(output)

    descriptor = -1
    temporary_name = ""
    try:
        descriptor, temporary_name = tempfile.mkstemp(
            prefix=".ostadix-iso-publish.", suffix=".tmp", dir=parent
        )
        copied_sha256 = _stream_copy_descriptor(
            source_descriptor, descriptor, source_state.st_size
        )
        if copied_sha256 != metadata["sha256"]:
            raise IsoError("published ISO copy digest differs from the inspected source")
        _require_path_identity(source, source_state, "ISO source")
        _require_descriptor_identity(source_descriptor, source_state, "ISO source")
        os.fchmod(descriptor, 0o444)
        os.fsync(descriptor)
        private_metadata = inspect_descriptor(descriptor, "private ISO output")
        if private_metadata != metadata:
            raise IsoError("private ISO output identity differs from the inspected source")
        os.close(descriptor)
        descriptor = -1
        _require_path_identity(source, source_state, "ISO source")
        _require_descriptor_identity(source_descriptor, source_state, "ISO source")
        _reject_output_link_or_special(output)
        os.replace(temporary_name, output)
        temporary_name = ""
        try:
            parent_descriptor = os.open(parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
            try:
                os.fsync(parent_descriptor)
            finally:
                os.close(parent_descriptor)
        except OSError:
            # Some filesystems do not support directory fsync. The file was
            # already atomically renamed, and a final strict read follows.
            pass
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        if temporary_name:
            try:
                os.unlink(temporary_name)
            except FileNotFoundError:
                pass
    published = inspect_path(output)
    if published != metadata:
        raise IsoError("published ISO identity differs from the inspected candidate")
    return published


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Strictly inspect or safely publish an OSTADIX boot ISO"
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    inspect_parser = subparsers.add_parser("inspect", help="inspect one ISO")
    inspect_parser.add_argument("path", type=Path)
    publish_parser = subparsers.add_parser(
        "publish", help="inspect a private candidate and atomically publish it"
    )
    publish_parser.add_argument("--source", required=True, type=Path)
    publish_parser.add_argument("--output", required=True, type=Path)
    return parser


def main(argv: list[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        if arguments.command == "inspect":
            result = inspect_path(arguments.path)
        else:
            result = publish_path(arguments.source, arguments.output)
    except IsoError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
