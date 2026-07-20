#!/usr/bin/env python3
"""Deterministically pack bounded files into an immutable OVFSIMG1 image."""

from __future__ import annotations

import argparse
import hashlib
import os
import struct
import tempfile
from pathlib import Path

MAGIC = b"OVFSIMG1"
VERSION = 1
HEADER_SIZE = 128
ENTRY_SIZE = 128
PAGE_SIZE = 4096
MAX_FILES = 16
MAX_FILE_SIZE = 1024 * 1024
MAX_IMAGE_SIZE = 64 * 1024
READ = 1
EXEC = 2
CORPUS = 4

HEADER_PREFIX = struct.Struct("<8sIIIIQQQ")
ENTRY_PREFIX = struct.Struct("<HHIQQ32s64s8s")


def align_up(value: int, alignment: int) -> int:
    return (value + alignment - 1) & ~(alignment - 1)


def canonical_path(raw: str) -> bytes:
    try:
        encoded = raw.encode("utf-8")
    except UnicodeEncodeError as exc:
        raise ValueError(f"path is not UTF-8: {raw!r}") from exc
    if not (1 <= len(encoded) <= 63) or not raw.startswith("/") or raw.endswith("/"):
        raise ValueError(f"noncanonical OVFS path: {raw!r}")
    components = raw.split("/")[1:]
    if any(not component or component in (".", "..") for component in components):
        raise ValueError(f"noncanonical OVFS path: {raw!r}")
    if any(byte == 0 or byte < 0x20 or byte == 0x7F for byte in encoded):
        raise ValueError(f"control byte in OVFS path: {raw!r}")
    return encoded


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output_dir", type=Path)
    parser.add_argument(
        "--entry",
        action="append",
        nargs=3,
        metavar=("OVFS_PATH", "HOST_FILE", "FLAGS"),
        required=True,
    )
    args = parser.parse_args()
    if not (1 <= len(args.entry) <= MAX_FILES):
        raise SystemExit(f"OVFS requires 1..{MAX_FILES} entries")

    inputs: list[tuple[bytes, Path, int, bytes]] = []
    for raw_path, host_file, raw_flags in args.entry:
        path = canonical_path(raw_path)
        flags = int(raw_flags, 0)
        if not (flags & READ) or flags & ~(READ | EXEC | CORPUS):
            raise SystemExit(f"invalid read-only flags for {raw_path}: {flags}")
        if flags & EXEC and flags & CORPUS:
            raise SystemExit(f"corpus file cannot carry execute authority: {raw_path}")
        source = Path(host_file)
        payload = source.read_bytes()
        if len(payload) > MAX_FILE_SIZE:
            raise SystemExit(f"file exceeds 1 MiB: {source}")
        inputs.append((path, source, flags, payload))
    inputs.sort(key=lambda item: item[0])
    if len({item[0] for item in inputs}) != len(inputs):
        raise SystemExit("duplicate canonical OVFS path")

    table_end = HEADER_SIZE + ENTRY_SIZE * len(inputs)
    data_offset = align_up(table_end, PAGE_SIZE)
    payload_offsets: list[int] = []
    cursor = data_offset
    for _, _, _, payload in inputs:
        cursor = align_up(cursor, PAGE_SIZE)
        payload_offsets.append(cursor)
        cursor += len(payload)
    image_size = cursor
    if image_size > MAX_IMAGE_SIZE:
        raise SystemExit("OVFS image exceeds the native 64 KiB bound")

    table = bytearray()
    for (path, _, flags, payload), offset in zip(inputs, payload_offsets):
        padded_path = path + bytes(64 - len(path))
        table.extend(
            ENTRY_PREFIX.pack(
                len(path),
                1,
                flags,
                offset,
                len(payload),
                hashlib.sha256(payload).digest(),
                padded_path,
                bytes(8),
            )
        )
    if len(table) != ENTRY_SIZE * len(inputs):
        raise AssertionError("entry encoder drift")

    data_region = bytearray(image_size - data_offset)
    for (_, _, _, payload), offset in zip(inputs, payload_offsets):
        relative = offset - data_offset
        data_region[relative : relative + len(payload)] = payload

    header = bytearray(HEADER_SIZE)
    HEADER_PREFIX.pack_into(
        header,
        0,
        MAGIC,
        VERSION,
        HEADER_SIZE,
        ENTRY_SIZE,
        len(inputs),
        HEADER_SIZE,
        data_offset,
        image_size,
    )
    header[48:80] = hashlib.sha256(table).digest()
    header[80:112] = hashlib.sha256(data_region).digest()
    image = bytes(header) + bytes(table)
    image += bytes(data_offset - len(image)) + bytes(data_region)
    if len(image) != image_size:
        raise AssertionError("image size drift")

    digest = hashlib.sha256(image).hexdigest()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    target = args.output_dir / f"root-{digest}.ovfs"
    if target.exists():
        if target.read_bytes() != image:
            raise SystemExit(f"immutable target collision: {target}")
    else:
        descriptor, temporary_name = tempfile.mkstemp(
            prefix=".ovfs-", dir=args.output_dir
        )
        try:
            with os.fdopen(descriptor, "wb") as temporary:
                temporary.write(image)
                temporary.flush()
                os.fsync(temporary.fileno())
            os.replace(temporary_name, target)
        finally:
            if os.path.exists(temporary_name):
                os.unlink(temporary_name)
    target.chmod(0o444)
    print(target.resolve())


if __name__ == "__main__":
    main()
