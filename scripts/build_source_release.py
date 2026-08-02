#!/usr/bin/env python3
"""Build and verify deterministic Ostadix-lang source-release ZIP files.

Release contents are read from Git objects at a resolved commit, never from
the working tree.  The explicit allowlist below defines the public source
surface; generated output and local development debris remain excluded even
when they were accidentally committed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import string
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from typing import Iterable, Sequence
from urllib.parse import unquote, urlsplit
import zipfile
import zlib


SCHEMA = "ostadix-source-release-v1"
MANIFEST_NAME = "SOURCE-MANIFEST.json"
CHECKSUMS_NAME = "SHA256SUMS"
FIXED_ZIP_TIMESTAMP = (1980, 1, 1, 0, 0, 0)

# Keep this list intentionally narrow.  Adding a new top-level project surface
# requires an explicit release-engineering decision here.
ALLOWED_TOP_LEVEL_FILES = frozenset(
    {
        ".dockerignore",
        ".gitignore",
        ".mcp.json",
        "ARCHITECTURE.md",
        "CITATION.cff",
        "Cargo.lock",
        "Cargo.toml",
        "DEVELOPMENT.md",
        "Dockerfile",
        "LICENSE",
        "llms.txt",
        "NOTICE",
        "ORIGIN.md",
        "README.md",
        "SPEC.md",
        "big_iron_to_my_texas_red.sh",
        "boot-and-test.sh",
        "setup.sh",
        "test_o_lang_examples.sh",
    }
)

# Keep nested exceptions exact so adding the mirrored aggregate launcher does
# not implicitly publish every file under a new top-level directory.
ALLOWED_EXACT_PATHS = frozenset(
    {
        "okernel-multikernel/boot-and-test.sh",
        "okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md",
    }
)

ALLOWED_TOP_LEVEL_DIRECTORIES = frozenset(
    {
        ".github",
        "assets",
        "backends",
        "c_cpp",
        "docs",
        "evidence",
        "examples",
        "fuzz",
        "mcp",
        "o_lang",
        "ocore",
        "scripts",
        "setup",
        "src",
        "tests",
    }
)

EXCLUDED_DIRECTORY_NAMES = frozenset(
    {
        ".cache",
        ".git",
        ".hypothesis",
        ".idea",
        ".mypy_cache",
        ".nox",
        ".ocore-repair-backups",
        ".pytest_cache",
        ".ruff_cache",
        ".tox",
        ".venv",
        ".vscode",
        "CMakeFiles",
        "__pycache__",
        "build",
        "dist",
        "htmlcov",
        "out",
        "target",
    }
)

EXCLUDED_EXACT_PATHS = frozenset(
    {
        "c_cpp/O",
        "c_cpp/olangc",
        "codebase_tape.md",
        "test.html",
    }
)

EXCLUDED_BASENAMES = frozenset({".DS_Store", "Thumbs.db"})
EXCLUDED_SUFFIXES = (
    ".a",
    ".d",
    ".dll",
    ".dylib",
    ".exe",
    ".html",
    ".lib",
    ".o",  # Deliberately case-sensitive: .O files are O language source.
    ".obj",
    ".patch",
    ".pdb",
    ".profdata",
    ".profraw",
    ".pyc",
    ".pyo",
    ".rmeta",
    ".rlib",
    ".so",
    ".wasm",
)

REQUIRED_RELEASE_PATHS = frozenset(
    {
        ".mcp.json",
        "Cargo.toml",
        "LICENSE",
        "llms.txt",
        "mcp/ostadix_lang_mcp_server/Cargo.lock",
        "mcp/ostadix_lang_mcp_server/Cargo.toml",
        "mcp/ostadix_lang_mcp_server/README.md",
        "mcp/ostadix_lang_mcp_server/src/main.rs",
        "README.md",
        "boot-and-test.sh",
        "docs/HOSTED_WORLD_REFERENCE_PROFILE.md",
        "docs/OSTADIX_WORLD.md",
        "evidence/gates.toml",
        "evidence/world_alpha_gates.toml",
        "examples/manifest.json",
        "okernel-multikernel/boot-and-test.sh",
        "okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md",
        "ocore/kernel/boot.S",
        "ocore/kernel/build.sh",
        "ocore/kernel/main.oc",
        "ocore/kernel/smoke-world-receipt-qemu.sh",
        "ocore/kernel/smoke-world-value-qemu.sh",
        "ocore/kernel/smoke-world-protocol-qemu.sh",
        "ocore/kernel/smoke-world-identity-qemu.sh",
        "ocore/kernel/world_value_semantics.oc",
        "ocore/kernel/world_value_semantics_stub.oc",
        "ocore/kernel/world_protocol_semantics.oc",
        "ocore/kernel/world_protocol_semantics_stub.oc",
        "ocore/kernel/world_identity_semantics.oc",
        "ocore/kernel/world_identity_semantics_stub.oc",
        "ocore/kernel/world_receipt_semantics.oc",
        "ocore/kernel/world_receipt_semantics_stub.oc",
        "ocore/runtime/x86_64/trap.oc",
        "ocore/world/codec.oc",
        "ocore/world/identity.oc",
        "ocore/world/protocol.oc",
        "ocore/world/receipt.oc",
        "ocore/world/receipt_codec.oc",
        "ocore/world/sha256.oc",
        "ocore/world/value.oc",
        "ocore/world/value_codec.oc",
        "scripts/smoke_ostadix_mcp.py",
        "scripts/release_evidence.py",
        "scripts/world_alpha_evidence.py",
        "src/world/identity.rs",
        "src/world/identity_wire.rs",
        "src/world/codec.rs",
        "src/world/mod.rs",
        "src/world/protocol.rs",
        "src/world/receipt.rs",
        "src/world/receipt_codec.rs",
        "src/world/value.rs",
        "src/world/value_codec.rs",
        "tests/example_manifest.py",
        "tests/fixtures/world_identity_v1.hex",
        "tests/fixtures/world_protocol_v1.hex",
        "tests/fixtures/world_receipt_v1.hex",
        "tests/fixtures/world_value_v1.hex",
        "tests/test_example_manifest.py",
        "tests/test_mcp_smoke.py",
        "tests/test_world_alpha_evidence.py",
        "tests/world_identity.rs",
        "tests/world_identity_wire.rs",
        "tests/world_protocol.rs",
        "tests/world_receipt.rs",
        "tests/world_value.rs",
    }
)
VALID_GIT_MODES = frozenset({"100644", "100755"})
SAFE_PREFIX = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")
HEX_DIGEST = re.compile(r"[0-9a-f]{64}\Z")
HEX_COMMIT = re.compile(r"[0-9a-f]{40,64}\Z")
URI_SCHEME = re.compile(r"[A-Za-z][A-Za-z0-9+.-]*:")
EXAMPLE_EDITIONS = frozenset({"rust", "c17", "python"})
EXAMPLE_CLASSIFICATIONS = frozenset({"unit", "integration", "manual"})
EXAMPLE_MODES = frozenset({"interpreter", "aot"})
EVIDENCE_CLASSES = frozenset({"portable_tcg", "hardware_kvm"})
EXPECTED_REQUIRED_EVIDENCE_GATES = 21
EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES = 1

# These three files jointly define the version-1 native World constitution and
# its definition-only G0-G13 registry.  Source releases are built from arbitrary
# committed refs and archive verification must not execute the Python shipped in
# an untrusted ZIP, so keep trusted byte seals here and recheck the inert data
# below.  Any intentional constitutional edit requires an explicit seal update.
SEALED_WORLD_ALPHA_SHA256 = {
    "docs/OSTADIX_WORLD.md": (
        "a81327a43e4cc91faf4f4d4d69de2978e349a5a8fc4b7f558697f75787e20a7b"
    ),
    "docs/HOSTED_WORLD_REFERENCE_PROFILE.md": (
        "eeb6fcac7a9e108221ce8e9d22a260b7d7433c6202114a0aafd251214f138f9c"
    ),
    "evidence/world_alpha_gates.toml": (
        "a4a15bda0771d22076624092768aa4219ae3074be261d80faed1381b8c5b5d42"
    ),
}
EXPECTED_WORLD_ALPHA_GATE_IDS = tuple(f"G{number}" for number in range(14))
EXPECTED_WORLD_ALPHA_CLASS_IDS = (
    "repository_conformance",
    "hosted_reference",
    "qemu_tcg_x86_64",
    "qemu_tcg_aarch64",
    "qemu_virtualization",
    "hardware_x86_64",
    "hardware_x86_64_iommu",
    "hardware_aarch64",
    "hardware_aarch64_smmu",
    "multinode_virtual",
    "multinode_physical",
    "fault_injection",
    "security_adversarial",
    "performance_characterization",
)


class ReleaseError(RuntimeError):
    """A source release could not be built or verified safely."""


@dataclass(frozen=True)
class SourceEntry:
    path: str
    mode: str
    data: bytes

    @property
    def sha256(self) -> str:
        return hashlib.sha256(self.data).hexdigest()


@dataclass(frozen=True)
class BuildResult:
    output: Path
    commit: str
    prefix: str
    file_count: int
    archive_sha256: str


def _git(repo: Path, *arguments: str) -> bytes:
    command = ["git", "-C", os.fspath(repo), *arguments]
    try:
        result = subprocess.run(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as error:
        raise ReleaseError(f"cannot execute Git: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise ReleaseError(
            f"Git command failed ({' '.join(arguments)}): {detail or 'unknown error'}"
        )
    return result.stdout


def discover_repository(path: Path | str) -> Path:
    candidate = Path(path).expanduser().resolve()
    root = _git(candidate, "rev-parse", "--show-toplevel")
    return Path(root.decode("utf-8", "surrogateescape").strip()).resolve()


def resolve_commit(repo: Path, ref: str) -> str:
    if not ref or "\x00" in ref:
        raise ReleaseError("Git ref must be a non-empty string without NUL bytes")
    commit = _git(repo, "rev-parse", "--verify", f"{ref}^{{commit}}")
    value = commit.decode("ascii", "strict").strip()
    if not HEX_COMMIT.fullmatch(value):
        raise ReleaseError(f"Git returned an invalid commit identifier: {value!r}")
    return value


def assert_clean_worktree(repo: Path, *, allow_dirty: bool) -> None:
    status = _git(
        repo,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--ignore-submodules=none",
        "-z",
    )
    if status and not allow_dirty:
        changed = sum(1 for record in status.split(b"\0") if record)
        raise ReleaseError(
            f"working tree is dirty ({changed} status record(s)); commit or stash "
            "the changes, or pass --allow-dirty to archive the selected commit anyway"
        )


def _validate_release_path(path: str) -> PurePosixPath:
    if not path or "\x00" in path or "\n" in path or "\r" in path:
        raise ReleaseError(f"unsafe release path: {path!r}")
    pure = PurePosixPath(path)
    if pure.is_absolute() or any(part in {"", ".", ".."} for part in pure.parts):
        raise ReleaseError(f"unsafe release path: {path!r}")
    if pure.as_posix() != path:
        raise ReleaseError(f"non-canonical release path: {path!r}")
    return pure


def is_allowed_release_path(path: str) -> bool:
    pure = _validate_release_path(path)
    parts = pure.parts
    top = parts[0]
    if path not in ALLOWED_EXACT_PATHS:
        if len(parts) == 1:
            if top not in ALLOWED_TOP_LEVEL_FILES:
                return False
        elif top not in ALLOWED_TOP_LEVEL_DIRECTORIES:
            return False

    if path in EXCLUDED_EXACT_PATHS:
        return False
    if any(
        part in EXCLUDED_DIRECTORY_NAMES
        or part.startswith("cmake-build-")
        or part.endswith(".dSYM")
        for part in parts[:-1]
    ):
        return False

    basename = parts[-1]
    if basename in EXCLUDED_BASENAMES or basename.startswith("cvelist"):
        return False
    if basename.endswith("~") or basename.startswith(".#"):
        return False
    if basename.endswith(EXCLUDED_SUFFIXES):
        return False
    return True


def _decode_git_path(raw_path: bytes) -> str:
    try:
        return raw_path.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError(
            "source releases require UTF-8 Git paths; found an undecodable path"
        ) from error


def collect_source_entries(repo: Path, commit: str) -> list[SourceEntry]:
    tree = _git(repo, "ls-tree", "-r", "-z", "--full-tree", commit)
    selected: list[tuple[str, str, str]] = []

    for record in tree.split(b"\0"):
        if not record:
            continue
        try:
            metadata, raw_path = record.split(b"\t", 1)
            raw_mode, raw_kind, raw_oid = metadata.split(b" ", 2)
        except ValueError as error:
            raise ReleaseError("Git produced a malformed tree record") from error
        path = _decode_git_path(raw_path)
        if not is_allowed_release_path(path):
            continue
        kind = raw_kind.decode("ascii", "strict")
        mode = raw_mode.decode("ascii", "strict")
        oid = raw_oid.decode("ascii", "strict")
        if kind != "blob":
            raise ReleaseError(f"allowlisted path is not a Git blob: {path}")
        if mode not in VALID_GIT_MODES:
            raise ReleaseError(f"unsupported Git mode {mode} for {path}")
        selected.append((path, mode, oid))

    selected.sort(key=lambda item: item[0].encode("utf-8"))
    paths = {path for path, _mode, _oid in selected}
    missing = sorted(REQUIRED_RELEASE_PATHS - paths)
    if missing:
        raise ReleaseError(
            "commit is not an Ostadix-lang source tree; missing required path(s): "
            + ", ".join(missing)
        )
    if len(paths) != len(selected):
        raise ReleaseError("Git tree contains duplicate release paths")

    entries = [
        SourceEntry(path=path, mode=mode, data=_git(repo, "cat-file", "blob", oid))
        for path, mode, oid in selected
    ]
    validate_document_links(entries)
    validate_release_metadata(entries)
    return entries


def _is_markdown_escaped(text: str, index: int) -> bool:
    backslashes = 0
    cursor = index - 1
    while cursor >= 0 and text[cursor] == "\\":
        backslashes += 1
        cursor -= 1
    return backslashes % 2 == 1


def _blank_markdown_range(characters: list[str], start: int, end: int) -> None:
    for index in range(start, end):
        if characters[index] not in "\r\n":
            characters[index] = " "


def _markdown_fence(line: str) -> tuple[str, int, str] | None:
    indent = len(line) - len(line.lstrip(" "))
    if indent > 3 or indent == len(line):
        return None
    marker = line[indent]
    if marker not in {"`", "~"}:
        return None
    cursor = indent
    while cursor < len(line) and line[cursor] == marker:
        cursor += 1
    length = cursor - indent
    if length < 3:
        return None
    return marker, length, line[cursor:]


def _markdown_visible_text(text: str) -> str:
    """Blank Markdown code and comments while preserving offsets and newlines."""

    characters = list(text)
    fence: tuple[str, int] | None = None
    offset = 0
    for line in text.splitlines(keepends=True):
        content = line.rstrip("\r\n")
        candidate = _markdown_fence(content)
        if fence is not None:
            _blank_markdown_range(characters, offset, offset + len(line))
            if (
                candidate is not None
                and candidate[0] == fence[0]
                and candidate[1] >= fence[1]
                and not candidate[2].strip(" \t")
            ):
                fence = None
        elif content.startswith(("    ", "\t")):
            _blank_markdown_range(characters, offset, offset + len(line))
        elif candidate is not None:
            marker, length, remainder = candidate
            # Backticks in a backtick fence's info string make it ordinary text
            # under CommonMark rather than the start of a fenced code block.
            if marker != "`" or "`" not in remainder:
                fence = (marker, length)
                _blank_markdown_range(characters, offset, offset + len(line))
        offset += len(line)

    visible = "".join(characters)
    cursor = 0
    while cursor < len(visible):
        if visible[cursor] != "`" or _is_markdown_escaped(visible, cursor):
            cursor += 1
            continue
        run_end = cursor + 1
        while run_end < len(visible) and visible[run_end] == "`":
            run_end += 1
        run_length = run_end - cursor
        closing = run_end
        while closing < len(visible):
            closing = visible.find("`", closing)
            if closing < 0:
                break
            if _is_markdown_escaped(visible, closing):
                closing += 1
                continue
            closing_end = closing + 1
            while closing_end < len(visible) and visible[closing_end] == "`":
                closing_end += 1
            if closing_end - closing == run_length:
                _blank_markdown_range(characters, cursor, closing_end)
                cursor = closing_end
                break
            closing = closing_end
        else:
            closing = -1
        if closing < 0:
            cursor = run_end

    visible = "".join(characters)
    cursor = 0
    while True:
        opening = visible.find("<!--", cursor)
        if opening < 0:
            break
        closing = visible.find("-->", opening + 4)
        end = len(visible) if closing < 0 else closing + 3
        _blank_markdown_range(characters, opening, end)
        cursor = end
    return "".join(characters)


def _find_matching_markdown_bracket(
    text: str, opening: int, end: int | None = None
) -> int | None:
    limit = len(text) if end is None else end
    depth = 1
    cursor = opening + 1
    while cursor < limit:
        if _is_markdown_escaped(text, cursor):
            cursor += 1
            continue
        if text[cursor] == "[":
            depth += 1
        elif text[cursor] == "]":
            depth -= 1
            if depth == 0:
                return cursor
        cursor += 1
    return None


def _markdown_unescape_destination(value: str) -> str:
    result: list[str] = []
    cursor = 0
    while cursor < len(value):
        if (
            value[cursor] == "\\"
            and cursor + 1 < len(value)
            and value[cursor + 1] in string.punctuation
        ):
            result.append(value[cursor + 1])
            cursor += 2
        else:
            result.append(value[cursor])
            cursor += 1
    return "".join(result)


def _inline_link_close(text: str, start: int) -> int | None:
    cursor = start
    while cursor < len(text) and text[cursor].isspace():
        cursor += 1
    if cursor < len(text) and text[cursor] == ")":
        return cursor
    if cursor >= len(text) or text[cursor] not in {'"', "'", "("}:
        return None

    opener = text[cursor]
    if opener in {'"', "'"}:
        cursor += 1
        while cursor < len(text):
            if text[cursor] == opener and not _is_markdown_escaped(text, cursor):
                cursor += 1
                break
            cursor += 1
        else:
            return None
    else:
        depth = 1
        cursor += 1
        while cursor < len(text) and depth:
            if _is_markdown_escaped(text, cursor):
                cursor += 1
            elif text[cursor] == "(":
                depth += 1
            elif text[cursor] == ")":
                depth -= 1
            cursor += 1
        if depth:
            return None

    while cursor < len(text) and text[cursor].isspace():
        cursor += 1
    return cursor if cursor < len(text) and text[cursor] == ")" else None


def _inline_link_destination(text: str, opening: int) -> tuple[str, int] | None:
    cursor = opening + 1
    while cursor < len(text) and text[cursor].isspace():
        cursor += 1
    if cursor >= len(text):
        return None
    if text[cursor] == ")":
        return "", cursor

    if text[cursor] == "<":
        start = cursor + 1
        cursor = start
        while cursor < len(text):
            if text[cursor] in "\r\n":
                return None
            if text[cursor] == ">" and not _is_markdown_escaped(text, cursor):
                destination = text[start:cursor]
                closing = _inline_link_close(text, cursor + 1)
                if closing is None:
                    return None
                return _markdown_unescape_destination(destination), closing
            cursor += 1
        return None

    start = cursor
    depth = 0
    while cursor < len(text):
        if text[cursor] == "\\" and cursor + 1 < len(text):
            cursor += 2
            continue
        if text[cursor] == "(":
            depth += 1
        elif text[cursor] == ")":
            if depth == 0:
                return _markdown_unescape_destination(text[start:cursor]), cursor
            depth -= 1
        elif text[cursor].isspace() and depth == 0:
            destination = text[start:cursor]
            closing = _inline_link_close(text, cursor)
            if closing is None:
                return None
            return _markdown_unescape_destination(destination), closing
        cursor += 1
    return None


def _reference_destination(text: str, start: int, end: int) -> str | None:
    cursor = start
    while cursor < end and text[cursor] in " \t":
        cursor += 1
    if cursor >= end:
        return None
    if text[cursor] == "<":
        opening = cursor + 1
        cursor = opening
        while cursor < end:
            if text[cursor] == ">" and not _is_markdown_escaped(text, cursor):
                return _markdown_unescape_destination(text[opening:cursor])
            cursor += 1
        return None

    opening = cursor
    depth = 0
    while cursor < end:
        if text[cursor] == "\\" and cursor + 1 < end:
            cursor += 2
            continue
        if text[cursor] == "(":
            depth += 1
        elif text[cursor] == ")":
            if depth == 0:
                break
            depth -= 1
        elif text[cursor].isspace() and depth == 0:
            break
        cursor += 1
    if cursor == opening or depth:
        return None
    return _markdown_unescape_destination(text[opening:cursor])


def _markdown_destinations(text: str) -> list[str]:
    visible = _markdown_visible_text(text)
    destinations: list[str] = []

    offset = 0
    for line in visible.splitlines(keepends=True):
        content_end = offset + len(line.rstrip("\r\n"))
        cursor = offset
        while cursor < content_end and visible[cursor] == " ":
            cursor += 1
        if cursor - offset <= 3 and cursor < content_end and visible[cursor] == "[":
            closing = _find_matching_markdown_bracket(visible, cursor, content_end)
            if (
                closing is not None
                and closing + 1 < content_end
                and visible[closing + 1] == ":"
                and visible[cursor + 1 : closing] != ""
                and not visible[cursor + 1 : closing].startswith("^")
            ):
                destination = _reference_destination(
                    visible, closing + 2, content_end
                )
                if destination is not None:
                    destinations.append(destination)
        offset += len(line)

    cursor = 0
    while cursor < len(visible):
        if visible[cursor] != "[" or _is_markdown_escaped(visible, cursor):
            cursor += 1
            continue
        closing = _find_matching_markdown_bracket(visible, cursor)
        if closing is None:
            cursor += 1
            continue
        if closing + 1 < len(visible) and visible[closing + 1] == "(":
            parsed = _inline_link_destination(visible, closing + 1)
            if parsed is not None:
                destination, link_end = parsed
                destinations.append(destination)
                cursor = link_end + 1
                continue
        cursor = closing + 1
    return destinations


def _resolve_document_target(source: str, destination: str) -> str | None:
    if not destination or destination.startswith(("#", "/", "//")):
        return None
    if URI_SCHEME.match(destination):
        return None

    split = urlsplit(destination)
    if split.scheme or split.netloc or not split.path:
        return None
    decoded = unquote(split.path)
    if (
        PurePosixPath(decoded).is_absolute()
        or "\\" in decoded
        or "\x00" in decoded
    ):
        raise ReleaseError(
            f"documentation link in {source} has an unsafe target: {destination!r}"
        )

    parts: list[str] = []
    for part in (PurePosixPath(source).parent / decoded).parts:
        if part in {"", "."}:
            continue
        if part == "..":
            if not parts:
                raise ReleaseError(
                    f"documentation link in {source} escapes the release root: "
                    f"{destination!r}"
                )
            parts.pop()
        else:
            parts.append(part)
    return "/".join(parts)


def validate_document_links(entries: Sequence[SourceEntry]) -> None:
    """Require every relative Markdown link target to exist in the release."""

    paths = {entry.path for entry in entries}
    directories = {""} | {
        "/".join(PurePosixPath(path).parts[:index])
        for path in paths
        for index in range(1, len(PurePosixPath(path).parts))
    }
    broken: list[str] = []
    for entry in entries:
        if not entry.path.lower().endswith(".md"):
            continue
        try:
            document = entry.data.decode("utf-8", "strict")
        except UnicodeDecodeError as error:
            raise ReleaseError(
                f"release documentation is not UTF-8: {entry.path}"
            ) from error
        for destination in _markdown_destinations(document):
            target = _resolve_document_target(entry.path, destination)
            if target is not None and target not in paths and target not in directories:
                broken.append(f"{entry.path} -> {destination} (resolved {target})")
    if broken:
        raise ReleaseError(
            "release documentation contains missing relative link target(s): "
            + "; ".join(sorted(set(broken)))
        )


def _strict_json(data: bytes, path: str) -> object:
    try:
        text = data.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError(f"{path} is not valid UTF-8") from error

    def object_pairs(pairs: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for key, value in pairs:
            if key in result:
                raise ReleaseError(f"{path} contains duplicate JSON key {key!r}")
            result[key] = value
        return result

    def invalid_constant(value: str) -> object:
        raise ReleaseError(f"{path} contains non-finite JSON number {value}")

    try:
        return json.loads(
            text,
            object_pairs_hook=object_pairs,
            parse_constant=invalid_constant,
        )
    except json.JSONDecodeError as error:
        raise ReleaseError(f"{path} is not valid JSON: {error}") from error


def _strict_toml(data: bytes, path: str) -> dict[str, object]:
    try:
        text = data.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError(f"{path} is not valid UTF-8") from error
    try:
        value = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise ReleaseError(f"{path} is not valid TOML: {error}") from error
    if not isinstance(value, dict):  # pragma: no cover - tomllib roots are tables
        raise ReleaseError(f"{path} root must be a TOML table")
    return value


def _required_string(value: object, owner: str) -> str:
    if not isinstance(value, str) or not value or value != value.strip():
        raise ReleaseError(f"{owner} must be a non-empty trimmed string")
    if "\x00" in value or "\r" in value or "\n" in value:
        raise ReleaseError(f"{owner} contains a forbidden control character")
    return value


def _required_string_list(
    value: object, owner: str, *, minimum: int = 0
) -> list[str]:
    if not isinstance(value, list) or len(value) < minimum:
        raise ReleaseError(f"{owner} must contain at least {minimum} string(s)")
    result = [
        _required_string(item, f"{owner}[{index}]")
        for index, item in enumerate(value)
    ]
    if len(result) != len(set(result)):
        raise ReleaseError(f"{owner} contains a duplicate")
    return result


def _pattern_string_list(value: object, owner: str) -> list[str]:
    if not isinstance(value, list) or not value:
        raise ReleaseError(f"{owner} must contain at least 1 string")
    if any(not isinstance(item, str) or not item or "\x00" in item for item in value):
        raise ReleaseError(f"{owner} must contain non-empty strings without NUL")
    if len(value) != len(set(value)):
        raise ReleaseError(f"{owner} contains a duplicate")
    return value


def _normalized_reference(value: object, owner: str) -> str:
    reference = _required_string(value, owner)
    try:
        _validate_release_path(reference)
    except ReleaseError as error:
        raise ReleaseError(f"{owner} must be a normalized release-relative path") from error
    return reference


def _validate_mcp_release_surface(
    files: dict[str, bytes], modes: dict[str, str]
) -> None:
    config_path = ".mcp.json"
    config = _strict_json(files[config_path], config_path)
    if not isinstance(config, dict) or set(config) != {"mcpServers"}:
        raise ReleaseError(".mcp.json must contain exactly the mcpServers object")
    servers = config["mcpServers"]
    if not isinstance(servers, dict) or set(servers) != {"ostadix"}:
        raise ReleaseError(".mcp.json mcpServers must contain exactly ostadix")
    server = servers["ostadix"]
    if not isinstance(server, dict):
        raise ReleaseError(".mcp.json must define the ostadix server")
    if set(server) != {"command", "args"}:
        raise ReleaseError(".mcp.json ostadix server must contain command and args only")
    if server["command"] != "ostadix-mcp":
        raise ReleaseError(".mcp.json ostadix command must be 'ostadix-mcp'")
    if _required_string_list(server["args"], ".mcp.json ostadix.args"):
        raise ReleaseError(".mcp.json ostadix.args must be empty")

    cargo_path = "mcp/ostadix_lang_mcp_server/Cargo.toml"
    cargo = _strict_toml(files[cargo_path], cargo_path)
    package = cargo.get("package")
    if not isinstance(package, dict):
        raise ReleaseError(f"{cargo_path} must contain a package table")
    if package.get("name") != "ostadix-mcp-server":
        raise ReleaseError(f"{cargo_path} package name must be 'ostadix-mcp-server'")
    if package.get("license") != "LGPL-2.1-only":
        raise ReleaseError(f"{cargo_path} license must be 'LGPL-2.1-only'")
    if package.get("publish") is not False:
        raise ReleaseError(f"{cargo_path} package must remain publish = false")
    binaries = cargo.get("bin")
    if not isinstance(binaries, list):
        raise ReleaseError(f"{cargo_path} must contain an ostadix-mcp bin target")
    matching = [
        binary
        for binary in binaries
        if isinstance(binary, dict) and binary.get("name") == "ostadix-mcp"
    ]
    if len(matching) != 1:
        raise ReleaseError(f"{cargo_path} must define exactly one ostadix-mcp bin target")
    binary_path = _normalized_reference(
        matching[0].get("path"), f"{cargo_path} ostadix-mcp.path"
    )
    referenced_binary = str(PurePosixPath(cargo_path).parent / binary_path)
    if referenced_binary not in files:
        raise ReleaseError(
            f"{cargo_path} references absent binary source {referenced_binary}"
        )
    if modes.get(referenced_binary) not in VALID_GIT_MODES:
        raise ReleaseError(f"{referenced_binary} has an invalid release mode")


def _validate_example_manifest(files: dict[str, bytes]) -> None:
    path = "examples/manifest.json"
    manifest = _strict_json(files[path], path)
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema_version",
        "examples",
    }:
        raise ReleaseError(f"{path} root keys differ from schema")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 1:
        raise ReleaseError(f"{path} schema_version must be 1")
    examples = manifest["examples"]
    if not isinstance(examples, list):
        raise ReleaseError(f"{path} examples must be a list")

    declared: list[str] = []
    required_entry_keys = {
        "path",
        "editions",
        "classification",
        "requirements",
        "expected",
    }
    allowed_entry_keys = required_entry_keys | {"timeout_seconds"}
    allowed_requirement_keys = {
        "backends",
        "programs",
        "guest_programs",
        "python_packages",
        "authorities",
        "opt_in",
        "files",
    }
    for index, entry in enumerate(examples):
        owner = f"{path} examples[{index}]"
        if not isinstance(entry, dict):
            raise ReleaseError(f"{owner} must be an object")
        if not required_entry_keys <= set(entry) or set(entry) - allowed_entry_keys:
            raise ReleaseError(f"{owner} has missing or unknown fields")

        relative = _normalized_reference(entry["path"], f"{owner}.path")
        if "/" in relative and PurePosixPath(relative).parts[0] == "examples":
            raise ReleaseError(f"{owner}.path must be relative to examples/")
        if not relative.endswith(".O"):
            raise ReleaseError(f"{owner}.path must name a .O source")
        declared.append(relative)
        source_path = f"examples/{relative}"
        if source_path not in files:
            raise ReleaseError(f"{owner}.path references absent {source_path}")

        editions = _required_string_list(
            entry["editions"], f"{owner}.editions", minimum=1
        )
        if not set(editions) <= EXAMPLE_EDITIONS:
            raise ReleaseError(f"{owner}.editions contains an unknown edition")
        if entry["classification"] not in EXAMPLE_CLASSIFICATIONS:
            raise ReleaseError(f"{owner}.classification is invalid")

        requirements = entry["requirements"]
        if not isinstance(requirements, dict):
            raise ReleaseError(f"{owner}.requirements must be an object")
        if set(requirements) - allowed_requirement_keys or not {
            "backends",
            "programs",
            "authorities",
        } <= set(requirements):
            raise ReleaseError(f"{owner}.requirements has missing or unknown fields")
        for field, value in requirements.items():
            values = _required_string_list(value, f"{owner}.requirements.{field}")
            if field == "files":
                for reference in values:
                    normalized = _normalized_reference(
                        reference, f"{owner}.requirements.files"
                    )
                    if normalized not in files:
                        raise ReleaseError(
                            f"{owner}.requirements.files references absent {normalized}"
                        )

        expected = entry["expected"]
        if not isinstance(expected, dict) or set(expected) != set(editions):
            raise ReleaseError(f"{owner}.expected keys must exactly match editions")
        for edition, expectation in expected.items():
            expectation_owner = f"{owner}.expected.{edition}"
            if not isinstance(expectation, dict) or set(expectation) - {
                "result",
                "patterns",
                "modes",
            }:
                raise ReleaseError(f"{expectation_owner} has an invalid structure")
            patterns = expectation.get("patterns")
            if patterns is not None:
                _pattern_string_list(patterns, f"{expectation_owner}.patterns")
            if "result" not in expectation and patterns is None:
                raise ReleaseError(f"{expectation_owner} needs result or patterns")
            modes = _required_string_list(
                expectation.get("modes", ["interpreter"]),
                f"{expectation_owner}.modes",
                minimum=1,
            )
            if not set(modes) <= EXAMPLE_MODES:
                raise ReleaseError(f"{expectation_owner}.modes contains an unknown mode")
            if edition != "c17" and "aot" in modes:
                raise ReleaseError(f"{expectation_owner}: only c17 supports aot mode")

        timeout = entry.get("timeout_seconds", 10)
        if type(timeout) is not int or timeout <= 0:
            raise ReleaseError(f"{owner}.timeout_seconds must be a positive integer")

    if declared != sorted(declared) or len(declared) != len(set(declared)):
        raise ReleaseError(f"{path} paths must be unique and sorted")
    actual = sorted(
        member[len("examples/") :]
        for member in files
        if member.startswith("examples/") and member.endswith(".O")
    )
    if declared != actual:
        raise ReleaseError(
            f"{path} coverage differs from release examples; "
            f"missing={sorted(set(actual) - set(declared))}, "
            f"extra={sorted(set(declared) - set(actual))}"
        )


def _validate_evidence_manifest(
    files: dict[str, bytes], modes: dict[str, str]
) -> None:
    path = "evidence/gates.toml"
    manifest = _strict_toml(files[path], path)
    expected_root_keys = {
        "schema_version",
        "required_gate_count",
        "supplemental_gate_count",
        "portable_command",
        "gate",
    }
    if set(manifest) != expected_root_keys:
        raise ReleaseError(f"{path} root keys differ from schema")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 1:
        raise ReleaseError(f"{path} schema_version must be 1")
    if type(manifest["required_gate_count"]) is not int or (
        manifest["required_gate_count"] != EXPECTED_REQUIRED_EVIDENCE_GATES
    ):
        raise ReleaseError(
            f"{path} required_gate_count must be {EXPECTED_REQUIRED_EVIDENCE_GATES}"
        )
    if type(manifest["supplemental_gate_count"]) is not int or (
        manifest["supplemental_gate_count"] != EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES
    ):
        raise ReleaseError(
            f"{path} supplemental_gate_count must be "
            f"{EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES}"
        )
    if manifest["portable_command"] != "./boot-and-test.sh smoke":
        raise ReleaseError(f"{path} portable_command must be './boot-and-test.sh smoke'")
    if modes.get("boot-and-test.sh") != "100755":
        raise ReleaseError(f"{path} portable command must reference executable boot-and-test.sh")

    gates = manifest["gate"]
    if not isinstance(gates, list):
        raise ReleaseError(f"{path} gate must be a list of tables")
    expected_gate_count = (
        EXPECTED_REQUIRED_EVIDENCE_GATES + EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES
    )
    if len(gates) != expected_gate_count:
        raise ReleaseError(f"{path} must contain exactly {expected_gate_count} gate tables")
    expected_gate_keys = {
        "id",
        "required",
        "milestone",
        "script",
        "evidence_class",
        "required_tools",
        "positive_claims",
        "nonclaims",
        "expected_markers",
    }
    identifiers: set[str] = set()
    scripts: set[str] = set()
    required_count = 0
    for index, gate in enumerate(gates):
        owner = f"{path} gate[{index}]"
        if not isinstance(gate, dict) or set(gate) != expected_gate_keys:
            raise ReleaseError(f"{owner} keys differ from schema")
        identifier = _required_string(gate["id"], f"{owner}.id")
        if identifier in identifiers:
            raise ReleaseError(f"{owner}.id is duplicated")
        identifiers.add(identifier)
        required = gate["required"]
        if not isinstance(required, bool):
            raise ReleaseError(f"{owner}.required must be a boolean")
        required_count += int(required)
        _required_string(gate["milestone"], f"{owner}.milestone")
        evidence_class = _required_string(
            gate["evidence_class"], f"{owner}.evidence_class"
        )
        if evidence_class not in EVIDENCE_CLASSES:
            raise ReleaseError(f"{owner}.evidence_class is invalid")
        if required and evidence_class != "portable_tcg":
            raise ReleaseError(f"{owner}: required gates must be portable_tcg")
        if not required and evidence_class != "hardware_kvm":
            raise ReleaseError(f"{owner}: the supplemental gate must be hardware_kvm")
        script = _normalized_reference(gate["script"], f"{owner}.script")
        script_path = PurePosixPath(script)
        if script_path.parent != PurePosixPath("ocore/kernel") or script_path.suffix != ".sh":
            raise ReleaseError(f"{owner}.script must name an ocore/kernel shell gate")
        if script in scripts:
            raise ReleaseError(f"{owner}.script is duplicated")
        scripts.add(script)
        if script not in files:
            raise ReleaseError(f"{owner}.script references absent {script}")
        if modes.get(script) != "100755":
            raise ReleaseError(f"{owner}.script references non-executable {script}")
        _required_string_list(
            gate["required_tools"], f"{owner}.required_tools", minimum=1
        )
        _required_string_list(
            gate["positive_claims"], f"{owner}.positive_claims", minimum=1
        )
        _required_string_list(gate["nonclaims"], f"{owner}.nonclaims", minimum=1)
        _required_string_list(
            gate["expected_markers"], f"{owner}.expected_markers", minimum=2
        )

    supplemental_count = len(gates) - required_count
    if required_count != EXPECTED_REQUIRED_EVIDENCE_GATES:
        raise ReleaseError(
            f"{path} must contain exactly {EXPECTED_REQUIRED_EVIDENCE_GATES} "
            "required gate tables"
        )
    if supplemental_count != EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES:
        raise ReleaseError(
            f"{path} must contain exactly {EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES} "
            "supplemental gate table"
        )
    if required_count != manifest["required_gate_count"]:
        raise ReleaseError(f"{path} required_gate_count does not match gate tables")
    if supplemental_count != manifest["supplemental_gate_count"]:
        raise ReleaseError(f"{path} supplemental_gate_count does not match gate tables")


def _sealed_world_alpha_text(
    files: dict[str, bytes], modes: dict[str, str], path: str
) -> str:
    if modes.get(path) != "100644":
        raise ReleaseError(f"{path} must use release mode 100644")
    expected = SEALED_WORLD_ALPHA_SHA256[path]
    actual = hashlib.sha256(files[path]).hexdigest()
    if actual != expected:
        raise ReleaseError(
            f"{path} SHA-256 differs from sealed World Alpha v1 bytes; "
            f"expected {expected}, got {actual}"
        )
    try:
        return files[path].decode("utf-8", "strict")
    except UnicodeDecodeError as error:  # The seal makes this corruption-only.
        raise ReleaseError(f"{path} is not valid UTF-8") from error


def _validate_world_alpha_release_surface(
    files: dict[str, bytes], modes: dict[str, str]
) -> None:
    texts = {
        path: _sealed_world_alpha_text(files, modes, path)
        for path in SEALED_WORLD_ALPHA_SHA256
    }
    required_document_markers = {
        "docs/OSTADIX_WORLD.md": (
            "# Ostadix World: Full-Stack Machine-Constructor Roadmap",
            "**Status:** normative native Alpha constitution and implementation program,",
            "| **G0 -- constitutional baseline** |",
            "| **G13 -- eight-node World Alpha** |",
            "Its first schema is definition-only and cannot certify a passage;",
            "# 28. Alpha non-claims",
        ),
        "docs/HOSTED_WORLD_REFERENCE_PROFILE.md": (
            "# Hosted World Reference Profile",
            "**Status:** design/reference profile with partial hosted foundations;",
            "non-qualifying for native Ostadix World release gates.",
            "cannot satisfy G0 through G13",
            "## Non-claims",
            "G12, G13, or the name **Ostadix World Alpha**.",
        ),
    }
    for path, markers in required_document_markers.items():
        for marker in markers:
            if marker not in texts[path]:
                raise ReleaseError(f"{path} is missing required World Alpha marker {marker!r}")

    path = "evidence/world_alpha_gates.toml"
    manifest = _strict_toml(files[path], path)
    expected_root_keys = {
        "schema_version",
        "constitution_version",
        "constitution",
        "hosted_reference_profile",
        "alpha_gate",
        "gate_count",
        "evidence_class",
        "gate",
    }
    if set(manifest) != expected_root_keys:
        raise ReleaseError(f"{path} root keys differ from schema")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 1:
        raise ReleaseError(f"{path} schema_version must be 1")
    if (
        type(manifest["constitution_version"]) is not int
        or manifest["constitution_version"] != 1
    ):
        raise ReleaseError(f"{path} constitution_version must be 1")
    if manifest["constitution"] != "docs/OSTADIX_WORLD.md":
        raise ReleaseError(f"{path} constitution must reference docs/OSTADIX_WORLD.md")
    if manifest["hosted_reference_profile"] != (
        "docs/HOSTED_WORLD_REFERENCE_PROFILE.md"
    ):
        raise ReleaseError(
            f"{path} hosted_reference_profile must reference "
            "docs/HOSTED_WORLD_REFERENCE_PROFILE.md"
        )
    if manifest["alpha_gate"] != "G13":
        raise ReleaseError(f"{path} alpha_gate must be G13")
    if type(manifest["gate_count"]) is not int or manifest["gate_count"] != 14:
        raise ReleaseError(f"{path} gate_count must be 14")

    evidence_classes = manifest["evidence_class"]
    if not isinstance(evidence_classes, list):
        raise ReleaseError(f"{path} evidence_class must be a list of tables")
    class_ids: list[str] = []
    for index, evidence_class in enumerate(evidence_classes):
        owner = f"{path} evidence_class[{index}]"
        if not isinstance(evidence_class, dict) or set(evidence_class) != {
            "id",
            "scope",
            "description",
        }:
            raise ReleaseError(f"{owner} keys differ from schema")
        class_ids.append(_required_string(evidence_class["id"], f"{owner}.id"))
        _required_string(evidence_class["scope"], f"{owner}.scope")
        _required_string(evidence_class["description"], f"{owner}.description")
    if tuple(class_ids) != EXPECTED_WORLD_ALPHA_CLASS_IDS:
        raise ReleaseError(f"{path} evidence-class IDs or order differ from schema")
    known_classes = set(class_ids)

    gates = manifest["gate"]
    if not isinstance(gates, list) or len(gates) != 14:
        raise ReleaseError(f"{path} must contain exactly 14 gate tables")
    gate_ids: list[str] = []
    expected_gate_keys = {
        "id",
        "title",
        "status",
        "depends_on",
        "required_classes",
        "one_of_classes",
        "acceptance",
        "prohibited_substitutes",
        "evidence",
    }
    for index, gate in enumerate(gates):
        owner = f"{path} gate[{index}]"
        if not isinstance(gate, dict) or set(gate) != expected_gate_keys:
            raise ReleaseError(f"{owner} keys differ from schema")
        gate_ids.append(_required_string(gate["id"], f"{owner}.id"))
        _required_string(gate["title"], f"{owner}.title")
        if gate["status"] != "defined":
            raise ReleaseError(f"{owner}.status must remain 'defined' in schema v1")
        dependencies = _required_string_list(gate["depends_on"], f"{owner}.depends_on")
        unknown_dependencies = set(dependencies) - set(EXPECTED_WORLD_ALPHA_GATE_IDS)
        if unknown_dependencies:
            raise ReleaseError(f"{owner}.depends_on references an unknown gate")
        required_classes = _required_string_list(
            gate["required_classes"], f"{owner}.required_classes", minimum=1
        )
        if set(required_classes) - known_classes:
            raise ReleaseError(f"{owner}.required_classes references an unknown class")
        alternatives = gate["one_of_classes"]
        if not isinstance(alternatives, list):
            raise ReleaseError(f"{owner}.one_of_classes must be a list")
        for group_index, group in enumerate(alternatives):
            choices = _required_string_list(
                group, f"{owner}.one_of_classes[{group_index}]", minimum=1
            )
            if set(choices) - known_classes:
                raise ReleaseError(f"{owner}.one_of_classes references an unknown class")
        _required_string(gate["acceptance"], f"{owner}.acceptance")
        _required_string_list(
            gate["prohibited_substitutes"],
            f"{owner}.prohibited_substitutes",
            minimum=1,
        )
        if gate["evidence"] != []:
            raise ReleaseError(f"{owner}.evidence must remain empty in schema v1")
    if tuple(gate_ids) != EXPECTED_WORLD_ALPHA_GATE_IDS:
        raise ReleaseError(f"{path} gate IDs or order differ from G0 through G13")


def validate_release_metadata(entries: Sequence[SourceEntry]) -> None:
    """Validate inert release metadata and every archive-local reference."""

    files = {entry.path: entry.data for entry in entries}
    modes = {entry.path: entry.mode for entry in entries}
    if len(files) != len(entries):
        raise ReleaseError("release contains duplicate metadata paths")
    _validate_mcp_release_surface(files, modes)
    _validate_example_manifest(files)
    _validate_evidence_manifest(files, modes)
    _validate_world_alpha_release_surface(files, modes)


def _canonical_json(value: object) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
        + "\n"
    ).encode("ascii")


def _manifest_bytes(commit: str, prefix: str, entries: Sequence[SourceEntry]) -> bytes:
    manifest = {
        "commit": commit,
        "file_count": len(entries),
        "files": [
            {
                "mode": entry.mode,
                "path": entry.path,
                "sha256": entry.sha256,
                "size": len(entry.data),
            }
            for entry in entries
        ],
        "prefix": prefix,
        "schema": SCHEMA,
    }
    return _canonical_json(manifest)


def _checksums_bytes(entries: Sequence[SourceEntry], manifest: bytes) -> bytes:
    lines = [f"{entry.sha256}  {entry.path}" for entry in entries]
    lines.append(f"{hashlib.sha256(manifest).hexdigest()}  {MANIFEST_NAME}")
    return ("\n".join(lines) + "\n").encode("utf-8")


def _zip_info(name: str, mode: str) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name, FIXED_ZIP_TIMESTAMP)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.create_system = 3
    info.external_attr = int(mode, 8) << 16
    info.flag_bits |= 0x800
    return info


def _zip_filename_bytes(info: zipfile.ZipInfo) -> bytes:
    encoding = "utf-8" if info.flag_bits & 0x800 else "cp437"
    return info.filename.encode(encoding, "strict")


def _validate_zip_member_metadata(
    info: zipfile.ZipInfo, mode: str, payload: bytes
) -> None:
    try:
        info.filename.encode("ascii", "strict")
        expected_flags = 0
    except UnicodeEncodeError:
        expected_flags = 0x800
    expected = {
        "date_time": FIXED_ZIP_TIMESTAMP,
        "compress_type": zipfile.ZIP_DEFLATED,
        "create_system": 3,
        "create_version": 20,
        "extract_version": 20,
        "reserved": 0,
        "flag_bits": expected_flags,
        "volume": 0,
        "internal_attr": 0,
        "external_attr": int(mode, 8) << 16,
        "extra": b"",
        "comment": b"",
        "file_size": len(payload),
        "CRC": zlib.crc32(payload) & 0xFFFFFFFF,
    }
    for field, value in expected.items():
        if getattr(info, field) != value:
            raise ReleaseError(
                f"non-canonical ZIP {field} for {info.filename}: "
                f"expected {value!r}, got {getattr(info, field)!r}"
            )


def _validate_zip_layout(
    release_path: Path,
    archive: zipfile.ZipFile,
    infos: Sequence[zipfile.ZipInfo],
) -> None:
    expected_offset = 0
    for info in infos:
        if info.header_offset != expected_offset:
            raise ReleaseError(
                f"non-canonical ZIP member offset for {info.filename}: "
                f"expected {expected_offset}, got {info.header_offset}"
            )
        expected_offset += 30 + len(_zip_filename_bytes(info)) + info.compress_size
    if archive.start_dir != expected_offset:
        raise ReleaseError("non-canonical ZIP local-header layout")

    central_size = sum(46 + len(_zip_filename_bytes(info)) for info in infos)
    expected_size = archive.start_dir + central_size + 22
    try:
        actual_size = release_path.stat().st_size
    except OSError as error:
        raise ReleaseError(f"cannot stat release ZIP {release_path}: {error}") from error
    if actual_size != expected_size:
        raise ReleaseError(
            f"non-canonical ZIP total size: expected {expected_size}, got {actual_size}"
        )


def _write_archive(
    output: Path,
    prefix: str,
    entries: Sequence[SourceEntry],
    manifest: bytes,
    checksums: bytes,
) -> None:
    with zipfile.ZipFile(
        output,
        mode="w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=9,
        strict_timestamps=True,
    ) as archive:
        archive.comment = b""
        for entry in entries:
            archive.writestr(
                _zip_info(f"{prefix}/{entry.path}", entry.mode),
                entry.data,
                compress_type=zipfile.ZIP_DEFLATED,
                compresslevel=9,
            )
        archive.writestr(
            _zip_info(f"{prefix}/{MANIFEST_NAME}", "100644"),
            manifest,
            compress_type=zipfile.ZIP_DEFLATED,
            compresslevel=9,
        )
        archive.writestr(
            _zip_info(f"{prefix}/{CHECKSUMS_NAME}", "100644"),
            checksums,
            compress_type=zipfile.ZIP_DEFLATED,
            compresslevel=9,
        )


def _archive_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as release:
        for chunk in iter(lambda: release.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _parse_checksums(data: bytes) -> dict[str, str]:
    try:
        text = data.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError("SHA256SUMS is not valid UTF-8") from error
    result: dict[str, str] = {}
    for line in text.splitlines():
        if "  " not in line:
            raise ReleaseError(f"malformed SHA256SUMS line: {line!r}")
        digest, path = line.split("  ", 1)
        _validate_release_path(path)
        if not HEX_DIGEST.fullmatch(digest):
            raise ReleaseError(f"invalid SHA-256 digest for {path}")
        if path in result:
            raise ReleaseError(f"duplicate SHA256SUMS path: {path}")
        result[path] = digest
    return result


def verify_archive(path: Path | str) -> dict[str, object]:
    release_path = Path(path)
    try:
        with zipfile.ZipFile(release_path, "r") as archive:
            if archive.comment:
                raise ReleaseError("release ZIP must not have an archive comment")
            infos = archive.infolist()
            names = [info.filename for info in infos]
            if len(names) != len(set(names)):
                raise ReleaseError("release ZIP contains duplicate member names")
            if not names:
                raise ReleaseError("release ZIP is empty")

            roots = {PurePosixPath(name).parts[0] for name in names}
            if len(roots) != 1:
                raise ReleaseError("release ZIP must contain exactly one top-level prefix")
            prefix = next(iter(roots))
            if not SAFE_PREFIX.fullmatch(prefix):
                raise ReleaseError(f"unsafe release prefix: {prefix!r}")

            manifest_name = f"{prefix}/{MANIFEST_NAME}"
            checksums_name = f"{prefix}/{CHECKSUMS_NAME}"
            if manifest_name not in names or checksums_name not in names:
                raise ReleaseError("release ZIP lacks its embedded manifest or SHA256SUMS")

            manifest_bytes = archive.read(manifest_name)
            try:
                manifest = json.loads(manifest_bytes.decode("ascii", "strict"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise ReleaseError("SOURCE-MANIFEST.json is not canonical JSON") from error
            if not isinstance(manifest, dict) or _canonical_json(manifest) != manifest_bytes:
                raise ReleaseError("SOURCE-MANIFEST.json is not canonical JSON")
            if manifest.get("schema") != SCHEMA:
                raise ReleaseError("unsupported source-release manifest schema")
            if manifest.get("prefix") != prefix:
                raise ReleaseError("manifest prefix does not match ZIP prefix")
            if not isinstance(manifest.get("commit"), str) or not HEX_COMMIT.fullmatch(
                manifest["commit"]
            ):
                raise ReleaseError("manifest contains an invalid commit identifier")

            raw_files = manifest.get("files")
            if not isinstance(raw_files, list):
                raise ReleaseError("manifest files field must be a list")
            if manifest.get("file_count") != len(raw_files):
                raise ReleaseError("manifest file_count does not match its files list")

            expected_names: list[str] = []
            expected_checksums: dict[str, str] = {}
            archive_entries: list[SourceEntry] = []
            previous_path: str | None = None
            info_by_name = {info.filename: info for info in infos}
            for item in raw_files:
                if not isinstance(item, dict) or set(item) != {
                    "mode",
                    "path",
                    "sha256",
                    "size",
                }:
                    raise ReleaseError("manifest contains a malformed file record")
                relative = item["path"]
                mode = item["mode"]
                digest = item["sha256"]
                size = item["size"]
                if not isinstance(relative, str) or not is_allowed_release_path(relative):
                    raise ReleaseError(f"manifest contains a non-allowlisted path: {relative!r}")
                if previous_path is not None and relative.encode("utf-8") <= previous_path.encode(
                    "utf-8"
                ):
                    raise ReleaseError("manifest file paths are not uniquely sorted")
                previous_path = relative
                if mode not in VALID_GIT_MODES:
                    raise ReleaseError(f"manifest contains an invalid mode for {relative}")
                if not isinstance(digest, str) or not HEX_DIGEST.fullmatch(digest):
                    raise ReleaseError(f"manifest contains an invalid digest for {relative}")
                if not isinstance(size, int) or isinstance(size, bool) or size < 0:
                    raise ReleaseError(f"manifest contains an invalid size for {relative}")

                member = f"{prefix}/{relative}"
                expected_names.append(member)
                expected_checksums[relative] = digest
                if member not in info_by_name:
                    raise ReleaseError(f"manifest member is absent from ZIP: {relative}")
                payload = archive.read(member)
                if len(payload) != size or hashlib.sha256(payload).hexdigest() != digest:
                    raise ReleaseError(f"payload does not match manifest: {relative}")
                archive_entries.append(
                    SourceEntry(path=relative, mode=mode, data=payload)
                )
                zip_mode = f"{(info_by_name[member].external_attr >> 16) & 0xFFFF:06o}"
                if zip_mode != mode:
                    raise ReleaseError(f"ZIP mode does not match manifest: {relative}")

            archived_paths = {entry.path for entry in archive_entries}
            missing_required = sorted(REQUIRED_RELEASE_PATHS - archived_paths)
            if missing_required:
                raise ReleaseError(
                    "release ZIP is missing required path(s): "
                    + ", ".join(missing_required)
                )
            validate_document_links(archive_entries)
            validate_release_metadata(archive_entries)

            expected_order = expected_names + [manifest_name, checksums_name]
            if names != expected_order:
                raise ReleaseError("release ZIP member set or ordering does not match manifest")

            expected_checksums[MANIFEST_NAME] = hashlib.sha256(manifest_bytes).hexdigest()
            checksums_bytes = archive.read(checksums_name)
            actual_checksums = _parse_checksums(checksums_bytes)
            if actual_checksums != expected_checksums:
                raise ReleaseError("SHA256SUMS does not match the release payload")

            canonical_members = {
                f"{prefix}/{entry.path}": (entry.mode, entry.data)
                for entry in archive_entries
            }
            canonical_members[manifest_name] = ("100644", manifest_bytes)
            canonical_members[checksums_name] = ("100644", checksums_bytes)
            for info in infos:
                mode, payload = canonical_members[info.filename]
                _validate_zip_member_metadata(info, mode, payload)
            _validate_zip_layout(release_path, archive, infos)
            return manifest
    except (OSError, zipfile.BadZipFile, KeyError) as error:
        if isinstance(error, ReleaseError):
            raise
        raise ReleaseError(f"cannot verify release ZIP {release_path}: {error}") from error


def build_release(
    repo: Path | str,
    ref: str,
    output: Path | str,
    *,
    allow_dirty: bool = False,
    prefix: str | None = None,
) -> BuildResult:
    root = discover_repository(repo)
    assert_clean_worktree(root, allow_dirty=allow_dirty)
    commit = resolve_commit(root, ref)
    release_prefix = prefix or f"Ostadix-lang-source-{commit[:12]}"
    if not SAFE_PREFIX.fullmatch(release_prefix):
        raise ReleaseError(
            "release prefix must start with an alphanumeric character and contain "
            "only letters, digits, dots, underscores, or hyphens"
        )

    entries = collect_source_entries(root, commit)
    manifest = _manifest_bytes(commit, release_prefix, entries)
    checksums = _checksums_bytes(entries, manifest)
    destination = Path(output).expanduser().resolve()
    destination.parent.mkdir(parents=True, exist_ok=True)

    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{destination.name}.", suffix=".tmp", dir=destination.parent
    )
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        _write_archive(temporary, release_prefix, entries, manifest, checksums)
        verified = verify_archive(temporary)
        if verified["commit"] != commit:
            raise ReleaseError("self-verification returned the wrong commit")
        os.replace(temporary, destination)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass

    return BuildResult(
        output=destination,
        commit=commit,
        prefix=release_prefix,
        file_count=len(entries),
        archive_sha256=_archive_sha256(destination),
    )


def _argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Build or verify a deterministic allowlist-driven source release"
    )
    parser.add_argument("--repo", default=".", help="repository path (default: current directory)")
    parser.add_argument("--ref", default="HEAD", help="committed Git ref to archive")
    parser.add_argument("--output", help="output ZIP path (default: dist/<prefix>.zip)")
    parser.add_argument("--prefix", help="override the deterministic top-level ZIP prefix")
    parser.add_argument(
        "--allow-dirty",
        action="store_true",
        help="allow a dirty worktree; archive bytes still come only from the resolved commit",
    )
    parser.add_argument("--verify", metavar="ZIP", help="verify an existing release instead")
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    arguments = _argument_parser().parse_args(list(argv) if argv is not None else None)
    try:
        if arguments.verify:
            manifest = verify_archive(arguments.verify)
            digest = _archive_sha256(Path(arguments.verify))
            print(
                f"verified {arguments.verify}: {manifest['file_count']} files, "
                f"commit {manifest['commit']}, sha256 {digest}"
            )
            return 0

        root = discover_repository(arguments.repo)
        commit = resolve_commit(root, arguments.ref)
        prefix = arguments.prefix or f"Ostadix-lang-source-{commit[:12]}"
        output = arguments.output or os.fspath(root / "dist" / f"{prefix}.zip")
        result = build_release(
            root,
            arguments.ref,
            output,
            allow_dirty=arguments.allow_dirty,
            prefix=prefix,
        )
        print(
            f"built {result.output}: {result.file_count} files, commit {result.commit}, "
            f"sha256 {result.archive_sha256}"
        )
        return 0
    except ReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
