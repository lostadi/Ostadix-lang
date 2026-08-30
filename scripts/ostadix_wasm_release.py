#!/usr/bin/env python3
"""Create and verify the source-bound Olangc WASM release artifact."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
from pathlib import PurePosixPath
import re
import stat
import sys


SCHEMA = "ostadix.olangc-wasm-release/v1"
TARGET = "wasm32-wasip1"
LOGICAL_INPUT = "examples/wasm_hello.O"
LOGICAL_GENERATOR = "/usr/local/bin/olangc"
LOGICAL_ARTIFACT = "/usr/share/ostadix/wasm/hello.wasm"
SHA256_RE = re.compile(r"[0-9a-f]{64}")
TREE_RE = re.compile(r"[0-9a-f]{40}")
MAX_MODULE_BYTES = 512 * 1024 * 1024
SECTION_ORDER = {
    1: 1,   # type
    2: 2,   # import
    3: 3,   # function
    4: 4,   # table
    5: 5,   # memory
    13: 6,  # tag
    6: 7,   # global
    7: 8,   # export
    8: 9,   # start
    9: 10,  # element
    12: 11, # data-count precedes code despite its numeric section id
    10: 12, # code
    11: 13, # data
}


class WasmReleaseError(RuntimeError):
    """Raised when a release input or binding is invalid."""


def file_identity(path: Path, *, label: str) -> dict[str, object]:
    try:
        state = path.lstat()
    except OSError as error:
        raise WasmReleaseError(f"{label} is unavailable: {path}: {error}") from error
    if stat.S_ISLNK(state.st_mode) or not stat.S_ISREG(state.st_mode):
        raise WasmReleaseError(f"{label} must be a regular non-symlink file: {path}")
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
    return {"bytes": size, "sha256": digest.hexdigest()}


def project_identity(root: Path) -> dict[str, object]:
    try:
        state = root.lstat()
    except OSError as error:
        raise WasmReleaseError(f"materialized project is unavailable: {root}: {error}") from error
    if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
        raise WasmReleaseError(
            f"materialized project must be a non-symlink directory: {root}"
        )
    if (root / "target").exists() or (root / "target").is_symlink():
        raise WasmReleaseError("materialized project must not contain Cargo target output")

    records: list[dict[str, object]] = []
    for directory, directory_names, file_names in os.walk(
        root, topdown=True, followlinks=False
    ):
        directory_names.sort()
        file_names.sort()
        directory_path = Path(directory)
        for name in directory_names:
            path = directory_path / name
            child_state = path.lstat()
            if stat.S_ISLNK(child_state.st_mode) or not stat.S_ISDIR(
                child_state.st_mode
            ):
                raise WasmReleaseError(
                    f"materialized project contains an unsafe directory: {path}"
                )
        for name in file_names:
            path = directory_path / name
            relative = path.relative_to(root).as_posix()
            pure = PurePosixPath(relative)
            if (
                pure.is_absolute()
                or not pure.parts
                or any(part in {"", ".", ".."} for part in pure.parts)
                or relative != pure.as_posix()
            ):
                raise WasmReleaseError(
                    f"materialized project contains a non-canonical path: {relative!r}"
                )
            identity = file_identity(path, label="materialized project member")
            records.append({"path": relative, **identity})

    if not records:
        raise WasmReleaseError("materialized project contains no regular files")
    records.sort(key=lambda record: str(record["path"]))
    encoded = (
        b"ostadix.olangc-materialized-project/v1\x00"
        + json.dumps(records, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )
    return {
        "file_count": len(records),
        "logical_bytes": sum(int(record["bytes"]) for record in records),
        "root_sha256": hashlib.sha256(encoded).hexdigest(),
    }


def _read_u32_leb(module: bytes, offset: int) -> tuple[int, int]:
    value = 0
    for index in range(5):
        if offset >= len(module):
            raise WasmReleaseError("WebAssembly section length is truncated")
        byte = module[offset]
        offset += 1
        value |= (byte & 0x7F) << (index * 7)
        if byte & 0x80 == 0:
            if index == 4 and byte > 0x0F:
                raise WasmReleaseError("WebAssembly section length overflows u32")
            return value, offset
    raise WasmReleaseError("WebAssembly section length uses an overlong u32 LEB128")


def validate_module(path: Path) -> dict[str, object]:
    identity = file_identity(path, label="WebAssembly artifact")
    size = int(identity["bytes"])
    if size > MAX_MODULE_BYTES:
        raise WasmReleaseError(
            f"WebAssembly artifact exceeds the {MAX_MODULE_BYTES}-byte validation limit"
        )
    module = path.read_bytes()
    if module[:8] != b"\x00asm\x01\x00\x00\x00":
        raise WasmReleaseError("WebAssembly artifact has the wrong magic or core version")

    offset = 8
    last_standard_section_order = 0
    standard_sections: set[int] = set()
    while offset < len(module):
        section_id = module[offset]
        offset += 1
        if section_id != 0 and section_id not in SECTION_ORDER:
            raise WasmReleaseError(
                f"WebAssembly artifact has an unsupported core section id: {section_id}"
            )
        payload_size, offset = _read_u32_leb(module, offset)
        end = offset + payload_size
        if end > len(module):
            raise WasmReleaseError("WebAssembly section payload is truncated")
        if section_id != 0:
            section_order = SECTION_ORDER[section_id]
            if (
                section_id in standard_sections
                or section_order < last_standard_section_order
            ):
                raise WasmReleaseError(
                    "WebAssembly standard sections are duplicated or out of order"
                )
            standard_sections.add(section_id)
            last_standard_section_order = section_order
        offset = end
    if 10 not in standard_sections:
        raise WasmReleaseError("WebAssembly artifact contains no code section")
    return identity


def _source_binding(
    tree: str, base_commit: str, archive_sha256: str
) -> dict[str, str]:
    if not TREE_RE.fullmatch(tree):
        raise WasmReleaseError("source tree must be a 40-character lowercase Git tree OID")
    if not SHA256_RE.fullmatch(archive_sha256):
        raise WasmReleaseError("source archive SHA-256 must be 64 lowercase hex characters")
    if not TREE_RE.fullmatch(base_commit):
        raise WasmReleaseError("base commit must be a 40-character lowercase Git OID")
    return {
        "staged_tree": tree,
        "base_commit": base_commit,
        "archive_sha256": archive_sha256,
    }


def create_manifest(arguments: argparse.Namespace) -> dict[str, object]:
    artifact = validate_module(arguments.artifact)
    payload = {
        "schema": SCHEMA,
        "source": _source_binding(
            arguments.source_tree,
            arguments.base_commit,
            arguments.source_archive_sha256,
        ),
        "input": {"path": LOGICAL_INPUT, **file_identity(arguments.input, label="O input")},
        "generator": {
            "path": LOGICAL_GENERATOR,
            **file_identity(arguments.generator, label="olangc generator"),
        },
        "project": project_identity(arguments.project),
        "artifact": {"path": LOGICAL_ARTIFACT, **artifact},
        "build": {
            "target": TARGET,
            "profile": "release",
            "opt_level": 1,
            "lto": False,
            "codegen_units": 16,
            "cargo_offline": True,
            "rust_toolchain": arguments.rust_toolchain,
        },
    }
    if not arguments.rust_toolchain.startswith("rustc "):
        raise WasmReleaseError("release compiler identity must begin with 'rustc '")
    return payload


def _load_manifest(path: Path) -> dict[str, object]:
    file_identity(path, label="WASM release manifest")
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise WasmReleaseError(f"WASM release manifest is unreadable: {error}") from error
    if not isinstance(payload, dict):
        raise WasmReleaseError("WASM release manifest must be a JSON object")
    return payload


def verify_manifest(arguments: argparse.Namespace) -> dict[str, object]:
    payload = _load_manifest(arguments.manifest)
    if set(payload) != {
        "schema",
        "source",
        "input",
        "generator",
        "project",
        "artifact",
        "build",
    } or payload.get("schema") != SCHEMA:
        raise WasmReleaseError("WASM release manifest has the wrong schema or shape")
    expected = {
        "source": _source_binding(
            arguments.source_tree,
            arguments.base_commit,
            arguments.source_archive_sha256,
        ),
        "input": {"path": LOGICAL_INPUT, **file_identity(arguments.input, label="O input")},
        "generator": {
            "path": LOGICAL_GENERATOR,
            **file_identity(arguments.generator, label="olangc generator"),
        },
        "project": project_identity(arguments.project),
        "artifact": {
            "path": LOGICAL_ARTIFACT,
            **validate_module(arguments.artifact),
        },
    }
    for key, value in expected.items():
        if payload.get(key) != value:
            raise WasmReleaseError(f"WASM release manifest {key} binding differs")
    build = payload.get("build")
    if not isinstance(build, dict) or set(build) != {
        "target",
        "profile",
        "opt_level",
        "lto",
        "codegen_units",
        "cargo_offline",
        "rust_toolchain",
    }:
        raise WasmReleaseError("WASM release manifest has the wrong build contract")
    rust_toolchain = build.get("rust_toolchain")
    if (
        not isinstance(rust_toolchain, str)
        or not rust_toolchain.startswith("rustc ")
        or len(rust_toolchain.encode("utf-8")) > 512
        or not rust_toolchain.isprintable()
    ):
        raise WasmReleaseError("WASM release manifest has an invalid compiler identity")
    expected_build = {
        "target": TARGET,
        "profile": "release",
        "opt_level": 1,
        "lto": False,
        "codegen_units": 16,
        "cargo_offline": True,
        "rust_toolchain": rust_toolchain,
    }
    if build != expected_build:
        raise WasmReleaseError("WASM release manifest has the wrong build contract")
    return payload


def _add_binding_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--generator", type=Path, required=True)
    parser.add_argument("--source-tree", required=True)
    parser.add_argument("--base-commit", required=True)
    parser.add_argument("--source-archive-sha256", required=True)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    create = subparsers.add_parser("create")
    _add_binding_arguments(create)
    create.add_argument("--rust-toolchain", required=True)
    create.add_argument("--output", type=Path, required=True)
    verify = subparsers.add_parser("verify")
    _add_binding_arguments(verify)
    verify.add_argument("--manifest", type=Path, required=True)
    module = subparsers.add_parser("verify-module")
    module.add_argument("artifact", type=Path)
    return parser.parse_args(argv)


def _write_exclusive(path: Path, payload: dict[str, object]) -> None:
    encoded = (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode("utf-8")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags, 0o444)
    try:
        with os.fdopen(descriptor, "wb", closefd=False) as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
    finally:
        os.close(descriptor)
    os.chmod(path, 0o444)


def main(argv: list[str] | None = None) -> int:
    arguments = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        if arguments.command == "create":
            payload = create_manifest(arguments)
            _write_exclusive(arguments.output, payload)
        elif arguments.command == "verify":
            payload = verify_manifest(arguments)
        else:
            identity = validate_module(arguments.artifact)
            payload = {"schema": "ostadix.wasm-module-verification/v1", **identity}
    except (OSError, WasmReleaseError) as error:
        print(f"ostadix-wasm-release: ERROR: {error}", file=sys.stderr)
        return 1
    print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
