#!/usr/bin/env python3
"""Normalize dynamic metadata in an Android ARM64 ELF in place.

Termux's Clang driver injects its private library directory as a RUNPATH.
Packaged Android executables must resolve only public Android libraries, so the
APK build removes that host-only search path before signing.

The optional ``--replace-needed OLD=NEW`` and ``--replace-soname OLD=NEW``
operations rewrite dynamic names without growing the string table. This is
useful for Android APK libraries: PackageManager extracts ``lib*.so`` entries,
while common Unix SONAMEs such as ``libreadline.so.8`` do not end in ``.so``.
The optional ``--set-runpath`` operation replaces one existing DT_RUNPATH
string instead of removing it; the standalone Bash bundle uses Android's
supported ``${ORIGIN}`` expansion to find sibling APK libraries.

The tool intentionally supports only little-endian ELF64 AArch64 files. It
locates PT_DYNAMIC from program headers, compacts the dynamic table while
preserving every other entry, and leaves the segment size unchanged.
"""

from __future__ import annotations

import argparse
import os
import struct
import sys
from pathlib import Path

ELF_MAGIC = b"\x7fELF"
ELFCLASS64 = 2
ELFDATA2LSB = 1
EM_AARCH64 = 183
PT_DYNAMIC = 2
PT_LOAD = 1
DT_NULL = 0
DT_NEEDED = 1
DT_STRTAB = 5
DT_STRSZ = 10
DT_SONAME = 14
DT_RPATH = 15
DT_RUNPATH = 29

ELF64_HEADER_SIZE = 64
ELF64_PROGRAM_HEADER_SIZE = 56
ELF64_DYNAMIC_SIZE = 16


class ElfError(ValueError):
    """Raised when an input is not the supported Android ELF shape."""


