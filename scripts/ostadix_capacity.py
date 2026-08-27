#!/usr/bin/env python3
"""Bounded host-side package manager for absorbed Ostadix capacity.

The manager stores exact local-file or HTTPS artifacts as immutable blobs,
stores domain-separated package records, and commits exact dependency closures
through revisioned current/previous generations.  Installed, active, and
qualified are deliberately separate states.  This module does not create
qualification records and does not claim that an installed artifact boots.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import errno
import fcntl
import hashlib
import heapq
import json
import os
from pathlib import Path
import re
import stat
import sys
import tempfile
import tomllib
from typing import Any, BinaryIO, Iterator, Mapping, Sequence
import urllib.parse
import urllib.request


CATALOG_SCHEMA = "ostadix.absorbed-capacity-catalog/v1"
PACKAGE_SCHEMA = "ostadix.absorbed-capacity-package/v1"
PLAN_SCHEMA = "ostadix.absorbed-capacity-plan/v1"
GENERATION_SCHEMA = "ostadix.absorbed-capacity-generation/v1"
HEAD_SCHEMA = "ostadix.absorbed-capacity-head/v1"
ALIASES_SCHEMA = "ostadix.absorbed-capacity-aliases/v1"

PACKAGE_DOMAIN = "ostadix.absorbed-capacity-package/v1"
PLAN_DOMAIN = "ostadix.absorbed-capacity-plan/v1"
GENERATION_DOMAIN = "ostadix.absorbed-capacity-generation/v1"

PROJECT_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CATALOG = PROJECT_ROOT / "evidence" / "absorbed_capacity_catalog.toml"
DEFAULT_STATE = Path(
    os.environ.get(
        "OSTADIX_CAPACITY_STATE",
        str(Path.home() / ".local" / "state" / "ostadix" / "absorbed-capacity"),
    )
)

STREAM_CHUNK_BYTES = 1024 * 1024
MAX_CATALOG_BYTES = 1024 * 1024
MAX_RECORD_BYTES = 2 * 1024 * 1024
MAX_PLAN_BYTES = 2 * 1024 * 1024
MAX_PACKAGES = 512
MAX_ARTIFACTS_PER_PACKAGE = 64
MAX_DEPENDENCIES_PER_PACKAGE = 64
MAX_ALIASES_PER_PACKAGE = 32
MAX_ALIASES = 4096
MAX_CLOSURE_PACKAGES = 1024
MAX_BLOB_BYTES = 64 * 1024 * 1024 * 1024
MAX_REVISION = 2**63 - 1
MAX_TOKEN_BYTES = 128
MAX_TEXT_BYTES = 4096
MAX_DESCRIPTION_BYTES = 8192

KINDS = frozenset({"os", "kernel", "userspace", "firmware", "bundle"})
ARCHITECTURES = frozenset(
    {"any", "x86_64", "aarch64", "i386", "armv7", "riscv64", "sparc64"}
)
LOADERS = frozenset(
    {
        "none",
        "linux",
        "multiboot2",
        "uefi",
        "bios",
        "plan9",
        "redox",
        "chainload",
    }
)
REDISTRIBUTION_POLICIES = frozenset(
    {"permitted", "restricted", "user-supplied"}
)
KIND_LOADERS = {
    "os": LOADERS - {"none"},
    "kernel": frozenset(
        {"linux", "multiboot2", "uefi", "bios", "plan9", "redox", "chainload"}
    ),
    "userspace": frozenset({"none"}),
    "firmware": frozenset({"none"}),
    "bundle": LOADERS,
}
DEPENDENCY_KINDS = {
    "os": frozenset({"kernel", "userspace", "firmware", "bundle"}),
    "kernel": frozenset({"firmware", "bundle"}),
    "userspace": frozenset({"firmware", "bundle"}),
    "firmware": frozenset(),
    "bundle": KINDS,
}

TOKEN_RE = re.compile(r"[a-z0-9][a-z0-9._+/-]{0,127}\Z")
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
DIGEST_REF_RE = re.compile(r"sha256:([0-9a-f]{64})\Z")
LICENSE_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9.+-]{0,127}\Z")
SAFE_FILENAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+()-]{0,254}\Z")


class CapacityError(ValueError):
    """Input or durable state is outside the absorbed-capacity contract."""


@dataclass(frozen=True)
class ArtifactSpec:
    id: str
    role: str
    filename: str
    source: str
    size_bytes: int
    sha256: str
    integrity: str

    def identity_record(self) -> dict[str, Any]:
        return {
            "filename": self.filename,
            "id": self.id,
            "role": self.role,
            "sha256": self.sha256,
            "size_bytes": self.size_bytes,
        }


@dataclass(frozen=True)
class DependencySpec:
    package: str
    kind: str


@dataclass(frozen=True)
class PackageSpec:
    id: str
    name: str
    version: str
    kind: str
    architecture: str
    loader: str
    license: str
    redistribution: str
    requires_acceptance: bool
    aliases: tuple[str, ...]
    description: str
    dependencies: tuple[DependencySpec, ...]
    artifacts: tuple[ArtifactSpec, ...]


@dataclass(frozen=True)
class Catalog:
    path: Path
    name: str
    packages: tuple[PackageSpec, ...]

    @property
    def by_id(self) -> dict[str, PackageSpec]:
        return {package.id: package for package in self.packages}


def _canonical_json(value: Any) -> bytes:
    return json.dumps(
        value, ensure_ascii=True, allow_nan=False, sort_keys=True, separators=(",", ":")
    ).encode("ascii")


def _domain_digest(domain: str, value: Any) -> str:
    payload = _canonical_json(value)
    digest = hashlib.sha256()
    encoded_domain = domain.encode("ascii")
    digest.update(len(encoded_domain).to_bytes(8, "big"))
    digest.update(encoded_domain)
    digest.update(len(payload).to_bytes(8, "big"))
    digest.update(payload)
    return "sha256:" + digest.hexdigest()


def _require_mapping(value: Any, context: str) -> dict[str, Any]:
    if type(value) is not dict:
        raise CapacityError(f"{context} must be a table/object")
    return value


def _strict_keys(
    value: Mapping[str, Any], required: set[str], optional: set[str], context: str
) -> None:
    keys = set(value)
    missing = required - keys
    unknown = keys - required - optional
    if missing:
        raise CapacityError(f"{context} is missing keys: {sorted(missing)!r}")
    if unknown:
        raise CapacityError(f"{context} has unknown keys: {sorted(unknown)!r}")


def _bounded_text(
    value: Any,
    context: str,
    *,
    maximum: int = MAX_TEXT_BYTES,
    pattern: re.Pattern[str] | None = None,
) -> str:
    if not isinstance(value, str) or not value:
        raise CapacityError(f"{context} must be a non-empty string")
    try:
        encoded = value.encode("utf-8", "strict")
    except UnicodeError as error:
        raise CapacityError(f"{context} must be valid UTF-8") from error
    if len(encoded) > maximum:
        raise CapacityError(f"{context} exceeds {maximum} UTF-8 bytes")
    if "\x00" in value:
        raise CapacityError(f"{context} contains NUL")
    if pattern is not None and pattern.fullmatch(value) is None:
        raise CapacityError(f"{context} has invalid syntax: {value!r}")
    return value


def _token(value: Any, context: str) -> str:
    token = _bounded_text(
        value, context, maximum=MAX_TOKEN_BYTES, pattern=TOKEN_RE
    )
    parts = token.split("/")
    if any(part in {"", ".", ".."} for part in parts):
        raise CapacityError(f"{context} contains an invalid path-like segment")
    return token


def _digest_hex(value: Any, context: str) -> str:
    return _bounded_text(value, context, maximum=64, pattern=SHA256_RE)


def _digest_ref(value: Any, context: str) -> str:
    text = _bounded_text(value, context, maximum=71)
    if DIGEST_REF_RE.fullmatch(text) is None:
        raise CapacityError(f"{context} must be sha256:<64 lowercase hex>")
    return text


def _positive_int(value: Any, context: str, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise CapacityError(f"{context} must be a positive integer")
    if value > maximum:
        raise CapacityError(f"{context} exceeds {maximum}")
    return value


def _nonnegative_int(value: Any, context: str, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise CapacityError(f"{context} must be a non-negative integer")
    if value > maximum:
        raise CapacityError(f"{context} exceeds {maximum}")
    return value


def _exact_bool(value: Any, context: str) -> bool:
    if type(value) is not bool:
        raise CapacityError(f"{context} must be a boolean")
    return value


def _bounded_list(value: Any, context: str, maximum: int) -> list[Any]:
    if not isinstance(value, list):
        raise CapacityError(f"{context} must be an array")
    if len(value) > maximum:
        raise CapacityError(f"{context} exceeds {maximum} entries")
    return value


def _file_identity(state: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        state.st_dev,
        state.st_ino,
        state.st_size,
        state.st_mtime_ns,
        state.st_ctime_ns,
    )


def _open_regular(path: Path, *, require_immutable: bool = False) -> int:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
    if not hasattr(os, "O_NOFOLLOW"):
        raise CapacityError("this host cannot pin inputs with O_NOFOLLOW")
    flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise CapacityError(f"cannot open regular file without following links: {path}: {error}") from error
    try:
        state = os.fstat(descriptor)
        if not stat.S_ISREG(state.st_mode):
            raise CapacityError(f"path is not a regular file: {path}")
        if require_immutable and state.st_mode & 0o222:
            raise CapacityError(f"immutable object has write permission bits: {path}")
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor


def _read_regular_bounded(
    path: Path, maximum: int, *, require_immutable: bool = False
) -> bytes:
    descriptor = _open_regular(path, require_immutable=require_immutable)
    try:
        before = os.fstat(descriptor)
        if before.st_size > maximum:
            raise CapacityError(f"file exceeds {maximum} bytes: {path}")
        chunks: list[bytes] = []
        total = 0
        while True:
            chunk = os.read(descriptor, min(STREAM_CHUNK_BYTES, maximum + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > maximum:
                raise CapacityError(f"file exceeds {maximum} bytes: {path}")
        after = os.fstat(descriptor)
        if _file_identity(before) != _file_identity(after):
            raise CapacityError(f"file changed while pinned: {path}")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def _require_directory(path: Path) -> None:
    try:
        state = path.lstat()
    except FileNotFoundError as error:
        raise CapacityError(f"required state directory is missing: {path}") from error
    if not stat.S_ISDIR(state.st_mode) or stat.S_ISLNK(state.st_mode):
        raise CapacityError(f"state path is not a non-symlink directory: {path}")


def _ensure_directory(path: Path) -> None:
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    _require_directory(path)


def _fsync_directory(path: Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_DIRECTORY", 0)
    descriptor = os.open(path, flags)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _write_all(descriptor: int, payload: bytes) -> None:
    view = memoryview(payload)
    offset = 0
    while offset < len(view):
        written = os.write(descriptor, view[offset:])
        if written <= 0:
            raise CapacityError("short write while staging immutable object")
        offset += written


def _stage_bytes(parent: Path, payload: bytes, mode: int) -> Path:
    descriptor, raw_path = tempfile.mkstemp(prefix=".capacity-stage-", dir=parent)
    path = Path(raw_path)
    try:
        os.fchmod(descriptor, 0o600)
        _write_all(descriptor, payload)
        os.fsync(descriptor)
        os.fchmod(descriptor, mode)
        os.fsync(descriptor)
    except BaseException:
        os.close(descriptor)
        path.unlink(missing_ok=True)
        raise
    os.close(descriptor)
    return path


def _publish_temp_no_clobber(staged: Path, target: Path) -> bool:
    try:
        os.link(staged, target, follow_symlinks=False)
    except FileExistsError:
        return False
    except OSError as error:
        if error.errno == errno.EEXIST:
            return False
        raise CapacityError(
            f"cannot atomically publish immutable object {target}: {error}"
        ) from error
    _fsync_directory(target.parent)
    return True


def _publish_immutable_bytes(target: Path, payload: bytes) -> bool:
    _ensure_directory(target.parent)
    if target.exists() or target.is_symlink():
        observed = _read_regular_bounded(
            target, max(len(payload), 1), require_immutable=True
        )
        if observed != payload:
            raise CapacityError(f"immutable object collision at {target}")
        return False
    staged = _stage_bytes(target.parent, payload, 0o444)
    try:
        published = _publish_temp_no_clobber(staged, target)
        if not published:
            observed = _read_regular_bounded(
                target, max(len(payload), 1), require_immutable=True
            )
            if observed != payload:
                raise CapacityError(f"immutable object collision at {target}")
        return published
    finally:
        staged.unlink(missing_ok=True)


def _atomic_replace_bytes(target: Path, payload: bytes, mode: int = 0o600) -> None:
    _ensure_directory(target.parent)
    try:
        existing = target.lstat()
    except FileNotFoundError:
        existing = None
    if existing is not None and (
        not stat.S_ISREG(existing.st_mode) or stat.S_ISLNK(existing.st_mode)
    ):
        raise CapacityError(f"refusing to replace non-regular state path: {target}")
    staged = _stage_bytes(target.parent, payload, mode)
    try:
        os.replace(staged, target)
        _fsync_directory(target.parent)
    finally:
        staged.unlink(missing_ok=True)


def _parse_source(value: Any, context: str) -> str:
    source = _bounded_text(value, context, maximum=MAX_TEXT_BYTES)
    parsed = urllib.parse.urlsplit(source)
    if parsed.scheme:
        if parsed.scheme not in {"https", "file"}:
            raise CapacityError(f"{context} must be a local path, file URL, or HTTPS URL")
        if parsed.scheme == "https":
            if not parsed.hostname or parsed.username or parsed.password or parsed.fragment:
                raise CapacityError(f"{context} is not an admissible HTTPS URL")
        elif parsed.netloc not in {"", "localhost"}:
            raise CapacityError(f"{context} file URL must name the local host")
    return source


def _parse_artifact(raw: Any, context: str) -> ArtifactSpec:
    table = _require_mapping(raw, context)
    required = {"id", "role", "filename", "source", "size_bytes", "sha256", "integrity"}
    _strict_keys(table, required, set(), context)
    filename = _bounded_text(
        table["filename"], f"{context}.filename", maximum=255, pattern=SAFE_FILENAME_RE
    )
    if filename in {".", ".."} or Path(filename).name != filename:
        raise CapacityError(f"{context}.filename must be one safe basename")
    return ArtifactSpec(
        id=_token(table["id"], f"{context}.id"),
        role=_token(table["role"], f"{context}.role"),
        filename=filename,
        source=_parse_source(table["source"], f"{context}.source"),
        size_bytes=_positive_int(
            table["size_bytes"], f"{context}.size_bytes", MAX_BLOB_BYTES
        ),
        sha256=_digest_hex(table["sha256"], f"{context}.sha256"),
        integrity=_bounded_text(
            table["integrity"], f"{context}.integrity", maximum=MAX_DESCRIPTION_BYTES
        ),
    )


def _parse_dependency(raw: Any, context: str) -> DependencySpec:
    table = _require_mapping(raw, context)
    _strict_keys(table, {"package", "kind"}, set(), context)
    kind = _bounded_text(table["kind"], f"{context}.kind", maximum=16)
    if kind not in KINDS:
        raise CapacityError(f"{context}.kind is unsupported: {kind!r}")
    return DependencySpec(
        package=_token(table["package"], f"{context}.package"), kind=kind
    )


def _parse_package(raw: Any, index: int) -> PackageSpec:
    context = f"packages[{index}]"
    table = _require_mapping(raw, context)
    required = {
        "id",
        "name",
        "version",
        "kind",
        "architecture",
        "loader",
        "license",
        "redistribution",
        "requires_acceptance",
        "aliases",
        "description",
        "dependencies",
        "artifacts",
    }
    _strict_keys(table, required, set(), context)
    kind = _bounded_text(table["kind"], f"{context}.kind", maximum=16)
    if kind not in KINDS:
        raise CapacityError(f"{context}.kind is unsupported: {kind!r}")
    architecture = _bounded_text(
        table["architecture"], f"{context}.architecture", maximum=16
    )
    if architecture not in ARCHITECTURES:
        raise CapacityError(
            f"{context}.architecture is unsupported: {architecture!r}"
        )
    loader = _bounded_text(table["loader"], f"{context}.loader", maximum=16)
    if loader not in LOADERS or loader not in KIND_LOADERS[kind]:
        raise CapacityError(f"{context} kind {kind!r} cannot use loader {loader!r}")
    license_name = _bounded_text(
        table["license"], f"{context}.license", maximum=MAX_TOKEN_BYTES, pattern=LICENSE_RE
    )
    redistribution = _bounded_text(
        table["redistribution"], f"{context}.redistribution", maximum=32
    )
    if redistribution not in REDISTRIBUTION_POLICIES:
        raise CapacityError(
            f"{context}.redistribution is unsupported: {redistribution!r}"
        )
    requires_acceptance = _exact_bool(
        table["requires_acceptance"], f"{context}.requires_acceptance"
    )
    if redistribution != "permitted" and not requires_acceptance:
        raise CapacityError(
            f"{context} non-permitted redistribution requires explicit acceptance"
        )
    aliases_raw = _bounded_list(
        table["aliases"], f"{context}.aliases", MAX_ALIASES_PER_PACKAGE
    )
    aliases = tuple(_token(item, f"{context}.aliases[{offset}]") for offset, item in enumerate(aliases_raw))
    if len(set(aliases)) != len(aliases):
        raise CapacityError(f"{context}.aliases contains duplicates")
    dependencies_raw = _bounded_list(
        table["dependencies"],
        f"{context}.dependencies",
        MAX_DEPENDENCIES_PER_PACKAGE,
    )
    dependencies = tuple(
        _parse_dependency(item, f"{context}.dependencies[{offset}]")
        for offset, item in enumerate(dependencies_raw)
    )
    if len({item.package for item in dependencies}) != len(dependencies):
        raise CapacityError(f"{context}.dependencies contains duplicate packages")
    artifacts_raw = _bounded_list(
        table["artifacts"], f"{context}.artifacts", MAX_ARTIFACTS_PER_PACKAGE
    )
    artifacts = tuple(
        _parse_artifact(item, f"{context}.artifacts[{offset}]")
        for offset, item in enumerate(artifacts_raw)
    )
    if len({item.id for item in artifacts}) != len(artifacts):
        raise CapacityError(f"{context}.artifacts contains duplicate ids")
    return PackageSpec(
        id=_token(table["id"], f"{context}.id"),
        name=_token(table["name"], f"{context}.name"),
        version=_bounded_text(table["version"], f"{context}.version", maximum=128),
        kind=kind,
        architecture=architecture,
        loader=loader,
        license=license_name,
        redistribution=redistribution,
        requires_acceptance=requires_acceptance,
        aliases=aliases,
        description=_bounded_text(
            table["description"],
            f"{context}.description",
            maximum=MAX_DESCRIPTION_BYTES,
        ),
        dependencies=dependencies,
        artifacts=artifacts,
    )


def _architectures_compatible(parent: str, child: str) -> bool:
    return parent == "any" or child == "any" or parent == child


def _validate_relation(
    parent_kind: str,
    parent_architecture: str,
    parent_loader: str,
    child_kind: str,
    child_architecture: str,
    child_loader: str,
    context: str,
) -> None:
    if child_kind not in DEPENDENCY_KINDS[parent_kind]:
        raise CapacityError(
            f"{context}: {parent_kind} packages cannot depend on {child_kind} packages"
        )
    if not _architectures_compatible(parent_architecture, child_architecture):
        raise CapacityError(
            f"{context}: architecture mismatch {parent_architecture!r} vs {child_architecture!r}"
        )
    if (
        parent_kind == "os"
        and child_kind == "kernel"
        and parent_loader != "chainload"
        and parent_loader != child_loader
    ):
        raise CapacityError(
            f"{context}: OS loader {parent_loader!r} does not match kernel loader {child_loader!r}"
        )


def _catalog_topological(packages: Mapping[str, PackageSpec]) -> list[str]:
    indegree = {package_id: 0 for package_id in packages}
    dependents: dict[str, list[str]] = {package_id: [] for package_id in packages}
    for package_id, package in packages.items():
        for dependency in package.dependencies:
            if dependency.package not in packages:
                raise CapacityError(
                    f"package {package_id!r} depends on unknown package {dependency.package!r}"
                )
            child = packages[dependency.package]
            if dependency.kind != child.kind:
                raise CapacityError(
                    f"package {package_id!r} requires {dependency.package!r} as "
                    f"{dependency.kind}, observed {child.kind}"
                )
            _validate_relation(
                package.kind,
                package.architecture,
                package.loader,
                child.kind,
                child.architecture,
                child.loader,
                f"dependency {package_id!r} -> {dependency.package!r}",
            )
            indegree[package_id] += 1
            dependents[dependency.package].append(package_id)
    ready = [package_id for package_id, count in indegree.items() if count == 0]
    heapq.heapify(ready)
    result: list[str] = []
    while ready:
        package_id = heapq.heappop(ready)
        result.append(package_id)
        for dependent in sorted(dependents[package_id]):
            indegree[dependent] -= 1
            if indegree[dependent] == 0:
                heapq.heappush(ready, dependent)
    if len(result) != len(packages):
        cyclic = sorted(package_id for package_id, count in indegree.items() if count)
        raise CapacityError(f"catalog dependency graph contains a cycle: {cyclic!r}")
    return result


def load_catalog(path: Path | str) -> Catalog:
    catalog_path = Path(path).expanduser().absolute()
    payload = _read_regular_bounded(catalog_path, MAX_CATALOG_BYTES)
    try:
        raw = tomllib.loads(payload.decode("utf-8", "strict"))
    except (UnicodeError, tomllib.TOMLDecodeError) as error:
        raise CapacityError(f"cannot parse strict catalog {catalog_path}: {error}") from error
    table = _require_mapping(raw, "catalog")
    _strict_keys(table, {"schema", "name", "packages"}, set(), "catalog")
    if table["schema"] != CATALOG_SCHEMA:
        raise CapacityError(
            f"catalog.schema must be exactly {CATALOG_SCHEMA!r}"
        )
    packages_raw = _bounded_list(table["packages"], "catalog.packages", MAX_PACKAGES)
    if not packages_raw:
        raise CapacityError("catalog.packages must not be empty")
    packages = tuple(_parse_package(item, index) for index, item in enumerate(packages_raw))
    by_id = {package.id: package for package in packages}
    if len(by_id) != len(packages):
        raise CapacityError("catalog contains duplicate package ids")
    aliases: dict[str, str] = {}
    for package in packages:
        for alias in (package.id, *package.aliases):
            previous = aliases.setdefault(alias, package.id)
            if previous != package.id:
                raise CapacityError(
                    f"catalog alias {alias!r} collides between {previous!r} and {package.id!r}"
                )
    _catalog_topological(by_id)
    return Catalog(
        path=catalog_path,
        name=_token(table["name"], "catalog.name"),
        packages=packages,
    )


def catalog_records(catalog: Catalog) -> dict[str, dict[str, Any]]:
    packages = catalog.by_id
    records: dict[str, dict[str, Any]] = {}
    for package_id in _catalog_topological(packages):
        package = packages[package_id]
        dependencies = sorted(
            (
                {
                    "digest": records[dependency.package]["digest"],
                    "kind": dependency.kind,
                }
                for dependency in package.dependencies
            ),
            key=lambda item: (item["digest"], item["kind"]),
        )
        artifacts = sorted(
            (artifact.identity_record() for artifact in package.artifacts),
            key=lambda item: (item["id"], item["sha256"]),
        )
        body: dict[str, Any] = {
            "architecture": package.architecture,
            "artifacts": artifacts,
            "catalog_id": package.id,
            "dependencies": dependencies,
            "kind": package.kind,
            "license": package.license,
            "loader": package.loader,
            "name": package.name,
            "redistribution": package.redistribution,
            "requires_acceptance": package.requires_acceptance,
            "schema": PACKAGE_SCHEMA,
            "version": package.version,
        }
        records[package_id] = {**body, "digest": _domain_digest(PACKAGE_DOMAIN, body)}
    return records


def _validate_artifact_record(raw: Any, context: str) -> dict[str, Any]:
    table = _require_mapping(raw, context)
    _strict_keys(table, {"filename", "id", "role", "sha256", "size_bytes"}, set(), context)
    artifact = {
        "filename": _bounded_text(
            table["filename"], f"{context}.filename", maximum=255, pattern=SAFE_FILENAME_RE
        ),
        "id": _token(table["id"], f"{context}.id"),
        "role": _token(table["role"], f"{context}.role"),
        "sha256": _digest_hex(table["sha256"], f"{context}.sha256"),
        "size_bytes": _positive_int(
            table["size_bytes"], f"{context}.size_bytes", MAX_BLOB_BYTES
        ),
    }
    if Path(artifact["filename"]).name != artifact["filename"]:
        raise CapacityError(f"{context}.filename is not a basename")
    return artifact


def _validate_dependency_record(raw: Any, context: str) -> dict[str, str]:
    table = _require_mapping(raw, context)
    _strict_keys(table, {"digest", "kind"}, set(), context)
    kind = _bounded_text(table["kind"], f"{context}.kind", maximum=16)
    if kind not in KINDS:
        raise CapacityError(f"{context}.kind is unsupported")
    return {"digest": _digest_ref(table["digest"], f"{context}.digest"), "kind": kind}


def validate_package_record(raw: Any, *, expected_digest: str | None = None) -> dict[str, Any]:
    table = _require_mapping(raw, "package record")
    body_keys = {
        "architecture",
        "artifacts",
        "catalog_id",
        "dependencies",
        "kind",
        "license",
        "loader",
        "name",
        "redistribution",
        "requires_acceptance",
        "schema",
        "version",
    }
    _strict_keys(table, body_keys | {"digest"}, set(), "package record")
    if table["schema"] != PACKAGE_SCHEMA:
        raise CapacityError("package record has the wrong schema")
    kind = _bounded_text(table["kind"], "package.kind", maximum=16)
    if kind not in KINDS:
        raise CapacityError("package.kind is unsupported")
    architecture = _bounded_text(table["architecture"], "package.architecture", maximum=16)
    if architecture not in ARCHITECTURES:
        raise CapacityError("package.architecture is unsupported")
    loader = _bounded_text(table["loader"], "package.loader", maximum=16)
    if loader not in KIND_LOADERS[kind]:
        raise CapacityError(f"package kind {kind!r} cannot use loader {loader!r}")
    redistribution = _bounded_text(
        table["redistribution"], "package.redistribution", maximum=32
    )
    if redistribution not in REDISTRIBUTION_POLICIES:
        raise CapacityError("package.redistribution is unsupported")
    requires_acceptance = _exact_bool(
        table["requires_acceptance"], "package.requires_acceptance"
    )
    if redistribution != "permitted" and not requires_acceptance:
        raise CapacityError("non-permitted package lacks required license acceptance")
    artifacts = [
        _validate_artifact_record(item, f"package.artifacts[{index}]")
        for index, item in enumerate(
            _bounded_list(
                table["artifacts"], "package.artifacts", MAX_ARTIFACTS_PER_PACKAGE
            )
        )
    ]
    dependencies = [
        _validate_dependency_record(item, f"package.dependencies[{index}]")
        for index, item in enumerate(
            _bounded_list(
                table["dependencies"],
                "package.dependencies",
                MAX_DEPENDENCIES_PER_PACKAGE,
            )
        )
    ]
    if artifacts != sorted(artifacts, key=lambda item: (item["id"], item["sha256"])):
        raise CapacityError("package.artifacts is not canonically sorted")
    if dependencies != sorted(
        dependencies, key=lambda item: (item["digest"], item["kind"])
    ):
        raise CapacityError("package.dependencies is not canonically sorted")
    if len({item["id"] for item in artifacts}) != len(artifacts):
        raise CapacityError("package.artifacts contains duplicate ids")
    if len({item["digest"] for item in dependencies}) != len(dependencies):
        raise CapacityError("package.dependencies contains duplicate digests")
    body = {
        "architecture": architecture,
        "artifacts": artifacts,
        "catalog_id": _token(table["catalog_id"], "package.catalog_id"),
        "dependencies": dependencies,
        "kind": kind,
        "license": _bounded_text(
            table["license"], "package.license", maximum=MAX_TOKEN_BYTES, pattern=LICENSE_RE
        ),
        "loader": loader,
        "name": _token(table["name"], "package.name"),
        "redistribution": redistribution,
        "requires_acceptance": requires_acceptance,
        "schema": PACKAGE_SCHEMA,
        "version": _bounded_text(table["version"], "package.version", maximum=128),
    }
    observed_digest = _digest_ref(table["digest"], "package.digest")
    computed_digest = _domain_digest(PACKAGE_DOMAIN, body)
    if observed_digest != computed_digest:
        raise CapacityError(
            f"package identity mismatch: expected {computed_digest}, observed {observed_digest}"
        )
    if expected_digest is not None and observed_digest != expected_digest:
        raise CapacityError(
            f"package path identity mismatch: expected {expected_digest}, observed {observed_digest}"
        )
    return {**body, "digest": observed_digest}


def _copy_stream(
    source: BinaryIO,
    destination_descriptor: int,
    *,
    expected_size: int,
    expected_sha256: str,
) -> tuple[int, str]:
    """Copy and hash without accepting an unbounded read from the source."""

    digest = hashlib.sha256()
    size = 0
    while True:
        chunk = source.read(STREAM_CHUNK_BYTES)
        if chunk is None:
            continue
        if not isinstance(chunk, (bytes, bytearray, memoryview)):
            raise CapacityError("artifact source returned non-byte content")
        if not chunk:
            break
        data = bytes(chunk)
        size += len(data)
        if size > expected_size:
            raise CapacityError(
                f"artifact exceeded expected size {expected_size}: observed more than {size}"
            )
        digest.update(data)
        _write_all(destination_descriptor, data)
    observed_sha256 = digest.hexdigest()
    if size != expected_size or observed_sha256 != expected_sha256:
        raise CapacityError(
            "artifact identity mismatch: "
            f"expected size={expected_size} sha256={expected_sha256}, "
            f"observed size={size} sha256={observed_sha256}"
        )
    return size, observed_sha256


def _local_source_path(source: str, base_directory: Path) -> Path:
    parsed = urllib.parse.urlsplit(source)
    if parsed.scheme == "file":
        path = Path(urllib.request.url2pathname(parsed.path))
    elif parsed.scheme:
        raise CapacityError("source is not local")
    else:
        path = Path(source).expanduser()
    if not path.is_absolute():
        path = base_directory / path
    return path.absolute()


class CapacityStore:
    """Local immutable object stores plus one revisioned activation head."""

    def __init__(self, root: Path | str):
        self.root = Path(root).expanduser().absolute()
        self.blobs = self.root / "capacity-blobs" / "sha256"
        self.packages = self.root / "capacity-packages" / "sha256"
        self.generations = self.root / "capacity-generations" / "sha256"
        self.aliases_path = self.root / "aliases.json"
        self.head_path = self.root / "head.json"
        self.lock_path = self.root / "state.lock"
        for directory in (self.root, self.blobs, self.packages, self.generations):
            _ensure_directory(directory)

    @contextmanager
    def locked(self) -> Iterator[None]:
        flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_CLOEXEC", 0)
        if not hasattr(os, "O_NOFOLLOW"):
            raise CapacityError("this host cannot pin the state lock with O_NOFOLLOW")
        flags |= os.O_NOFOLLOW
        try:
            descriptor = os.open(self.lock_path, flags, 0o600)
        except OSError as error:
            raise CapacityError(f"cannot open capacity state lock: {error}") from error
        try:
            state = os.fstat(descriptor)
            if not stat.S_ISREG(state.st_mode):
                raise CapacityError("capacity state lock is not a regular file")
            os.fchmod(descriptor, 0o600)
            fcntl.flock(descriptor, fcntl.LOCK_EX)
            yield
        finally:
            try:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
            finally:
                os.close(descriptor)

    def blob_path(self, sha256: str) -> Path:
        return self.blobs / _digest_hex(sha256, "blob sha256")

    def package_path(self, digest: str) -> Path:
        match = DIGEST_REF_RE.fullmatch(_digest_ref(digest, "package digest"))
        assert match is not None
        return self.packages / f"{match.group(1)}.json"

    def generation_path(self, digest: str) -> Path:
        match = DIGEST_REF_RE.fullmatch(_digest_ref(digest, "generation digest"))
        assert match is not None
        return self.generations / f"{match.group(1)}.json"

    def verify_blob(self, sha256: str, size_bytes: int) -> dict[str, Any]:
        expected_sha = _digest_hex(sha256, "blob sha256")
        expected_size = _positive_int(size_bytes, "blob size", MAX_BLOB_BYTES)
        path = self.blob_path(expected_sha)
        descriptor = _open_regular(path, require_immutable=True)
        digest = hashlib.sha256()
        observed_size = 0
        try:
            before = os.fstat(descriptor)
            if before.st_size != expected_size:
                raise CapacityError(
                    f"blob size mismatch for {expected_sha}: expected {expected_size}, observed {before.st_size}"
                )
            while True:
                chunk = os.read(descriptor, STREAM_CHUNK_BYTES)
                if not chunk:
                    break
                observed_size += len(chunk)
                if observed_size > expected_size:
                    raise CapacityError(f"blob exceeded expected size: {expected_sha}")
                digest.update(chunk)
            after = os.fstat(descriptor)
            if _file_identity(before) != _file_identity(after):
                raise CapacityError(f"blob changed while pinned: {expected_sha}")
        finally:
            os.close(descriptor)
        observed_sha = digest.hexdigest()
        if observed_size != expected_size or observed_sha != expected_sha:
            raise CapacityError(
                f"blob identity mismatch for {expected_sha}: "
                f"expected size={expected_size} sha256={expected_sha}, "
                f"observed size={observed_size} sha256={observed_sha}"
            )
        return {"sha256": observed_sha, "size_bytes": observed_size, "path": str(path)}

    def install_blob(
        self,
        artifact: ArtifactSpec,
        source: str,
        *,
        base_directory: Path,
        network_timeout: float = 30.0,
    ) -> dict[str, Any]:
        target = self.blob_path(artifact.sha256)
        if target.exists() or target.is_symlink():
            verified = self.verify_blob(artifact.sha256, artifact.size_bytes)
            return {**verified, "installed": False}
        descriptor, raw_staged = tempfile.mkstemp(prefix=".capacity-blob-", dir=self.blobs)
        staged = Path(raw_staged)
        response: Any | None = None
        local_descriptor: int | None = None
        try:
            os.fchmod(descriptor, 0o600)
            parsed = urllib.parse.urlsplit(source)
            if parsed.scheme == "https":
                request = urllib.request.Request(
                    source,
                    headers={"User-Agent": "ostadix-absorbed-capacity/1"},
                )
                try:
                    response = urllib.request.urlopen(request, timeout=network_timeout)
                except OSError as error:
                    raise CapacityError(f"HTTPS artifact fetch failed for {source}: {error}") from error
                final_url = response.geturl()
                if urllib.parse.urlsplit(final_url).scheme != "https":
                    raise CapacityError("HTTPS artifact redirected to a non-HTTPS URL")
                content_length = response.headers.get("Content-Length")
                if content_length is not None:
                    try:
                        declared_length = int(content_length)
                    except ValueError as error:
                        raise CapacityError("HTTPS Content-Length is not an integer") from error
                    if declared_length != artifact.size_bytes:
                        raise CapacityError(
                            f"HTTPS Content-Length mismatch: expected {artifact.size_bytes}, observed {declared_length}"
                        )
                _copy_stream(
                    response,
                    descriptor,
                    expected_size=artifact.size_bytes,
                    expected_sha256=artifact.sha256,
                )
            else:
                source_path = _local_source_path(source, base_directory)
                local_descriptor = _open_regular(source_path)
                before = os.fstat(local_descriptor)
                if before.st_size != artifact.size_bytes:
                    raise CapacityError(
                        f"local artifact size mismatch for {source_path}: "
                        f"expected {artifact.size_bytes}, observed {before.st_size}"
                    )
                with os.fdopen(os.dup(local_descriptor), "rb", closefd=True) as stream:
                    _copy_stream(
                        stream,
                        descriptor,
                        expected_size=artifact.size_bytes,
                        expected_sha256=artifact.sha256,
                    )
                after = os.fstat(local_descriptor)
                if _file_identity(before) != _file_identity(after):
                    raise CapacityError(f"local artifact changed while pinned: {source_path}")
            os.fsync(descriptor)
            os.fchmod(descriptor, 0o444)
            os.fsync(descriptor)
            os.close(descriptor)
            descriptor = -1
            published = _publish_temp_no_clobber(staged, target)
            verified = self.verify_blob(artifact.sha256, artifact.size_bytes)
            return {**verified, "installed": published}
        finally:
            if response is not None:
                response.close()
            if local_descriptor is not None:
                os.close(local_descriptor)
            if descriptor >= 0:
                os.close(descriptor)
            staged.unlink(missing_ok=True)

    def publish_package(self, record: Mapping[str, Any]) -> bool:
        validated = validate_package_record(dict(record))
        payload = _canonical_json(validated) + b"\n"
        return _publish_immutable_bytes(
            self.package_path(validated["digest"]), payload
        )

    def load_package(self, digest: str, *, verify_blobs: bool = False) -> dict[str, Any]:
        expected = _digest_ref(digest, "package digest")
        path = self.package_path(expected)
        payload = _read_regular_bounded(path, MAX_RECORD_BYTES, require_immutable=True)
        try:
            raw = json.loads(payload.decode("ascii", "strict"))
        except (UnicodeError, json.JSONDecodeError) as error:
            raise CapacityError(f"cannot parse immutable package {expected}: {error}") from error
        record = validate_package_record(raw, expected_digest=expected)
        if payload != _canonical_json(record) + b"\n":
            raise CapacityError(f"immutable package is not canonically encoded: {expected}")
        if verify_blobs:
            for artifact in record["artifacts"]:
                self.verify_blob(artifact["sha256"], artifact["size_bytes"])
        return record

    def installed_digests(self) -> list[str]:
        digests: list[str] = []
        for path in sorted(self.packages.iterdir()):
            if path.name.startswith("."):
                continue
            match = re.fullmatch(r"([0-9a-f]{64})\.json", path.name)
            if match is None:
                raise CapacityError(f"unexpected package-store entry: {path}")
            digest = "sha256:" + match.group(1)
            self.load_package(digest)
            digests.append(digest)
        return digests

    def _load_aliases_unlocked(self) -> dict[str, str]:
        if not self.aliases_path.exists() and not self.aliases_path.is_symlink():
            return {}
        payload = _read_regular_bounded(self.aliases_path, MAX_RECORD_BYTES)
        try:
            raw = json.loads(payload.decode("ascii", "strict"))
        except (UnicodeError, json.JSONDecodeError) as error:
            raise CapacityError(f"cannot parse alias state: {error}") from error
        table = _require_mapping(raw, "aliases state")
        _strict_keys(table, {"schema", "aliases"}, set(), "aliases state")
        if table["schema"] != ALIASES_SCHEMA:
            raise CapacityError("aliases state has the wrong schema")
        mapping = _require_mapping(table["aliases"], "aliases")
        if len(mapping) > MAX_ALIASES:
            raise CapacityError(f"aliases exceeds {MAX_ALIASES} entries")
        aliases: dict[str, str] = {}
        for alias, digest in mapping.items():
            aliases[_token(alias, "alias name")] = _digest_ref(digest, f"alias {alias!r}")
        if list(mapping) != sorted(mapping):
            raise CapacityError("aliases state is not canonically sorted")
        canonical = {"aliases": aliases, "schema": ALIASES_SCHEMA}
        if payload != _canonical_json(canonical) + b"\n":
            raise CapacityError("aliases state is not canonically encoded")
        return aliases

    def update_aliases(self, updates: Mapping[str, str], *, replace: bool) -> dict[str, str]:
        with self.locked():
            aliases = self._load_aliases_unlocked()
            for raw_alias, raw_digest in sorted(updates.items()):
                alias = _token(raw_alias, "alias")
                digest = _digest_ref(raw_digest, f"alias {alias!r} digest")
                self.load_package(digest)
                previous = aliases.get(alias)
                if previous is not None and previous != digest and not replace:
                    raise CapacityError(
                        f"alias {alias!r} already resolves to {previous}; use explicit replacement"
                    )
                aliases[alias] = digest
            if len(aliases) > MAX_ALIASES:
                raise CapacityError(f"aliases exceeds {MAX_ALIASES} entries")
            aliases = dict(sorted(aliases.items()))
            payload = _canonical_json({"aliases": aliases, "schema": ALIASES_SCHEMA}) + b"\n"
            _atomic_replace_bytes(self.aliases_path, payload)
            return aliases

    def resolve_ref(self, reference: str) -> tuple[str, bool]:
        with self.locked():
            return self._resolve_ref_unlocked(reference, self._load_aliases_unlocked())

    def _resolve_ref_unlocked(
        self, reference: str, aliases: Mapping[str, str]
    ) -> tuple[str, bool]:
        if DIGEST_REF_RE.fullmatch(reference):
            digest = _digest_ref(reference, "exact package reference")
            self.load_package(digest)
            return digest, False
        alias = _token(reference, "package alias")
        try:
            digest = aliases[alias]
        except KeyError as error:
            raise CapacityError(f"unknown installed package alias: {alias!r}") from error
        self.load_package(digest)
        return digest, True

    def _load_head_unlocked(self) -> dict[str, Any]:
        if not self.head_path.exists() and not self.head_path.is_symlink():
            return {"current": None, "previous": None, "revision": 0, "schema": HEAD_SCHEMA}
        payload = _read_regular_bounded(self.head_path, MAX_RECORD_BYTES)
        try:
            raw = json.loads(payload.decode("ascii", "strict"))
        except (UnicodeError, json.JSONDecodeError) as error:
            raise CapacityError(f"cannot parse activation head: {error}") from error
        table = _require_mapping(raw, "activation head")
        _strict_keys(table, {"current", "previous", "revision", "schema"}, set(), "activation head")
        if table["schema"] != HEAD_SCHEMA:
            raise CapacityError("activation head has the wrong schema")
        revision = _nonnegative_int(table["revision"], "head.revision", MAX_REVISION)
        current = None if table["current"] is None else _digest_ref(table["current"], "head.current")
        previous = None if table["previous"] is None else _digest_ref(table["previous"], "head.previous")
        head = {"current": current, "previous": previous, "revision": revision, "schema": HEAD_SCHEMA}
        if payload != _canonical_json(head) + b"\n":
            raise CapacityError("activation head is not canonically encoded")
        return head

    def read_head(self) -> dict[str, Any]:
        with self.locked():
            return self._load_head_unlocked()

    def _write_head_unlocked(self, head: Mapping[str, Any]) -> None:
        _atomic_replace_bytes(self.head_path, _canonical_json(dict(head)) + b"\n")

    def publish_generation(self, generation: Mapping[str, Any]) -> bool:
        validated = validate_generation(dict(generation))
        return _publish_immutable_bytes(
            self.generation_path(validated["digest"]),
            _canonical_json(validated) + b"\n",
        )

    def load_generation(self, digest: str) -> dict[str, Any]:
        expected = _digest_ref(digest, "generation digest")
        path = self.generation_path(expected)
        payload = _read_regular_bounded(path, MAX_RECORD_BYTES, require_immutable=True)
        try:
            raw = json.loads(payload.decode("ascii", "strict"))
        except (UnicodeError, json.JSONDecodeError) as error:
            raise CapacityError(f"cannot parse generation {expected}: {error}") from error
        generation = validate_generation(raw, expected_digest=expected)
        if payload != _canonical_json(generation) + b"\n":
            raise CapacityError(f"generation is not canonically encoded: {expected}")
        return generation


def _catalog_closure(catalog: Catalog, root_id: str) -> list[str]:
    packages = catalog.by_id
    if root_id not in packages:
        raise CapacityError(f"catalog contains no package {root_id!r}")
    wanted: set[str] = set()

    def visit(package_id: str) -> None:
        if package_id in wanted:
            return
        wanted.add(package_id)
        for dependency in packages[package_id].dependencies:
            visit(dependency.package)

    visit(root_id)
    return [item for item in _catalog_topological(packages) if item in wanted]


def _resolve_install_sources(
    catalog: Catalog,
    closure_ids: Sequence[str],
    overrides: Mapping[str, str],
) -> dict[tuple[str, str], str]:
    packages = catalog.by_id
    artifact_owners: dict[str, list[str]] = {}
    for package_id in closure_ids:
        for artifact in packages[package_id].artifacts:
            artifact_owners.setdefault(artifact.id, []).append(package_id)
    resolved: dict[tuple[str, str], str] = {}
    used: set[str] = set()
    for package_id in closure_ids:
        for artifact in packages[package_id].artifacts:
            qualified = f"{package_id}/{artifact.id}"
            if qualified in overrides:
                source = overrides[qualified]
                used.add(qualified)
            elif artifact.id in overrides:
                if len(artifact_owners[artifact.id]) != 1:
                    raise CapacityError(
                        f"source override {artifact.id!r} is ambiguous; use package/artifact"
                    )
                source = overrides[artifact.id]
                used.add(artifact.id)
            else:
                source = artifact.source
            resolved[(package_id, artifact.id)] = _parse_source(
                source, f"source for {qualified}"
            )
    unused = set(overrides) - used
    if unused:
        raise CapacityError(f"unused source overrides: {sorted(unused)!r}")
    return resolved


def install_catalog_package(
    store: CapacityStore,
    catalog: Catalog,
    package_id: str,
    *,
    source_overrides: Mapping[str, str] | None = None,
    extra_aliases: Sequence[str] = (),
    replace_aliases: bool = False,
    network_timeout: float = 30.0,
) -> dict[str, Any]:
    package_id = _token(package_id, "catalog package id")
    closure_ids = _catalog_closure(catalog, package_id)
    records = catalog_records(catalog)
    overrides = dict(source_overrides or {})
    sources = _resolve_install_sources(catalog, closure_ids, overrides)
    packages = catalog.by_id
    installed_blobs: list[dict[str, Any]] = []
    installed_packages: list[dict[str, Any]] = []
    for current_id in closure_ids:
        package = packages[current_id]
        for artifact in sorted(package.artifacts, key=lambda item: item.id):
            result = store.install_blob(
                artifact,
                sources[(current_id, artifact.id)],
                base_directory=catalog.path.parent,
                network_timeout=network_timeout,
            )
            installed_blobs.append(
                {"artifact": artifact.id, "package": current_id, **result}
            )
        published = store.publish_package(records[current_id])
        installed_packages.append(
            {
                "digest": records[current_id]["digest"],
                "installed": published,
                "package": current_id,
            }
        )
    updates: dict[str, str] = {}
    for current_id in closure_ids:
        package = packages[current_id]
        for alias in (package.id, *package.aliases):
            updates[alias] = records[current_id]["digest"]
    target_digest = records[package_id]["digest"]
    for alias in extra_aliases:
        updates[_token(alias, "extra alias")] = target_digest
    aliases = store.update_aliases(updates, replace=replace_aliases)
    return {
        "aliases_updated": sorted(updates),
        "blobs": installed_blobs,
        "packages": installed_packages,
        "resolved_alias_count": len(aliases),
        "target": target_digest,
    }


def _closure_from_records(
    store: CapacityStore, roots: Sequence[str]
) -> tuple[list[str], list[str], dict[str, dict[str, Any]]]:
    if not roots:
        raise CapacityError("a plan requires at least one root package")
    if len(roots) > MAX_CLOSURE_PACKAGES:
        raise CapacityError("too many root packages")
    records: dict[str, dict[str, Any]] = {}
    visiting: set[str] = set()
    visited: set[str] = set()

    def visit(digest: str) -> None:
        digest = _digest_ref(digest, "closure package digest")
        if digest in visited:
            return
        if digest in visiting:
            raise CapacityError(f"installed dependency graph contains a cycle at {digest}")
        if len(records) >= MAX_CLOSURE_PACKAGES:
            raise CapacityError(f"dependency closure exceeds {MAX_CLOSURE_PACKAGES} packages")
        visiting.add(digest)
        record = store.load_package(digest)
        records[digest] = record
        for dependency in record["dependencies"]:
            visit(dependency["digest"])
        visiting.remove(digest)
        visited.add(digest)

    for root in sorted(set(roots)):
        visit(root)
    for parent_digest, parent in records.items():
        for dependency in parent["dependencies"]:
            child = records[dependency["digest"]]
            if dependency["kind"] != child["kind"]:
                raise CapacityError(
                    f"package {parent_digest} requires {dependency['digest']} as "
                    f"{dependency['kind']}, observed {child['kind']}"
                )
            _validate_relation(
                parent["kind"],
                parent["architecture"],
                parent["loader"],
                child["kind"],
                child["architecture"],
                child["loader"],
                f"installed dependency {parent_digest} -> {dependency['digest']}",
            )
    indegree = {digest: 0 for digest in records}
    dependents: dict[str, list[str]] = {digest: [] for digest in records}
    for parent_digest, parent in records.items():
        for dependency in parent["dependencies"]:
            child_digest = dependency["digest"]
            indegree[parent_digest] += 1
            dependents[child_digest].append(parent_digest)
    ready = [digest for digest, count in indegree.items() if count == 0]
    heapq.heapify(ready)
    activation_order: list[str] = []
    while ready:
        digest = heapq.heappop(ready)
        activation_order.append(digest)
        for dependent in sorted(dependents[digest]):
            indegree[dependent] -= 1
            if indegree[dependent] == 0:
                heapq.heappush(ready, dependent)
    if len(activation_order) != len(records):
        raise CapacityError("installed dependency graph contains a cycle")
    closure = sorted(records)
    return closure, activation_order, records


def create_plan(
    store: CapacityStore,
    references: Sequence[str],
    *,
    accepted_license_refs: Sequence[str] = (),
) -> dict[str, Any]:
    with store.locked():
        aliases = store._load_aliases_unlocked()
        head = store._load_head_unlocked()
        roots = sorted(
            {
                store._resolve_ref_unlocked(reference, aliases)[0]
                for reference in references
            }
        )
        accepted = sorted(
            {
                store._resolve_ref_unlocked(reference, aliases)[0]
                for reference in accepted_license_refs
            }
        )
    closure, activation_order, records = _closure_from_records(store, roots)
    outside = sorted(set(accepted) - set(closure))
    if outside:
        raise CapacityError(
            f"license acceptance names packages outside the plan closure: {outside!r}"
        )
    required = sorted(
        digest for digest, record in records.items() if record["requires_acceptance"]
    )
    missing = sorted(set(required) - set(accepted))
    if missing:
        raise CapacityError(
            "explicit license acceptance is required for exact packages: " + repr(missing)
        )
    body: dict[str, Any] = {
        "accepted_licenses": accepted,
        "activation_order": activation_order,
        "base_revision": head["revision"],
        "closure": closure,
        "roots": roots,
        "schema": PLAN_SCHEMA,
    }
    return {**body, "digest": _domain_digest(PLAN_DOMAIN, body)}


def validate_plan(raw: Any) -> dict[str, Any]:
    table = _require_mapping(raw, "activation plan")
    body_keys = {
        "accepted_licenses",
        "activation_order",
        "base_revision",
        "closure",
        "roots",
        "schema",
    }
    _strict_keys(table, body_keys | {"digest"}, set(), "activation plan")
    if table["schema"] != PLAN_SCHEMA:
        raise CapacityError("activation plan has the wrong schema")

    def digest_list(key: str, *, nonempty: bool = False) -> list[str]:
        values = _bounded_list(table[key], f"plan.{key}", MAX_CLOSURE_PACKAGES)
        parsed = [_digest_ref(item, f"plan.{key}[]") for item in values]
        if nonempty and not parsed:
            raise CapacityError(f"plan.{key} must not be empty")
        if len(set(parsed)) != len(parsed):
            raise CapacityError(f"plan.{key} contains duplicates")
        return parsed

    closure = digest_list("closure", nonempty=True)
    roots = digest_list("roots", nonempty=True)
    activation_order = digest_list("activation_order", nonempty=True)
    accepted = digest_list("accepted_licenses")
    if closure != sorted(closure):
        raise CapacityError("plan.closure is not canonically sorted")
    if roots != sorted(roots):
        raise CapacityError("plan.roots is not canonically sorted")
    if accepted != sorted(accepted):
        raise CapacityError("plan.accepted_licenses is not canonically sorted")
    if set(activation_order) != set(closure):
        raise CapacityError("plan.activation_order does not exactly cover plan.closure")
    if not set(roots).issubset(closure):
        raise CapacityError("plan.roots is not contained in plan.closure")
    if not set(accepted).issubset(closure):
        raise CapacityError("plan.accepted_licenses is not contained in plan.closure")
    body = {
        "accepted_licenses": accepted,
        "activation_order": activation_order,
        "base_revision": _nonnegative_int(
            table["base_revision"], "plan.base_revision", MAX_REVISION
        ),
        "closure": closure,
        "roots": roots,
        "schema": PLAN_SCHEMA,
    }
    observed = _digest_ref(table["digest"], "plan.digest")
    computed = _domain_digest(PLAN_DOMAIN, body)
    if observed != computed:
        raise CapacityError(
            f"activation plan identity mismatch: expected {computed}, observed {observed}"
        )
    return {**body, "digest": observed}


def validate_generation(
    raw: Any, *, expected_digest: str | None = None
) -> dict[str, Any]:
    table = _require_mapping(raw, "capacity generation")
    body_keys = {
        "activation_order",
        "closure",
        "qualified_packages",
        "roots",
        "schema",
    }
    _strict_keys(table, body_keys | {"digest"}, set(), "capacity generation")
    if table["schema"] != GENERATION_SCHEMA:
        raise CapacityError("capacity generation has the wrong schema")

    def digests(key: str, *, nonempty: bool = False) -> list[str]:
        values = _bounded_list(table[key], f"generation.{key}", MAX_CLOSURE_PACKAGES)
        parsed = [_digest_ref(item, f"generation.{key}[]") for item in values]
        if nonempty and not parsed:
            raise CapacityError(f"generation.{key} must not be empty")
        if len(set(parsed)) != len(parsed):
            raise CapacityError(f"generation.{key} contains duplicates")
        return parsed

    closure = digests("closure", nonempty=True)
    roots = digests("roots", nonempty=True)
    activation_order = digests("activation_order", nonempty=True)
    qualified = digests("qualified_packages")
    if closure != sorted(closure) or roots != sorted(roots) or qualified != sorted(qualified):
        raise CapacityError("capacity generation contains a non-canonical sorted set")
    if set(activation_order) != set(closure):
        raise CapacityError("generation.activation_order does not exactly cover closure")
    if not set(roots).issubset(closure):
        raise CapacityError("generation.roots is not contained in closure")
    if not set(qualified).issubset(closure):
        raise CapacityError("generation qualified set is outside closure")
    body = {
        "activation_order": activation_order,
        "closure": closure,
        "qualified_packages": qualified,
        "roots": roots,
        "schema": GENERATION_SCHEMA,
    }
    observed = _digest_ref(table["digest"], "generation.digest")
    computed = _domain_digest(GENERATION_DOMAIN, body)
    if observed != computed:
        raise CapacityError(
            f"generation identity mismatch: expected {computed}, observed {observed}"
        )
    if expected_digest is not None and observed != expected_digest:
        raise CapacityError("generation path and record identities differ")
    return {**body, "digest": observed}


def _generation_from_plan(plan: Mapping[str, Any]) -> dict[str, Any]:
    body = {
        "activation_order": list(plan["activation_order"]),
        "closure": list(plan["closure"]),
        # V1 has no qualification-producing command.  Applying never promotes
        # install or foreign-lab observations into qualification.
        "qualified_packages": [],
        "roots": list(plan["roots"]),
        "schema": GENERATION_SCHEMA,
    }
    return {**body, "digest": _domain_digest(GENERATION_DOMAIN, body)}


def apply_plan(store: CapacityStore, raw_plan: Any) -> dict[str, Any]:
    plan = validate_plan(raw_plan)
    # Reject an already-stale plan before hashing a potentially multi-gigabyte
    # closure.  The same revision is checked again under the commit lock after
    # verification, which is the authoritative compare-and-swap.
    with store.locked():
        initial_head = store._load_head_unlocked()
        if initial_head["revision"] != plan["base_revision"]:
            raise CapacityError(
                f"stale activation plan: base revision {plan['base_revision']}, "
                f"current revision {initial_head['revision']}"
            )
        if initial_head["revision"] == MAX_REVISION:
            raise CapacityError("activation revision is exhausted")
    closure, activation_order, records = _closure_from_records(store, plan["roots"])
    if closure != plan["closure"] or activation_order != plan["activation_order"]:
        raise CapacityError("installed dependency closure no longer matches the exact plan")
    missing_acceptance = sorted(
        digest
        for digest, record in records.items()
        if record["requires_acceptance"] and digest not in plan["accepted_licenses"]
    )
    if missing_acceptance:
        raise CapacityError(
            f"plan lacks exact required license acceptance: {missing_acceptance!r}"
        )
    for digest in plan["activation_order"]:
        store.load_package(digest, verify_blobs=True)
    generation = _generation_from_plan(plan)
    store.publish_generation(generation)
    with store.locked():
        head = store._load_head_unlocked()
        if head["revision"] != plan["base_revision"]:
            raise CapacityError(
                f"stale activation plan: base revision {plan['base_revision']}, "
                f"current revision {head['revision']}"
            )
        if head["revision"] == MAX_REVISION:
            raise CapacityError("activation revision is exhausted")
        updated = {
            "current": generation["digest"],
            "previous": head["current"],
            "revision": head["revision"] + 1,
            "schema": HEAD_SCHEMA,
        }
        store._write_head_unlocked(updated)
    return {"generation": generation, "head": updated}


def rollback(store: CapacityStore) -> dict[str, Any]:
    with store.locked():
        head = store._load_head_unlocked()
        if head["previous"] is None:
            raise CapacityError("no previous absorbed-capacity generation is retained")
        if head["revision"] == MAX_REVISION:
            raise CapacityError("activation revision is exhausted")
        store.load_generation(head["previous"])
        if head["current"] is not None:
            store.load_generation(head["current"])
        updated = {
            "current": head["previous"],
            "previous": head["current"],
            "revision": head["revision"] + 1,
            "schema": HEAD_SCHEMA,
        }
        store._write_head_unlocked(updated)
    return updated


def status(store: CapacityStore) -> dict[str, Any]:
    head = store.read_head()
    current = store.load_generation(head["current"]) if head["current"] else None
    previous = store.load_generation(head["previous"]) if head["previous"] else None
    installed = store.installed_digests()
    return {
        "active_packages": [] if current is None else current["closure"],
        "current_generation": head["current"],
        "installed_package_count": len(installed),
        "previous_generation": head["previous"],
        "qualified_packages": [] if current is None else current["qualified_packages"],
        "revision": head["revision"],
        "schema": HEAD_SCHEMA,
    }


def list_packages(store: CapacityStore) -> dict[str, Any]:
    head = store.read_head()
    current = store.load_generation(head["current"]) if head["current"] else None
    active = set() if current is None else set(current["closure"])
    qualified = set() if current is None else set(current["qualified_packages"])
    with store.locked():
        aliases = store._load_aliases_unlocked()
    reverse_aliases: dict[str, list[str]] = {}
    for alias, digest in aliases.items():
        reverse_aliases.setdefault(digest, []).append(alias)
    packages = []
    for digest in store.installed_digests():
        record = store.load_package(digest)
        packages.append(
            {
                "active": digest in active,
                "aliases": sorted(reverse_aliases.get(digest, [])),
                "architecture": record["architecture"],
                "digest": digest,
                "kind": record["kind"],
                "name": record["name"],
                "qualified": digest in qualified,
                "version": record["version"],
            }
        )
    return {"packages": packages, "schema": PACKAGE_SCHEMA}


def show_package(store: CapacityStore, reference: str) -> dict[str, Any]:
    digest, used_alias = store.resolve_ref(reference)
    record = store.load_package(digest)
    return {
        "alias_is_authority": False,
        "record": record,
        "requested_ref": reference,
        "resolved_digest": digest,
        "resolved_through_alias": used_alias,
    }


def verify_packages(store: CapacityStore, references: Sequence[str]) -> dict[str, Any]:
    if references:
        digests = sorted({store.resolve_ref(reference)[0] for reference in references})
    else:
        digests = store.installed_digests()
    results = []
    for digest in digests:
        record = store.load_package(digest, verify_blobs=True)
        results.append(
            {"artifacts": len(record["artifacts"]), "digest": digest, "verified": True}
        )
    head = store.read_head()
    for generation_digest in (head["current"], head["previous"]):
        if generation_digest is not None:
            store.load_generation(generation_digest)
    return {"generations_verified": sum(item is not None for item in (head["current"], head["previous"])), "packages": results}


def gc_dry_run(store: CapacityStore) -> dict[str, Any]:
    installed = store.installed_digests()
    reachable_blobs: set[str] = set()
    for digest in installed:
        record = store.load_package(digest)
        reachable_blobs.update(artifact["sha256"] for artifact in record["artifacts"])
    blob_candidates: list[str] = []
    for path in sorted(store.blobs.iterdir()):
        if path.name.startswith("."):
            continue
        sha = _digest_hex(path.name, f"blob-store entry {path.name!r}")
        if sha not in reachable_blobs:
            blob_candidates.append(sha)
    head = store.read_head()
    retained_generations = {item for item in (head["current"], head["previous"]) if item}
    generation_candidates: list[str] = []
    for path in sorted(store.generations.iterdir()):
        if path.name.startswith("."):
            continue
        match = re.fullmatch(r"([0-9a-f]{64})\.json", path.name)
        if match is None:
            raise CapacityError(f"unexpected generation-store entry: {path}")
        digest = "sha256:" + match.group(1)
        if digest not in retained_generations:
            generation_candidates.append(digest)
    return {
        "blob_candidates": blob_candidates,
        "destructive": False,
        "dry_run": True,
        "generation_candidates": generation_candidates,
        "installed_packages_retained": installed,
        "note": "v1 never deletes objects",
    }


def inspect_catalog_package(catalog: Catalog, package_id: str) -> dict[str, Any]:
    package_id = _token(package_id, "catalog package id")
    packages = catalog.by_id
    if package_id not in packages:
        raise CapacityError(f"catalog contains no package {package_id!r}")
    records = catalog_records(catalog)
    closure = _catalog_closure(catalog, package_id)
    spec = packages[package_id]
    return {
        "aliases": list(spec.aliases),
        "artifacts": [
            {
                **artifact.identity_record(),
                "integrity": artifact.integrity,
                "source": artifact.source,
            }
            for artifact in spec.artifacts
        ],
        "closure": [records[item]["digest"] for item in closure],
        "description": spec.description,
        "package": records[package_id],
        "qualification_claimed": False,
    }


def _load_json_file(path: Path, maximum: int) -> Any:
    payload = _read_regular_bounded(path, maximum)
    try:
        return json.loads(payload.decode("ascii", "strict"))
    except (UnicodeError, json.JSONDecodeError) as error:
        raise CapacityError(f"cannot parse JSON file {path}: {error}") from error


def _parse_source_overrides(values: Sequence[str]) -> dict[str, str]:
    result: dict[str, str] = {}
    for value in values:
        if "=" not in value:
            raise CapacityError("--source must be PACKAGE/ARTIFACT=SOURCE or ARTIFACT=SOURCE")
        key, source = value.split("=", 1)
        key = _token(key, "source override key")
        if key in result:
            raise CapacityError(f"duplicate source override: {key!r}")
        result[key] = _parse_source(source, f"source override {key!r}")
    return result


def _emit(value: Any) -> None:
    print(json.dumps(value, ensure_ascii=True, allow_nan=False, indent=2, sort_keys=True))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Install and activate exact OS, kernel, userspace, firmware, and bundle capacity"
    )
    parser.add_argument("--state", type=Path, default=DEFAULT_STATE)
    parser.add_argument("--catalog", type=Path, default=DEFAULT_CATALOG)
    parser.add_argument("--network-timeout", type=float, default=30.0)
    commands = parser.add_subparsers(dest="command", required=True)

    inspect_parser = commands.add_parser("inspect", help="strictly inspect one catalog package")
    inspect_parser.add_argument("package")

    install_parser = commands.add_parser("install", help="install one catalog package and its exact closure")
    install_parser.add_argument("package")
    install_parser.add_argument("--source", action="append", default=[])
    install_parser.add_argument("--alias", action="append", default=[])
    install_parser.add_argument("--replace-alias", action="store_true")

    commands.add_parser("list", help="list installed, active, and qualified states separately")
    show_parser = commands.add_parser("show", help="show an installed exact package record")
    show_parser.add_argument("reference")
    verify_parser = commands.add_parser("verify", help="rehash installed package blobs and state")
    verify_parser.add_argument("references", nargs="*")
    commands.add_parser("status", help="show the revisioned activation head")

    plan_parser = commands.add_parser("plan", help="create an exact revision-bound activation plan")
    plan_parser.add_argument("references", nargs="+")
    plan_parser.add_argument("--accept-license", action="append", default=[])
    plan_parser.add_argument("--output", type=Path)

    apply_parser = commands.add_parser("apply", help="CAS-apply an exact activation plan")
    apply_parser.add_argument("plan", type=Path)
    commands.add_parser("rollback", help="swap current and retained previous generations")

    gc_parser = commands.add_parser("gc", help="report unreachable objects without deleting")
    gc_parser.add_argument("--dry-run", action="store_true", required=True)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.network_timeout <= 0 or args.network_timeout > 3600:
            raise CapacityError("--network-timeout must be within (0, 3600] seconds")
        if args.command == "inspect":
            _emit(inspect_catalog_package(load_catalog(args.catalog), args.package))
            return 0
        store = CapacityStore(args.state)
        if args.command == "install":
            result = install_catalog_package(
                store,
                load_catalog(args.catalog),
                args.package,
                source_overrides=_parse_source_overrides(args.source),
                extra_aliases=args.alias,
                replace_aliases=args.replace_alias,
                network_timeout=args.network_timeout,
            )
        elif args.command == "list":
            result = list_packages(store)
        elif args.command == "show":
            result = show_package(store, args.reference)
        elif args.command == "verify":
            result = verify_packages(store, args.references)
        elif args.command == "status":
            result = status(store)
        elif args.command == "plan":
            result = create_plan(
                store,
                args.references,
                accepted_license_refs=args.accept_license,
            )
            if args.output is not None:
                _atomic_replace_bytes(
                    args.output.expanduser().absolute(), _canonical_json(result) + b"\n"
                )
                result = {"digest": result["digest"], "output": str(args.output)}
        elif args.command == "apply":
            result = apply_plan(store, _load_json_file(args.plan.expanduser().absolute(), MAX_PLAN_BYTES))
        elif args.command == "rollback":
            result = rollback(store)
        elif args.command == "gc":
            result = gc_dry_run(store)
        else:  # pragma: no cover - argparse owns this invariant.
            raise CapacityError(f"unhandled command: {args.command}")
        _emit(result)
        return 0
    except CapacityError as error:
        print(f"ostadix-capacity: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
