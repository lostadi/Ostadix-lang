#!/usr/bin/env python3
"""Build and inspect deterministic OSTADIX GPT boot-media containers.

This tool deliberately owns only the outer GPT container.  A platform media
builder supplies a complete EFI System Partition image, and this module binds
those exact bytes into a reproducible disk layout with deterministic UUIDs.
It does not write physical devices.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import struct
import sys
import uuid
import zlib


SCHEMA = "ostadix.boot-media/v1"
SECTOR_SIZE = 512
PARTITION_ENTRY_COUNT = 128
PARTITION_ENTRY_SIZE = 128
PARTITION_TABLE_BYTES = PARTITION_ENTRY_COUNT * PARTITION_ENTRY_SIZE
PARTITION_TABLE_SECTORS = PARTITION_TABLE_BYTES // SECTOR_SIZE
ESP_START_LBA = 2048
TAIL_GAP_SECTORS = 2048
MAX_ESP_BYTES = 512 * 1024 * 1024
MAX_IMAGE_BYTES = (
    ESP_START_LBA + MAX_ESP_BYTES // SECTOR_SIZE + TAIL_GAP_SECTORS
) * SECTOR_SIZE
GPT_SIGNATURE = b"EFI PART"
GPT_REVISION = 0x0001_0000
GPT_HEADER_SIZE = 92
PROTECTIVE_MBR_TYPE = 0xEE
ESP_TYPE_GUID = uuid.UUID("c12a7328-f81f-11d2-ba4b-00a0c93ec93b")
OSTADIX_MEDIA_NAMESPACE = uuid.UUID("bf9adb46-c30e-5cc0-94ec-a28553635412")


class MediaError(ValueError):
    """The supplied media container is malformed or outside bounded v1."""


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _read_bounded(path: Path, maximum: int) -> bytes:
    try:
        size = path.stat().st_size
    except OSError as error:
        raise MediaError(f"cannot stat {path}: {error}") from error
    if not path.is_file():
        raise MediaError(f"not a regular file: {path}")
    if size <= 0 or size > maximum:
        raise MediaError(f"file size outside 1..{maximum} bytes: {path}")
    try:
        return path.read_bytes()
    except OSError as error:
        raise MediaError(f"cannot read {path}: {error}") from error


def _derived_uuid(kind: str, digest: str) -> uuid.UUID:
    return uuid.uuid5(OSTADIX_MEDIA_NAMESPACE, f"{kind}:{digest}")


def _partition_entry(first_lba: int, last_lba: int, unique: uuid.UUID) -> bytes:
    name = "OSTADIX".encode("utf-16-le")
    if len(name) > 72:
        raise AssertionError("partition name exceeds GPT field")
    return b"".join(
        (
            ESP_TYPE_GUID.bytes_le,
            unique.bytes_le,
            struct.pack("<QQQ", first_lba, last_lba, 0),
            name.ljust(72, b"\0"),
        )
    )


def _gpt_header(
    *,
    current_lba: int,
    backup_lba: int,
    first_usable: int,
    last_usable: int,
    disk_guid: uuid.UUID,
    entries_lba: int,
    entries_crc: int,
) -> bytes:
    header = bytearray(SECTOR_SIZE)
    struct.pack_into(
        "<8sIIIIQQQQ16sQIII",
        header,
        0,
        GPT_SIGNATURE,
        GPT_REVISION,
        GPT_HEADER_SIZE,
        0,
        0,
        current_lba,
        backup_lba,
        first_usable,
        last_usable,
        disk_guid.bytes_le,
        entries_lba,
        PARTITION_ENTRY_COUNT,
        PARTITION_ENTRY_SIZE,
        entries_crc,
    )
    crc = zlib.crc32(header[:GPT_HEADER_SIZE]) & 0xFFFF_FFFF
    struct.pack_into("<I", header, 16, crc)
    return bytes(header)


def build_image(esp: bytes) -> tuple[bytes, dict[str, object]]:
    if not esp or len(esp) > MAX_ESP_BYTES:
        raise MediaError(f"ESP size must be within 1..{MAX_ESP_BYTES} bytes")
    if len(esp) % SECTOR_SIZE != 0:
        raise MediaError("ESP image size must be a multiple of 512 bytes")

    esp_digest = _sha256(esp)
    disk_guid = _derived_uuid("disk", esp_digest)
    partition_guid = _derived_uuid("esp", esp_digest)
    esp_sectors = len(esp) // SECTOR_SIZE
    esp_last_lba = ESP_START_LBA + esp_sectors - 1
    total_sectors = ESP_START_LBA + esp_sectors + TAIL_GAP_SECTORS
    backup_header_lba = total_sectors - 1
    backup_entries_lba = backup_header_lba - PARTITION_TABLE_SECTORS
    first_usable = 2 + PARTITION_TABLE_SECTORS
    last_usable = backup_entries_lba - 1
    if esp_last_lba > last_usable:
        raise AssertionError("ESP overlaps backup GPT table")

    entries = bytearray(PARTITION_TABLE_BYTES)
    entry = _partition_entry(ESP_START_LBA, esp_last_lba, partition_guid)
    entries[: len(entry)] = entry
    entries_crc = zlib.crc32(entries) & 0xFFFF_FFFF

    primary_header = _gpt_header(
        current_lba=1,
        backup_lba=backup_header_lba,
        first_usable=first_usable,
        last_usable=last_usable,
        disk_guid=disk_guid,
        entries_lba=2,
        entries_crc=entries_crc,
    )
    backup_header = _gpt_header(
        current_lba=backup_header_lba,
        backup_lba=1,
        first_usable=first_usable,
        last_usable=last_usable,
        disk_guid=disk_guid,
        entries_lba=backup_entries_lba,
        entries_crc=entries_crc,
    )

    image = bytearray(total_sectors * SECTOR_SIZE)
    image[510:512] = b"\x55\xaa"
    mbr_size = min(total_sectors - 1, 0xFFFF_FFFF)
    struct.pack_into("<B3sB3sII", image, 446, 0, b"\0\x02\0", PROTECTIVE_MBR_TYPE, b"\xff\xff\xff", 1, mbr_size)
    image[SECTOR_SIZE : 2 * SECTOR_SIZE] = primary_header
    image[2 * SECTOR_SIZE : 2 * SECTOR_SIZE + len(entries)] = entries
    esp_offset = ESP_START_LBA * SECTOR_SIZE
    image[esp_offset : esp_offset + len(esp)] = esp
    backup_entries_offset = backup_entries_lba * SECTOR_SIZE
    image[backup_entries_offset : backup_entries_offset + len(entries)] = entries
    backup_header_offset = backup_header_lba * SECTOR_SIZE
    image[backup_header_offset : backup_header_offset + SECTOR_SIZE] = backup_header

    data = bytes(image)
    metadata: dict[str, object] = {
        "schema": SCHEMA,
        "bytes": len(data),
        "sha256": _sha256(data),
        "disk_guid": str(disk_guid),
        "partition_guid": str(partition_guid),
        "esp_sha256": esp_digest,
        "esp_bytes": len(esp),
        "esp_first_lba": ESP_START_LBA,
        "esp_last_lba": esp_last_lba,
        "sector_size": SECTOR_SIZE,
    }
    return data, metadata


def _validated_header(image: bytes, lba: int) -> dict[str, object]:
    offset = lba * SECTOR_SIZE
    if offset + SECTOR_SIZE > len(image):
        raise MediaError("GPT header lies outside image")
    sector = image[offset : offset + SECTOR_SIZE]
    if sector[:8] != GPT_SIGNATURE:
        raise MediaError(f"missing GPT signature at LBA {lba}")
    revision, size, stored_crc, reserved = struct.unpack_from("<IIII", sector, 8)
    if revision != GPT_REVISION or size != GPT_HEADER_SIZE or reserved != 0:
        raise MediaError("unsupported GPT header revision, size, or reserved field")
    if any(sector[size:]):
        raise MediaError(f"GPT header reserved tail is not zero at LBA {lba}")
    header = bytearray(sector[:size])
    struct.pack_into("<I", header, 16, 0)
    if zlib.crc32(header) & 0xFFFF_FFFF != stored_crc:
        raise MediaError(f"GPT header CRC mismatch at LBA {lba}")
    current, backup, first, last = struct.unpack_from("<QQQQ", sector, 24)
    if current != lba or backup >= len(image) // SECTOR_SIZE or first > last:
        raise MediaError("invalid GPT header topology")
    disk_guid = uuid.UUID(bytes_le=sector[56:72])
    entries_lba, count, entry_size, entries_crc = struct.unpack_from("<QIII", sector, 72)
    if count != PARTITION_ENTRY_COUNT or entry_size != PARTITION_ENTRY_SIZE:
        raise MediaError("unsupported GPT partition-table geometry")
    entries_offset = entries_lba * SECTOR_SIZE
    entries_end = entries_offset + PARTITION_TABLE_BYTES
    if entries_end > len(image):
        raise MediaError("GPT partition table lies outside image")
    entries = image[entries_offset:entries_end]
    if zlib.crc32(entries) & 0xFFFF_FFFF != entries_crc:
        raise MediaError("GPT partition-table CRC mismatch")
    return {
        "current": current,
        "backup": backup,
        "first_usable": first,
        "last_usable": last,
        "disk_guid": disk_guid,
        "entries_lba": entries_lba,
        "entries": entries,
    }


def inspect_image(image: bytes) -> dict[str, object]:
    minimum = (ESP_START_LBA + PARTITION_TABLE_SECTORS + 2) * SECTOR_SIZE
    if len(image) < minimum or len(image) % SECTOR_SIZE != 0:
        raise MediaError("image is too small or not sector aligned")
    if image[510:512] != b"\x55\xaa":
        raise MediaError("missing protective MBR")

    sectors = len(image) // SECTOR_SIZE
    boot, first_chs, kind, last_chs, first_mbr_lba, mbr_sectors = struct.unpack_from(
        "<B3sB3sII", image, 446
    )
    expected_mbr_sectors = min(sectors - 1, 0xFFFF_FFFF)
    if (
        boot != 0
        or first_chs != b"\0\x02\0"
        or kind != PROTECTIVE_MBR_TYPE
        or last_chs != b"\xff\xff\xff"
        or first_mbr_lba != 1
        or mbr_sectors != expected_mbr_sectors
        or any(image[462:510])
    ):
        raise MediaError("invalid protective MBR topology")
    if any(image[:446]):
        raise MediaError("protective MBR reserved region is not zero")

    primary = _validated_header(image, 1)
    backup = _validated_header(image, sectors - 1)
    if primary["backup"] != sectors - 1 or backup["backup"] != 1:
        raise MediaError("primary and backup GPT headers do not mirror each other")
    if primary["disk_guid"] != backup["disk_guid"] or primary["entries"] != backup["entries"]:
        raise MediaError("primary and backup GPT metadata differ")
    if (
        primary["entries_lba"] != 2
        or backup["entries_lba"] != sectors - 1 - PARTITION_TABLE_SECTORS
        or primary["first_usable"] != 2 + PARTITION_TABLE_SECTORS
        or backup["first_usable"] != primary["first_usable"]
        or primary["last_usable"] != sectors - 2 - PARTITION_TABLE_SECTORS
        or backup["last_usable"] != primary["last_usable"]
    ):
        raise MediaError("GPT geometry differs from bounded OSTADIX v1")

    entry = primary["entries"][:PARTITION_ENTRY_SIZE]
    if uuid.UUID(bytes_le=entry[:16]) != ESP_TYPE_GUID:
        raise MediaError("first partition is not an EFI System Partition")
    partition_guid = uuid.UUID(bytes_le=entry[16:32])
    first_lba, last_lba, attributes = struct.unpack_from("<QQQ", entry, 32)
    if attributes != 0 or first_lba != ESP_START_LBA or last_lba < first_lba:
        raise MediaError("invalid OSTADIX ESP range or attributes")
    if last_lba > int(primary["last_usable"]):
        raise MediaError("ESP extends beyond usable GPT range")
    if sectors != last_lba + 1 + TAIL_GAP_SECTORS:
        raise MediaError("disk tail geometry differs from bounded OSTADIX v1")
    if any(primary["entries"][PARTITION_ENTRY_SIZE:]):
        raise MediaError("OSTADIX v1 admits exactly one partition")
    expected_name = "OSTADIX".encode("utf-16-le").ljust(72, b"\0")
    if entry[56:] != expected_name:
        raise MediaError("OSTADIX ESP partition name or padding differs from bounded v1")

    esp_offset = first_lba * SECTOR_SIZE
    esp_end = (last_lba + 1) * SECTOR_SIZE
    primary_entries_end = (2 + PARTITION_TABLE_SECTORS) * SECTOR_SIZE
    if any(image[primary_entries_end:esp_offset]):
        raise MediaError("pre-ESP reserved padding is not zero")
    backup_entries_offset = int(backup["entries_lba"]) * SECTOR_SIZE
    if any(image[esp_end:backup_entries_offset]):
        raise MediaError("post-ESP reserved tail is not zero")
    esp = image[esp_offset:esp_end]
    if len(esp) < 512 or esp[510:512] != b"\x55\xaa":
        raise MediaError("ESP does not contain a FAT-compatible boot signature")
    esp_digest = _sha256(esp)
    disk_guid = primary["disk_guid"]
    if disk_guid != _derived_uuid("disk", esp_digest):
        raise MediaError("disk GUID is not bound to the ESP digest")
    if partition_guid != _derived_uuid("esp", esp_digest):
        raise MediaError("partition GUID is not bound to the ESP digest")

    return {
        "schema": SCHEMA,
        "bytes": len(image),
        "sha256": _sha256(image),
        "disk_guid": str(disk_guid),
        "partition_guid": str(partition_guid),
        "esp_sha256": esp_digest,
        "esp_bytes": len(esp),
        "esp_first_lba": first_lba,
        "esp_last_lba": last_lba,
        "sector_size": SECTOR_SIZE,
    }


def _require_safe_output(path: Path) -> None:
    """Reject an existing output unless it is an ordinary regular file.

    ``lstat`` is intentional: a symlink to a regular file is still an unsafe
    output identity.  The eventual rename replaces the directory entry and
    never opens the existing destination, so no device/FIFO target is followed.
    """

    try:
        mode = path.lstat().st_mode
    except FileNotFoundError:
        return
    except OSError as error:
        raise MediaError(f"cannot inspect output path {path}: {error}") from error
    if not stat.S_ISREG(mode):
        raise MediaError(f"output exists and is not a regular file: {path}")


def _write_atomic(path: Path, data: bytes) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        raise MediaError(f"cannot create output directory {path.parent}: {error}") from error
    _require_safe_output(path)
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    temporary_created = False
    try:
        try:
            stream = temporary.open("xb")
            temporary_created = True
        except OSError as error:
            raise MediaError(f"cannot create temporary output {temporary}: {error}") from error
        with stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        _require_safe_output(path)
        try:
            os.replace(temporary, path)
        except OSError as error:
            raise MediaError(f"cannot replace output {path}: {error}") from error
    finally:
        if temporary_created:
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass


def _emit(metadata: dict[str, object]) -> None:
    print(json.dumps(metadata, sort_keys=True, separators=(",", ":")))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    pack = commands.add_parser("pack", help="bind one ESP image into deterministic GPT media")
    pack.add_argument("--esp", type=Path, required=True)
    pack.add_argument("--output", type=Path, required=True)
    inspect = commands.add_parser("inspect", help="strictly validate OSTADIX GPT media")
    inspect.add_argument("image", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "pack":
            image, metadata = build_image(_read_bounded(args.esp, MAX_ESP_BYTES))
            _write_atomic(args.output, image)
            _emit(metadata)
        else:
            _emit(inspect_image(_read_bounded(args.image, MAX_IMAGE_BYTES)))
    except MediaError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