def scrub(
    path: Path,
    needed_replacements: dict[str, str],
    soname_replacements: dict[str, str],
    runpath_replacement: str | None,
) -> tuple[int, int, int, int]:
    data = bytearray(path.read_bytes())
    if len(data) < ELF64_HEADER_SIZE or data[:4] != ELF_MAGIC:
        raise ElfError("not an ELF file")
    if data[4] != ELFCLASS64 or data[5] != ELFDATA2LSB:
        raise ElfError("expected a little-endian ELF64 file")

    machine = struct.unpack_from("<H", data, 18)[0]
    if machine != EM_AARCH64:
        raise ElfError(f"expected AArch64 machine {EM_AARCH64}, found {machine}")

    program_offset = struct.unpack_from("<Q", data, 32)[0]
    program_entry_size = struct.unpack_from("<H", data, 54)[0]
    program_count = struct.unpack_from("<H", data, 56)[0]
    if program_entry_size < ELF64_PROGRAM_HEADER_SIZE:
        raise ElfError("invalid ELF64 program-header size")
    if program_offset + program_entry_size * program_count > len(data):
        raise ElfError("program-header table extends beyond the file")

    load_segments: list[tuple[int, int, int]] = []
    dynamic_offset = None
    dynamic_size = None
    for index in range(program_count):
        offset = program_offset + index * program_entry_size
        program_type = struct.unpack_from("<I", data, offset)[0]
        if program_type == PT_LOAD:
            file_offset = struct.unpack_from("<Q", data, offset + 8)[0]
            virtual_address = struct.unpack_from("<Q", data, offset + 16)[0]
            file_size = struct.unpack_from("<Q", data, offset + 32)[0]
            load_segments.append((file_offset, virtual_address, file_size))
        if program_type != PT_DYNAMIC:
            continue
        if dynamic_offset is not None:
            raise ElfError("multiple PT_DYNAMIC segments are unsupported")
        dynamic_offset = struct.unpack_from("<Q", data, offset + 8)[0]
        dynamic_size = struct.unpack_from("<Q", data, offset + 32)[0]

    if dynamic_offset is None or dynamic_size is None:
        raise ElfError("ELF has no PT_DYNAMIC segment")
    if dynamic_size < ELF64_DYNAMIC_SIZE or dynamic_size % ELF64_DYNAMIC_SIZE != 0:
        raise ElfError("invalid PT_DYNAMIC segment size")
    if dynamic_offset + dynamic_size > len(data):
        raise ElfError("PT_DYNAMIC segment extends beyond the file")

    entries: list[tuple[int, int]] = []
    saw_null = False
    for offset in range(
        dynamic_offset,
        dynamic_offset + dynamic_size,
        ELF64_DYNAMIC_SIZE,
    ):
        tag, value = struct.unpack_from("<qQ", data, offset)
        entries.append((tag, value))
        if tag == DT_NULL:
            saw_null = True
            break
    if not saw_null:
        raise ElfError("unterminated dynamic table")

    replaced = 0
    sonames_replaced = 0
    runpaths_set = 0
    if needed_replacements or soname_replacements or runpath_replacement is not None:
        string_tables = [value for tag, value in entries if tag == DT_STRTAB]
        string_sizes = [value for tag, value in entries if tag == DT_STRSZ]
        if len(string_tables) != 1 or len(string_sizes) != 1:
            raise ElfError("expected exactly one dynamic string table")
        string_address = string_tables[0]
        string_size = string_sizes[0]
        string_offset = None
        for file_offset, virtual_address, file_size in load_segments:
            if (
                virtual_address <= string_address
                and string_address + string_size <= virtual_address + file_size
            ):
                string_offset = file_offset + string_address - virtual_address
                break
        if string_offset is None or string_offset + string_size > len(data):
            raise ElfError("dynamic string table is not backed by a PT_LOAD segment")

        def dynamic_string(value: int, kind: str) -> tuple[int, int, str]:
            if value >= string_size:
                raise ElfError(f"{kind} string offset is out of bounds")
            start = string_offset + value
            end = data.find(b"\0", start, string_offset + string_size)
            if end < 0:
                raise ElfError(f"unterminated {kind} string")
            try:
                decoded = bytes(data[start:end]).decode("ascii")
            except UnicodeDecodeError as error:
                raise ElfError(f"non-ASCII {kind} string") from error
            return start, end, decoded

        seen: set[str] = set()
        for tag, value in entries:
            if tag != DT_NEEDED:
                continue
            start, end, original = dynamic_string(value, "DT_NEEDED")
            replacement = needed_replacements.get(original)
            if replacement is None:
                continue
            encoded = replacement.encode("ascii")
            if not encoded or b"\0" in encoded:
                raise ElfError(f"invalid replacement name for {original!r}")
            if len(encoded) > len(original):
                raise ElfError(
                    f"replacement {replacement!r} is longer than {original!r}"
                )
            data[start:end] = encoded + b"\0" * (len(original) - len(encoded))
            seen.add(original)
            replaced += 1

        missing = sorted(set(needed_replacements) - seen)
        if missing:
            raise ElfError("DT_NEEDED name(s) not found: " + ", ".join(missing))

        seen_sonames: set[str] = set()
        for tag, value in entries:
            if tag != DT_SONAME:
                continue
            start, end, original = dynamic_string(value, "DT_SONAME")
            replacement = soname_replacements.get(original)
            if replacement is None:
                continue
            encoded = replacement.encode("ascii")
            if not encoded or b"\0" in encoded:
                raise ElfError(f"invalid replacement name for {original!r}")
            if len(encoded) > len(original):
                raise ElfError(
                    f"replacement {replacement!r} is longer than {original!r}"
                )
            data[start:end] = encoded + b"\0" * (len(original) - len(encoded))
            seen_sonames.add(original)
            sonames_replaced += 1

        missing_sonames = sorted(set(soname_replacements) - seen_sonames)
        if missing_sonames:
            raise ElfError(
                "DT_SONAME name(s) not found: " + ", ".join(missing_sonames)
            )

        if runpath_replacement is not None:
            encoded_runpath = runpath_replacement.encode("ascii")
            for tag, value in entries:
                if tag != DT_RUNPATH:
                    continue
                start, end, original = dynamic_string(value, "DT_RUNPATH")
                if len(encoded_runpath) > len(original):
                    raise ElfError(
                        f"replacement runpath is longer than {original!r}"
                    )
                data[start:end] = encoded_runpath + b"\0" * (
                    len(original) - len(encoded_runpath)
                )
                runpaths_set += 1
            if runpaths_set != 1:
                raise ElfError(
                    f"expected exactly one DT_RUNPATH, found {runpaths_set}"
                )

    filtered = (
        entries
        if runpath_replacement is not None
        else [entry for entry in entries if entry[0] not in (DT_RPATH, DT_RUNPATH)]
    )
    removed = len(entries) - len(filtered)
    if removed:
        if not filtered or filtered[-1][0] != DT_NULL:
            raise ElfError("dynamic table lost its terminator")

        write_offset = dynamic_offset
        for tag, value in filtered:
            struct.pack_into("<qQ", data, write_offset, tag, value)
            write_offset += ELF64_DYNAMIC_SIZE
        data[write_offset : dynamic_offset + dynamic_size] = b"\0" * (
            dynamic_offset + dynamic_size - write_offset
        )

    if removed == 0 and replaced == 0 and sonames_replaced == 0 and runpaths_set == 0:
        return 0, 0, 0, 0

    with path.open("r+b") as output:
        output.write(data)
        output.truncate()
        output.flush()
        os.fsync(output.fileno())
    return removed, replaced, sonames_replaced, runpaths_set


