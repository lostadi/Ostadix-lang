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
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from typing import Iterable, Sequence
import zipfile


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
        "ARCHITECTURE.md",
        "CITATION.cff",
        "Cargo.lock",
        "Cargo.toml",
        "DEVELOPMENT.md",
        "Dockerfile",
        "LICENSE",
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
    }
)

ALLOWED_TOP_LEVEL_DIRECTORIES = frozenset(
    {
        ".github",
        "assets",
        "backends",
        "c_cpp",
        "docs",
        "examples",
        "fuzz",
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
        "Cargo.toml",
        "README.md",
        "boot-and-test.sh",
        "okernel-multikernel/boot-and-test.sh",
    }
)
VALID_GIT_MODES = frozenset({"100644", "100755", "120000"})
SAFE_PREFIX = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")
HEX_DIGEST = re.compile(r"[0-9a-f]{64}\Z")
HEX_COMMIT = re.compile(r"[0-9a-f]{40,64}\Z")


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

    return [
        SourceEntry(path=path, mode=mode, data=_git(repo, "cat-file", "blob", oid))
        for path, mode, oid in selected
    ]


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
                zip_mode = f"{(info_by_name[member].external_attr >> 16) & 0xFFFF:06o}"
                if zip_mode != mode:
                    raise ReleaseError(f"ZIP mode does not match manifest: {relative}")

            expected_order = expected_names + [manifest_name, checksums_name]
            if names != expected_order:
                raise ReleaseError("release ZIP member set or ordering does not match manifest")

            expected_checksums[MANIFEST_NAME] = hashlib.sha256(manifest_bytes).hexdigest()
            actual_checksums = _parse_checksums(archive.read(checksums_name))
            if actual_checksums != expected_checksums:
                raise ReleaseError("SHA256SUMS does not match the release payload")

            for info in infos:
                if info.date_time != FIXED_ZIP_TIMESTAMP:
                    raise ReleaseError(f"non-deterministic ZIP timestamp: {info.filename}")
                if info.create_system != 3:
                    raise ReleaseError(f"non-Unix ZIP metadata: {info.filename}")
                if info.compress_type != zipfile.ZIP_DEFLATED:
                    raise ReleaseError(f"unexpected ZIP compression: {info.filename}")
                if info.extra or info.comment:
                    raise ReleaseError(f"non-deterministic ZIP metadata: {info.filename}")
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
