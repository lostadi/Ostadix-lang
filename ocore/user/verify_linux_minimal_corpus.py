#!/usr/bin/env python3
"""Fail-closed verifier for the pinned minimal Linux x86-64 ET_EXEC corpus."""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
from pathlib import Path

ELF_HEADER = struct.Struct("<16sHHIQQQIHHHHHH")
PROGRAM_HEADER = struct.Struct("<IIQQQQQQ")
SECTION_HEADER = struct.Struct("<IIQQQQIIQQ")
PT_LOAD = 1
PT_GNU_STACK = 0x6474E551
PF_X = 1
PF_W = 2
PF_R = 4


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def exact_keys(value: dict[str, object], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise ValueError(
            f"{label} keys differ: expected {sorted(expected)}, got {sorted(value)}"
        )


def load_strict_json(path: Path) -> object:
    def object_pairs(pairs: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate oracle key: {key!r}")
            result[key] = value
        return result

    def invalid_constant(value: str) -> object:
        raise ValueError(f"non-finite oracle number: {value}")

    try:
        return json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=object_pairs,
            parse_constant=invalid_constant,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read strict oracle JSON: {error}") from error


def load_oracle(path: Path) -> dict[str, object]:
    value = load_strict_json(path)
    if not isinstance(value, dict):
        raise ValueError("oracle root must be an object")
    exact_keys(
        value,
        {
            "schema",
            "scope",
            "architecture",
            "elf",
            "sources",
            "expected",
            "native_linux_confirmation",
            "nonclaims",
        },
        "oracle",
    )
    if value["schema"] != "ocore.linux-minimal-x86_64/v1":
        raise ValueError("unsupported oracle schema")
    if value["architecture"] != "x86_64":
        raise ValueError("oracle architecture is not x86_64")
    if value["scope"] != (
        "one pinned static ET_EXEC; write fd 1/2, unknown -ENOSYS, "
        "exit_group only"
    ):
        raise ValueError("oracle scope drift")

    elf = value["elf"]
    sources = value["sources"]
    expected = value["expected"]
    confirmation = value["native_linux_confirmation"]
    if not all(isinstance(item, dict) for item in (elf, sources, expected, confirmation)):
        raise ValueError("oracle nested records must be objects")
    exact_keys(
        elf,
        {"sha256", "size", "entry", "text_sha256", "rodata_sha256"},
        "elf",
    )
    exact_keys(sources, {"assembly_sha256", "linker_sha256"}, "sources")
    exact_keys(
        expected,
        {"stdout_utf8", "stderr_utf8", "exit_status", "syscalls"},
        "expected",
    )
    exact_keys(confirmation, {"confirmed", "status"}, "native confirmation")
    if (
        elf["sha256"] != "06240b6a840ed4262835aceff64a94f6ebd77838666f05eb7415d9a0d1b5868d"
        or elf["size"] != 8520
        or elf["entry"] != 0x02000000
        or elf["text_sha256"]
        != "bdd4a96c98c3484098f1fcc8e3915a77eaf9c6d408b8533bab7ed981ae6636f6"
        or elf["rodata_sha256"]
        != "7a3aabae23c2cdb90e2d7be1bd576cfa67d2e013aa8417bc8a964983e2bec5ce"
        or sources["assembly_sha256"]
        != "d1b6391d7288bddb2a799296e961f4cb9ffd95a2cca0949de0b29f715ff0d52c"
        or sources["linker_sha256"]
        != "b7e85fd1dc50631dcb4c5ea1196422aa1d075d6bfc713dc13571ebc6f5ce06bc"
    ):
        raise ValueError("pinned ELF/source identity drift")
    if confirmation["confirmed"] is not False or confirmation["status"] != (
        "pending native x86_64 Linux CI replay of these exact ELF bytes"
    ):
        raise ValueError("native confirmation must remain explicitly pending")
    if value["nonclaims"] != [
        "not a Linux kernel or distribution boot",
        "not a general Linux userspace or syscall ABI",
        "no read, brk, mmap, clock_gettime, signals, threads, dynamic linker, or root filesystem",
        "no native Linux observation on the Darwin arm64 development host",
    ]:
        raise ValueError("oracle nonclaims drift")
    return value


def verify_sources(root: Path, oracle: dict[str, object]) -> None:
    sources = oracle["sources"]
    assert isinstance(sources, dict)
    assembly = (root / "linux_minimal_guest.S").read_bytes()
    linker = (root / "linux-minimal-user.ld").read_bytes()
    if digest(assembly) != sources["assembly_sha256"]:
        raise ValueError("pinned assembly SHA-256 mismatch")
    if digest(linker) != sources["linker_sha256"]:
        raise ValueError("pinned linker-script SHA-256 mismatch")


def verify_expected(oracle: dict[str, object]) -> tuple[bytes, bytes]:
    expected = oracle["expected"]
    assert isinstance(expected, dict)
    stdout = expected["stdout_utf8"].encode("utf-8")
    stderr = expected["stderr_utf8"].encode("utf-8")
    if stdout != b"o-core linux stdout\n" or stderr != b"o-core linux stderr\n":
        raise ValueError("expected output bytes drift")
    if expected["exit_status"] != 42:
        raise ValueError("exit status drift")
    if expected["syscalls"] != [
        {"number": 1, "fd": 1, "length": 20, "return": 20},
        {"number": 1, "fd": 2, "length": 20, "return": 20},
        {"number": 2147483647, "return": -38},
        {"number": 231, "status": 42, "returns": False},
    ]:
        raise ValueError("syscall oracle drift")
    return stdout, stderr


def verify_elf(path: Path, oracle: dict[str, object], stdout: bytes, stderr: bytes) -> None:
    raw = path.read_bytes()
    elf_oracle = oracle["elf"]
    assert isinstance(elf_oracle, dict)
    if len(raw) != elf_oracle["size"] or digest(raw) != elf_oracle["sha256"]:
        raise ValueError("ELF byte identity differs from pinned oracle")
    if len(raw) < ELF_HEADER.size:
        raise ValueError("ELF header is truncated")
    header = ELF_HEADER.unpack_from(raw)
    ident = header[0]
    if (
        ident != b"\x7fELF\x02\x01\x01" + bytes(9)
        or header[1] != 2
        or header[2] != 62
        or header[3] != 1
        or header[4] != elf_oracle["entry"]
        or header[5] != ELF_HEADER.size
        or header[7] != 0
        or header[8] != ELF_HEADER.size
        or header[9] != PROGRAM_HEADER.size
        or header[10] != 3
        or header[11] != SECTION_HEADER.size
        or header[12] != 4
        or header[13] != 3
    ):
        raise ValueError("noncanonical static x86-64 ET_EXEC header")

    programs = [
        PROGRAM_HEADER.unpack_from(raw, header[5] + i * PROGRAM_HEADER.size)
        for i in range(header[10])
    ]
    expected_programs = [
        (PT_LOAD, PF_R | PF_X, 0x1000, 0x02000000, 0x02000000, 0x74, 0x74, 0x1000),
        (PT_LOAD, PF_R, 0x2000, 0x02001000, 0x02001000, 0x28, 0x28, 0x1000),
        (PT_GNU_STACK, PF_R | PF_W, 0, 0, 0, 0, 0, 0),
    ]
    if programs != expected_programs:
        raise ValueError(f"program-header layout drift: {programs!r}")
    if any(flags & PF_W and flags & PF_X for _, flags, *_ in programs):
        raise ValueError("W+X program header")

    expected_sections = [
        (0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
        (1, 1, 6, 0x02000000, 0x1000, 0x74, 0, 0, 0x1000, 0),
        (7, 1, 2, 0x02001000, 0x2000, 0x28, 0, 0, 1, 0),
        (15, 3, 0, 0, 0x2028, 0x19, 0, 0, 1, 0),
    ]
    section_table_end = header[6] + header[12] * header[11]
    if header[6] != 0x2048 or section_table_end != len(raw):
        raise ValueError("noncanonical section-table geometry")
    sections = [
        SECTION_HEADER.unpack_from(raw, header[6] + i * header[11])
        for i in range(header[12])
    ]
    if sections != expected_sections:
        raise ValueError("section/symbol layout drift or alloc section outside fixed window")
    for section in sections:
        _, _, flags, address, _, size, *_ = section
        if flags & 2 and not (0x02000000 <= address <= address + size <= 0x02100000):
            raise ValueError("allocated corpus section escapes fixed loader window")

    text = raw[0x1000 : 0x1000 + 0x74]
    rodata = raw[0x2000 : 0x2000 + 0x28]
    if digest(text) != elf_oracle["text_sha256"]:
        raise ValueError("pinned text SHA-256 mismatch")
    if digest(rodata) != elf_oracle["rodata_sha256"]:
        raise ValueError("pinned rodata SHA-256 mismatch")
    if rodata != stdout + stderr:
        raise ValueError("ELF rodata does not exactly equal stdout+stderr oracle")
    if text.count(b"\x0f\x05") != 5:
        raise ValueError("expected four success-path syscalls plus one failure exit")
    if raw.count(stdout) != 1 or raw.count(stderr) != 1:
        raise ValueError("expected output corpus is absent or duplicated")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("elf", type=Path)
    parser.add_argument("oracle", type=Path)
    args = parser.parse_args()
    oracle = load_oracle(args.oracle)
    root = args.oracle.resolve().parent
    verify_sources(root, oracle)
    stdout, stderr = verify_expected(oracle)
    verify_elf(args.elf, oracle, stdout, stderr)
    print(
        "minimal Linux corpus: PASS "
        f"({args.elf.stat().st_size} bytes, sha256={digest(args.elf.read_bytes())})"
    )
    print("native x86_64 Linux replay: PENDING (not claimed by this host gate)")


if __name__ == "__main__":
    main()
