#!/usr/bin/env python3
"""Negative corpus tests that bypass only the outer pinned digest check."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import struct
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent
VERIFY_PATH = ROOT / "verify_linux_minimal_corpus.py"
SPEC = importlib.util.spec_from_file_location("linux_corpus_verify", VERIFY_PATH)
if SPEC is None or SPEC.loader is None:
    raise SystemExit("cannot load corpus verifier")
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


def updated_oracle(oracle: dict[str, object], raw: bytes) -> dict[str, object]:
    value = copy.deepcopy(oracle)
    elf = value["elf"]
    assert isinstance(elf, dict)
    elf["sha256"] = hashlib.sha256(raw).hexdigest()
    elf["size"] = len(raw)
    return value


def expect_elf_rejection(
    label: str, raw: bytes, oracle: dict[str, object]
) -> None:
    with tempfile.TemporaryDirectory(prefix="ocore-linux-negative-") as directory:
        path = Path(directory) / "mutant.elf"
        path.write_bytes(raw)
        try:
            VERIFY.verify_elf(path, oracle, b"o-core linux stdout\n", b"o-core linux stderr\n")
        except ValueError:
            return
    raise AssertionError(f"{label} mutant was accepted")


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: test_linux_minimal_corpus.py ELF ORACLE")
    elf_path = Path(sys.argv[1])
    oracle_path = Path(sys.argv[2])
    raw = elf_path.read_bytes()
    oracle = json.loads(oracle_path.read_text(encoding="utf-8"))

    digest_mutant = bytearray(raw)
    digest_mutant[0x1000] ^= 1
    expect_elf_rejection("digest", bytes(digest_mutant), oracle)

    machine_mutant = bytearray(raw)
    struct.pack_into("<H", machine_mutant, 18, 183)
    expect_elf_rejection(
        "wrong machine", bytes(machine_mutant), updated_oracle(oracle, machine_mutant)
    )

    entry_mutant = bytearray(raw)
    struct.pack_into("<Q", entry_mutant, 24, 0x02000010)
    expect_elf_rejection(
        "wrong entry", bytes(entry_mutant), updated_oracle(oracle, entry_mutant)
    )

    interp_mutant = bytearray(raw)
    struct.pack_into("<I", interp_mutant, 64 + 2 * 56, 3)
    expect_elf_rejection(
        "PT_INTERP", bytes(interp_mutant), updated_oracle(oracle, interp_mutant)
    )

    wx_mutant = bytearray(raw)
    struct.pack_into("<I", wx_mutant, 64 + 4, 7)
    expect_elf_rejection(
        "W+X", bytes(wx_mutant), updated_oracle(oracle, wx_mutant)
    )

    load_mutant = bytearray(raw)
    struct.pack_into("<Q", load_mutant, 64 + 8, 0x1100)
    expect_elf_rejection(
        "noncanonical load", bytes(load_mutant), updated_oracle(oracle, load_mutant)
    )

    window_mutant = bytearray(raw)
    struct.pack_into("<Q", window_mutant, 64 + 16, 0x02100000)
    struct.pack_into("<Q", window_mutant, 64 + 24, 0x02100000)
    expect_elf_rejection(
        "load outside fixed window",
        bytes(window_mutant),
        updated_oracle(oracle, window_mutant),
    )

    symbol_mutant = bytearray(raw)
    section_table = struct.unpack_from("<Q", symbol_mutant, 40)[0]
    struct.pack_into("<I", symbol_mutant, section_table + 3 * 64 + 4, 2)
    expect_elf_rejection(
        "symbol table", bytes(symbol_mutant), updated_oracle(oracle, symbol_mutant)
    )

    rodata_mutant = bytearray(raw)
    rodata_mutant[0x2000] ^= 1
    rodata_oracle = updated_oracle(oracle, rodata_mutant)
    rodata_record = rodata_oracle["elf"]
    assert isinstance(rodata_record, dict)
    rodata_record["rodata_sha256"] = hashlib.sha256(
        rodata_mutant[0x2000 : 0x2028]
    ).hexdigest()
    expect_elf_rejection("rodata", bytes(rodata_mutant), rodata_oracle)

    drift = copy.deepcopy(oracle)
    drift_expected = drift["expected"]
    assert isinstance(drift_expected, dict)
    drift_expected["exit_status"] = 0
    try:
        VERIFY.verify_expected(drift)
    except ValueError:
        pass
    else:
        raise AssertionError("oracle expectation drift was accepted")

    duplicate_oracle = oracle_path.read_text(encoding="utf-8").replace(
        '  "schema":',
        '  "schema": "duplicate-must-fail",\n  "schema":',
        1,
    )
    with tempfile.TemporaryDirectory(prefix="ocore-linux-oracle-negative-") as directory:
        duplicate_path = Path(directory) / "duplicate-oracle.json"
        duplicate_path.write_text(duplicate_oracle, encoding="utf-8")
        try:
            VERIFY.load_oracle(duplicate_path)
        except ValueError:
            pass
        else:
            raise AssertionError("duplicate oracle key was accepted")

    print(
        "minimal Linux corpus negative suite: PASS "
        "(9 ELF mutants + oracle drift/duplicate rejection)"
    )


if __name__ == "__main__":
    main()