def parse_replacement(value: str) -> tuple[str, str]:
    if "=" not in value:
        raise argparse.ArgumentTypeError("expected OLD=NEW")
    original, replacement = value.split("=", 1)
    if not original or not replacement:
        raise argparse.ArgumentTypeError("both OLD and NEW must be non-empty")
    try:
        original.encode("ascii")
        replacement.encode("ascii")
    except UnicodeEncodeError as error:
        raise argparse.ArgumentTypeError("library names must be ASCII") from error
    if len(replacement) > len(original):
        raise argparse.ArgumentTypeError("NEW must not be longer than OLD")
    return original, replacement


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--replace-needed",
        action="append",
        default=[],
        metavar="OLD=NEW",
        type=parse_replacement,
        help="replace one DT_NEEDED name (NEW must not be longer)",
    )
    parser.add_argument(
        "--replace-soname",
        action="append",
        default=[],
        metavar="OLD=NEW",
        type=parse_replacement,
        help="replace one DT_SONAME (NEW must not be longer)",
    )
    parser.add_argument(
        "--set-runpath",
        metavar="PATH",
        help="replace one DT_RUNPATH string instead of removing it",
    )
    parser.add_argument("elf", nargs="+", type=Path)
    arguments = parser.parse_args()

    replacements = dict(arguments.replace_needed)
    if len(replacements) != len(arguments.replace_needed):
        parser.error("duplicate OLD name in --replace-needed")
    soname_replacements = dict(arguments.replace_soname)
    if len(soname_replacements) != len(arguments.replace_soname):
        parser.error("duplicate OLD name in --replace-soname")
    if arguments.set_runpath is not None:
        try:
            arguments.set_runpath.encode("ascii")
        except UnicodeEncodeError:
            parser.error("--set-runpath must be ASCII")
        if not arguments.set_runpath or "\0" in arguments.set_runpath:
            parser.error("--set-runpath must be non-empty and contain no NUL")

    for path in arguments.elf:
        try:
            removed, replaced, sonames_replaced, runpaths_set = scrub(
                path, replacements, soname_replacements, arguments.set_runpath
            )
        except (OSError, ElfError) as error:
            print(f"scrub_elf_runpath: {path}: {error}", file=sys.stderr)
            return 1
        print(
            f"scrub_elf_runpath: {path}: removed {removed} path tag(s), "
            f"replaced {replaced} needed name(s), replaced "
            f"{sonames_replaced} soname(s), set {runpaths_set} runpath(s)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
