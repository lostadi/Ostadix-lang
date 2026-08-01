#!/usr/bin/env python3
"""Independent strict parser for OVFSIMG1 and its M4 ELF corpus."""

from __future__ import annotations

import argparse
import hashlib
import os
import struct
from pathlib import Path

HEADER = struct.Struct("<8sIIIIQQQ32s32s16s")
ENTRY = struct.Struct("<HHIQQ32s64s8s")
ELF_HEADER = struct.Struct("<16sHHIQQQIHHHHHH")
PROGRAM_HEADER = struct.Struct("<IIQQQQQQ")

PT_LOAD = 1
PT_GNU_STACK = 0x6474E551
PF_X = 1
PF_W = 2
PF_R = 4
READ = 1
EXEC = 2
CORPUS = 4


def canonical_path(encoded: bytes) -> str:
    path = encoded.decode("utf-8", "strict")
    if not path.startswith("/") or path.endswith("/"):
        raise ValueError("path is not canonical absolute UTF-8")
    components = path.split("/")[1:]
    if any(not component or component in (".", "..") for component in components):
        raise ValueError("path contains a noncanonical component")
    if any(byte == 0 or byte < 0x20 or byte == 0x7F for byte in encoded):
        raise ValueError("path contains a control byte")
    return path


def elf_classification(data: bytes) -> str:
    if len(data) < ELF_HEADER.size:
        return "malformed"
    header = ELF_HEADER.unpack_from(data)
    ident = header[0]
    if (
        ident[:7] != b"\x7fELF\x02\x01\x01"
        or header[1] != 2
        or header[2] != 62
        or header[3] != 1
        or header[8] != ELF_HEADER.size
        or header[9] != PROGRAM_HEADER.size
        or not (1 <= header[10] <= 16)
    ):
        return "malformed"
    entrypoint = header[4]
    phoff = header[5]
    phnum = header[10]
    table_end = phoff + phnum * PROGRAM_HEADER.size
    if phoff < ELF_HEADER.size or table_end > len(data):
        return "malformed"

    loads: list[tuple[int, int, int]] = []
    entry_executable = False
    for index in range(phnum):
        ph = PROGRAM_HEADER.unpack_from(data, phoff + index * PROGRAM_HEADER.size)
        p_type, flags, offset, vaddr, _, filesz, memsz, alignment = ph
        if p_type != PT_LOAD:
            continue
        if filesz > memsz or offset + filesz > len(data):
            return "malformed"
        if alignment not in (0, 1):
            if alignment & (alignment - 1) or vaddr % alignment != offset % alignment:
                return "malformed"
        if flags & PF_W and flags & PF_X:
            return "wx"
        if memsz:
            end = vaddr + memsz
            if end < vaddr:
                return "malformed"
            loads.append((vaddr, end, flags))
            if flags & PF_X and vaddr <= entrypoint < end:
                entry_executable = True
    if not loads or not entry_executable:
        return "malformed"
    loads.sort()
    if any(left[1] > right[0] for left, right in zip(loads, loads[1:])):
        return "overlap"
    return "valid"


