#!/usr/bin/env python3
"""Build and verify the deterministic Ostadix Boot Object Store v1.

The store is a read-only, content-addressed projection of one exact Git tree.
The domain-separated digest appended to ``index.bin`` is its only public root;
v1 deliberately has no parallel receipt or admission-authority object.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
from typing import Iterable, Sequence


INDEX_MAGIC = b"OBOIDX\0\0"
INDEX_VERSION = 1
INDEX_HEADER_LENGTH = 80
INDEX_DIGEST_DOMAIN = b"ostadix.boot-object-index/v1\0"
INDEX_MAX_BYTES = 16 * 1024 * 1024
MAX_OBJECTS = 4096
MAX_BINDINGS = 4096
MAX_OBJECT_BYTES = 64 * 1024 * 1024
MAX_LOGICAL_BYTES = 256 * 1024 * 1024
MAX_STORED_BYTES = 256 * 1024 * 1024
MAX_PATH_BYTES = 4096
MAX_PATH_COMPONENTS = 32
MAX_COMPONENT_BYTES = 255
SOURCE_DATE_EPOCH = 315_532_800
ALLOWED_MODES = frozenset((0o100644, 0o100755))
RESULT_SCHEMA = "ostadix.boot-object-store-result/v1"

_HEADER = struct.Struct(">8sHHI20s20sIIQQ")
_OBJECT = struct.Struct(">32s20sQ")
_BINDING_PREFIX = struct.Struct(">HI32s")


class BootObjectError(RuntimeError):
    """The object store cannot be built or validated safely."""


@dataclass(frozen=True)
class GitEntry:
    path: str
    path_bytes: bytes
    mode: int
    git_sha1: bytes


@dataclass(frozen=True)
class ObjectRecord:
    sha256: bytes
    git_sha1: bytes
    length: int


@dataclass(frozen=True)
class BindingRecord:
    path: str
    path_bytes: bytes
    mode: int
    sha256: bytes


@dataclass(frozen=True)
class ParsedIndex:
    commit_sha1: bytes
    tree_sha1: bytes
    objects: tuple[ObjectRecord, ...]
    bindings: tuple[BindingRecord, ...]
    logical_bytes: int
    stored_bytes: int
    domain_digest: bytes

    def summary(self) -> dict[str, object]:
        return {
            "schema": RESULT_SCHEMA,
            "format_version": INDEX_VERSION,
            "commit": self.commit_sha1.hex(),
            "tree": self.tree_sha1.hex(),
            "index_root_sha256": self.domain_digest.hex(),
            "root_sha256": self.domain_digest.hex(),
            "object_count": len(self.objects),
            "base_commit_sha1": self.commit_sha1.hex(),
            "git_tree_sha1": self.tree_sha1.hex(),
            "unique_object_count": len(self.objects),
            "binding_count": len(self.bindings),
            "logical_bytes": self.logical_bytes,
            "stored_bytes": self.stored_bytes,
            "index_domain_digest_sha256": self.domain_digest.hex(),
        }


@dataclass(frozen=True)
class SourceModel:
    commit_sha1: bytes
    tree_sha1: bytes
    objects: tuple[ObjectRecord, ...]
    bindings: tuple[BindingRecord, ...]
    object_data: dict[bytes, bytes]
    logical_bytes: int
    stored_bytes: int


def _canonical_json_bytes(payload: object) -> bytes:
    return (
        json.dumps(
            payload,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    ).encode("utf-8")


def _sha256_bytes(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def _git_object_sha1(kind: bytes, data: bytes) -> bytes:
    return hashlib.sha1(kind + b" " + str(len(data)).encode("ascii") + b"\0" + data).digest()


def _git(repo: Path, *arguments: str) -> bytes:
    result = subprocess.run(
        ["git", "-C", str(repo), *arguments],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise BootObjectError(
            f"git {' '.join(arguments)} failed with status {result.returncode}"
            + (f": {detail[-4096:]}" if detail else "")
        )
    return result.stdout


def _repository_root(repo: Path) -> Path:
    try:
        requested = repo.resolve(strict=True)
    except OSError as exc:
        raise BootObjectError(f"repository does not exist: {repo}: {exc}") from exc
    top_raw = _git(requested, "rev-parse", "--show-toplevel").strip()
    try:
        top = Path(os.fsdecode(top_raw)).resolve(strict=True)
    except OSError as exc:
        raise BootObjectError(f"Git returned an unusable repository root: {top_raw!r}") from exc
    if top != requested:
        raise BootObjectError(f"repository root mismatch: expected {requested}, got {top}")
    object_format = _git(requested, "rev-parse", "--show-object-format").strip()
    if object_format != b"sha1":
        raise BootObjectError(
            "Boot Object Store v1 requires a SHA-1-format Git repository; "
            f"Git reported {object_format.decode('ascii', 'replace')!r}"
        )
    return requested


def _resolve_oid(repo: Path, revision: str, kind: str) -> bytes:
    if kind not in {"commit", "tree"}:
        raise AssertionError(f"unsupported Git object kind: {kind}")
    raw = _git(repo, "rev-parse", "--verify", f"{revision}^{{{kind}}}").strip()
    if len(raw) != 40:
        raise BootObjectError(f"Git returned a non-SHA-1 {kind} identity for {revision!r}")
    try:
        oid = bytes.fromhex(raw.decode("ascii"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise BootObjectError(f"Git returned an invalid {kind} identity for {revision!r}") from exc
    if oid == bytes(20):
        raise BootObjectError(f"Git returned the zero {kind} identity for {revision!r}")
    return oid


def _validate_path(path_bytes: bytes) -> str:
    if not path_bytes or len(path_bytes) > MAX_PATH_BYTES:
        raise BootObjectError(
            f"Git path length must be between 1 and {MAX_PATH_BYTES} bytes"
        )
    if path_bytes.startswith(b"/") or b"\0" in path_bytes or b"\\" in path_bytes:
        raise BootObjectError(f"unsafe Git path bytes: {path_bytes!r}")
    components = path_bytes.split(b"/")
    if len(components) > MAX_PATH_COMPONENTS:
        raise BootObjectError(
            f"Git path exceeds {MAX_PATH_COMPONENTS} components: {path_bytes!r}"
        )
    for component in components:
        if component in {b"", b".", b".."}:
            raise BootObjectError(f"unsafe Git path component in {path_bytes!r}")
        if len(component) > MAX_COMPONENT_BYTES:
            raise BootObjectError(
                f"Git path component exceeds {MAX_COMPONENT_BYTES} bytes: {path_bytes!r}"
            )
    try:
        path = path_bytes.decode("utf-8", "strict")
    except UnicodeDecodeError as exc:
        raise BootObjectError(f"Git path is not valid UTF-8: {path_bytes!r}") from exc
    if path.encode("utf-8") != path_bytes:
        raise BootObjectError(f"Git path does not round-trip as UTF-8: {path_bytes!r}")
    return path


def _git_entries(repo: Path, tree_sha1: bytes) -> tuple[GitEntry, ...]:
    output = _git(repo, "ls-tree", "-r", "-z", "--full-tree", tree_sha1.hex())
    entries: list[GitEntry] = []
    seen: set[bytes] = set()
    for raw_record in output.split(b"\0"):
        if not raw_record:
            continue
        try:
            metadata, path_bytes = raw_record.split(b"\t", 1)
            mode_raw, kind, oid_raw = metadata.split(b" ", 2)
            mode = int(mode_raw, 8)
            oid = bytes.fromhex(oid_raw.decode("ascii"))
        except (ValueError, UnicodeDecodeError) as exc:
            raise BootObjectError(f"unparseable git ls-tree record: {raw_record!r}") from exc
        path = _validate_path(path_bytes)
        if path_bytes in seen:
            raise BootObjectError(f"duplicate Git path: {path!r}")
        seen.add(path_bytes)
        if kind != b"blob" or mode not in ALLOWED_MODES:
            shown_mode = mode_raw.decode("ascii", "replace")
            shown_kind = kind.decode("ascii", "replace")
            raise BootObjectError(
                f"unsupported Git entry {path!r}: mode={shown_mode} type={shown_kind}; "
                "v1 accepts only regular blobs with modes 100644 and 100755"
            )
        if len(oid) != 20 or oid == bytes(20):
            raise BootObjectError(f"invalid Git blob identity for {path!r}")
        entries.append(GitEntry(path, path_bytes, mode, oid))
    entries.sort(key=lambda entry: entry.path_bytes)
    if len(entries) > MAX_BINDINGS:
        raise BootObjectError(
            f"Git tree has {len(entries)} bindings; v1 permits at most {MAX_BINDINGS}"
        )
    return tuple(entries)


def _expected_directories(entries: Iterable[GitEntry]) -> set[str]:
    directories: set[str] = set()
    for entry in entries:
        components = entry.path.split("/")[:-1]
        for length in range(1, len(components) + 1):
            directories.add("/".join(components[:length]))
    return directories


def _validate_exact_source_root(source_root: Path, entries: tuple[GitEntry, ...]) -> Path:
    try:
        root_lstat = source_root.lstat()
    except OSError as exc:
        raise BootObjectError(f"source root does not exist: {source_root}: {exc}") from exc
    if stat.S_ISLNK(root_lstat.st_mode):
        raise BootObjectError(f"source root must not be a symlink: {source_root}")
    if not stat.S_ISDIR(root_lstat.st_mode):
        raise BootObjectError(f"source root is not a directory: {source_root}")
    root = source_root.resolve(strict=True)

    actual_files: set[str] = set()
    actual_directories: set[str] = set()
    for current_raw, directory_names, file_names in os.walk(root, topdown=True, followlinks=False):
        directory_names.sort()
        file_names.sort()
        current = Path(current_raw)
        for name in tuple(directory_names):
            candidate = current / name
            relative = candidate.relative_to(root).as_posix()
            metadata = candidate.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                raise BootObjectError(f"source contains a non-directory or symlink: {relative!r}")
            try:
                _validate_path(relative.encode("utf-8"))
            except UnicodeEncodeError as exc:
                raise BootObjectError(f"source path is not valid UTF-8: {relative!r}") from exc
            actual_directories.add(relative)
        for name in file_names:
            candidate = current / name
            relative = candidate.relative_to(root).as_posix()
            metadata = candidate.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
                raise BootObjectError(f"source contains a non-regular file or symlink: {relative!r}")
            try:
                _validate_path(relative.encode("utf-8"))
            except UnicodeEncodeError as exc:
                raise BootObjectError(f"source path is not valid UTF-8: {relative!r}") from exc
            actual_files.add(relative)

    expected_files = {entry.path for entry in entries}
    expected_directories = _expected_directories(entries)
    missing_files = sorted(expected_files - actual_files)
    extra_files = sorted(actual_files - expected_files)
    missing_directories = sorted(expected_directories - actual_directories)
    extra_directories = sorted(actual_directories - expected_directories)
    if missing_files or extra_files or missing_directories or extra_directories:
        details: list[str] = []
        for label, paths in (
            ("missing files", missing_files),
            ("extra files", extra_files),
            ("missing directories", missing_directories),
            ("extra directories", extra_directories),
        ):
            if paths:
                details.append(f"{label}: {', '.join(repr(path) for path in paths[:20])}")
        raise BootObjectError("source root is not the exact Git tree; " + "; ".join(details))
    return root


def _read_source_file(path: Path, expected_mode: int) -> bytes:
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise BootObjectError(f"cannot open source blob safely: {path}: {exc}") from exc
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise BootObjectError(f"source blob is not a regular file: {path}")
        executable = bool(before.st_mode & 0o111)
        if executable != (expected_mode == 0o100755):
            raise BootObjectError(
                f"source executable bit disagrees with Git mode {expected_mode:o}: {path}"
            )
        if before.st_size > MAX_OBJECT_BYTES:
            raise BootObjectError(
                f"source blob exceeds {MAX_OBJECT_BYTES} bytes: {path} ({before.st_size})"
            )
        chunks: list[bytes] = []
        remaining = before.st_size
        while remaining:
            chunk = os.read(descriptor, min(1024 * 1024, remaining))
            if not chunk:
                raise BootObjectError(f"source blob shortened while reading: {path}")
            chunks.append(chunk)
            remaining -= len(chunk)
        if os.read(descriptor, 1):
            raise BootObjectError(f"source blob grew while reading: {path}")
        after = os.fstat(descriptor)
        stable_fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
        if any(getattr(before, field) != getattr(after, field) for field in stable_fields):
            raise BootObjectError(f"source blob changed while reading: {path}")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def _recompute_tree_sha1(bindings: Iterable[tuple[GitEntry, bytes]]) -> bytes:
    root: dict[bytes, object] = {}
    for entry, blob_sha1 in bindings:
        node = root
        components = entry.path_bytes.split(b"/")
        for component in components[:-1]:
            existing = node.get(component)
            if existing is None:
                child: dict[bytes, object] = {}
                node[component] = child
                node = child
            elif isinstance(existing, dict):
                node = existing
            else:
                raise BootObjectError(f"Git path has a file/directory collision: {entry.path!r}")
        leaf = components[-1]
        if leaf in node:
            raise BootObjectError(f"Git path collides with another entry: {entry.path!r}")
        node[leaf] = (entry.mode, blob_sha1)

    def hash_node(node: dict[bytes, object]) -> bytes:
        encoded_entries: list[tuple[bytes, bytes]] = []
        for name, value in node.items():
            if isinstance(value, dict):
                oid = hash_node(value)
                encoded = b"40000 " + name + b"\0" + oid
                sort_key = name + b"/"
            else:
                mode, oid = value
                encoded = f"{mode:o}".encode("ascii") + b" " + name + b"\0" + oid
                sort_key = name
            encoded_entries.append((sort_key, encoded))
        body = b"".join(encoded for _, encoded in sorted(encoded_entries, key=lambda item: item[0]))
        return _git_object_sha1(b"tree", body)

    return hash_node(root)


def load_source_model(
    repo: Path,
    commit: str,
    tree: str | None,
    source_root: Path,
) -> SourceModel:
    """Read an exact Git archive tree and derive its canonical object model."""

    repository = _repository_root(repo)
    commit_sha1 = _resolve_oid(repository, commit, "commit")
    tree_sha1 = _resolve_oid(repository, tree or commit, "tree")
    entries = _git_entries(repository, tree_sha1)
    root = _validate_exact_source_root(source_root, entries)

    logical_bytes = 0
    object_data: dict[bytes, bytes] = {}
    objects_by_sha256: dict[bytes, ObjectRecord] = {}
    bindings: list[BindingRecord] = []
    tree_bindings: list[tuple[GitEntry, bytes]] = []
    for entry in entries:
        data = _read_source_file(root / entry.path, entry.mode)
        if len(data) > MAX_OBJECT_BYTES:
            raise BootObjectError(f"source blob exceeds v1 size limit: {entry.path!r}")
        git_sha1 = _git_object_sha1(b"blob", data)
        if git_sha1 != entry.git_sha1:
            raise BootObjectError(
                f"source bytes do not match Git blob for {entry.path!r}: "
                f"expected {entry.git_sha1.hex()}, got {git_sha1.hex()}"
            )
        sha256 = _sha256_bytes(data)
        if sha256 == bytes(32):
            raise BootObjectError(f"zero SHA-256 identity is forbidden: {entry.path!r}")
        logical_bytes += len(data)
        if logical_bytes > MAX_LOGICAL_BYTES:
            raise BootObjectError(
                f"logical payload exceeds {MAX_LOGICAL_BYTES} bytes"
            )
        existing = objects_by_sha256.get(sha256)
        if existing is None:
            objects_by_sha256[sha256] = ObjectRecord(sha256, git_sha1, len(data))
            object_data[sha256] = data
        elif existing.git_sha1 != git_sha1 or existing.length != len(data) or object_data[sha256] != data:
            raise BootObjectError(f"SHA-256 collision while processing {entry.path!r}")
        bindings.append(BindingRecord(entry.path, entry.path_bytes, entry.mode, sha256))
        tree_bindings.append((entry, git_sha1))

    objects = tuple(sorted(objects_by_sha256.values(), key=lambda item: item.sha256))
    if len(objects) > MAX_OBJECTS:
        raise BootObjectError(
            f"tree has {len(objects)} unique objects; v1 permits at most {MAX_OBJECTS}"
        )
    stored_bytes = sum(item.length for item in objects)
    if stored_bytes > MAX_STORED_BYTES:
        raise BootObjectError(f"stored payload exceeds {MAX_STORED_BYTES} bytes")
    bindings.sort(key=lambda item: item.path_bytes)
    recomputed_tree = _recompute_tree_sha1(tree_bindings)
    if recomputed_tree != tree_sha1:
        raise BootObjectError(
            "source paths, modes, and blobs do not reconstruct the selected Git tree: "
            f"expected {tree_sha1.hex()}, got {recomputed_tree.hex()}"
        )
    return SourceModel(
        commit_sha1=commit_sha1,
        tree_sha1=tree_sha1,
        objects=objects,
        bindings=tuple(bindings),
        object_data=object_data,
        logical_bytes=logical_bytes,
        stored_bytes=stored_bytes,
    )


def encode_index(model: SourceModel) -> bytes:
    object_table = b"".join(
        _OBJECT.pack(record.sha256, record.git_sha1, record.length)
        for record in model.objects
    )
    binding_table = b"".join(
        _BINDING_PREFIX.pack(len(record.path_bytes), record.mode, record.sha256)
        + record.path_bytes
        for record in model.bindings
    )
    total_length = INDEX_HEADER_LENGTH + len(object_table) + len(binding_table) + 32
    if total_length > INDEX_MAX_BYTES:
        raise BootObjectError(
            f"index would be {total_length} bytes; v1 permits at most {INDEX_MAX_BYTES}"
        )
    header = _HEADER.pack(
        INDEX_MAGIC,
        INDEX_VERSION,
        INDEX_HEADER_LENGTH,
        total_length,
        model.commit_sha1,
        model.tree_sha1,
        len(model.objects),
        len(model.bindings),
        model.logical_bytes,
        model.stored_bytes,
    )
    prior = header + object_table + binding_table
    return prior + _sha256_bytes(INDEX_DIGEST_DOMAIN + prior)


def parse_index(data: bytes) -> ParsedIndex:
    if len(data) < INDEX_HEADER_LENGTH + 32:
        raise BootObjectError("index is shorter than the v1 header and digest")
    if len(data) > INDEX_MAX_BYTES:
        raise BootObjectError(f"index exceeds {INDEX_MAX_BYTES} bytes")
    try:
        (
            magic,
            version,
            header_length,
            total_length,
            commit_sha1,
            tree_sha1,
            object_count,
            binding_count,
            logical_bytes,
            stored_bytes,
        ) = _HEADER.unpack_from(data, 0)
    except struct.error as exc:
        raise BootObjectError("index header is truncated") from exc
    if magic != INDEX_MAGIC:
        raise BootObjectError(f"invalid index magic: {magic!r}")
    if version != INDEX_VERSION or header_length != INDEX_HEADER_LENGTH:
        raise BootObjectError(
            f"unsupported index version/header: version={version} header={header_length}"
        )
    if total_length != len(data):
        raise BootObjectError(
            f"index total length mismatch: header={total_length} actual={len(data)}"
        )
    if commit_sha1 == bytes(20) or tree_sha1 == bytes(20):
        raise BootObjectError("index contains a zero Git source identity")
    if object_count > MAX_OBJECTS or binding_count > MAX_BINDINGS:
        raise BootObjectError("index object or binding count exceeds v1 limits")
    if logical_bytes > MAX_LOGICAL_BYTES or stored_bytes > MAX_STORED_BYTES:
        raise BootObjectError("index logical or stored byte count exceeds v1 limits")
    expected_digest = _sha256_bytes(INDEX_DIGEST_DOMAIN + data[:-32])
    if data[-32:] != expected_digest:
        raise BootObjectError("index domain-separated SHA-256 digest does not match")

    cursor = header_length
    objects: list[ObjectRecord] = []
    object_by_sha256: dict[bytes, ObjectRecord] = {}
    git_sha1s: set[bytes] = set()
    previous_sha256: bytes | None = None
    for _ in range(object_count):
        if cursor + _OBJECT.size > len(data) - 32:
            raise BootObjectError("object table is truncated")
        sha256, git_sha1, length = _OBJECT.unpack_from(data, cursor)
        cursor += _OBJECT.size
        if sha256 == bytes(32) or git_sha1 == bytes(20):
            raise BootObjectError("object table contains a zero digest")
        if previous_sha256 is not None and sha256 <= previous_sha256:
            raise BootObjectError("object table is not strictly sorted by unique SHA-256")
        if git_sha1 in git_sha1s:
            raise BootObjectError("object table contains a duplicate Git blob SHA-1")
        if length > MAX_OBJECT_BYTES:
            raise BootObjectError("object table contains an object larger than the v1 limit")
        record = ObjectRecord(sha256, git_sha1, length)
        objects.append(record)
        object_by_sha256[sha256] = record
        git_sha1s.add(git_sha1)
        previous_sha256 = sha256
    if sum(record.length for record in objects) != stored_bytes:
        raise BootObjectError("object table lengths do not equal stored_bytes")

    bindings: list[BindingRecord] = []
    referenced: set[bytes] = set()
    previous_path: bytes | None = None
    computed_logical_bytes = 0
    for _ in range(binding_count):
        if cursor + _BINDING_PREFIX.size > len(data) - 32:
            raise BootObjectError("binding table is truncated")
        path_length, mode, sha256 = _BINDING_PREFIX.unpack_from(data, cursor)
        cursor += _BINDING_PREFIX.size
        if path_length == 0 or path_length > MAX_PATH_BYTES:
            raise BootObjectError(f"invalid binding path length: {path_length}")
        if cursor + path_length > len(data) - 32:
            raise BootObjectError("binding path is truncated")
        path_bytes = data[cursor : cursor + path_length]
        cursor += path_length
        path = _validate_path(path_bytes)
        if previous_path is not None and path_bytes <= previous_path:
            raise BootObjectError("binding table is not strictly sorted by unique UTF-8 path")
        if mode not in ALLOWED_MODES:
            raise BootObjectError(f"binding {path!r} has unsupported Git mode {mode:o}")
        object_record = object_by_sha256.get(sha256)
        if object_record is None:
            raise BootObjectError(f"binding {path!r} references an absent object")
        computed_logical_bytes += object_record.length
        if computed_logical_bytes > MAX_LOGICAL_BYTES:
            raise BootObjectError("binding table logical bytes exceed the v1 limit")
        referenced.add(sha256)
        bindings.append(BindingRecord(path, path_bytes, mode, sha256))
        previous_path = path_bytes
    if cursor != len(data) - 32:
        raise BootObjectError("index contains trailing or unparsed bytes before its digest")
    if computed_logical_bytes != logical_bytes:
        raise BootObjectError("binding table lengths do not equal logical_bytes")
    if referenced != set(object_by_sha256):
        raise BootObjectError("index contains one or more unreferenced objects")
    return ParsedIndex(
        commit_sha1=commit_sha1,
        tree_sha1=tree_sha1,
        objects=tuple(objects),
        bindings=tuple(bindings),
        logical_bytes=logical_bytes,
        stored_bytes=stored_bytes,
        domain_digest=data[-32:],
    )


def _set_reproducible_mtime(path: Path) -> None:
    os.utime(path, (SOURCE_DATE_EPOCH, SOURCE_DATE_EPOCH), follow_symlinks=False)


def _resolved_new_output(output: Path) -> Path:
    if output.exists() or output.is_symlink():
        raise BootObjectError(f"output already exists; refusing to replace it: {output}")
    try:
        parent = output.parent.resolve(strict=True)
    except OSError as exc:
        raise BootObjectError(f"output parent does not exist: {output.parent}: {exc}") from exc
    if not output.name or output.name in {".", ".."}:
        raise BootObjectError(f"invalid output directory: {output}")
    return parent / output.name


def build_store(
    repo: Path,
    commit: str,
    tree: str | None,
    source_root: Path,
    output: Path,
) -> dict[str, object]:
    model = load_source_model(repo, commit, tree, source_root)
    target = _resolved_new_output(output)
    source = source_root.resolve(strict=True)
    try:
        target.relative_to(source)
    except ValueError:
        pass
    else:
        raise BootObjectError("output must not be inside the exact source tree")

    index_bytes = encode_index(model)
    parsed = parse_index(index_bytes)
    temporary = Path(tempfile.mkdtemp(prefix=f".{target.name}.tmp.", dir=target.parent))
    try:
        cas = temporary / "objects" / "sha256"
        cas.mkdir(parents=True, mode=0o755)
        index_path = temporary / "index.bin"
        index_path.write_bytes(index_bytes)
        index_path.chmod(0o444)
        _set_reproducible_mtime(index_path)
        for record in model.objects:
            object_path = cas / record.sha256.hex()
            object_path.write_bytes(model.object_data[record.sha256])
            object_path.chmod(0o444)
            _set_reproducible_mtime(object_path)
        for directory in (cas, cas.parent, temporary):
            _set_reproducible_mtime(directory)
        temporary.rename(target)
    except Exception:
        shutil.rmtree(temporary, ignore_errors=True)
        raise

    result = parsed.summary()
    result.update(
        {
            "ok": True,
            "operation": "build",
            "store": str(target),
            "index_bytes": len(index_bytes),
            "index_sha256": _sha256_bytes(index_bytes).hex(),
        }
    )
    return result


def _require_directory(path: Path, label: str) -> None:
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise BootObjectError(f"missing {label}: {path}: {exc}") from exc
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise BootObjectError(f"{label} is not a real directory: {path}")


def _require_regular(path: Path, label: str) -> None:
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise BootObjectError(f"missing {label}: {path}: {exc}") from exc
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise BootObjectError(f"{label} is not a regular file: {path}")


def verify_store(store: Path) -> tuple[ParsedIndex, bytes]:
    _require_directory(store, "store")
    root_names = sorted(path.name for path in store.iterdir())
    expected_root_names = ["index.bin", "objects"]
    if root_names != expected_root_names:
        raise BootObjectError(
            f"store root shape mismatch: expected {expected_root_names}, got {root_names}"
        )
    index_path = store / "index.bin"
    objects_root = store / "objects"
    cas = objects_root / "sha256"
    _require_regular(index_path, "index")
    _require_directory(objects_root, "objects directory")
    object_children = sorted(path.name for path in objects_root.iterdir())
    if object_children != ["sha256"]:
        raise BootObjectError(
            f"objects directory shape mismatch: expected ['sha256'], got {object_children}"
        )
    _require_directory(cas, "SHA-256 CAS")

    index_bytes = index_path.read_bytes()
    parsed = parse_index(index_bytes)
    expected_names = [record.sha256.hex() for record in parsed.objects]
    actual_names = sorted(path.name for path in cas.iterdir())
    if actual_names != sorted(expected_names):
        raise BootObjectError("CAS filenames do not exactly match the canonical object table")
    for record in parsed.objects:
        object_path = cas / record.sha256.hex()
        _require_regular(object_path, f"CAS object {record.sha256.hex()}")
        data = object_path.read_bytes()
        if len(data) != record.length:
            raise BootObjectError(
                f"CAS object length mismatch for {record.sha256.hex()}: "
                f"expected {record.length}, got {len(data)}"
            )
        if _sha256_bytes(data) != record.sha256:
            raise BootObjectError(f"CAS SHA-256 mismatch for {record.sha256.hex()}")
        if _git_object_sha1(b"blob", data) != record.git_sha1:
            raise BootObjectError(f"CAS Git blob SHA-1 mismatch for {record.sha256.hex()}")

    return parsed, index_bytes


def _compare_source_to_index(model: SourceModel, parsed: ParsedIndex) -> None:
    if model.commit_sha1 != parsed.commit_sha1 or model.tree_sha1 != parsed.tree_sha1:
        raise BootObjectError("store source identities do not match the requested Git source")
    if model.objects != parsed.objects or model.bindings != parsed.bindings:
        raise BootObjectError("store object or binding table does not match the exact source tree")
    if model.logical_bytes != parsed.logical_bytes or model.stored_bytes != parsed.stored_bytes:
        raise BootObjectError("store byte totals do not match the exact source tree")


def _compare_source_root_to_index(source_root: Path, parsed: ParsedIndex) -> None:
    """Verify an extracted tree against a parsed index without a Git database."""

    objects = {record.sha256: record for record in parsed.objects}
    entries = tuple(
        GitEntry(
            binding.path,
            binding.path_bytes,
            binding.mode,
            objects[binding.sha256].git_sha1,
        )
        for binding in parsed.bindings
    )
    root = _validate_exact_source_root(source_root, entries)
    logical_bytes = 0
    seen_objects: set[bytes] = set()
    tree_bindings: list[tuple[GitEntry, bytes]] = []
    for entry, binding in zip(entries, parsed.bindings, strict=True):
        data = _read_source_file(root / entry.path, entry.mode)
        object_record = objects[binding.sha256]
        if len(data) != object_record.length:
            raise BootObjectError(
                f"source length differs from index for {entry.path!r}: "
                f"expected {object_record.length}, got {len(data)}"
            )
        if _sha256_bytes(data) != binding.sha256:
            raise BootObjectError(f"source SHA-256 differs from index for {entry.path!r}")
        git_sha1 = _git_object_sha1(b"blob", data)
        if git_sha1 != object_record.git_sha1:
            raise BootObjectError(f"source Git blob SHA-1 differs from index for {entry.path!r}")
        logical_bytes += len(data)
        seen_objects.add(binding.sha256)
        tree_bindings.append((entry, git_sha1))
    if logical_bytes != parsed.logical_bytes:
        raise BootObjectError("source logical byte count differs from index")
    if seen_objects != set(objects):
        raise BootObjectError("source tree does not reference the complete indexed object set")
    if _recompute_tree_sha1(tree_bindings) != parsed.tree_sha1:
        raise BootObjectError("source paths, modes, and blobs do not reconstruct the indexed tree")


def _literal_oid(value: str, label: str) -> bytes:
    if len(value) != 40:
        raise BootObjectError(f"{label} must be an exact 40-character SHA-1 identity")
    try:
        result = bytes.fromhex(value)
    except ValueError as exc:
        raise BootObjectError(f"{label} is not hexadecimal") from exc
    if result == bytes(20):
        raise BootObjectError(f"{label} must not be the zero identity")
    return result


def _inspect_result(
    parsed: ParsedIndex,
    index_bytes: bytes,
    store: Path,
    *,
    full: bool,
) -> dict[str, object]:
    result = parsed.summary()
    result.update(
        {
            "ok": True,
            "operation": "inspect",
            "store": str(store.resolve()),
            "index_bytes": len(index_bytes),
            "index_sha256": _sha256_bytes(index_bytes).hex(),
        }
    )
    if full:
        result["objects"] = [
            {
                "sha256": record.sha256.hex(),
                "git_blob_sha1": record.git_sha1.hex(),
                "bytes": record.length,
            }
            for record in parsed.objects
        ]
        result["bindings"] = [
            {
                "path": record.path,
                "mode": f"{record.mode:o}",
                "mode_numeric": record.mode,
                "sha256": record.sha256.hex(),
            }
            for record in parsed.bindings
        ]
    return result


def _print_result(result: dict[str, object], as_json: bool) -> None:
    if as_json:
        sys.stdout.buffer.write(_canonical_json_bytes(result))
        return
    print(f"{result['operation']}: PASS")
    print(f"store: {result['store']}")
    print(f"base commit: {result['commit']}")
    print(f"Git tree: {result['tree']}")
    print(
        "objects/bindings: "
        f"{result['unique_object_count']}/{result['binding_count']}"
    )
    print(f"logical/stored bytes: {result['logical_bytes']}/{result['stored_bytes']}")
    print(f"index SHA-256: {result['index_sha256']}")
    print(f"index root SHA-256: {result['index_root_sha256']}")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    build = commands.add_parser("build", help="build a store from one exact Git archive tree")
    build.add_argument("--repo", type=Path, required=True)
    build.add_argument("--commit", required=True, help="base commit recorded in the index")
    build.add_argument(
        "--tree",
        help="exact tree-ish to package; defaults to the selected commit tree",
    )
    build.add_argument("--source-root", type=Path, required=True)
    build.add_argument("--output", type=Path, required=True)
    build.add_argument("--json", action="store_true", help="emit canonical machine JSON")

    verify = commands.add_parser("verify", help="verify the index and every CAS blob")
    verify.add_argument("--store", type=Path, required=True)
    verify.add_argument("--repo", type=Path)
    verify.add_argument(
        "--commit",
        help="expected base commit (exact SHA-1 without --repo, or a revision with it)",
    )
    verify.add_argument(
        "--tree",
        help="expected exact tree (exact SHA-1 without --repo, or a tree-ish with it)",
    )
    verify.add_argument(
        "--source-root",
        type=Path,
        help="also compare against the complete extracted Git archive tree",
    )
    verify.add_argument("--json", action="store_true", help="emit canonical machine JSON")

    inspect = commands.add_parser("inspect", help="inspect a fully verified store")
    inspect.add_argument("--store", type=Path, required=True)
    inspect.add_argument("--full", action="store_true", help="include all objects and bindings")
    inspect.add_argument("--json", action="store_true", help="emit canonical machine JSON")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "build":
            result = build_store(args.repo, args.commit, args.tree, args.source_root, args.output)
        elif args.command == "verify":
            parsed, index_bytes = verify_store(args.store)
            if args.repo is not None:
                repository = _repository_root(args.repo)
                commit_ref = args.commit or parsed.commit_sha1.hex()
                tree_ref = args.tree or parsed.tree_sha1.hex()
                expected_commit = _resolve_oid(repository, commit_ref, "commit")
                expected_tree = _resolve_oid(repository, tree_ref, "tree")
                if expected_commit != parsed.commit_sha1 or expected_tree != parsed.tree_sha1:
                    raise BootObjectError("store Git identities do not match requested constraints")
                if args.source_root is not None:
                    model = load_source_model(repository, commit_ref, tree_ref, args.source_root)
                    _compare_source_to_index(model, parsed)
            else:
                if args.commit is not None and _literal_oid(args.commit, "commit") != parsed.commit_sha1:
                    raise BootObjectError("store base commit does not match requested constraint")
                if args.tree is not None and _literal_oid(args.tree, "tree") != parsed.tree_sha1:
                    raise BootObjectError("store Git tree does not match requested constraint")
                if args.source_root is not None:
                    _compare_source_root_to_index(args.source_root, parsed)
            result = _inspect_result(parsed, index_bytes, args.store, full=False)
            result["operation"] = "verify"
        elif args.command == "inspect":
            parsed, index_bytes = verify_store(args.store)
            result = _inspect_result(parsed, index_bytes, args.store, full=args.full)
        else:  # pragma: no cover - argparse owns this invariant.
            raise AssertionError(args.command)
        _print_result(result, args.json)
        return 0
    except (BootObjectError, OSError) as exc:
        if getattr(args, "json", False):
            sys.stdout.buffer.write(
                _canonical_json_bytes(
                    {
                        "schema": RESULT_SCHEMA,
                        "ok": False,
                        "operation": args.command,
                        "error": str(exc),
                    }
                )
            )
        else:
            print(f"ostadix boot objects: FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
