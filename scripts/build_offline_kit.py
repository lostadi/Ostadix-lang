#!/usr/bin/env python3
"""Build, verify, and unpack deterministic per-host Ostadix offline kits.

The ordinary Ostadix source release deliberately contains source rather than a
native Rust toolchain.  This companion format seals three independently
auditable inputs into one host-labelled ZIP:

* the allowlisted source tree selected by ``build_source_release.py``;
* one caller-supplied Rust sysroot, including Cargo and ``wasm32-wasip1`` std;
* one caller-supplied Cargo directory source (normally produced by
  ``cargo vendor --versioned-dirs`` for every supported manifest).

The kit is deliberately not universal.  Extraction fails before writing when
the current POSIX host does not match the compiler host recorded in the
canonical manifest.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
from typing import BinaryIO, Iterable, Sequence
import zipfile

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10 can still verify and extract a kit.
    tomllib = None  # type: ignore[assignment]

SCHEMA = "ostadix-offline-ai-build-kit/v1"
MANIFEST_NAME = "OFFLINE-KIT-MANIFEST.json"
CHECKSUMS_NAME = "SHA256SUMS"
TOOLCHAIN_PAYLOAD = "payloads/rust-toolchain.tar.gz"
VENDOR_PAYLOAD = "payloads/cargo-vendor.tar.gz"
BOOTSTRAP_NAME = "bootstrap-offline.sh"
SOURCE_BOOTSTRAP_NAME = "source/scripts/bootstrap_offline_kit.sh"
FIXED_ZIP_TIMESTAMP = (1980, 1, 1, 0, 0, 0)
HEX_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
HEX_COMMIT = re.compile(r"[0-9a-f]{40,64}\Z")
SAFE_PREFIX = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")
RUST_RELEASE = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?\Z")
CRATES_IO_REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
CARGO_CHECKSUM_REQUIRED_KEYS = frozenset({"files", "package"})
CARGO_CHECKSUM_OPTIONAL_KEYS = frozenset({"$comment"})

# These are the POSIX hosts for which the checked-in bootstrap has an explicit
# uname mapping.  A target being listed here does not assert that its system
# linker, SDK, Python, or hosted backend runtimes are bundled.
SUPPORTED_POSIX_HOSTS = frozenset(
    {
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
    }
)

PROFILE_COMMANDS: dict[str, list[list[str]]] = {
    "check": [
        ["cargo", "metadata", "--frozen", "--locked", "--format-version", "1"],
        [
            "cargo",
            "metadata",
            "--frozen",
            "--locked",
            "--format-version",
            "1",
            "--manifest-path",
            "mcp/ostadix_lang_mcp_server/Cargo.toml",
        ],
        [
            "cargo",
            "metadata",
            "--frozen",
            "--locked",
            "--format-version",
            "1",
            "--manifest-path",
            "apps/android-terminal/runtime/Cargo.toml",
        ],
        [
            "cargo",
            "metadata",
            "--frozen",
            "--locked",
            "--format-version",
            "1",
            "--manifest-path",
            "fuzz/Cargo.toml",
        ],
    ],
    "hosted-rust": [
        [
            "cargo",
            "build",
            "--frozen",
            "--release",
            "--locked",
            "--package",
            "o-lang",
            "--all-features",
            "--bins",
        ]
    ],
    "mcp": [
        [
            "cargo",
            "build",
            "--frozen",
            "--release",
            "--locked",
            "--manifest-path",
            "mcp/ostadix_lang_mcp_server/Cargo.toml",
        ]
    ],
    "all-supported": [
        [
            "cargo",
            "build",
            "--frozen",
            "--release",
            "--locked",
            "--package",
            "o-lang",
            "--all-features",
            "--bins",
        ],
        [
            "cargo",
            "build",
            "--frozen",
            "--release",
            "--locked",
            "--manifest-path",
            "mcp/ostadix_lang_mcp_server/Cargo.toml",
        ],
    ],
    "wasm-std-check": [
        ["rustc", "--print", "target-libdir", "--target", "wasm32-wasip1"]
    ],
}

NONCLAIMS = (
    "The kit is valid only for the exact host triple in its manifest; it is not a universal or cross-host Rust installation.",
    "The kit does not bundle a platform linker, macOS SDK, libc development files, Python, or arbitrary hosted-language runtimes.",
    "The named build profiles cover the hosted Rust binaries and the separate MCP crate only; they do not build Android APKs, fuzz targets, O-core/QEMU media, the C17 edition, or every repository test.",
    "Bundled wasm32-wasip1 standard-library files establish target availability only; they do not by themselves prove browser execution or wasm32-unknown-unknown compatibility.",
    "The builder preserves Rust and vendored-crate license files but does not perform a legal compatibility review of third-party dependencies.",
)


class OfflineKitError(RuntimeError):
    """An offline kit could not be built, verified, or unpacked safely."""


def _toml_loads(data: str, subject: str) -> dict[str, object]:
    if tomllib is None:
        raise OfflineKitError(
            f"building an offline kit requires Python 3.11+ to parse {subject}; "
            "Python 3.10 remains supported for recipient verification/extraction"
        )
    try:
        return tomllib.loads(data)
    except tomllib.TOMLDecodeError as error:
        raise OfflineKitError(f"cannot parse {subject}: {error}") from error


def _load_source_release_module():
    """Import build-only release logic lazily so Python 3.10 can extract."""

    try:
        from scripts import build_source_release
    except ImportError:  # Direct ``python3 scripts/build_offline_kit.py``.
        import build_source_release  # type: ignore[no-redef]
    return build_source_release


@dataclass(frozen=True)
class BytesEntry:
    path: str
    mode: str
    data: bytes

    @property
    def size(self) -> int:
        return len(self.data)

    @property
    def sha256(self) -> str:
        return hashlib.sha256(self.data).hexdigest()


@dataclass(frozen=True)
class DiskEntry:
    path: str
    mode: str
    source: Path
    size: int


@dataclass(frozen=True)
class ToolchainIdentity:
    host: str
    release: str
    commit_hash: str
    rustc_verbose: str
    cargo_version: str


@dataclass(frozen=True)
class BuildResult:
    output: Path
    prefix: str
    commit: str
    host: str
    archive_sha256: str


def _canonical_json(value: object) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
        + "\n"
    ).encode("ascii")


def _sha256_stream(stream: BinaryIO) -> str:
    digest = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        digest.update(chunk)
    return digest.hexdigest()


def _sha256_path(path: Path) -> str:
    with path.open("rb") as source:
        return _sha256_stream(source)


def _safe_relative(path: str) -> PurePosixPath:
    if not path or "\x00" in path or "\r" in path or "\n" in path:
        raise OfflineKitError(f"unsafe kit path: {path!r}")
    pure = PurePosixPath(path)
    if pure.is_absolute() or any(part in {"", ".", ".."} for part in pure.parts):
        raise OfflineKitError(f"unsafe kit path: {path!r}")
    if pure.as_posix() != path:
        raise OfflineKitError(f"non-canonical kit path: {path!r}")
    return pure


def _canonical_mode(raw_mode: int) -> str:
    return "100755" if raw_mode & 0o111 else "100644"


def _snapshot_regular_tree(source: Path, destination: Path) -> None:
    """Copy one tree through no-follow descriptors, rejecting concurrent drift."""

    supplied = source.expanduser()
    directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    cloexec = getattr(os, "O_CLOEXEC", 0)
    try:
        root_fd = os.open(supplied, directory_flags | nofollow | cloexec)
    except OSError as error:
        raise OfflineKitError(f"cannot open payload root {supplied}: {error}") from error
    destination.mkdir(mode=0o700, parents=True, exist_ok=False)

    def same_object(left: os.stat_result, right: os.stat_result) -> bool:
        return (
            left.st_dev,
            left.st_ino,
            stat.S_IFMT(left.st_mode),
        ) == (
            right.st_dev,
            right.st_ino,
            stat.S_IFMT(right.st_mode),
        )

    def visit(directory_fd: int, relative: PurePosixPath | None) -> None:
        before = os.fstat(directory_fd)
        if not stat.S_ISDIR(before.st_mode):
            raise OfflineKitError("payload traversal encountered a non-directory")
        try:
            names = sorted(os.listdir(directory_fd), key=os.fsencode)
        except OSError as error:
            raise OfflineKitError(f"cannot enumerate payload directory: {error}") from error
        for name in names:
            child_relative = PurePosixPath(name) if relative is None else relative / name
            relative_text = child_relative.as_posix()
            _safe_relative(relative_text)
            try:
                metadata = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
            except OSError as error:
                raise OfflineKitError(f"cannot stat payload path {relative_text}: {error}") from error
            if stat.S_ISLNK(metadata.st_mode):
                raise OfflineKitError(f"payload symlinks are forbidden: {relative_text}")
            target = destination.joinpath(*child_relative.parts)
            if stat.S_ISDIR(metadata.st_mode):
                try:
                    child_fd = os.open(
                        name,
                        directory_flags | nofollow | cloexec,
                        dir_fd=directory_fd,
                    )
                except OSError as error:
                    raise OfflineKitError(
                        f"cannot open payload directory {relative_text}: {error}"
                    ) from error
                try:
                    if not same_object(metadata, os.fstat(child_fd)):
                        raise OfflineKitError(
                            f"payload directory changed during snapshot: {relative_text}"
                        )
                    target.mkdir(mode=0o700)
                    visit(child_fd, child_relative)
                finally:
                    os.close(child_fd)
                continue
            if not stat.S_ISREG(metadata.st_mode):
                raise OfflineKitError(f"payload contains a special file: {relative_text}")
            try:
                file_fd = os.open(
                    name,
                    os.O_RDONLY | nofollow | cloexec,
                    dir_fd=directory_fd,
                )
            except OSError as error:
                raise OfflineKitError(f"cannot open payload file {relative_text}: {error}") from error
            try:
                opened = os.fstat(file_fd)
                if not same_object(metadata, opened) or opened.st_size != metadata.st_size:
                    raise OfflineKitError(
                        f"payload file changed before snapshot: {relative_text}"
                    )
                target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                with os.fdopen(file_fd, "rb", closefd=False) as input_file, target.open(
                    "xb"
                ) as output_file:
                    shutil.copyfileobj(input_file, output_file, length=1024 * 1024)
                after = os.fstat(file_fd)
                stable_fields = (
                    "st_dev",
                    "st_ino",
                    "st_size",
                    "st_mtime_ns",
                    "st_ctime_ns",
                )
                if any(getattr(opened, field) != getattr(after, field) for field in stable_fields):
                    raise OfflineKitError(
                        f"payload file changed during snapshot: {relative_text}"
                    )
                target.chmod(int(_canonical_mode(metadata.st_mode)[-3:], 8))
            finally:
                os.close(file_fd)
        after_names = sorted(os.listdir(directory_fd), key=os.fsencode)
        after = os.fstat(directory_fd)
        if names != after_names or any(
            getattr(before, field) != getattr(after, field)
            for field in ("st_dev", "st_ino", "st_mtime_ns", "st_ctime_ns")
        ):
            raise OfflineKitError("payload directory changed during snapshot")

    try:
        visit(root_fd, None)
    except Exception:
        shutil.rmtree(destination, ignore_errors=True)
        raise
    finally:
        os.close(root_fd)


def _walk_regular_files(root: Path, archive_prefix: str) -> list[DiskEntry]:
    supplied_root = root.expanduser()
    try:
        supplied_metadata = supplied_root.lstat()
    except OSError as error:
        raise OfflineKitError(f"cannot stat payload root {supplied_root}: {error}") from error
    if stat.S_ISLNK(supplied_metadata.st_mode):
        raise OfflineKitError(f"payload root symlinks are forbidden: {supplied_root}")
    root = supplied_root.resolve(strict=True)
    if not stat.S_ISDIR(supplied_metadata.st_mode):
        raise OfflineKitError(f"payload root is not a real directory: {root}")
    _safe_relative(archive_prefix)
    entries: list[DiskEntry] = []

    def visit(directory: Path, relative: PurePosixPath | None) -> None:
        try:
            children = sorted(
                os.scandir(directory), key=lambda entry: os.fsencode(entry.name)
            )
        except OSError as error:
            raise OfflineKitError(f"cannot enumerate payload directory {directory}: {error}") from error
        for child in children:
            child_relative = (
                PurePosixPath(child.name)
                if relative is None
                else relative / child.name
            )
            relative_text = child_relative.as_posix()
            _safe_relative(relative_text)
            try:
                metadata = child.stat(follow_symlinks=False)
            except OSError as error:
                raise OfflineKitError(f"cannot stat payload path {child.path}: {error}") from error
            if stat.S_ISLNK(metadata.st_mode):
                raise OfflineKitError(f"payload symlinks are forbidden: {child.path}")
            if stat.S_ISDIR(metadata.st_mode):
                visit(Path(child.path), child_relative)
                continue
            if not stat.S_ISREG(metadata.st_mode):
                raise OfflineKitError(f"payload contains a special file: {child.path}")
            archive_path = f"{archive_prefix}/{relative_text}"
            _safe_relative(archive_path)
            entries.append(
                DiskEntry(
                    path=archive_path,
                    mode=_canonical_mode(metadata.st_mode),
                    source=Path(child.path),
                    size=metadata.st_size,
                )
            )

    visit(root, None)
    if not entries:
        raise OfflineKitError(f"payload directory is empty: {root}")
    return entries


def _tree_seal(entries: Sequence[DiskEntry]) -> str:
    records: list[dict[str, object]] = []
    for entry in sorted(entries, key=lambda item: item.path.encode("utf-8")):
        records.append(
            {
                "mode": entry.mode,
                "path": entry.path,
                "sha256": _sha256_path(entry.source),
                "size": entry.size,
            }
        )
    return hashlib.sha256(_canonical_json(records)).hexdigest()


def _write_deterministic_tar_gz(entries: Sequence[DiskEntry], output: Path) -> None:
    ordered = sorted(entries, key=lambda entry: entry.path.encode("utf-8"))
    with output.open("wb") as raw:
        with gzip.GzipFile(
            filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0
        ) as compressed:
            with tarfile.open(
                fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT
            ) as archive:
                for entry in ordered:
                    info = tarfile.TarInfo(entry.path)
                    info.size = entry.size
                    info.mode = int(entry.mode[-3:], 8)
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    info.mtime = 0
                    info.type = tarfile.REGTYPE
                    with entry.source.open("rb") as payload:
                        archive.addfile(info, payload)


def _validate_tar(path: Path, required_prefix: str) -> list[tarfile.TarInfo]:
    _safe_relative(required_prefix)
    try:
        with path.open("rb") as payload:
            gzip_header = payload.read(10)
    except OSError as error:
        raise OfflineKitError(f"cannot read {required_prefix} payload: {error}") from error
    if gzip_header != b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff":
        raise OfflineKitError(
            f"non-canonical gzip metadata for {required_prefix} payload"
        )
    try:
        with tarfile.open(path, "r:gz") as archive:
            members = archive.getmembers()
    except (OSError, tarfile.TarError) as error:
        raise OfflineKitError(f"invalid {required_prefix} payload: {error}") from error
    if not members:
        raise OfflineKitError(f"{required_prefix} payload is empty")
    names: list[str] = []
    for member in members:
        pure = _safe_relative(member.name)
        if len(pure.parts) < 2 or pure.parts[0] != required_prefix:
            raise OfflineKitError(
                f"tar member is not below required {required_prefix}/ prefix: {member.name}"
            )
        if not member.isfile():
            raise OfflineKitError(
                f"tar links, directories, and special members are forbidden: {member.name}"
            )
        if (
            member.uid != 0
            or member.gid != 0
            or member.uname != ""
            or member.gname != ""
            or member.mtime != 0
            or member.pax_headers
        ):
            raise OfflineKitError(f"non-canonical tar metadata: {member.name}")
        if member.mode not in {0o644, 0o755}:
            raise OfflineKitError(f"non-canonical tar mode: {member.name}")
        names.append(member.name)
    if names != sorted(set(names), key=lambda name: name.encode("utf-8")):
        raise OfflineKitError("tar member paths are not uniquely byte-sorted")
    name_set = set(names)
    for name in names:
        parents = PurePosixPath(name).parents
        if any(parent.as_posix() in name_set for parent in parents):
            raise OfflineKitError(f"tar file/directory path conflict: {name}")
    return members


def _tar_tree_summary(path: Path, required_prefix: str) -> tuple[int, str, int]:
    """Return file count, canonical tree seal, and immediate child count."""

    _validate_tar(path, required_prefix)
    records: list[dict[str, object]] = []
    children: set[str] = set()
    with tarfile.open(path, "r:gz") as archive:
        for member in archive.getmembers():
            parts = PurePosixPath(member.name).parts
            if len(parts) >= 2:
                children.add(parts[1])
            payload = archive.extractfile(member)
            if payload is None:
                raise OfflineKitError(f"cannot read tar member {member.name}")
            with payload:
                digest = _sha256_stream(payload)
            records.append(
                {
                    "mode": _canonical_mode(member.mode),
                    "path": member.name,
                    "sha256": digest,
                    "size": member.size,
                }
            )
    records.sort(key=lambda record: str(record["path"]).encode("utf-8"))
    return len(records), hashlib.sha256(_canonical_json(records)).hexdigest(), len(children)


def _run_identity_command(command: Sequence[str], label: str) -> str:
    try:
        completed = subprocess.run(
            list(command),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise OfflineKitError(f"cannot execute caller-supplied {label}: {error}") from error
    if completed.returncode != 0:
        detail = completed.stderr.strip() or f"exit {completed.returncode}"
        raise OfflineKitError(f"caller-supplied {label} failed: {detail}")
    output = completed.stdout.strip()
    if not output:
        raise OfflineKitError(f"caller-supplied {label} returned no identity")
    return output


def inspect_toolchain(sysroot: Path) -> ToolchainIdentity:
    supplied_sysroot = sysroot.expanduser()
    try:
        supplied_metadata = supplied_sysroot.lstat()
    except OSError as error:
        raise OfflineKitError(f"cannot stat Rust sysroot {supplied_sysroot}: {error}") from error
    if stat.S_ISLNK(supplied_metadata.st_mode):
        raise OfflineKitError(f"Rust sysroot symlinks are forbidden: {supplied_sysroot}")
    sysroot = supplied_sysroot.resolve(strict=True)
    rustc = sysroot / "bin" / "rustc"
    cargo = sysroot / "bin" / "cargo"
    for label, executable in (("rustc", rustc), ("cargo", cargo)):
        try:
            metadata = executable.lstat()
        except OSError as error:
            raise OfflineKitError(f"toolchain lacks {label}: {executable}") from error
        if not stat.S_ISREG(metadata.st_mode) or not metadata.st_mode & 0o111:
            raise OfflineKitError(f"toolchain {label} is not an executable regular file")

    rustc_verbose = _run_identity_command([os.fspath(rustc), "-vV"], "rustc -vV")
    fields: dict[str, str] = {}
    for line in rustc_verbose.splitlines()[1:]:
        if ": " in line:
            key, value = line.split(": ", 1)
            if key in fields:
                raise OfflineKitError(f"rustc -vV repeats identity field {key!r}")
            fields[key] = value
    host = fields.get("host", "")
    release = fields.get("release", "")
    commit_hash = fields.get("commit-hash", "")
    if host not in SUPPORTED_POSIX_HOSTS:
        raise OfflineKitError(f"unsupported POSIX Rust host triple: {host!r}")
    if not release or not commit_hash:
        raise OfflineKitError("rustc -vV lacks release or commit-hash identity")
    cargo_version = _run_identity_command([os.fspath(cargo), "--version"], "cargo --version")
    cargo_match = re.fullmatch(r"cargo ([^ ]+)(?: .*)?", cargo_version)
    if cargo_match is None or cargo_match.group(1) != release:
        raise OfflineKitError(
            f"Cargo/Rust release mismatch: cargo={cargo_version!r}, rustc={release!r}"
        )

    wasm_lib = sysroot / "lib" / "rustlib" / "wasm32-wasip1" / "lib"
    if not wasm_lib.is_dir() or not any(
        child.is_file() and child.name.startswith("libstd-") and child.suffix == ".rlib"
        for child in wasm_lib.iterdir()
    ):
        raise OfflineKitError(
            "toolchain lacks the wasm32-wasip1 Rust standard library (libstd-*.rlib)"
        )
    required_legal = (
        sysroot / "share" / "doc" / "cargo" / "LICENSE-APACHE",
        sysroot / "share" / "doc" / "cargo" / "LICENSE-MIT",
        sysroot / "share" / "doc" / "rust" / "COPYRIGHT.html",
    )
    missing_legal = [os.fspath(path.relative_to(sysroot)) for path in required_legal if not path.is_file()]
    if missing_legal:
        raise OfflineKitError(
            "toolchain omits required redistribution notices: " + ", ".join(missing_legal)
        )
    return ToolchainIdentity(
        host=host,
        release=release,
        commit_hash=commit_hash,
        rustc_verbose=rustc_verbose,
        cargo_version=cargo_version,
    )


RELEASED_CARGO_LOCKS = (
    "source/Cargo.lock",
    "source/fuzz/Cargo.lock",
    "source/mcp/ostadix_lang_mcp_server/Cargo.lock",
    "source/apps/android-terminal/runtime/Cargo.lock",
)


def _locked_registry_packages(
    source_entries: Sequence[BytesEntry],
) -> dict[tuple[str, str], str]:
    source_by_path = {entry.path: entry.data for entry in source_entries}
    packages: dict[tuple[str, str], str] = {}
    for lock_path in RELEASED_CARGO_LOCKS:
        data = source_by_path.get(lock_path)
        if data is None:
            raise OfflineKitError(f"source closure lacks released lockfile {lock_path}")
        try:
            lock_text = data.decode("utf-8", "strict")
        except UnicodeDecodeError as error:
            raise OfflineKitError(f"cannot parse released lockfile {lock_path}: {error}") from error
        lock = _toml_loads(lock_text, f"released lockfile {lock_path}")
        raw_packages = lock.get("package")
        if not isinstance(raw_packages, list):
            raise OfflineKitError(f"released lockfile lacks package array: {lock_path}")
        for package in raw_packages:
            if not isinstance(package, dict):
                raise OfflineKitError(f"malformed package in {lock_path}")
            source = package.get("source")
            if not isinstance(source, str) or not source.startswith("registry+"):
                continue
            if source != CRATES_IO_REGISTRY:
                raise OfflineKitError(
                    f"unsupported registry source in {lock_path}: {source}"
                )
            name = package.get("name")
            version = package.get("version")
            checksum = package.get("checksum")
            if (
                not isinstance(name, str)
                or not isinstance(version, str)
                or not isinstance(checksum, str)
                or not HEX_SHA256.fullmatch(checksum)
            ):
                raise OfflineKitError(f"malformed registry package in {lock_path}")
            key = (name, version)
            previous = packages.get(key)
            if previous is not None and previous != checksum:
                raise OfflineKitError(
                    f"released locks disagree on checksum for {name} {version}"
                )
            packages[key] = checksum
    if not packages:
        raise OfflineKitError("released lockfiles contain no registry packages")
    return packages


def _validate_toolchain_source_contract(
    source_entries: Sequence[BytesEntry], identity: ToolchainIdentity
) -> None:
    source_by_path = {entry.path: entry.data for entry in source_entries}
    path = "source/rust-toolchain.toml"
    data = source_by_path.get(path)
    if data is None:
        raise OfflineKitError(f"source closure lacks {path}")
    try:
        toolchain_text = data.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise OfflineKitError(f"cannot parse {path}: {error}") from error
    document = _toml_loads(toolchain_text, path)
    toolchain = document.get("toolchain")
    if not isinstance(toolchain, dict) or toolchain.get("channel") != identity.release:
        raise OfflineKitError(
            f"source rust-toolchain channel must equal bundled rustc release {identity.release}"
        )
    components = toolchain.get("components")
    if not isinstance(components, list) or not all(
        isinstance(component, str) for component in components
    ):
        raise OfflineKitError(f"{path} has an invalid components list")
    targets = toolchain.get("targets")
    if not isinstance(targets, list) or "wasm32-wasip1" not in targets:
        raise OfflineKitError(
            f"{path} must pin the bundled wasm32-wasip1 standard library"
        )


def validate_vendor_tree(
    entries: Sequence[DiskEntry], source_entries: Sequence[BytesEntry]
) -> int:
    locked = _locked_registry_packages(source_entries)
    entries_by_path = {entry.path: entry for entry in entries}
    malformed_top_level = [
        entry.path
        for entry in entries
        if len(PurePosixPath(entry.path).parts) < 3
    ]
    if malformed_top_level:
        raise OfflineKitError(
            "vendor directory source contains top-level files: "
            + ", ".join(sorted(malformed_top_level))
        )
    actual_roots = {
        PurePosixPath(entry.path).parts[1]
        for entry in entries
        if len(PurePosixPath(entry.path).parts) >= 3
    }
    expected_roots = {f"{name}-{version}" for name, version in locked}
    if actual_roots != expected_roots:
        missing = sorted(expected_roots - actual_roots)
        extra = sorted(actual_roots - expected_roots)
        raise OfflineKitError(
            f"vendor crate closure differs from released locks; missing={missing}, extra={extra}"
        )
    for (name, version), package_checksum in sorted(locked.items()):
        root = f"vendor/{name}-{version}"
        checksum_path = f"{root}/.cargo-checksum.json"
        checksum_entry = entries_by_path.get(checksum_path)
        if checksum_entry is None:
            raise OfflineKitError(f"vendor crate lacks {checksum_path}")
        try:
            checksum_document = json.loads(
                checksum_entry.source.read_text(encoding="utf-8")
            )
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
            raise OfflineKitError(f"invalid {checksum_path}: {error}") from error
        if not isinstance(checksum_document, dict):
            raise OfflineKitError(f"malformed {checksum_path}")
        checksum_keys = set(checksum_document)
        if (
            not CARGO_CHECKSUM_REQUIRED_KEYS.issubset(checksum_keys)
            or checksum_keys
            - CARGO_CHECKSUM_REQUIRED_KEYS
            - CARGO_CHECKSUM_OPTIONAL_KEYS
        ):
            raise OfflineKitError(f"malformed {checksum_path}")
        comment = checksum_document.get("$comment")
        if "$comment" in checksum_document and not isinstance(comment, str):
            raise OfflineKitError(f"malformed {checksum_path}")
        if checksum_document["package"] != package_checksum:
            raise OfflineKitError(f"package checksum mismatch in {checksum_path}")
        declared_files = checksum_document["files"]
        if not isinstance(declared_files, dict):
            raise OfflineKitError(f"files map is malformed in {checksum_path}")
        actual_files = {
            entry.path.removeprefix(f"{root}/"): entry
            for entry in entries
            if entry.path.startswith(f"{root}/")
            and entry.path != checksum_path
        }
        if set(declared_files) != set(actual_files):
            raise OfflineKitError(f"file closure mismatch in {checksum_path}")
        for relative, digest in declared_files.items():
            if not isinstance(relative, str) or not isinstance(digest, str):
                raise OfflineKitError(f"invalid file checksum record in {checksum_path}")
            if not HEX_SHA256.fullmatch(digest):
                raise OfflineKitError(f"invalid file digest in {checksum_path}: {relative}")
            if _sha256_path(actual_files[relative].source) != digest:
                raise OfflineKitError(f"vendored file checksum mismatch: {root}/{relative}")
    return len(locked)


def _zip_info(name: str, mode: str, compression: int) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name, FIXED_ZIP_TIMESTAMP)
    info.compress_type = compression
    info.create_system = 3
    info.external_attr = int(mode, 8) << 16
    return info


def _write_zip_bytes(
    archive: zipfile.ZipFile, prefix: str, entry: BytesEntry, compression: int
) -> None:
    archive.writestr(
        _zip_info(f"{prefix}/{entry.path}", entry.mode, compression),
        entry.data,
        compress_type=compression,
        compresslevel=9 if compression == zipfile.ZIP_DEFLATED else None,
    )


def _write_zip_path(
    archive: zipfile.ZipFile, prefix: str, entry: DiskEntry, compression: int
) -> None:
    info = _zip_info(f"{prefix}/{entry.path}", entry.mode, compression)
    with archive.open(info, "w", force_zip64=True) as destination, entry.source.open(
        "rb"
    ) as source:
        shutil.copyfileobj(source, destination, length=1024 * 1024)


def _file_record(path: str, mode: str, size: int, digest: str) -> dict[str, object]:
    return {"mode": mode, "path": path, "sha256": digest, "size": size}


def _source_bytes_entries(
    entries: Iterable[object],
) -> list[BytesEntry]:
    return [
        BytesEntry(path=f"source/{entry.path}", mode=entry.mode, data=entry.data)
        for entry in entries
    ]


def build_kit_from_entries(
    source_entries: Sequence[BytesEntry],
    *,
    commit: str,
    toolchain: Path,
    vendor: Path,
    output: Path,
    prefix: str | None = None,
) -> BuildResult:
    with tempfile.TemporaryDirectory(prefix="ostadix-offline-inputs-") as temporary_name:
        temporary = Path(temporary_name)
        toolchain_snapshot = temporary / "toolchain"
        vendor_snapshot = temporary / "vendor"
        _snapshot_regular_tree(toolchain, toolchain_snapshot)
        _snapshot_regular_tree(vendor, vendor_snapshot)
        return _build_kit_from_snapshots(
            source_entries,
            commit=commit,
            toolchain=toolchain_snapshot,
            vendor=vendor_snapshot,
            output=output,
            prefix=prefix,
        )


def _build_kit_from_snapshots(
    source_entries: Sequence[BytesEntry],
    *,
    commit: str,
    toolchain: Path,
    vendor: Path,
    output: Path,
    prefix: str | None = None,
) -> BuildResult:
    if not HEX_COMMIT.fullmatch(commit):
        raise OfflineKitError(f"invalid source commit: {commit!r}")
    identity = inspect_toolchain(toolchain)
    kit_prefix = prefix or f"Ostadix-lang-offline-{commit[:12]}-{identity.host}"
    if not SAFE_PREFIX.fullmatch(kit_prefix):
        raise OfflineKitError(f"unsafe kit prefix: {kit_prefix!r}")

    ordered_source = sorted(source_entries, key=lambda entry: entry.path.encode("utf-8"))
    source_paths = [entry.path for entry in ordered_source]
    if source_paths != sorted(set(source_paths), key=lambda path: path.encode("utf-8")):
        raise OfflineKitError("source entry paths are not uniquely byte-sorted")
    for entry in ordered_source:
        pure = _safe_relative(entry.path)
        if pure.parts[0] != "source" or entry.mode not in {"100644", "100755"}:
            raise OfflineKitError(f"invalid source entry: {entry.path}")
    selected_bootstraps = [
        entry for entry in ordered_source if entry.path == SOURCE_BOOTSTRAP_NAME
    ]
    if len(selected_bootstraps) != 1:
        raise OfflineKitError(
            f"selected source must contain exactly one {SOURCE_BOOTSTRAP_NAME}"
        )
    selected_bootstrap = selected_bootstraps[0]
    if selected_bootstrap.mode != "100755":
        raise OfflineKitError("offline bootstrap must be executable in the selected source")
    bootstrap_data = selected_bootstrap.data
    if not bootstrap_data.startswith(b"#!/bin/sh\n"):
        raise OfflineKitError("offline bootstrap must be a POSIX /bin/sh script")
    _validate_toolchain_source_contract(ordered_source, identity)

    with tempfile.TemporaryDirectory(prefix="ostadix-offline-kit-") as temporary_name:
        temporary = Path(temporary_name)
        toolchain_payload = temporary / "rust-toolchain.tar.gz"
        vendor_payload = temporary / "cargo-vendor.tar.gz"
        toolchain_entries = _walk_regular_files(toolchain, "toolchain")
        vendor_entries = _walk_regular_files(vendor, "vendor")
        vendor_crates = validate_vendor_tree(vendor_entries, ordered_source)
        toolchain_tree_sha256 = _tree_seal(toolchain_entries)
        vendor_tree_sha256 = _tree_seal(vendor_entries)
        _write_deterministic_tar_gz(toolchain_entries, toolchain_payload)
        _write_deterministic_tar_gz(vendor_entries, vendor_payload)
        _validate_tar(toolchain_payload, "toolchain")
        _validate_tar(vendor_payload, "vendor")

        bootstrap_entry = BytesEntry(BOOTSTRAP_NAME, "100755", bootstrap_data)
        payload_entries = [
            DiskEntry(
                TOOLCHAIN_PAYLOAD,
                "100644",
                toolchain_payload,
                toolchain_payload.stat().st_size,
            ),
            DiskEntry(
                VENDOR_PAYLOAD,
                "100644",
                vendor_payload,
                vendor_payload.stat().st_size,
            ),
        ]
        records = [
            _file_record(entry.path, entry.mode, entry.size, entry.sha256)
            for entry in [*ordered_source, bootstrap_entry]
        ]
        records.extend(
            _file_record(
                entry.path, entry.mode, entry.size, _sha256_path(entry.source)
            )
            for entry in payload_entries
        )
        records.sort(key=lambda record: str(record["path"]).encode("utf-8"))
        manifest = {
            "commit": commit,
            "files": records,
            "nonclaims": list(NONCLAIMS),
            "prefix": kit_prefix,
            "profiles": PROFILE_COMMANDS,
            "schema": SCHEMA,
            "toolchain": {
                "cargo_version": identity.cargo_version,
                "commit_hash": identity.commit_hash,
                "file_count": len(toolchain_entries),
                "host": identity.host,
                "release": identity.release,
                "rustc_verbose": identity.rustc_verbose,
                "tree_sha256": toolchain_tree_sha256,
                "wasm_target": "wasm32-wasip1",
            },
            "vendor": {
                "crate_directories": vendor_crates,
                "file_count": len(vendor_entries),
                "format": "cargo-directory-source",
                "tree_sha256": vendor_tree_sha256,
            },
        }
        manifest_bytes = _canonical_json(manifest)
        checksums = [
            f"{record['sha256']}  {record['path']}" for record in records
        ]
        checksums.append(
            f"{hashlib.sha256(manifest_bytes).hexdigest()}  {MANIFEST_NAME}"
        )
        checksums_entry = BytesEntry(
            CHECKSUMS_NAME, "100644", ("\n".join(checksums) + "\n").encode("ascii")
        )
        manifest_entry = BytesEntry(MANIFEST_NAME, "100644", manifest_bytes)

        supplied_output = output.expanduser()
        supplied_output.parent.mkdir(parents=True, exist_ok=True)
        output = supplied_output.parent.resolve(strict=True) / supplied_output.name
        if output.exists() or output.is_symlink():
            raise OfflineKitError(f"refusing to clobber existing output: {output}")
        descriptor, temporary_output_name = tempfile.mkstemp(
            prefix=f".{output.name}.", suffix=".tmp", dir=output.parent
        )
        os.close(descriptor)
        temporary_output = Path(temporary_output_name)
        try:
            bytes_by_path = {
                entry.path: entry for entry in [*ordered_source, bootstrap_entry]
            }
            payload_by_path = {entry.path: entry for entry in payload_entries}
            with zipfile.ZipFile(temporary_output, "w", allowZip64=True) as archive:
                archive.comment = b""
                for record in records:
                    relative = str(record["path"])
                    if relative in bytes_by_path:
                        _write_zip_bytes(
                            archive,
                            kit_prefix,
                            bytes_by_path[relative],
                            zipfile.ZIP_DEFLATED,
                        )
                    else:
                        _write_zip_path(
                            archive,
                            kit_prefix,
                            payload_by_path[relative],
                            zipfile.ZIP_STORED,
                        )
                _write_zip_bytes(
                    archive, kit_prefix, manifest_entry, zipfile.ZIP_DEFLATED
                )
                _write_zip_bytes(
                    archive, kit_prefix, checksums_entry, zipfile.ZIP_DEFLATED
                )
            verify_archive(temporary_output)
            try:
                os.link(temporary_output, output)
            except FileExistsError as error:
                raise OfflineKitError(
                    f"refusing to clobber concurrently created output: {output}"
                ) from error
            temporary_output.unlink()
        finally:
            try:
                temporary_output.unlink()
            except FileNotFoundError:
                pass

    return BuildResult(
        output=output,
        prefix=kit_prefix,
        commit=commit,
        host=identity.host,
        archive_sha256=_sha256_path(output),
    )


def build_from_repository(
    repo: Path,
    ref: str,
    toolchain: Path,
    vendor: Path,
    output: Path,
    prefix: str | None,
) -> BuildResult:
    source_release = _load_source_release_module()
    root = source_release.discover_repository(repo)
    source_release.assert_clean_worktree(root, allow_dirty=False)
    commit = source_release.resolve_commit(root, ref)
    selected = source_release.collect_source_entries(root, commit)
    source_entries = _source_bytes_entries(selected)
    return build_kit_from_entries(
        source_entries,
        commit=commit,
        toolchain=toolchain,
        vendor=vendor,
        output=output,
        prefix=prefix,
    )


def _load_manifest_bytes(data: bytes) -> dict[str, object]:
    try:
        value = json.loads(data.decode("ascii", "strict"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise OfflineKitError("offline-kit manifest is not canonical JSON") from error
    if not isinstance(value, dict) or _canonical_json(value) != data:
        raise OfflineKitError("offline-kit manifest is not canonical JSON")
    required = {
        "commit",
        "files",
        "nonclaims",
        "prefix",
        "profiles",
        "schema",
        "toolchain",
        "vendor",
    }
    if set(value) != required or value.get("schema") != SCHEMA:
        raise OfflineKitError("unsupported or malformed offline-kit manifest")
    prefix = value.get("prefix")
    commit = value.get("commit")
    if not isinstance(prefix, str) or not SAFE_PREFIX.fullmatch(prefix):
        raise OfflineKitError("offline-kit manifest has an unsafe prefix")
    if not isinstance(commit, str) or not HEX_COMMIT.fullmatch(commit):
        raise OfflineKitError("offline-kit manifest has an invalid source commit")
    if value.get("profiles") != PROFILE_COMMANDS or value.get("nonclaims") != list(
        NONCLAIMS
    ):
        raise OfflineKitError("offline-kit profile or nonclaim contract drift")
    toolchain = value.get("toolchain")
    if not isinstance(toolchain, dict) or set(toolchain) != {
        "cargo_version",
        "commit_hash",
        "file_count",
        "host",
        "release",
        "rustc_verbose",
        "tree_sha256",
        "wasm_target",
    }:
        raise OfflineKitError("offline-kit manifest has malformed toolchain identity")
    if toolchain.get("host") not in SUPPORTED_POSIX_HOSTS:
        raise OfflineKitError("offline-kit manifest has an unsupported host")
    if toolchain.get("wasm_target") != "wasm32-wasip1":
        raise OfflineKitError("offline-kit manifest has the wrong wasm target")
    release = toolchain.get("release")
    commit_hash = toolchain.get("commit_hash")
    cargo_version = toolchain.get("cargo_version")
    rustc_verbose = toolchain.get("rustc_verbose")
    if (
        not isinstance(release, str)
        or not RUST_RELEASE.fullmatch(release)
        or not isinstance(commit_hash, str)
        or not HEX_COMMIT.fullmatch(commit_hash)
        or not isinstance(cargo_version, str)
        or re.fullmatch(rf"cargo {re.escape(release)}(?: .*)?", cargo_version) is None
        or not isinstance(rustc_verbose, str)
    ):
        raise OfflineKitError("offline-kit manifest has malformed toolchain strings")
    verbose_fields: dict[str, str] = {}
    for line in rustc_verbose.splitlines()[1:]:
        if ": " in line:
            key, field_value = line.split(": ", 1)
            if key in verbose_fields:
                raise OfflineKitError(
                    f"offline-kit rustc identity repeats field {key!r}"
                )
            verbose_fields[key] = field_value
    if (
        verbose_fields.get("host") != toolchain["host"]
        or verbose_fields.get("release") != release
        or verbose_fields.get("commit-hash") != commit_hash
    ):
        raise OfflineKitError("offline-kit rustc identity fields disagree")
    if (
        not isinstance(toolchain.get("file_count"), int)
        or toolchain["file_count"] <= 0
        or not isinstance(toolchain.get("tree_sha256"), str)
        or not HEX_SHA256.fullmatch(toolchain["tree_sha256"])
    ):
        raise OfflineKitError("offline-kit manifest has an invalid toolchain tree seal")
    vendor = value.get("vendor")
    if not isinstance(vendor, dict) or set(vendor) != {
        "crate_directories",
        "file_count",
        "format",
        "tree_sha256",
    }:
        raise OfflineKitError("offline-kit manifest has malformed vendor identity")
    if (
        vendor.get("format") != "cargo-directory-source"
        or not isinstance(vendor.get("crate_directories"), int)
        or vendor["crate_directories"] <= 0
        or not isinstance(vendor.get("file_count"), int)
        or vendor["file_count"] <= 0
        or not isinstance(vendor.get("tree_sha256"), str)
        or not HEX_SHA256.fullmatch(vendor["tree_sha256"])
    ):
        raise OfflineKitError("offline-kit manifest has an invalid vendor tree seal")
    return value


def _manifest_records(manifest: dict[str, object]) -> list[dict[str, object]]:
    raw = manifest.get("files")
    if not isinstance(raw, list):
        raise OfflineKitError("offline-kit manifest files must be a list")
    records: list[dict[str, object]] = []
    previous: bytes | None = None
    for item in raw:
        if not isinstance(item, dict) or set(item) != {
            "mode",
            "path",
            "sha256",
            "size",
        }:
            raise OfflineKitError("offline-kit manifest has a malformed file record")
        path = item.get("path")
        mode = item.get("mode")
        digest = item.get("sha256")
        size = item.get("size")
        if not isinstance(path, str):
            raise OfflineKitError("offline-kit manifest file path is not a string")
        _safe_relative(path)
        encoded = path.encode("utf-8")
        if previous is not None and encoded <= previous:
            raise OfflineKitError("offline-kit manifest paths are not uniquely sorted")
        previous = encoded
        if mode not in {"100644", "100755"}:
            raise OfflineKitError(f"invalid file mode for {path}")
        if not isinstance(digest, str) or not HEX_SHA256.fullmatch(digest):
            raise OfflineKitError(f"invalid file digest for {path}")
        if not isinstance(size, int) or isinstance(size, bool) or size < 0:
            raise OfflineKitError(f"invalid file size for {path}")
        records.append(item)
    required = {
        BOOTSTRAP_NAME,
        SOURCE_BOOTSTRAP_NAME,
        TOOLCHAIN_PAYLOAD,
        VENDOR_PAYLOAD,
    }
    paths = {str(record["path"]) for record in records}
    if not required <= paths:
        raise OfflineKitError("offline-kit manifest lacks source, bootstrap, or payload closure")
    records_by_path = {str(record["path"]): record for record in records}
    top_bootstrap = records_by_path[BOOTSTRAP_NAME]
    source_bootstrap = records_by_path[SOURCE_BOOTSTRAP_NAME]
    if (
        top_bootstrap["mode"] != "100755"
        or source_bootstrap["mode"] != "100755"
        or top_bootstrap["size"] != source_bootstrap["size"]
        or top_bootstrap["sha256"] != source_bootstrap["sha256"]
    ):
        raise OfflineKitError(
            "top-level bootstrap is not bound to the selected source bootstrap"
        )
    return records


def _expected_checksums(manifest_bytes: bytes, records: Sequence[dict[str, object]]) -> bytes:
    lines = [f"{record['sha256']}  {record['path']}" for record in records]
    lines.append(f"{hashlib.sha256(manifest_bytes).hexdigest()}  {MANIFEST_NAME}")
    return ("\n".join(lines) + "\n").encode("ascii")


def _copy_zip_member_to_temp(
    archive: zipfile.ZipFile, name: str, temporary: Path
) -> Path:
    destination = temporary / PurePosixPath(name).name
    with archive.open(name, "r") as source, destination.open("xb") as output:
        shutil.copyfileobj(source, output, length=1024 * 1024)
    return destination


def _validate_zip_info(
    info: zipfile.ZipInfo, *, mode: str, compression: int
) -> None:
    expected_version = 45 if compression == zipfile.ZIP_STORED else 20
    expected_flags = 0x800 if any(byte >= 0x80 for byte in _zip_filename_bytes(info)) else 0
    if (
        info.date_time != FIXED_ZIP_TIMESTAMP
        or info.create_system != 3
        or info.create_version != expected_version
        or info.extract_version != expected_version
        or info.reserved != 0
        or info.flag_bits != expected_flags
        or info.volume != 0
        or info.internal_attr != 0
        or info.external_attr != int(mode, 8) << 16
        or info.compress_type != compression
        or info.extra
        or info.comment
        or info.is_dir()
    ):
        raise OfflineKitError(f"non-canonical ZIP metadata for {info.filename}")
    zip_mode = f"{(info.external_attr >> 16) & 0xFFFF:06o}"
    if zip_mode != mode:
        raise OfflineKitError(f"ZIP mode mismatch for {info.filename}")


def _zip_filename_bytes(info: zipfile.ZipInfo) -> bytes:
    encoding = "utf-8" if info.flag_bits & 0x800 else "cp437"
    return info.filename.encode(encoding, "strict")


def _validate_zip_layout(
    release: Path,
    archive: zipfile.ZipFile,
    infos: Sequence[zipfile.ZipInfo],
    stored_payload_names: set[str],
) -> None:
    if len(infos) > 0xFFFF:
        raise OfflineKitError("offline-kit ZIP64 central directory is unsupported")
    expected_offset = 0
    with release.open("rb") as raw:
        for info in infos:
            if info.header_offset != expected_offset:
                raise OfflineKitError(
                    f"non-canonical ZIP member offset for {info.filename}"
                )
            raw.seek(expected_offset)
            header = raw.read(30)
            if len(header) != 30:
                raise OfflineKitError("truncated ZIP local header")
            (
                signature,
                extract_version,
                flags,
                compression,
                modified_time,
                modified_date,
                crc32,
                compressed_size,
                file_size,
                name_length,
                extra_length,
            ) = struct.unpack("<IHHHHHIIIHH", header)
            if signature != 0x04034B50:
                raise OfflineKitError("non-canonical ZIP local-header signature")
            filename = raw.read(name_length)
            extra = raw.read(extra_length)
            expected_filename = _zip_filename_bytes(info)
            expected_flags = 0x800 if any(byte >= 0x80 for byte in expected_filename) else 0
            if (
                filename != expected_filename
                or extract_version
                != (45 if info.filename in stored_payload_names else 20)
                or flags != expected_flags
                or compression != info.compress_type
                or modified_time != 0
                or modified_date != 33
                or crc32 != info.CRC
            ):
                raise OfflineKitError(f"non-canonical ZIP local header for {info.filename}")
            if info.filename in stored_payload_names:
                expected_extra = struct.pack(
                    "<HHQQ", 0x0001, 16, info.file_size, info.compress_size
                )
                if (
                    file_size != 0xFFFF_FFFF
                    or compressed_size != 0xFFFF_FFFF
                    or extra != expected_extra
                ):
                    raise OfflineKitError(
                        f"non-canonical ZIP64 payload header for {info.filename}"
                    )
            elif (
                file_size != info.file_size
                or compressed_size != info.compress_size
                or extra
            ):
                raise OfflineKitError(f"non-canonical ZIP size header for {info.filename}")
            expected_offset += 30 + name_length + extra_length + info.compress_size

        if archive.start_dir != expected_offset:
            raise OfflineKitError("non-canonical ZIP local-header layout")
        central_size = sum(
            46
            + len(_zip_filename_bytes(info))
            + len(info.extra)
            + len(info.comment)
            for info in infos
        )
        expected_size = archive.start_dir + central_size + 22
        raw.seek(archive.start_dir + central_size)
        eocd = raw.read(22)
        if len(eocd) != 22:
            raise OfflineKitError("truncated ZIP end-of-central-directory record")
        (
            eocd_signature,
            disk_number,
            central_disk,
            entries_on_disk,
            entries_total,
            recorded_central_size,
            central_offset,
            comment_length,
        ) = struct.unpack("<IHHHHIIH", eocd)
        if (
            eocd_signature != 0x06054B50
            or disk_number != 0
            or central_disk != 0
            or entries_on_disk != len(infos)
            or entries_total != len(infos)
            or recorded_central_size != central_size
            or central_offset != archive.start_dir
            or comment_length != 0
            or release.stat().st_size != expected_size
        ):
            raise OfflineKitError("non-canonical ZIP central-directory layout")


def verify_archive(path: Path | str) -> dict[str, object]:
    release = Path(path).expanduser().resolve(strict=True)
    try:
        with zipfile.ZipFile(release, "r") as archive:
            if archive.comment:
                raise OfflineKitError("offline-kit ZIP must not have a comment")
            infos = archive.infolist()
            names = [info.filename for info in infos]
            if not names or len(names) != len(set(names)):
                raise OfflineKitError("offline-kit ZIP is empty or has duplicate names")
            for name in names:
                _safe_relative(name)
            roots = {PurePosixPath(name).parts[0] for name in names}
            if len(roots) != 1:
                raise OfflineKitError("offline-kit ZIP must have one top-level prefix")
            prefix = next(iter(roots))
            manifest_name = f"{prefix}/{MANIFEST_NAME}"
            checksums_name = f"{prefix}/{CHECKSUMS_NAME}"
            if manifest_name not in names or checksums_name not in names:
                raise OfflineKitError("offline-kit ZIP lacks manifest or checksums")
            manifest_bytes = archive.read(manifest_name)
            manifest = _load_manifest_bytes(manifest_bytes)
            if manifest["prefix"] != prefix:
                raise OfflineKitError("ZIP and manifest prefixes differ")
            records = _manifest_records(manifest)
            expected_names = [f"{prefix}/{record['path']}" for record in records]
            expected_names.extend([manifest_name, checksums_name])
            if names != expected_names:
                raise OfflineKitError("offline-kit ZIP member set or order differs from manifest")
            _validate_zip_layout(
                release,
                archive,
                infos,
                {
                    f"{prefix}/{TOOLCHAIN_PAYLOAD}",
                    f"{prefix}/{VENDOR_PAYLOAD}",
                },
            )
            info_by_name = {info.filename: info for info in infos}
            with tempfile.TemporaryDirectory(prefix="ostadix-offline-verify-") as temp:
                temporary = Path(temp)
                payload_paths: dict[str, Path] = {}
                for record in records:
                    relative = str(record["path"])
                    member = f"{prefix}/{relative}"
                    info = info_by_name[member]
                    expected_compression = (
                        zipfile.ZIP_STORED
                        if relative in {TOOLCHAIN_PAYLOAD, VENDOR_PAYLOAD}
                        else zipfile.ZIP_DEFLATED
                    )
                    _validate_zip_info(
                        info,
                        mode=str(record["mode"]),
                        compression=expected_compression,
                    )
                    if info.file_size != record["size"]:
                        raise OfflineKitError(f"ZIP size mismatch for {relative}")
                    with archive.open(member, "r") as payload:
                        if _sha256_stream(payload) != record["sha256"]:
                            raise OfflineKitError(f"ZIP digest mismatch for {relative}")
                    if relative in {TOOLCHAIN_PAYLOAD, VENDOR_PAYLOAD}:
                        payload_paths[relative] = _copy_zip_member_to_temp(
                            archive, member, temporary
                        )
                if archive.read(f"{prefix}/{BOOTSTRAP_NAME}") != archive.read(
                    f"{prefix}/{SOURCE_BOOTSTRAP_NAME}"
                ):
                    raise OfflineKitError(
                        "top-level bootstrap bytes differ from selected source bootstrap"
                    )
                toolchain_summary = _tar_tree_summary(
                    payload_paths[TOOLCHAIN_PAYLOAD], "toolchain"
                )
                vendor_summary = _tar_tree_summary(
                    payload_paths[VENDOR_PAYLOAD], "vendor"
                )
                toolchain = manifest["toolchain"]
                vendor = manifest["vendor"]
                assert isinstance(toolchain, dict)
                assert isinstance(vendor, dict)
                if toolchain_summary[:2] != (
                    toolchain["file_count"],
                    toolchain["tree_sha256"],
                ):
                    raise OfflineKitError(
                        "toolchain tar tree differs from manifest seal"
                    )
                if vendor_summary != (
                    vendor["file_count"],
                    vendor["tree_sha256"],
                    vendor["crate_directories"],
                ):
                    raise OfflineKitError("vendor tar tree differs from manifest seal")
            _validate_zip_info(
                info_by_name[manifest_name], mode="100644", compression=zipfile.ZIP_DEFLATED
            )
            _validate_zip_info(
                info_by_name[checksums_name], mode="100644", compression=zipfile.ZIP_DEFLATED
            )
            if archive.read(checksums_name) != _expected_checksums(
                manifest_bytes, records
            ):
                raise OfflineKitError("offline-kit SHA256SUMS does not match manifest")
            return manifest
    except (OSError, zipfile.BadZipFile, KeyError, RuntimeError) as error:
        if isinstance(error, OfflineKitError):
            raise
        raise OfflineKitError(f"cannot verify offline-kit ZIP: {error}") from error


def detect_posix_host() -> str:
    system = platform.system().lower()
    machine = platform.machine().lower()
    normalized_machine = "aarch64" if machine in {"arm64", "aarch64"} else machine
    if system == "darwin" and normalized_machine in {"aarch64", "x86_64"}:
        return f"{normalized_machine}-apple-darwin"
    if system == "linux" and normalized_machine in {"aarch64", "x86_64"}:
        libc_name, _version = platform.libc_ver()
        if not libc_name or libc_name.lower() not in {"glibc", "gnu libc"}:
            raise OfflineKitError(
                f"GNU-host kit requires positively identified glibc: {libc_name or '<unknown>'}"
            )
        return f"{normalized_machine}-unknown-linux-gnu"
    raise OfflineKitError(f"unsupported POSIX bootstrap host: {system}/{machine}")


def verify_extracted_kit(root: Path) -> dict[str, object]:
    root = root.expanduser().resolve(strict=True)
    manifest_path = root / MANIFEST_NAME
    checksums_path = root / CHECKSUMS_NAME
    for required in (manifest_path, checksums_path):
        if required.is_symlink() or not required.is_file():
            raise OfflineKitError(f"extracted kit lacks regular file {required.name}")
    manifest_bytes = manifest_path.read_bytes()
    manifest = _load_manifest_bytes(manifest_bytes)
    records = _manifest_records(manifest)
    expected_files = {
        *(str(record["path"]) for record in records),
        MANIFEST_NAME,
        CHECKSUMS_NAME,
    }
    expected_directories: set[str] = set()
    for relative in expected_files:
        parts = PurePosixPath(relative).parts
        expected_directories.update(
            PurePosixPath(*parts[:index]).as_posix()
            for index in range(1, len(parts))
        )
    actual_files: set[str] = set()
    actual_directories: set[str] = set()

    def inspect_directory(directory: Path, relative: PurePosixPath | None) -> None:
        try:
            children = sorted(
                os.scandir(directory), key=lambda entry: os.fsencode(entry.name)
            )
        except OSError as error:
            raise OfflineKitError(f"cannot enumerate extracted kit: {error}") from error
        for child in children:
            child_relative = (
                PurePosixPath(child.name)
                if relative is None
                else relative / child.name
            )
            relative_text = child_relative.as_posix()
            try:
                metadata = child.stat(follow_symlinks=False)
            except OSError as error:
                raise OfflineKitError(
                    f"cannot stat extracted kit member {relative_text}: {error}"
                ) from error
            if stat.S_ISLNK(metadata.st_mode):
                raise OfflineKitError(
                    f"extracted kit symlinks are forbidden: {relative_text}"
                )
            if relative is None and child.name == ".offline":
                if not stat.S_ISDIR(metadata.st_mode):
                    raise OfflineKitError("generated .offline path is not a directory")
                continue
            if stat.S_ISDIR(metadata.st_mode):
                actual_directories.add(relative_text)
                inspect_directory(Path(child.path), child_relative)
            elif stat.S_ISREG(metadata.st_mode):
                actual_files.add(relative_text)
            else:
                raise OfflineKitError(
                    f"extracted kit contains a special member: {relative_text}"
                )

    inspect_directory(root, None)
    if actual_files != expected_files or actual_directories != expected_directories:
        raise OfflineKitError(
            "extracted kit member closure differs from manifest: "
            f"extra_files={sorted(actual_files - expected_files)}, "
            f"missing_files={sorted(expected_files - actual_files)}, "
            f"extra_directories={sorted(actual_directories - expected_directories)}, "
            f"missing_directories={sorted(expected_directories - actual_directories)}"
        )
    for record in records:
        relative = str(record["path"])
        path = root.joinpath(*PurePosixPath(relative).parts)
        try:
            metadata = path.lstat()
        except OSError as error:
            raise OfflineKitError(f"extracted kit lacks {relative}: {error}") from error
        if not stat.S_ISREG(metadata.st_mode):
            raise OfflineKitError(f"extracted kit member is not regular: {relative}")
        if metadata.st_size != record["size"] or _sha256_path(path) != record["sha256"]:
            raise OfflineKitError(f"extracted kit digest mismatch for {relative}")
    if (root / BOOTSTRAP_NAME).read_bytes() != root.joinpath(
        *PurePosixPath(SOURCE_BOOTSTRAP_NAME).parts
    ).read_bytes():
        raise OfflineKitError(
            "top-level bootstrap bytes differ from selected source bootstrap"
        )
    if checksums_path.read_bytes() != _expected_checksums(manifest_bytes, records):
        raise OfflineKitError("extracted kit SHA256SUMS does not match manifest")
    _validate_tar(root / TOOLCHAIN_PAYLOAD, "toolchain")
    _validate_tar(root / VENDOR_PAYLOAD, "vendor")
    return manifest


def _extract_tar_safely(payload: Path, destination: Path, required_prefix: str) -> None:
    _validate_tar(payload, required_prefix)
    with tarfile.open(payload, "r:gz") as archive:
        for member in archive.getmembers():
            pure = _safe_relative(member.name)
            target = destination.joinpath(*pure.parts)
            target.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
            extracted = archive.extractfile(member)
            if extracted is None:
                raise OfflineKitError(f"cannot read tar member {member.name}")
            with extracted, target.open("xb") as output:
                shutil.copyfileobj(extracted, output, length=1024 * 1024)
            target.chmod(member.mode)


def _cargo_config_text(vendor_path: Path) -> str:
    return (
        "[source.crates-io]\n"
        'replace-with = "vendored-sources"\n\n'
        "[source.vendored-sources]\n"
        f'directory = {json.dumps(os.fspath(vendor_path))}\n\n'
        "[net]\n"
        "offline = true\n"
    )


def _validate_unpacked_payload_seals(
    destination: Path, manifest: dict[str, object]
) -> None:
    toolchain = manifest["toolchain"]
    vendor = manifest["vendor"]
    assert isinstance(toolchain, dict)
    assert isinstance(vendor, dict)
    toolchain_entries = _walk_regular_files(destination / "toolchain", "toolchain")
    vendor_entries = _walk_regular_files(destination / "vendor", "vendor")
    if (
        len(toolchain_entries) != toolchain["file_count"]
        or _tree_seal(toolchain_entries) != toolchain["tree_sha256"]
    ):
        raise OfflineKitError("existing extracted Rust toolchain does not match kit seal")
    if (
        len(vendor_entries) != vendor["file_count"]
        or _tree_seal(vendor_entries) != vendor["tree_sha256"]
    ):
        raise OfflineKitError("existing extracted Cargo vendor tree does not match kit seal")


def _extraction_receipt(
    manifest: dict[str, object], manifest_sha256: str
) -> bytes:
    toolchain = manifest["toolchain"]
    assert isinstance(toolchain, dict)
    return _canonical_json(
        {
            "commit": manifest["commit"],
            "host": toolchain["host"],
            "manifest_sha256": manifest_sha256,
            "schema": SCHEMA,
        }
    )


def _validate_existing_extraction(
    destination: Path,
    manifest: dict[str, object],
    manifest_sha256: str,
) -> None:
    try:
        metadata = destination.lstat()
    except OSError as error:
        raise OfflineKitError(f"cannot stat existing extraction path: {error}") from error
    if not stat.S_ISDIR(metadata.st_mode):
        raise OfflineKitError(f"refusing to clobber existing extraction path: {destination}")
    receipt = destination / "EXTRACTED-KIT.json"
    cargo_config = destination / "cargo-home" / "config.toml"
    legacy_cargo_config = destination / "cargo-home" / "config"
    if receipt.is_symlink() or not receipt.is_file():
        raise OfflineKitError(f"refusing unsealed existing extraction path: {destination}")
    if receipt.read_bytes() != _extraction_receipt(manifest, manifest_sha256):
        raise OfflineKitError("existing extraction receipt belongs to a different kit")
    for name in ("toolchain", "vendor", "cargo-home"):
        child = destination / name
        try:
            child_metadata = child.lstat()
        except OSError as error:
            raise OfflineKitError(
                f"existing extraction lacks real {name} directory: {error}"
            ) from error
        if not stat.S_ISDIR(child_metadata.st_mode):
            raise OfflineKitError(
                f"existing extraction {name} path is not a real directory"
            )
    target = destination / "target"
    try:
        target_metadata = target.lstat()
    except FileNotFoundError:
        target_metadata = None
    except OSError as error:
        raise OfflineKitError(f"cannot stat existing target directory: {error}") from error
    if target_metadata is not None and not stat.S_ISDIR(target_metadata.st_mode):
        raise OfflineKitError("existing extraction target path is not a real directory")
    expected_config = _cargo_config_text(destination / "vendor")
    if cargo_config.is_symlink() or not cargo_config.is_file():
        raise OfflineKitError("existing extraction lacks its sealed Cargo configuration")
    if cargo_config.read_text(encoding="utf-8") != expected_config:
        raise OfflineKitError("existing extraction Cargo configuration was modified")
    if legacy_cargo_config.exists() or legacy_cargo_config.is_symlink():
        raise OfflineKitError("existing extraction has a forbidden legacy Cargo configuration")
    _validate_unpacked_payload_seals(destination, manifest)


def extract_payloads(
    kit_root: Path, destination: Path, *, detected_host: str | None = None
) -> dict[str, object]:
    manifest = verify_extracted_kit(kit_root)
    actual_host = detected_host or detect_posix_host()
    toolchain = manifest["toolchain"]
    assert isinstance(toolchain, dict)
    expected_host = toolchain["host"]
    if actual_host != expected_host:
        raise OfflineKitError(
            f"offline kit host mismatch: kit={expected_host}, current={actual_host}"
        )
    root = kit_root.expanduser().resolve(strict=True)
    manifest_sha256 = _sha256_path(root / MANIFEST_NAME)
    supplied_destination = destination.expanduser()
    if supplied_destination.is_symlink():
        raise OfflineKitError(
            f"refusing symlink extraction path: {supplied_destination}"
        )
    destination = supplied_destination.resolve(strict=False)
    if destination.exists():
        _validate_existing_extraction(destination, manifest, manifest_sha256)
        return manifest
    destination.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
    destination.mkdir(mode=0o755, exist_ok=False)
    try:
        _extract_tar_safely(root / TOOLCHAIN_PAYLOAD, destination, "toolchain")
        _extract_tar_safely(root / VENDOR_PAYLOAD, destination, "vendor")
        _validate_unpacked_payload_seals(destination, manifest)
        cargo_home = destination / "cargo-home"
        cargo_home.mkdir(mode=0o755)
        config = _cargo_config_text(destination / "vendor")
        (cargo_home / "config.toml").write_text(config, encoding="utf-8")
        receipt = destination / "EXTRACTED-KIT.json"
        receipt.write_bytes(_extraction_receipt(manifest, manifest_sha256))
        receipt.chmod(0o444)
    except Exception:
        shutil.rmtree(destination)
        raise
    return manifest


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    build = subparsers.add_parser("build", help="build one per-host offline kit")
    build.add_argument("--repo", default=".")
    build.add_argument("--ref", default="HEAD")
    build.add_argument("--toolchain", required=True, help="Rust sysroot directory")
    build.add_argument("--vendor", required=True, help="cargo vendor directory")
    build.add_argument("--output", required=True)
    build.add_argument("--prefix")
    verify = subparsers.add_parser("verify", help="verify a kit ZIP without extracting")
    verify.add_argument("archive")
    extract = subparsers.add_parser(
        "extract", help="verify an already-extracted kit and unpack sealed payloads"
    )
    extract.add_argument("--kit-root", required=True)
    extract.add_argument("--destination", required=True)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        if arguments.command == "build":
            result = build_from_repository(
                Path(arguments.repo),
                arguments.ref,
                Path(arguments.toolchain),
                Path(arguments.vendor),
                Path(arguments.output),
                arguments.prefix,
            )
            print(
                f"built {result.output} commit={result.commit} host={result.host} "
                f"sha256={result.archive_sha256}"
            )
        elif arguments.command == "verify":
            manifest = verify_archive(arguments.archive)
            toolchain = manifest["toolchain"]
            assert isinstance(toolchain, dict)
            print(
                f"verified {arguments.archive} commit={manifest['commit']} "
                f"host={toolchain['host']}"
            )
        else:
            manifest = extract_payloads(
                Path(arguments.kit_root), Path(arguments.destination)
            )
            print(
                f"extracted host={manifest['toolchain']['host']} "
                f"destination={Path(arguments.destination).resolve()}"
            )
    except (RuntimeError, OSError) as error:
        print(f"offline-kit error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
