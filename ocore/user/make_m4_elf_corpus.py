#!/usr/bin/env python3
"""Create deterministic negative ELF64 loader fixtures from a valid image."""

from __future__ import annotations

import argparse
import struct
from pathlib import Path

ELF_HEADER = struct.Struct("<16sHHIQQQIHHHHHH")
PROGRAM_HEADER = struct.Struct("<IIQQQQQQ")
PT_LOAD = 1
PF_X = 1
PF_W = 2


def elf_layout(data: bytes) -> tuple[int, int, int, list[tuple[int, tuple[int, ...]]]]:
    if len(data) < ELF_HEADER.size:
        raise ValueError("source ELF header is truncated")
    header = ELF_HEADER.unpack_from(data)
    ident = header[0]
    if ident[:7] != b"\x7fELF\x02\x01\x01":
        raise ValueError("source is not little-endian ELF64")
    phoff = header[5]
    phentsize = header[9]
    phnum = header[10]
    if phentsize != PROGRAM_HEADER.size or phnum < 2 or phnum > 16:
        raise ValueError("source has unsupported program-header geometry")
    end = phoff + phentsize * phnum
    if phoff < ELF_HEADER.size or end > len(data):
        raise ValueError("source program-header table is out of range")
    headers: list[tuple[int, tuple[int, ...]]] = []
    for index in range(phnum):
        offset = phoff + index * phentsize
        headers.append((offset, PROGRAM_HEADER.unpack_from(data, offset)))
    return phoff, phentsize, phnum, headers


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("output_dir", type=Path)
    args = parser.parse_args()

    source = args.source.read_bytes()
    _, _, _, headers = elf_layout(source)
    loads = [(offset, ph) for offset, ph in headers if ph[0] == PT_LOAD and ph[6] != 0]
    executable = [(offset, ph) for offset, ph in loads if ph[1] & PF_X]
    if len(loads) < 2 or not executable:
        raise SystemExit("source ELF needs two nonempty LOADs and executable text")

    args.output_dir.mkdir(parents=True, exist_ok=True)

    # Valid ELF identification followed by a deliberately truncated header.
    malformed = bytearray(32)
    malformed[:16] = b"\x7fELF\x02\x01\x01" + bytes(9)
    (args.output_dir / "malformed.elf").write_bytes(malformed)

    overlap = bytearray(source)
    first_offset, first = loads[0]
    second_offset, second = loads[1]
    del first_offset
    second_fields = list(second)
    second_fields[3] = first[3]
    second_fields[4] = first[4]
    PROGRAM_HEADER.pack_into(overlap, second_offset, *second_fields)
    (args.output_dir / "overlap.elf").write_bytes(overlap)

    wx = bytearray(source)
    executable_offset, executable_header = executable[0]
    executable_fields = list(executable_header)
    executable_fields[1] |= PF_W
    PROGRAM_HEADER.pack_into(wx, executable_offset, *executable_fields)
    (args.output_dir / "wx.elf").write_bytes(wx)


if __name__ == "__main__":
    main()