def verify_m5_service_elf(path: str, data: bytes) -> None:
    """Enforce the exact loader-facing protection shape of an M5 service."""
    if elf_classification(data) != "valid":
        raise ValueError(f"{path} is not a structurally valid ELF")
    header = ELF_HEADER.unpack_from(data)
    ident = header[0]
    entrypoint = header[4]
    phoff = header[5]
    phnum = header[10]
    if (
        ident != b"\x7fELF\x02\x01\x01" + bytes(9)
        or header[1] != 2
        or header[2] != 62
        or header[3] != 1
        or entrypoint != 0x02000000
        or phoff != ELF_HEADER.size
        or header[7] != 0
        or header[8] != ELF_HEADER.size
        or header[9] != PROGRAM_HEADER.size
        or phnum != 3
    ):
        raise ValueError(f"{path} has a noncanonical M5 ELF header")

    headers = [
        PROGRAM_HEADER.unpack_from(data, phoff + index * PROGRAM_HEADER.size)
        for index in range(phnum)
    ]
    loads = [program for program in headers if program[0] == PT_LOAD]
    stacks = [program for program in headers if program[0] == PT_GNU_STACK]
    if len(loads) != 2 or len(stacks) != 1 or len(headers) != 3:
        raise ValueError(f"{path} must contain exactly two loads and one GNU stack")

    expected_flags = (PF_R | PF_X, PF_R)
    prior_end = 0x02000000
    for ordinal, (program, flags_expected) in enumerate(
        zip(loads, expected_flags)
    ):
        _, flags, offset, vaddr, paddr, filesz, memsz, alignment = program
        if (
            flags != flags_expected
            or (ordinal == 0 and (offset != 0x1000 or vaddr != 0x02000000))
            or offset % 0x1000
            or vaddr % 0x1000
            or offset != 0x1000 + (vaddr - 0x02000000)
            or paddr != vaddr
            or filesz == 0
            or filesz != memsz
            or offset + filesz > len(data)
            or alignment != 0x1000
            or not (0x02000000 <= vaddr < vaddr + memsz <= 0x02100000)
            or vaddr < prior_end
        ):
            raise ValueError(f"{path} has a noncanonical M5 PT_LOAD layout")
        prior_end = vaddr + memsz

    (
        _,
        stack_flags,
        stack_offset,
        stack_vaddr,
        stack_paddr,
        stack_filesz,
        stack_memsz,
        stack_align,
    ) = stacks[0]
    if (
        stack_flags != (PF_R | PF_W)
        or stack_offset != 0
        or stack_vaddr != 0
        or stack_paddr != 0
        or stack_filesz != 0
        or stack_memsz != 0
        or stack_align != 0
    ):
        raise ValueError(f"{path} must declare one non-executable GNU stack")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("image", type=Path)
    parser.add_argument(
        "--max-image-bytes",
        type=int,
        choices=(64 * 1024, 96 * 1024),
        default=64 * 1024,
        help="select the historical 64 KiB or Mode 26 96 KiB native profile",
    )
    parser.add_argument("--expect-m4", action="store_true")
    parser.add_argument("--expect-m5", action="store_true")
    args = parser.parse_args()
    if args.expect_m4 and args.expect_m5:
        raise SystemExit("choose only one expected OVFS profile")
    raw = args.image.read_bytes()
    if len(raw) < HEADER.size:
        raise SystemExit("OVFS header is truncated")
    (
        magic,
        version,
        header_size,
        entry_size,
        file_count,
        table_offset,
        data_offset,
        image_size,
        table_digest,
        data_digest,
        reserved,
    ) = HEADER.unpack_from(raw)
    if (
        magic != b"OVFSIMG1"
        or version != 1
        or header_size != 128
        or entry_size != 128
        or not (1 <= file_count <= 16)
        or table_offset != 128
        or data_offset % 4096
        or image_size != len(raw)
        or image_size > args.max_image_bytes
        or reserved != bytes(16)
    ):
        raise SystemExit("OVFS header is noncanonical")
    table_end = table_offset + file_count * entry_size
    if table_end > data_offset or any(raw[table_end:data_offset]):
        raise SystemExit("OVFS table geometry or zero padding is invalid")
    table = raw[table_offset:table_end]
    data_region = raw[data_offset:image_size]
    if hashlib.sha256(table).digest() != table_digest:
        raise SystemExit("OVFS table digest mismatch")
    if hashlib.sha256(data_region).digest() != data_digest:
        raise SystemExit("OVFS data-region digest mismatch")

    records: list[tuple[str, int, int, int, bytes]] = []
    for index in range(file_count):
        fields = ENTRY.unpack_from(table, index * entry_size)
        path_len, kind, flags, offset, size, digest, path_field, entry_reserved = fields
        if (
            not (1 <= path_len <= 63)
            or kind != 1
            or not (flags & READ)
            or flags & ~(READ | EXEC | CORPUS)
            or flags & EXEC and flags & CORPUS
            or offset % 4096
            or size > 1024 * 1024
            or offset < data_offset
            or offset + size > image_size
            or path_field[path_len:] != bytes(64 - path_len)
            or entry_reserved != bytes(8)
        ):
            raise SystemExit(f"OVFS entry {index} is noncanonical")
        path = canonical_path(path_field[:path_len])
        payload = raw[offset : offset + size]
        if hashlib.sha256(payload).digest() != digest:
            raise SystemExit(f"OVFS content digest mismatch: {path}")
        records.append((path, flags, offset, size, payload))
    if [record[0] for record in records] != sorted(record[0] for record in records):
        raise SystemExit("OVFS entries are not path-sorted")
    if len({record[0] for record in records}) != len(records):
        raise SystemExit("OVFS contains duplicate paths")

    cursor = data_offset
    for path, _, offset, size, _ in records:
        if offset < cursor or any(raw[cursor:offset]):
            raise SystemExit(f"OVFS payload overlap or nonzero gap before {path}")
        cursor = offset + size
    if cursor != image_size:
        raise SystemExit("OVFS has unclaimed trailing bytes")
    if os.stat(args.image).st_mode & 0o222:
        raise SystemExit("OVFS host artifact is not read-only")
    expected_name = f"root-{hashlib.sha256(raw).hexdigest()}.ovfs"
    if args.image.name != expected_name:
        raise SystemExit("OVFS filename does not match immutable image digest")

    classifications = {
        path: elf_classification(payload)
        for path, flags, _, _, payload in records
        if flags & (EXEC | CORPUS)
    }
    if args.expect_m4:
        expected = {
            "/bin/personality-alpha.elf": "valid",
            "/bin/personality-beta.elf": "valid",
            "/corpus/malformed.elf": "malformed",
            "/corpus/overlap.elf": "overlap",
            "/corpus/wx.elf": "wx",
        }
        if classifications != expected:
            raise SystemExit(
                f"unexpected M4 ELF classifications: {classifications!r}"
            )
    if args.expect_m5:
        expected = {
            "/sbin/init.elf": "valid",
            "/sbin/pkgd.elf": "valid",
            "/sbin/repl.elf": "valid",
            "/sbin/supervisor.elf": "valid",
        }
        if classifications != expected:
            raise SystemExit(
                f"unexpected M5 ELF classifications: {classifications!r}"
            )
        if [record[0] for record in records] != sorted(expected):
            raise SystemExit("M5 OVFS must contain exactly the four service ELFs")
        records_by_path = {
            path: (flags, payload) for path, flags, _, _, payload in records
        }
        try:
            for path in expected:
                flags, payload = records_by_path[path]
                if flags != (READ | EXEC):
                    raise ValueError(
                        f"{path} must carry exact OVFS read+execute authority"
                    )
                verify_m5_service_elf(path, payload)
        except ValueError as error:
            raise SystemExit(str(error)) from error
    print(
        f"OVFSIMG1 verified: {file_count} files, {image_size} bytes, "
        f"sha256={hashlib.sha256(raw).hexdigest()}"
    )
    for path, classification in classifications.items():
        print(f"{path}: {classification}")


if __name__ == "__main__":
    main()
