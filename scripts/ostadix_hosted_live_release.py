#!/usr/bin/env python3
"""Build, boot-gate, and no-clobber publish the exact staged hosted-live tree."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
from pathlib import PurePosixPath
import secrets
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from typing import Callable, Mapping, Sequence


ROOT = Path(__file__).resolve().parent.parent
CANONICAL_REMOTE = "https://github.com/lostadi/Ostadix-lang.git"
DEFAULT_OUTPUT_DIRECTORY = ROOT / "target/ostadix-hosted-live/x86_64"
DEFAULT_VM = "moral-gaur"
GUEST_RELEASE_BASE = Path("/home/ubuntu/.cache/ostadix/hosted-live-release")
MIN_GUEST_FREE_BYTES = 12 * 1024 * 1024 * 1024
MIN_GRAPHICAL_NONBLACK_PIXELS = 20_000
MIN_GRAPHICAL_UNIQUE_COLORS = 8
MIN_GRAPHICAL_CHROMATIC_PIXELS = 500
MIN_GRAPHICAL_CHROMATIC_HUE_BUCKETS = 3
MIN_GRAPHICAL_PIXELS_PER_HUE_BUCKET = 20
MIN_GRAPHICAL_CHROMATIC_MAX_CHANNEL = 48
MIN_GRAPHICAL_CHROMATIC_CHANNEL_SPREAD = 32
MIN_GRAPHICAL_CHANGED_PIXELS = 200
QEMU_ENTROPY_DEVICE = "virtio-rng-pci"
ENTROPY_PROBE_BYTES = 32
MIN_GUEST_ENTROPY_BITS = 128
DEFAULT_HOSTED_SMOKE_TIMEOUT_SECONDS = 1800.0
MAX_HOSTED_SMOKE_TIMEOUT_SECONDS = 1800.0
DEFAULT_OCORE_SMOKE_TIMEOUT_SECONDS = 900.0
MAX_OCORE_SMOKE_TIMEOUT_SECONDS = 900.0
ARCHIVE_MTIME = "@315532800"
SOURCE_DATE_EPOCH = 315532800
NATIVE_GUEST_FILESYSTEMS = frozenset({"ext4", "xfs", "btrfs"})
PINNED_MINIROOTFS_BYTES = 3698422
PINNED_MINIROOTFS_SHA256 = (
    "41f73e3cf5fa919b8aa5ca6b30dc48f0da2720776d7423e2a7748211456fe081"
)
PINNED_LTS_KERNEL_BYTES = 14_468_096
PINNED_LTS_KERNEL_SHA256 = (
    "77007123c0591ab4b2a5434ffa1b6a3985b3037d534be78bccfb30f3c9536c54"
)
PINNED_LTS_INITRAMFS_BYTES = 27_951_899
PINNED_LTS_INITRAMFS_SHA256 = (
    "e1649e94ef1b276bf22ea4ed2628dd17c7fa7505cd40b2c7aa7fd9ebb71fe5c9"
)
PINNED_LTS_MODLOOP_BYTES = 303_034_368
PINNED_LTS_MODLOOP_SHA256 = (
    "871ef51ed6378283db9462947bb7fb84c1ec31376611eb1a2281b02b9404c0f6"
)
PINNED_VIRT_MODLOOP_BYTES = 22_867_968
PINNED_VIRT_MODLOOP_SHA256 = (
    "78907e7cc812d555f08d4e1133d090cf11fa197370882adfe67b0a5986ccb3f9"
)
REQUIRED_SMOKE_MARKERS = (
    "OSTADIX HOSTED ROOTFS: PASS bytes=",
    "OSTADIX HOSTED ROOTFS OVERLAY: PASS",
    "OSTADIX HOSTED READ-ONLY TREES: PASS",
    "OSTADIX HOSTED LOOPBACK: PASS",
    "OSTADIX HOSTED O SMOKE: PASS",
    "OSTADIX HOSTED BASH: PASS",
    "OSTADIX HOSTED APK: PASS",
    "OSTADIX HOSTED SQLITE: PASS",
    "OSTADIX HOSTED OLANGC IR: PASS",
    "OSTADIX HOSTED O-CLI: PASS",
    "OSTADIX HOSTED O-LINK: PASS",
    "OSTADIX HOSTED RUSTC: PASS",
    "OSTADIX HOSTED CARGO: PASS",
    "OSTADIX HOSTED RUSTFMT: PASS",
    "OSTADIX HOSTED CLIPPY: PASS",
    "OSTADIX HOSTED CARGO HELLO: PASS",
    "OSTADIX HOSTED ENTROPY: PASS",
    "OSTADIX HOSTED O-NODE: PASS",
    "OSTADIX HOSTED NOTEBOOK: PASS",
    "OSTADIX HOSTED STANDARD BINARIES: PASS",
    "OSTADIX HOSTED DECLARED ROOT BINARIES: PASS",
    "OSTADIX HOSTED UNIFIED ROUTES: PASS",
    "OSTADIX HOSTED SOURCE SNAPSHOT: PASS",
    "OSTADIX HOSTED OLANGC MATERIALIZATION: PASS",
    "OSTADIX HOSTED OLANGC WASM ARTIFACT: PASS",
    "OSTADIX HOSTED RUST WASM: PASS",
    "OSTADIX HOSTED WASM RUNTIME: PASS",
    "OSTADIX HOSTED OLANGC WASM EXECUTION: PASS",
    "OSTADIX HOSTED WEBASSEMBLY BACKEND: PASS",
    "OSTADIX HOSTED MCP: PASS",
    "OSTADIX BOOT OBJECTS: PASS",
    "OSTADIX HOSTED SOURCE OBJECT CLOSURE: PASS",
    "OSTADIX HOSTED LIVE READY",
)
REQUIRED_VISUAL_SMOKE_MARKERS = REQUIRED_SMOKE_MARKERS + (
    "OSTADIX HOSTED X11 FONT: PASS",
    "OSTADIX HOSTED PTY: PASS",
    "OSTADIX HOSTED EVDEV: PASS",
    "OSTADIX HOSTED NOTEBOOK GUI READY: PASS",
    "OSTADIX HOSTED DESKTOP READY: PASS",
)
REQUIRED_OCORE_SMOKE_MARKERS = (
    "O-core kernel: serial online",
    "page protections: W^X online",
    "CPL3 native[0]: online",
    "timer CPL3 return: online",
    "CPL3 heartbeat: online",
)
REQUIRED_HOSTED_BINARIES = frozenset(
    {
        "O",
        "o-cli",
        "olangc",
        "ocorec",
        "o-link",
        "o-unlink",
        "o-notebook",
        "ogit",
        "o-live-host",
        "o-node",
        "octl",
        "o-registry",
        "o-info",
        "ocore-kernel-world-record",
        "ostadix-mcp",
    }
)
EXPECTED_WORKSTATION_PACKAGE_ROOTS = (
    "build-base=0.5-r4",
    "cargo=1.96.1-r0",
    "clang22=22.1.3-r2",
    "eudev=3.2.14-r6",
    "firefox-esr=140.12.0-r0",
    "git=2.54.0-r0",
    "lld22=22.1.3-r0",
    "openbox=3.6.1-r8",
    "openssl=3.5.8-r0",
    "rust=1.96.1-r0",
    "rust-clippy=1.96.1-r0",
    "rust-wasm=1.96.1-r0",
    "rustfmt=1.96.1-r0",
    "wasm-tools=1.236.0-r0",
    "wasmtime=44.0.1-r0",
    "xdg-utils=1.2.1-r1",
    "xf86-input-libinput=1.5.0-r0",
    "xinit=1.4.4-r0",
    "xorg-server=21.1.24-r0",
    "xset=1.2.5-r1",
    "xsetroot=1.1.3-r1",
    "xterm=410-r0",
)
EXPECTED_SYSROOT_PACKAGE_LOCK = tuple(
    sorted(
        (
            "alpine-baselayout=3.7.2-r1",
            "alpine-baselayout-data=3.7.2-r1",
            "alpine-keys=2.6-r0",
            "alpine-release=3.24.1-r0",
            "apk-tools=3.0.6-r0",
            "busybox=1.37.0-r31",
            "busybox-binsh=1.37.0-r31",
            "ca-certificates-bundle=20260611-r0",
            "libapk=3.0.6-r0",
            "libcrypto3=3.5.7-r0",
            "libssl3=3.5.7-r0",
            "musl=1.2.6-r2",
            "musl-dev=1.2.6-r2",
            "musl-utils=1.2.6-r2",
            "scanelf=1.3.9-r1",
            "ssl_client=1.37.0-r31",
            "zlib=1.3.2-r0",
        )
    )
)
REQUIRED_ARCHIVE_PATHS = {
    "scripts/foreign_kernel_lab.py",
    "scripts/ostadix_boot_objects.py",
    "scripts/ostadix_wasm_release.py",
    "scripts/build-x86_64-hosted-live-linux.sh",
    "scripts/ostadix-hosted-live-desktop.sh",
    "scripts/smoke_ostadix_mcp.py",
    "ocore/kernel/smoke-x86_64-hosted-live-all.py",
    "ocore/kernel/smoke-x86_64-hosted-live-qemu.py",
    "ocore/kernel/smoke-x86_64-hosted-live-ocore-qemu.py",
    "ocore/kernel/smoke-x86_64-hosted-live-vga-qemu.py",
    "ocore/kernel/resolve-x86_64-ovmf-code.sh",
    "ocore/kernel/build.sh",
    "scripts/prepare-x86_64-capacity-host.sh",
    "ocore/kernel/build-x86_64-hosted-live-iso.sh",
    "evidence/foreign_kernel_lab.toml",
    "evidence/hosted_live_apk_packages.txt",
    "evidence/hosted_live_physical_iso.toml",
    "evidence/hosted_live_workstation_apk_packages.txt",
}


class ReleaseError(RuntimeError):
    """The hosted-live release pipeline failed closed."""


@dataclass(frozen=True)
class SourceSnapshot:
    tree: str
    head: str
    branch: str
    origin: str
    archive: Path
    archive_sha256: str


@dataclass(frozen=True)
class BootObjectSnapshot:
    archive: Path
    archive_sha256: str
    summary: dict[str, object]


@dataclass(frozen=True)
class SmokeTimeoutPolicy:
    hosted_seconds: str
    ocore_seconds: str


RunCallable = Callable[..., subprocess.CompletedProcess[str]]


def _bounded_timeout(
    environment: Mapping[str, str],
    name: str,
    *,
    default: float,
    maximum: float,
) -> str:
    raw = environment.get(name, f"{default:g}")
    try:
        value = float(raw)
    except ValueError as error:
        raise ReleaseError(
            f"{name} must be a finite number from 1 through {maximum:g}"
        ) from error
    if not math.isfinite(value) or not (1 <= value <= maximum):
        raise ReleaseError(f"{name} must be a finite number from 1 through {maximum:g}")
    return f"{value:g}"


def resolve_smoke_timeout_policy(
    environment: Mapping[str, str] | None = None,
) -> SmokeTimeoutPolicy:
    source = os.environ if environment is None else environment
    return SmokeTimeoutPolicy(
        hosted_seconds=_bounded_timeout(
            source,
            "OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT",
            default=DEFAULT_HOSTED_SMOKE_TIMEOUT_SECONDS,
            maximum=MAX_HOSTED_SMOKE_TIMEOUT_SECONDS,
        ),
        ocore_seconds=_bounded_timeout(
            source,
            "OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT",
            default=DEFAULT_OCORE_SMOKE_TIMEOUT_SECONDS,
            maximum=MAX_OCORE_SMOKE_TIMEOUT_SECONDS,
        ),
    )


def _guest_worker_environment(
    *,
    snapshot: SourceSnapshot,
    boot_objects: BootObjectSnapshot,
    guest_boot_objects_archive: Path,
    smoke_timeouts: SmokeTimeoutPolicy,
) -> list[str]:
    return [
        "env",
        f"OSTADIX_HOSTED_LIVE_ARCHIVE_SHA256={snapshot.archive_sha256}",
        f"OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE={guest_boot_objects_archive}",
        f"OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE_SHA256={boot_objects.archive_sha256}",
        f"OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT={smoke_timeouts.hosted_seconds}",
        f"OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT={smoke_timeouts.ocore_seconds}",
    ]


def _invoke(
    arguments: Sequence[str],
    *,
    cwd: Path | None = None,
    capture_output: bool = True,
    runner: RunCallable | None = None,
) -> subprocess.CompletedProcess[str]:
    function = runner or subprocess.run
    result = function(
        list(arguments),
        cwd=str(cwd) if cwd is not None else None,
        text=True,
        stdout=subprocess.PIPE if capture_output else None,
        stderr=subprocess.PIPE if capture_output else None,
        check=False,
    )
    if result.returncode != 0:
        stderr = (result.stderr or "").strip()
        raise ReleaseError(
            f"command failed with status {result.returncode}: {arguments!r}"
            + (f": {stderr[-4096:]}" if stderr else "")
        )
    return result


def _git(
    repo: Path,
    *arguments: str,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        ["git", "-C", str(repo), *arguments],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        raise ReleaseError(
            f"git {' '.join(arguments)} failed with status {result.returncode}: "
            f"{result.stderr.strip()[-4096:]}"
        )
    return result


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def create_source_snapshot(repo: Path, archive: Path) -> SourceSnapshot:
    """Archive the exact staged index after rejecting tracked worktree drift."""

    repo = repo.resolve(strict=True)
    top = Path(_git(repo, "rev-parse", "--show-toplevel").stdout.strip()).resolve()
    if top != repo:
        raise ReleaseError(f"repository root mismatch: expected {repo}, got {top}")

    dirty = _git(repo, "diff", "--quiet", "--no-ext-diff", "--", check=False)
    if dirty.returncode == 1:
        raise ReleaseError(
            "tracked files contain unstaged changes; stage them or restore them before release"
        )
    if dirty.returncode != 0:
        raise ReleaseError(f"git diff failed while checking unstaged changes: {dirty.stderr.strip()}")

    tree = _git(repo, "write-tree").stdout.strip()
    head = _git(repo, "rev-parse", "HEAD").stdout.strip()
    if len(tree) != 40 or len(head) != 40:
        raise ReleaseError("Git did not return 40-character staged-tree and HEAD identities")
    branch_result = _git(repo, "symbolic-ref", "--short", "-q", "HEAD", check=False)
    branch = branch_result.stdout.strip() if branch_result.returncode == 0 else "DETACHED"
    origin_result = _git(repo, "remote", "get-url", "origin", check=False)
    origin = origin_result.stdout.strip() if origin_result.returncode == 0 else ""

    archive.parent.mkdir(parents=True, exist_ok=True)
    _git(
        repo,
        "archive",
        "--format=tar",
        f"--mtime={ARCHIVE_MTIME}",
        "--output",
        str(archive),
        tree,
    )
    with tarfile.open(archive, "r:") as source:
        members = source.getmembers()
        names = {member.name for member in members}
    unsafe_members = sorted(
        member.name
        for member in members
        if member.issym() or member.islnk() or not (member.isfile() or member.isdir())
    )
    if unsafe_members:
        raise ReleaseError(
            "staged release tree contains symlinks or special archive members: "
            + ", ".join(unsafe_members[:20])
        )
    missing = sorted(REQUIRED_ARCHIVE_PATHS - names)
    if missing:
        raise ReleaseError(
            "staged tree omits required hosted-live release paths: " + ", ".join(missing)
        )
    return SourceSnapshot(
        tree=tree,
        head=head,
        branch=branch,
        origin=origin,
        archive=archive,
        archive_sha256=_sha256(archive),
    )


def _extract_regular_snapshot(archive: Path, destination: Path) -> None:
    """Extract the already-admitted Git archive without trusting tar paths."""

    destination.mkdir(mode=0o700)
    destination_root = destination.resolve(strict=True)
    with tarfile.open(archive, "r:") as source:
        for member in source.getmembers():
            pure = PurePosixPath(member.name)
            if (
                not member.name
                or pure.is_absolute()
                or any(part in {"", ".", ".."} for part in pure.parts)
                or "\\" in member.name
                or not (member.isfile() or member.isdir())
            ):
                raise ReleaseError(f"unsafe staged archive member during extraction: {member.name!r}")
            target = destination_root.joinpath(*pure.parts)
            try:
                target.relative_to(destination_root)
            except ValueError as error:
                raise ReleaseError(
                    f"staged archive member escaped extraction root: {member.name!r}"
                ) from error
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                target.chmod(0o755)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
            if hasattr(os, "O_NOFOLLOW"):
                flags |= os.O_NOFOLLOW
            descriptor = os.open(target, flags, 0o600)
            try:
                stream = source.extractfile(member)
                if stream is None:
                    raise ReleaseError(f"archive file has no payload: {member.name!r}")
                remaining = member.size
                while remaining:
                    chunk = stream.read(min(1024 * 1024, remaining))
                    if not chunk:
                        raise ReleaseError(f"archive file is truncated: {member.name!r}")
                    view = memoryview(chunk)
                    while view:
                        written = os.write(descriptor, view)
                        if written <= 0:
                            raise OSError("short write while extracting staged archive")
                        view = view[written:]
                    remaining -= len(chunk)
                if stream.read(1):
                    raise ReleaseError(f"archive file exceeds recorded size: {member.name!r}")
                os.fchmod(descriptor, 0o755 if member.mode & 0o111 else 0o644)
            finally:
                os.close(descriptor)


def _write_deterministic_store_tar(store: Path, archive: Path) -> None:
    paths = sorted(store.rglob("*"), key=lambda path: path.relative_to(store).as_posix())
    with tarfile.open(archive, "w", format=tarfile.USTAR_FORMAT) as output:
        for path in paths:
            relative = path.relative_to(store).as_posix()
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                raise ReleaseError(f"boot-object store contains a symlink: {relative}")
            information = tarfile.TarInfo(
                relative + ("/" if stat.S_ISDIR(metadata.st_mode) else "")
            )
            information.uid = 0
            information.gid = 0
            information.uname = ""
            information.gname = ""
            information.mtime = SOURCE_DATE_EPOCH
            if stat.S_ISDIR(metadata.st_mode):
                information.type = tarfile.DIRTYPE
                information.mode = 0o755
                output.addfile(information)
            elif stat.S_ISREG(metadata.st_mode):
                information.type = tarfile.REGTYPE
                information.mode = 0o444
                information.size = metadata.st_size
                with path.open("rb") as stream:
                    output.addfile(information, stream)
            else:
                raise ReleaseError(f"boot-object store contains a special file: {relative}")
    archive.chmod(0o444)


def create_boot_object_snapshot(
    repo: Path,
    snapshot: SourceSnapshot,
    temporary_root: Path,
) -> BootObjectSnapshot:
    """Build and twice verify the exact tree's platform-neutral object store."""

    source_root = temporary_root / "boot-object-source"
    store = temporary_root / "boot-objects"
    archive = temporary_root / "boot-objects.tar"
    _extract_regular_snapshot(snapshot.archive, source_root)
    command = [
        sys.executable,
        str(ROOT / "scripts/ostadix_boot_objects.py"),
        "build",
        "--repo",
        str(repo),
        "--commit",
        snapshot.head,
        "--tree",
        snapshot.tree,
        "--source-root",
        str(source_root),
        "--output",
        str(store),
        "--json",
    ]
    try:
        summary = json.loads(_invoke(command).stdout)
    except json.JSONDecodeError as error:
        raise ReleaseError("boot-object builder returned invalid JSON") from error
    required = {
        "schema": "ostadix.boot-object-store-result/v1",
        "ok": True,
        "operation": "build",
        "commit": snapshot.head,
        "tree": snapshot.tree,
    }
    if not isinstance(summary, dict) or any(summary.get(key) != value for key, value in required.items()):
        raise ReleaseError("boot-object builder did not bind the exact staged snapshot")
    for field in ("object_count", "binding_count", "logical_bytes", "stored_bytes"):
        if type(summary.get(field)) is not int or summary[field] <= 0:
            raise ReleaseError(f"boot-object builder returned invalid {field}")
    root = summary.get("root_sha256")
    if not isinstance(root, str) or len(root) != 64 or any(c not in "0123456789abcdef" for c in root):
        raise ReleaseError("boot-object builder returned an invalid root SHA-256")
    verify = [
        sys.executable,
        str(ROOT / "scripts/ostadix_boot_objects.py"),
        "verify",
        "--store",
        str(store),
        "--commit",
        snapshot.head,
        "--tree",
        snapshot.tree,
        "--source-root",
        str(source_root),
        "--json",
    ]
    try:
        verified = json.loads(_invoke(verify).stdout)
    except json.JSONDecodeError as error:
        raise ReleaseError("boot-object verifier returned invalid JSON") from error
    for field in (
        "commit",
        "tree",
        "root_sha256",
        "object_count",
        "binding_count",
        "logical_bytes",
        "stored_bytes",
    ):
        if verified.get(field) != summary.get(field):
            raise ReleaseError(f"boot-object build/verify disagreement for {field}")
    _write_deterministic_store_tar(store, archive)
    return BootObjectSnapshot(
        archive=archive,
        archive_sha256=_sha256(archive),
        summary=summary,
    )


def receipt_path_for(output: Path) -> Path:
    return output.with_name(output.name + ".release.json")


def publication_lock_path_for(output: Path) -> Path:
    return output.with_name(f".{output.name}.release.lock")


@contextmanager
def _publication_lock(output: Path):
    """Serialize all repository-owned state transitions for one host output."""

    path = publication_lock_path_for(output)
    flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_CLOEXEC", 0)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise ReleaseError(f"publication lock is not a regular file: {path}")
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield
    finally:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_UN)
        finally:
            os.close(descriptor)


def default_output_for(tree: str) -> Path:
    if len(tree) != 40 or any(character not in "0123456789abcdef" for character in tree):
        raise ReleaseError("cannot derive a release output from an invalid staged tree OID")
    return DEFAULT_OUTPUT_DIRECTORY / (
        f"ostadix-hosted-live-x86_64-uefi-{tree[:12]}_VTGRUB2.iso"
    )


def validate_no_clobber(output: Path) -> Path:
    receipt = receipt_path_for(output)
    for path in (output, receipt):
        if path.is_symlink() or path.exists():
            raise ReleaseError(f"refusing to clobber existing release path: {path}")
    return receipt


def assert_snapshot_still_current(repo: Path, snapshot: SourceSnapshot) -> None:
    dirty = _git(repo, "diff", "--quiet", "--no-ext-diff", "--", check=False)
    if dirty.returncode == 1:
        raise ReleaseError("tracked files gained unstaged changes during the release build")
    if dirty.returncode != 0:
        raise ReleaseError("git diff failed while revalidating the release source boundary")
    if _git(repo, "write-tree").stdout.strip() != snapshot.tree:
        raise ReleaseError("staged Git tree changed during the release build")
    if _git(repo, "rev-parse", "HEAD").stdout.strip() != snapshot.head:
        raise ReleaseError("HEAD changed during the release build")


class MultipassClient:
    def __init__(
        self,
        executable: str,
        vm: str,
        *,
        runner: RunCallable | None = None,
    ) -> None:
        self.executable = executable
        self.vm = vm
        self.runner = runner

    def _run(
        self, arguments: Sequence[str], *, capture_output: bool = True
    ) -> subprocess.CompletedProcess[str]:
        return _invoke(
            [self.executable, *arguments],
            capture_output=capture_output,
            runner=self.runner,
        )

    def ensure_running(self) -> dict[str, object]:
        def information() -> dict[str, object]:
            result = self._run(["info", "--format", "json", self.vm])
            try:
                payload = json.loads(result.stdout)
                return payload["info"][self.vm]
            except (KeyError, TypeError, json.JSONDecodeError) as error:
                raise ReleaseError("multipass returned malformed VM information") from error

        info = information()
        state = info.get("state")
        if state == "Stopped":
            self._run(["start", self.vm], capture_output=False)
            info = information()
            state = info.get("state")
        if state != "Running":
            raise ReleaseError(f"Multipass VM {self.vm!r} is not running: state={state!r}")
        return info

    def execute(
        self, arguments: Sequence[str], *, capture_output: bool = True
    ) -> subprocess.CompletedProcess[str]:
        return self._run(
            ["exec", self.vm, "--", *arguments], capture_output=capture_output
        )

    def transfer_to_guest(self, source: Path, destination: str) -> None:
        self._run(["transfer", str(source), f"{self.vm}:{destination}"], capture_output=False)

    def transfer_from_guest(self, source: str, destination: Path) -> None:
        self._run(["transfer", f"{self.vm}:{source}", str(destination)], capture_output=False)


def _exclusive_json(path: Path, payload: dict[str, object]) -> None:
    encoded = (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode("utf-8")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags, 0o444)
    try:
        remaining = memoryview(encoded)
        while remaining:
            written = os.write(descriptor, remaining)
            if written <= 0:
                raise OSError("short write while publishing hosted-live receipt")
            remaining = remaining[written:]
        os.fchmod(descriptor, 0o444)
        os.fsync(descriptor)
    except BaseException:
        os.close(descriptor)
        path.unlink(missing_ok=True)
        raise
    else:
        os.close(descriptor)


def verify_guest_archive(
    client: MultipassClient, archive: Path, expected_sha256: str
) -> None:
    """Bind extraction to bytes actually received inside the guest."""

    result = client.execute(["sha256sum", "--", str(archive)])
    fields = result.stdout.strip().split()
    if len(fields) < 2 or fields[0] != expected_sha256:
        actual = fields[0] if fields else "unparseable"
        raise ReleaseError(
            "transferred staged archive failed guest SHA-256 verification: "
            f"expected {expected_sha256}, got {actual}"
        )


def verify_guest_native_paths(
    client: MultipassClient, paths: Sequence[Path]
) -> list[dict[str, object]]:
    """Require release inputs/outputs to resolve on native guest filesystems."""

    program = r'''
import json
import os
from pathlib import Path
import sys

def unescape(value):
    for encoded, decoded in (("\\040", " "), ("\\011", "\t"), ("\\012", "\n"), ("\\134", "\\")):
        value = value.replace(encoded, decoded)
    return value

mounts = []
for line in Path("/proc/self/mountinfo").read_text(encoding="utf-8").splitlines():
    fields = line.split()
    separator = fields.index("-")
    mounts.append((unescape(fields[4]), fields[separator + 1]))

result = []
for value in sys.argv[1:]:
    resolved = os.path.realpath(value)
    candidates = [
        item for item in mounts
        if resolved == item[0] or resolved.startswith(item[0].rstrip("/") + "/")
    ]
    mount_point, filesystem = max(candidates, key=lambda item: len(item[0]))
    anchor = Path(resolved)
    while not anchor.exists():
        anchor = anchor.parent
    state = anchor.stat()
    result.append({
        "requested": value,
        "resolved": resolved,
        "mount_point": mount_point,
        "filesystem": filesystem,
        "ownership_anchor": str(anchor),
        "owner_uid": state.st_uid,
        "owner_gid": state.st_gid,
        "guest_uid": os.getuid(),
        "mode": state.st_mode & 0o7777,
    })
print(json.dumps(result, sort_keys=True, separators=(",", ":")))
'''
    result = client.execute(["python3", "-c", program, *(str(path) for path in paths)])
    try:
        bindings = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ReleaseError("guest returned malformed native-path boundary evidence") from error
    if not isinstance(bindings, list) or len(bindings) != len(paths):
        raise ReleaseError("guest returned incomplete native-path boundary evidence")
    admitted: list[dict[str, object]] = []
    for binding in bindings:
        if not isinstance(binding, dict) or not all(
            isinstance(binding.get(key), str)
            for key in (
                "requested",
                "resolved",
                "mount_point",
                "filesystem",
                "ownership_anchor",
            )
        ) or not all(
            type(binding.get(key)) is int
            for key in ("owner_uid", "owner_gid", "guest_uid", "mode")
        ):
            raise ReleaseError("guest returned invalid native-path boundary evidence")
        if binding["requested"] != binding["resolved"]:
            raise ReleaseError(
                "release path traversed a symlinked guest component: "
                f"{binding['requested']} -> {binding['resolved']}"
            )
        filesystem = binding["filesystem"].lower()
        if filesystem not in NATIVE_GUEST_FILESYSTEMS:
            raise ReleaseError(
                "release source/run/output/cache is not on an admitted native filesystem: "
                f"{binding['resolved']} ({binding['filesystem']})"
            )
        if binding["owner_uid"] not in (0, binding["guest_uid"]):
            raise ReleaseError(
                "release path ownership is neither guest-user nor guest-root: "
                f"{binding['ownership_anchor']} uid={binding['owner_uid']}"
            )
        if binding["mode"] & 0o002:
            raise ReleaseError(
                "release path ownership anchor is world-writable: "
                f"{binding['ownership_anchor']}"
            )
        admitted.append({key: binding[key] for key in sorted(binding)})
    return admitted


def _best_effort_guest_cleanup(
    client: MultipassClient,
    guest_run: Path,
    *,
    publication_succeeded: bool,
) -> None:
    """Remove only the random private run without changing the primary outcome."""

    try:
        client.execute(
            ["sudo", "-n", "rm", "-rf", "--", str(guest_run)],
            capture_output=False,
        )
    except BaseException as error:
        outcome = (
            "published release remains valid"
            if publication_succeeded
            else "failed or interrupted guest run may remain"
        )
        print(
            f"hosted-live-release: WARNING: guest cleanup failed for {guest_run}; "
            f"{outcome}: {error}",
            file=sys.stderr,
        )


def _read_existing_receipt(path: Path) -> dict[str, object]:
    if path.is_symlink() or not path.is_file():
        raise ReleaseError(f"release receipt is not a regular non-symlink file: {path}")
    if stat.S_IMODE(path.stat().st_mode) & 0o222:
        raise ReleaseError(f"release receipt is not read-only: {path}")
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ReleaseError(f"release receipt is unreadable or malformed: {path}") from error
    if not isinstance(payload, dict):
        raise ReleaseError(f"release receipt is not a JSON object: {path}")
    return payload


def _validate_receipt_binding(
    payload: dict[str, object],
    *,
    output: Path,
    receipt: Path,
    inspection: dict[str, object],
    snapshot: SourceSnapshot,
) -> None:
    source = payload.get("source")
    publication = payload.get("host_publication")
    iso = payload.get("iso")
    if payload.get("schema") != "ostadix.hosted-live-release/v6":
        raise ReleaseError("existing receipt has an unexpected schema")
    if not isinstance(source, dict) or (
        source.get("staged_tree") != snapshot.tree
        or source.get("base_commit") != snapshot.head
        or source.get("archive_sha256") != snapshot.archive_sha256
    ):
        raise ReleaseError("existing receipt is not bound to the current staged snapshot")
    if not isinstance(publication, dict) or (
        publication.get("output") != str(output)
        or publication.get("receipt") != str(receipt)
        or publication.get("sha256") != inspection.get("sha256")
        or publication.get("bytes") != inspection.get("bytes")
        or publication.get("branch") != snapshot.branch
        or publication.get("origin") != snapshot.origin
        or not isinstance(publication.get("published_utc"), str)
    ):
        raise ReleaseError("existing receipt is not bound to the exact publication paths/ISO")
    if not isinstance(iso, dict) or iso != inspection:
        raise ReleaseError("existing receipt's strict ISO identity differs from the candidate")

    def sha256_hex(value: object) -> bool:
        return (
            isinstance(value, str)
            and len(value) == 64
            and all(character in "0123456789abcdef" for character in value)
        )

    def identity(value: object, label: str) -> dict[str, object]:
        if not isinstance(value, dict):
            raise ReleaseError(f"existing receipt omitted {label} identity")
        size = value.get("bytes")
        digest = value.get("sha256")
        if type(size) is not int or size <= 0 or not sha256_hex(digest):
            raise ReleaseError(f"existing receipt has an invalid {label} identity")
        return value

    def entropy_evidence(value: object, label: str) -> dict[str, object]:
        if not isinstance(value, dict) or set(value) != {
            "device",
            "crng_bytes",
            "available",
        }:
            raise ReleaseError(f"existing receipt omitted {label} entropy evidence")
        if (
            value.get("device") != QEMU_ENTROPY_DEVICE
            or value.get("crng_bytes") != ENTROPY_PROBE_BYTES
            or type(value.get("available")) is not int
            or value["available"] < MIN_GUEST_ENTROPY_BITS
        ):
            raise ReleaseError(f"existing receipt has invalid {label} entropy evidence")
        return value

    if not sha256_hex(source.get("boot_objects_archive_sha256")):
        raise ReleaseError("existing receipt omitted the boot-object archive identity")
    boot_objects = source.get("boot_objects")
    if not isinstance(boot_objects, dict) or (
        boot_objects.get("schema") != "ostadix.boot-object-store-result/v1"
        or boot_objects.get("ok") is not True
        or boot_objects.get("operation") != "verify"
        or boot_objects.get("commit") != snapshot.head
        or boot_objects.get("tree") != snapshot.tree
        or not sha256_hex(boot_objects.get("root_sha256"))
    ):
        raise ReleaseError("existing receipt omitted the verified staged boot-object binding")
    for field in ("object_count", "binding_count", "logical_bytes", "stored_bytes"):
        if type(boot_objects.get(field)) is not int or boot_objects[field] <= 0:
            raise ReleaseError(f"existing receipt has invalid boot-object {field}")
    if (
        boot_objects["object_count"] > boot_objects["binding_count"]
        or boot_objects["stored_bytes"] > boot_objects["logical_bytes"]
    ):
        raise ReleaseError("existing receipt has an impossible boot-object closure")

    required_iso_fields = {
        "schema",
        "architecture",
        "volume_id",
        "default_entry",
        "bytes",
        "sha256",
        "entries",
        "artifacts",
        "capacity_lock_bytes",
        "capacity_lock_sha256",
        "efi_boot_image_bytes",
        "efi_boot_image_sha256",
        "efi_bootloader_bytes",
        "efi_bootloader_sha256",
        "grub_config_bytes",
        "grub_config_sha256",
    }
    if not required_iso_fields.issubset(inspection):
        raise ReleaseError("existing receipt omitted full strict ISO metadata")
    if (
        inspection.get("schema") != "ostadix.capacity-iso/v1"
        or inspection.get("architecture") != "x86_64"
        or inspection.get("volume_id") != "OSTADIX_CAPACITY"
        or inspection.get("default_entry") != "hosted"
    ):
        raise ReleaseError("existing receipt has invalid strict ISO invariants")
    capacity_arguments = [
        "console=tty0",
        "console=ttyS0,115200n8",
        "rdinit=/init",
        "panic=0",
        "loglevel=4",
    ]
    expected_entries = [
        {
            "id": "hosted",
            "title": "OSTADIX Hosted Workstation [physical x86_64]",
            "hotkey": "h",
            "adapter": "linux-live-rootfs",
            "arguments": [
                "console=ttyS0,115200n8",
                "console=tty0",
                "rdinit=/init",
                "panic=0",
                "loglevel=7",
                "ignore_loglevel",
            ],
            "kernel_path": "/boot/hosted/vmlinuz-lts",
            "initrd_paths": ["/boot/hosted/initramfs.cpio.gz"],
            "selection_id": "hosted",
            "rootfs_path": "/boot/hosted/rootfs.squashfs",
            "modloop_path": "/boot/modloop-lts",
        },
        {
            "id": "ocore",
            "title": "OSTADIX O-core [direct Multiboot2, serial console]",
            "hotkey": "o",
            "adapter": "multiboot2",
            "arguments": [],
            "kernel_path": "/boot/ocore/kernel.elf",
        },
        {
            "id": "alpine",
            "title": "Alpine Linux 3.24.1 [direct kernel/initramfs]",
            "hotkey": "a",
            "adapter": "linux",
            "arguments": [
                "console=tty0",
                "console=ttyS0,115200n8",
                "rdinit=/bin/sh",
                "panic=0",
                "loglevel=4",
            ],
            "kernel_path": "/boot/capacity-host/vmlinuz-virt",
            "initrd_paths": ["/boot/entry/010-alpine/initramfs-virt"],
        },
        {
            "id": "guix",
            "title": "GNU Guix System 1.5.0 [virtualized/TCG]",
            "hotkey": "g",
            "adapter": "qemu-tcg-linux-direct",
            "arguments": capacity_arguments,
            "host_kernel_path": "/boot/capacity-host/vmlinuz-virt",
            "host_initrd_path": "/boot/capacity-host/initramfs.cpio.gz",
            "selection_id": "guix-system-1.5.0-x86_64",
            "guest_artifact_paths": [
                "/ostadix/guix/linux-libre-6.17.12-bzimage",
                "/ostadix/guix/guix-1.5.0-initrd.cpio.gz",
                "/ostadix/guix/guix-system-install-1.5.0.x86_64-linux.iso",
            ],
        },
        {
            "id": "openbsd",
            "title": "OpenBSD 7.9 offline installer [virtualized/TCG]",
            "hotkey": "b",
            "adapter": "qemu-tcg-raw-cd-curses",
            "arguments": capacity_arguments,
            "host_kernel_path": "/boot/capacity-host/vmlinuz-virt",
            "host_initrd_path": "/boot/capacity-host/initramfs.cpio.gz",
            "selection_id": "openbsd-7.9-amd64",
            "guest_artifact_paths": ["/ostadix/openbsd/install79.iso"],
        },
        {
            "id": "plan9",
            "title": "9front Plan 9 build 11983 [virtualized/TCG]",
            "hotkey": "p",
            "adapter": "qemu-tcg-qcow2",
            "arguments": capacity_arguments,
            "host_kernel_path": "/boot/capacity-host/vmlinuz-virt",
            "host_initrd_path": "/boot/capacity-host/initramfs.cpio.gz",
            "selection_id": "plan9-9front-11983-amd64",
            "guest_artifact_paths": [
                "/ostadix/9front/9front-11983.amd64.qcow2"
            ],
        },
        {
            "id": "redox",
            "title": "Redox OS 0.9.0 [virtualized/TCG]",
            "hotkey": "r",
            "adapter": "qemu-tcg-raw-cd",
            "arguments": capacity_arguments,
            "host_kernel_path": "/boot/capacity-host/vmlinuz-virt",
            "host_initrd_path": "/boot/capacity-host/initramfs.cpio.gz",
            "selection_id": "redox-0.9.0-server-x86_64",
            "guest_artifact_paths": [
                "/ostadix/redox/redox-server-0.9.0-livedisk.iso"
            ],
        },
    ]
    entries = inspection.get("entries")
    if entries != expected_entries:
        raise ReleaseError("existing receipt omitted the exact seven-entry boot closure")
    artifacts = inspection.get("artifacts")
    if (
        not isinstance(artifacts, list)
        or len(artifacts) != 14
        or not all(isinstance(artifact, dict) for artifact in artifacts)
    ):
        raise ReleaseError("existing receipt omitted the exact 14 typed ISO artifacts")
    artifact_closure = {
        (artifact.get("iso_path"), artifact.get("role"))
        for artifact in artifacts
        if isinstance(artifact, dict)
    }
    artifact_by_path = {artifact["iso_path"]: artifact for artifact in artifacts}
    if artifact_closure != {
        ("/boot/hosted/vmlinuz-lts", "linux-kernel"),
        ("/boot/hosted/initramfs.cpio.gz", "linux-initrd"),
        ("/boot/hosted/rootfs.squashfs", "linux-rootfs"),
        ("/boot/modloop-lts", "linux-modloop"),
        ("/boot/ocore/kernel.elf", "ocore-kernel"),
        ("/boot/capacity-host/vmlinuz-virt", "linux-kernel"),
        ("/boot/capacity-host/initramfs.cpio.gz", "linux-initrd"),
        ("/boot/entry/010-alpine/initramfs-virt", "linux-initrd"),
        ("/ostadix/guix/linux-libre-6.17.12-bzimage", "linux-kernel"),
        ("/ostadix/guix/guix-1.5.0-initrd.cpio.gz", "linux-initrd"),
        (
            "/ostadix/guix/guix-system-install-1.5.0.x86_64-linux.iso",
            "guest-rootfs",
        ),
        ("/ostadix/openbsd/install79.iso", "guest-raw-cd"),
        ("/ostadix/9front/9front-11983.amd64.qcow2", "guest-qcow2"),
        ("/ostadix/redox/redox-server-0.9.0-livedisk.iso", "guest-raw-cd"),
    }:
        raise ReleaseError("existing receipt has the wrong 14-artifact ISO closure")
    for index, artifact in enumerate(artifacts):
        identity(artifact, f"ISO artifact {index}")
        if not isinstance(artifact.get("iso_path"), str) or not isinstance(
            artifact.get("role"), str
        ):
            raise ReleaseError("existing receipt has malformed strict ISO artifact metadata")
    identity(inspection, "ISO")
    for prefix in ("capacity_lock", "efi_boot_image", "efi_bootloader", "grub_config"):
        identity(
            {"bytes": inspection.get(f"{prefix}_bytes"), "sha256": inspection.get(f"{prefix}_sha256")},
            prefix,
        )

    build = payload.get("build")
    if not isinstance(build, dict) or (
        build.get("host_architecture") not in ("aarch64", "arm64", "x86_64")
        or
        build.get("target") != "x86_64-unknown-linux-musl"
        or not isinstance(build.get("rust_toolchain"), str)
        or not build["rust_toolchain"].startswith("rustc 1.97.1 ")
        or build.get("cargo_build_jobs") != 1
        or build.get("cargo_codegen_units") != 16
        or build.get("cargo_lto") is not False
        or build.get("source_date_epoch") != 315532800
        or build.get("musl_dev_version") != "1.2.6-r2"
        or build.get("sysroot_package_lock") != list(EXPECTED_SYSROOT_PACKAGE_LOCK)
        or build.get("workstation_package_roots")
        != list(EXPECTED_WORKSTATION_PACKAGE_ROOTS)
        or build.get("workstation_source_path") != "/usr/src/ostadix"
    ):
        raise ReleaseError("existing receipt omitted the exact hosted build/toolchain lock")
    identity(build.get("sysroot_manifest"), "sysroot manifest")
    identity(build.get("hosted_live_package_lock"), "hosted-live package lock")
    identity(build.get("cargo_vendor_manifest"), "Cargo vendor manifest")
    if build.get("ocore") != {
        "compiler_target": "x86_64-unknown-none",
        "assembler_target": "x86_64-unknown-none-elf",
        "probe_mode": 0,
        "boot_info_enabled": True,
        "linker": "ld.lld",
        "cargo_build_jobs": 1,
        "cargo_offline": True,
    }:
        raise ReleaseError("existing receipt omitted the exact O-core build profile")
    apk_boundary = build.get("apk_repository_boundary")
    if apk_boundary != {
        "exact_versions": True,
        "signed_index_and_packages": True,
        "independent_apk_blob_hash_lock": False,
        "repository_availability_required": True,
    }:
        raise ReleaseError("existing receipt omitted the signed APK repository boundary")
    cache = build.get("cache_inputs")
    if not isinstance(cache, dict) or set(cache) != {
        "alpine_minirootfs",
        "alpine_lts_kernel",
        "alpine_lts_initramfs",
        "alpine_lts_modloop",
    }:
        raise ReleaseError("existing receipt omitted the exact physical cache inputs")
    minirootfs = identity(cache.get("alpine_minirootfs"), "Alpine minirootfs")
    if minirootfs != {"bytes": PINNED_MINIROOTFS_BYTES, "sha256": PINNED_MINIROOTFS_SHA256}:
        raise ReleaseError("existing receipt has the wrong pinned Alpine minirootfs identity")
    lts_kernel = identity(cache.get("alpine_lts_kernel"), "Alpine LTS kernel")
    if lts_kernel != {
        "bytes": PINNED_LTS_KERNEL_BYTES,
        "sha256": PINNED_LTS_KERNEL_SHA256,
    }:
        raise ReleaseError("existing receipt has the wrong pinned Alpine LTS kernel identity")
    lts_initramfs = identity(cache.get("alpine_lts_initramfs"), "Alpine LTS initramfs")
    if lts_initramfs != {
        "bytes": PINNED_LTS_INITRAMFS_BYTES,
        "sha256": PINNED_LTS_INITRAMFS_SHA256,
    }:
        raise ReleaseError("existing receipt has the wrong pinned Alpine LTS initramfs identity")
    lts_modloop = identity(cache.get("alpine_lts_modloop"), "Alpine LTS modloop")
    if lts_modloop != {
        "bytes": PINNED_LTS_MODLOOP_BYTES,
        "sha256": PINNED_LTS_MODLOOP_SHA256,
    }:
        raise ReleaseError("existing receipt has the wrong pinned Alpine LTS modloop identity")

    binaries = payload.get("binaries")
    if not isinstance(binaries, dict) or set(binaries) != REQUIRED_HOSTED_BINARIES:
        raise ReleaseError("existing receipt omitted the exact hosted binary set")
    for name in sorted(binaries):
        identity(binaries[name], f"hosted binary {name}")
    rootfs_objects = payload.get("rootfs_objects")
    if not isinstance(rootfs_objects, dict) or set(rootfs_objects) != {
        "olangc_wasm_hello"
    }:
        raise ReleaseError("existing receipt omitted the exact SquashFS object set")
    wasm_object = rootfs_objects.get("olangc_wasm_hello")
    if not isinstance(wasm_object, dict) or set(wasm_object) != {
        "manifest_path",
        "artifact_path",
        "manifest",
        "descriptor",
    } or (
        wasm_object.get("manifest_path")
        != "/usr/share/ostadix/wasm/hello.release.json"
        or wasm_object.get("artifact_path") != "/usr/share/ostadix/wasm/hello.wasm"
    ):
        raise ReleaseError("existing receipt has the wrong Olangc WASM rootfs object paths")
    wasm_manifest_identity = identity(
        wasm_object.get("manifest"), "Olangc WASM release manifest"
    )
    wasm_descriptor = wasm_object.get("descriptor")
    if not isinstance(wasm_descriptor, dict) or set(wasm_descriptor) != {
        "schema",
        "source",
        "input",
        "generator",
        "project",
        "artifact",
        "build",
    } or wasm_descriptor.get("schema") != "ostadix.olangc-wasm-release/v1":
        raise ReleaseError("existing receipt has an invalid Olangc WASM descriptor")
    canonical_descriptor = (
        json.dumps(wasm_descriptor, indent=2, sort_keys=True) + "\n"
    ).encode("utf-8")
    if wasm_manifest_identity != {
        "bytes": len(canonical_descriptor),
        "sha256": hashlib.sha256(canonical_descriptor).hexdigest(),
    }:
        raise ReleaseError(
            "existing receipt's Olangc WASM manifest identity differs from its descriptor"
        )
    wasm_source = wasm_descriptor.get("source")
    if not isinstance(wasm_source, dict) or wasm_source != {
        "staged_tree": snapshot.tree,
        "base_commit": snapshot.head,
        "archive_sha256": snapshot.archive_sha256,
    }:
        raise ReleaseError("existing receipt's Olangc WASM source binding differs")
    wasm_input = wasm_descriptor.get("input")
    if (
        not isinstance(wasm_input, dict)
        or wasm_input.get("path") != "examples/wasm_hello.O"
    ):
        raise ReleaseError("existing receipt has the wrong Olangc WASM source input")
    identity(wasm_input, "Olangc WASM source input")
    wasm_generator = wasm_descriptor.get("generator")
    if (
        not isinstance(wasm_generator, dict)
        or wasm_generator.get("path") != "/usr/local/bin/olangc"
        or {
            "bytes": wasm_generator.get("bytes"),
            "sha256": wasm_generator.get("sha256"),
        }
        != binaries["olangc"]
    ):
        raise ReleaseError("existing receipt's Olangc WASM generator differs")
    wasm_project = wasm_descriptor.get("project")
    if not isinstance(wasm_project, dict) or set(wasm_project) != {
        "file_count",
        "logical_bytes",
        "root_sha256",
    } or (
        type(wasm_project.get("file_count")) is not int
        or wasm_project["file_count"] <= 0
        or type(wasm_project.get("logical_bytes")) is not int
        or wasm_project["logical_bytes"] <= 0
        or not sha256_hex(wasm_project.get("root_sha256"))
    ):
        raise ReleaseError("existing receipt has an invalid materialized WASM project")
    wasm_artifact = wasm_descriptor.get("artifact")
    if (
        not isinstance(wasm_artifact, dict)
        or wasm_artifact.get("path") != "/usr/share/ostadix/wasm/hello.wasm"
    ):
        raise ReleaseError("existing receipt has the wrong Olangc WASM artifact path")
    identity(wasm_artifact, "Olangc WASM artifact")
    wasm_build = wasm_descriptor.get("build")
    if not isinstance(wasm_build, dict) or set(wasm_build) != {
        "target",
        "profile",
        "opt_level",
        "lto",
        "codegen_units",
        "cargo_offline",
        "rust_toolchain",
    } or (
        wasm_build.get("target") != "wasm32-wasip1"
        or wasm_build.get("profile") != "release"
        or wasm_build.get("opt_level") != 1
        or wasm_build.get("lto") is not False
        or wasm_build.get("codegen_units") != 16
        or wasm_build.get("cargo_offline") is not True
        or not isinstance(wasm_build.get("rust_toolchain"), str)
        or not wasm_build["rust_toolchain"].startswith("rustc 1.97.1 ")
    ):
        raise ReleaseError("existing receipt has the wrong Olangc WASM build contract")
    expected_wasm_evidence = {
        "staged_tree": snapshot.tree,
        "bytes": wasm_artifact["bytes"],
        "sha256": wasm_artifact["sha256"],
        "materialized_project_sha256": wasm_project["root_sha256"],
    }
    capacity = payload.get("capacity")
    if not isinstance(capacity, dict) or set(capacity) != {
        "host_initramfs",
        "foreign_manifest",
        "package_lock",
        "guest_verification",
        "virt_modloop",
        "boot_routes",
    }:
        raise ReleaseError("existing receipt omitted the exact foreign-capacity closure")
    capacity_host_initramfs = identity(
        capacity.get("host_initramfs"), "capacity-host initramfs"
    )
    identity(capacity.get("foreign_manifest"), "foreign-kernel manifest")
    identity(capacity.get("package_lock"), "capacity-host package lock")
    virt_modloop = identity(capacity.get("virt_modloop"), "Alpine virt modloop")
    if virt_modloop != {
        "bytes": PINNED_VIRT_MODLOOP_BYTES,
        "sha256": PINNED_VIRT_MODLOOP_SHA256,
    }:
        raise ReleaseError("existing receipt has the wrong pinned Alpine virt modloop identity")
    if capacity.get("boot_routes") != {
        "direct": ["hosted", "ocore", "alpine"],
        "nested_qemu_tcg": ["guix", "openbsd", "plan9", "redox"],
    }:
        raise ReleaseError("existing receipt has the wrong direct/QEMU boot-route split")
    guest_verification = capacity.get("guest_verification")
    if not isinstance(guest_verification, dict) or set(guest_verification) != {
        "identity",
        "records",
    }:
        raise ReleaseError("existing receipt omitted exact foreign guest verification")
    guest_records = guest_verification.get("records")
    if not isinstance(guest_records, list) or not all(
        isinstance(record, str) and record for record in guest_records
    ):
        raise ReleaseError("existing receipt has malformed foreign guest verification records")
    encoded_guest_records = ("\n".join(guest_records) + "\n").encode("utf-8")
    if identity(
        guest_verification.get("identity"), "foreign guest verification record"
    ) != {
        "bytes": len(encoded_guest_records),
        "sha256": hashlib.sha256(encoded_guest_records).hexdigest(),
    }:
        raise ReleaseError("existing receipt's foreign guest verification identity differs")
    verified_guests: dict[tuple[str, str], dict[str, object]] = {}
    for record in guest_records:
        parts = record.split()
        if len(parts) != 5 or parts[0] != "verified":
            raise ReleaseError("existing receipt has malformed foreign guest verification records")
        fields: dict[str, str] = {}
        for token in parts[1:]:
            key, separator, value = token.partition("=")
            if not separator or not key or not value or key in fields:
                raise ReleaseError(
                    "existing receipt has malformed foreign guest verification records"
                )
            fields[key] = value
        if set(fields) != {"guest", "artifact", "size", "sha256"}:
            raise ReleaseError("existing receipt has malformed foreign guest verification records")
        try:
            size = int(fields["size"])
        except ValueError as error:
            raise ReleaseError(
                "existing receipt has malformed foreign guest verification records"
            ) from error
        key = (fields["guest"], fields["artifact"])
        if key in verified_guests or size <= 0 or not sha256_hex(fields["sha256"]):
            raise ReleaseError("existing receipt has malformed foreign guest verification records")
        verified_guests[key] = {"bytes": size, "sha256": fields["sha256"]}
    expected_guest_records = {
        ("linux-alpine-3.24.1-x86_64", "kernel"),
        ("linux-alpine-3.24.1-x86_64", "initramfs"),
        ("guix-system-1.5.0-x86_64", "media"),
        ("guix-system-1.5.0-x86_64", "media_signature"),
        ("guix-system-1.5.0-x86_64", "kernel"),
        ("guix-system-1.5.0-x86_64", "initrd"),
        ("plan9-9front-11983-amd64", "disk_gz"),
        ("plan9-9front-11983-amd64", "disk"),
        ("redox-0.9.0-server-x86_64", "media_zst"),
        ("redox-0.9.0-server-x86_64", "media"),
        ("openbsd-7.9-amd64", "media"),
    }
    if set(verified_guests) != expected_guest_records:
        raise ReleaseError("existing receipt omitted the exact verified foreign guest set")
    guest_artifact_bindings = {
        "/boot/capacity-host/vmlinuz-virt": (
            "linux-alpine-3.24.1-x86_64",
            "kernel",
        ),
        "/boot/entry/010-alpine/initramfs-virt": (
            "linux-alpine-3.24.1-x86_64",
            "initramfs",
        ),
        "/ostadix/guix/linux-libre-6.17.12-bzimage": (
            "guix-system-1.5.0-x86_64",
            "kernel",
        ),
        "/ostadix/guix/guix-1.5.0-initrd.cpio.gz": (
            "guix-system-1.5.0-x86_64",
            "initrd",
        ),
        "/ostadix/guix/guix-system-install-1.5.0.x86_64-linux.iso": (
            "guix-system-1.5.0-x86_64",
            "media",
        ),
        "/ostadix/openbsd/install79.iso": ("openbsd-7.9-amd64", "media"),
        "/ostadix/9front/9front-11983.amd64.qcow2": (
            "plan9-9front-11983-amd64",
            "disk",
        ),
        "/ostadix/redox/redox-server-0.9.0-livedisk.iso": (
            "redox-0.9.0-server-x86_64",
            "media",
        ),
    }
    for path, verified_key in guest_artifact_bindings.items():
        artifact = artifact_by_path[path]
        if {
            "bytes": artifact.get("bytes"),
            "sha256": artifact.get("sha256"),
        } != verified_guests[verified_key]:
            raise ReleaseError(
                "existing receipt's ISO foreign artifacts differ from guest verification"
            )
    capacity_host_artifact = artifact_by_path[
        "/boot/capacity-host/initramfs.cpio.gz"
    ]
    if {
        "bytes": capacity_host_artifact.get("bytes"),
        "sha256": capacity_host_artifact.get("sha256"),
    } != capacity_host_initramfs:
        raise ReleaseError(
            "existing receipt's capacity-host initramfs differs from its ISO artifact"
        )
    initramfs = identity(payload.get("initramfs"), "hosted initramfs")
    rootfs = identity(payload.get("rootfs"), "hosted SquashFS root")
    ventoy_modloop = identity(payload.get("ventoy_modloop"), "Ventoy compatibility modloop")
    if {
        "bytes": artifact_by_path["/boot/hosted/vmlinuz-lts"].get("bytes"),
        "sha256": artifact_by_path["/boot/hosted/vmlinuz-lts"].get("sha256"),
    } != lts_kernel:
        raise ReleaseError("existing receipt's Hosted entry uses the wrong pinned kernel")
    if {
        "bytes": artifact_by_path["/boot/hosted/initramfs.cpio.gz"].get("bytes"),
        "sha256": artifact_by_path["/boot/hosted/initramfs.cpio.gz"].get("sha256"),
    } != initramfs:
        raise ReleaseError("existing receipt's Hosted entry uses a different initramfs")
    if {
        "bytes": artifact_by_path["/boot/hosted/rootfs.squashfs"].get("bytes"),
        "sha256": artifact_by_path["/boot/hosted/rootfs.squashfs"].get("sha256"),
    } != rootfs:
        raise ReleaseError("existing receipt's Hosted entry uses a different SquashFS root")
    if {
        "bytes": artifact_by_path["/boot/modloop-lts"].get("bytes"),
        "sha256": artifact_by_path["/boot/modloop-lts"].get("sha256"),
    } != ventoy_modloop:
        raise ReleaseError("existing receipt's Hosted entry uses a different Ventoy modloop")
    ocore_kernel = identity(payload.get("ocore_kernel"), "built O-core kernel")
    if {
        "bytes": artifact_by_path["/boot/ocore/kernel.elf"].get("bytes"),
        "sha256": artifact_by_path["/boot/ocore/kernel.elf"].get("sha256"),
    } != ocore_kernel:
        raise ReleaseError("existing receipt's built O-core kernel differs from its ISO artifact")

    if payload.get("boot_profile") != {
        "kind": "physical-hosted-workstation-plus-capacity",
        "default_entry": "hosted",
        "kernel_flavor": "alpine-lts",
        "rootfs_layout": "verified-squashfs-plus-tmpfs-overlay",
        "ventoy_compatibility": "alpine-hook-plus-minimal-modloop",
        "desktop_session": "openbox-xterm",
        "preferred_console": "tty0",
        "panic_timeout_seconds": 0,
        "ocore_entry": "direct-multiboot2-serial",
        "direct_entries": ["hosted", "ocore", "alpine"],
        "nested_qemu_tcg_entries": ["guix", "openbsd", "plan9", "redox"],
        "ventoy_mode": "grub2-filename-suffix",
    }:
        raise ReleaseError("existing receipt omitted the exact physical workstation boot profile")

    smoke = payload.get("smoke")
    expected_smoke_iso = {
        "bytes": inspection.get("bytes"),
        "sha256": inspection.get("sha256"),
    }
    if (
        not isinstance(smoke, dict)
        or smoke.get("schema") != "ostadix.hosted-live-boot-gates/v6"
        or set(smoke) != {"schema", "serial", "graphical", "ocore"}
    ):
        raise ReleaseError("existing receipt omitted the three exact firmware boot gates")
    serial_smoke = smoke.get("serial")
    if not isinstance(serial_smoke, dict) or (
        serial_smoke.get("schema") != "ostadix.hosted-live-qemu-smoke/v4"
        or serial_smoke.get("markers") != list(REQUIRED_SMOKE_MARKERS)
        or serial_smoke.get("exit_code") != 0
        or serial_smoke.get("acceleration") != "tcg"
        or serial_smoke.get("firmware_path") != "ovmf-through-capacity-runner"
        or serial_smoke.get("physical_hardware_proof") is not False
    ):
        raise ReleaseError("existing receipt omitted the exact ordered serial boot gate")
    identity(
        {
            "bytes": serial_smoke.get("transcript_bytes"),
            "sha256": serial_smoke.get("transcript_sha256"),
        },
        "QEMU serial smoke transcript",
    )
    if identity(serial_smoke.get("iso"), "QEMU serial smoke ISO") != expected_smoke_iso:
        raise ReleaseError("existing receipt's serial gate booted a different ISO")
    if identity(serial_smoke.get("rootfs"), "QEMU serial smoke rootfs") != rootfs:
        raise ReleaseError("existing receipt's serial gate verified a different SquashFS root")
    entropy_evidence(serial_smoke.get("entropy"), "QEMU serial smoke")
    if serial_smoke.get("olangc_wasm") != expected_wasm_evidence:
        raise ReleaseError("existing receipt's serial Olangc WASM evidence differs")
    graphical = smoke.get("graphical")
    expected_visual_thresholds = {
        "minimum_nonblack_pixels": MIN_GRAPHICAL_NONBLACK_PIXELS,
        "minimum_unique_colors": MIN_GRAPHICAL_UNIQUE_COLORS,
        "minimum_chromatic_pixels": MIN_GRAPHICAL_CHROMATIC_PIXELS,
        "minimum_chromatic_hue_buckets": MIN_GRAPHICAL_CHROMATIC_HUE_BUCKETS,
        "minimum_pixels_per_hue_bucket": MIN_GRAPHICAL_PIXELS_PER_HUE_BUCKET,
        "minimum_chromatic_max_channel": MIN_GRAPHICAL_CHROMATIC_MAX_CHANNEL,
        "minimum_chromatic_channel_spread": MIN_GRAPHICAL_CHROMATIC_CHANNEL_SPREAD,
        "minimum_changed_pixels": MIN_GRAPHICAL_CHANGED_PIXELS,
    }
    if not isinstance(graphical, dict) or (
        graphical.get("schema") != "ostadix.hosted-live-qemu-visual-smoke/v7"
        or graphical.get("markers") != list(REQUIRED_VISUAL_SMOKE_MARKERS)
        or graphical.get("font_marker") != "OSTADIX HOSTED X11 FONT: PASS"
        or graphical.get("pty_marker") != "OSTADIX HOSTED PTY: PASS"
        or graphical.get("evdev_marker") != "OSTADIX HOSTED EVDEV: PASS"
        or graphical.get("notebook_gui_marker")
        != "OSTADIX HOSTED NOTEBOOK GUI READY: PASS"
        or graphical.get("desktop_marker") != "OSTADIX HOSTED DESKTOP READY: PASS"
        or graphical.get("input_marker") != "vga-input-pass"
        or graphical.get("session") != "openbox-xterm"
        or graphical.get("acceleration") != "tcg"
        or graphical.get("display_device") != "VGA"
        or graphical.get("input_device") != "usb-kbd"
        or graphical.get("network") != "none"
        or graphical.get("visual_thresholds") != expected_visual_thresholds
        or graphical.get("physical_hardware_proof") is not False
        or type(graphical.get("changed_pixels")) is not int
        or graphical["changed_pixels"] < MIN_GRAPHICAL_CHANGED_PIXELS
    ):
        raise ReleaseError("existing receipt omitted the interactive graphical boot gate")
    identity(graphical.get("serial"), "QEMU graphical serial transcript")
    if identity(graphical.get("iso"), "QEMU graphical ISO") != expected_smoke_iso:
        raise ReleaseError("existing receipt's graphical gate booted a different ISO")
    if identity(graphical.get("rootfs"), "QEMU graphical rootfs") != rootfs:
        raise ReleaseError("existing receipt's graphical gate verified a different SquashFS root")
    entropy_evidence(graphical.get("entropy"), "QEMU graphical smoke")
    if graphical.get("olangc_wasm") != expected_wasm_evidence:
        raise ReleaseError("existing receipt's graphical Olangc WASM evidence differs")
    graphical_firmware = identity(graphical.get("firmware"), "QEMU graphical firmware")
    for label in ("frame_before", "frame_after"):
        frame = identity(graphical.get(label), f"QEMU graphical {label}")
        if (
            type(frame.get("width")) is not int
            or type(frame.get("height")) is not int
            or type(frame.get("nonblack_pixels")) is not int
            or type(frame.get("unique_colors")) is not int
            or type(frame.get("chromatic_pixels")) is not int
            or type(frame.get("chromatic_hue_buckets")) is not int
            or frame["width"] < 320
            or frame["height"] < 200
            or frame["nonblack_pixels"] < MIN_GRAPHICAL_NONBLACK_PIXELS
            or frame["unique_colors"] < MIN_GRAPHICAL_UNIQUE_COLORS
            or frame["chromatic_pixels"] < MIN_GRAPHICAL_CHROMATIC_PIXELS
            or frame["chromatic_hue_buckets"] < MIN_GRAPHICAL_CHROMATIC_HUE_BUCKETS
        ):
            raise ReleaseError("existing receipt contains invalid graphical frame evidence")
    ocore = smoke.get("ocore")
    if not isinstance(ocore, dict) or (
        ocore.get("schema") != "ostadix.hosted-live-ocore-qemu-smoke/v1"
        or ocore.get("selected_entry") != "ocore"
        or ocore.get("selection_method") != "grub-hotkey-o"
        or ocore.get("markers") != list(REQUIRED_OCORE_SMOKE_MARKERS)
        or ocore.get("exit_code") != 0
        or ocore.get("acceleration") != "tcg"
        or ocore.get("network") != "none"
        or ocore.get("physical_hardware_proof") is not False
    ):
        raise ReleaseError("existing receipt omitted the direct O-core firmware boot gate")
    identity(
        {
            "bytes": ocore.get("transcript_bytes"),
            "sha256": ocore.get("transcript_sha256"),
        },
        "QEMU O-core smoke transcript",
    )
    ocore_firmware = identity(ocore.get("firmware"), "QEMU O-core firmware")
    if {
        "bytes": ocore_firmware.get("bytes"),
        "sha256": ocore_firmware.get("sha256"),
    } != {
        "bytes": graphical_firmware.get("bytes"),
        "sha256": graphical_firmware.get("sha256"),
    }:
        raise ReleaseError("existing receipt's graphical and O-core gates used different firmware")
    if identity(ocore.get("iso"), "QEMU O-core ISO") != expected_smoke_iso:
        raise ReleaseError("existing receipt's O-core gate booted a different ISO")
    claim = payload.get("claim_boundary")
    if not isinstance(claim, dict) or (
        not isinstance(claim.get("substrate"), str)
        or not claim["substrate"]
        or claim.get("physical_hardware_proof") is not False
        or claim.get("secure_boot_proof") is not False
        or claim.get("hermetic") is not False
        or claim.get("host_mounts_may_be_visible") is not True
        or claim.get("foreign_entries_nested_qemu_tcg") is not True
        or claim.get("foreign_entries_direct_grub") is not False
        or claim.get("combined_capacity_menu_execution_proof") is not False
        or claim.get("foreign_guest_gui_proof") is not False
        or claim.get("foreign_guest_package_manager_execution_proof") is not False
        or claim.get("ventoy_foreign_route_proof") is not False
    ):
        raise ReleaseError("existing receipt omitted the hosted-live claim boundary")
    guest_boundary = payload.get("guest_path_boundary")
    if not isinstance(guest_boundary, dict) or (
        guest_boundary.get("hermetic") is not False
        or guest_boundary.get("host_mounts_may_be_visible") is not True
        or not isinstance(guest_boundary.get("native_paths"), list)
        or not guest_boundary["native_paths"]
    ):
        raise ReleaseError("existing receipt omitted guest-native path evidence")
    for binding in guest_boundary["native_paths"]:
        if not isinstance(binding, dict) or binding.get("filesystem") not in NATIVE_GUEST_FILESYSTEMS:
            raise ReleaseError("existing receipt contains invalid guest-native path evidence")


def _read_back_new_receipt(path: Path, payload: dict[str, object]) -> None:
    try:
        admitted = json.loads(path.read_text(encoding="utf-8"))
        if admitted != payload:
            raise ReleaseError("published receipt read-back differs from its admitted payload")
    except (OSError, json.JSONDecodeError, ReleaseError):
        path.unlink(missing_ok=True)
        raise


def _adopt_existing_pair(
    output: Path, receipt: Path, snapshot: SourceSnapshot
) -> dict[str, object] | None:
    if output.is_symlink() or receipt.is_symlink():
        raise ReleaseError("release output and receipt must not be symlinks")
    output_exists = output.exists()
    receipt_exists = receipt.exists()
    if not output_exists or not receipt_exists:
        return None
    inspection = _strict_inspect(output)
    payload = _read_existing_receipt(receipt)
    _validate_receipt_binding(
        payload,
        output=output,
        receipt=receipt,
        inspection=inspection,
        snapshot=snapshot,
    )
    return payload


def _ensure_publication_receipt(
    publication_payload: dict[str, object],
    *,
    output: Path,
    receipt: Path,
    inspection: dict[str, object],
    snapshot: SourceSnapshot,
) -> dict[str, object]:
    if output.is_symlink() or not output.is_file():
        raise ReleaseError("ISO publisher returned without a regular output file")
    if not receipt.exists():
        _exclusive_json(receipt, publication_payload)
        _read_back_new_receipt(receipt, publication_payload)
    admitted = _read_existing_receipt(receipt)
    _validate_receipt_binding(
        admitted,
        output=output,
        receipt=receipt,
        inspection=inspection,
        snapshot=snapshot,
    )
    return admitted


def _publish_verified_release_locked(
    *,
    candidate: Path,
    output: Path,
    receipt: Path,
    inspection: dict[str, object],
    payload: dict[str, object],
    snapshot: SourceSnapshot,
) -> dict[str, object]:
    """Recover or no-clobber commit one coherent exact ISO+receipt pair."""

    output.parent.mkdir(parents=True, exist_ok=True)
    if output.is_symlink() or receipt.is_symlink():
        raise ReleaseError("release output and receipt must not be symlinks")
    publication = {
        "output": str(output),
        "receipt": str(receipt),
        "sha256": inspection["sha256"],
        "bytes": inspection["bytes"],
        "branch": snapshot.branch,
        "origin": snapshot.origin,
        "published_utc": datetime.now(timezone.utc).isoformat(),
    }
    payload["host_publication"] = publication
    existing_receipt: dict[str, object] | None = None
    if receipt.exists():
        existing_receipt = _read_existing_receipt(receipt)
        _validate_receipt_binding(
            existing_receipt,
            output=output,
            receipt=receipt,
            inspection=inspection,
            snapshot=snapshot,
        )

    if existing_receipt is None:
        _validate_receipt_binding(
            payload,
            output=output,
            receipt=receipt,
            inspection=inspection,
            snapshot=snapshot,
        )

    if output.exists():
        published = _strict_inspect(output)
        if (
            published.get("sha256") != inspection.get("sha256")
            or published.get("bytes") != inspection.get("bytes")
        ):
            raise ReleaseError(f"refusing to clobber a different existing ISO: {output}")
        if existing_receipt is not None:
            return existing_receipt
        _exclusive_json(receipt, payload)
        _read_back_new_receipt(receipt, payload)
        return payload

    receipt_created = existing_receipt is None
    publication_payload = existing_receipt or payload
    if receipt_created:
        _exclusive_json(receipt, payload)
        _read_back_new_receipt(receipt, payload)

    try:
        _invoke(
            [
                sys.executable,
                str(ROOT / "scripts/ostadix_capacity_iso.py"),
                "publish",
                "--source",
                str(candidate),
                "--output",
                str(output),
            ]
        )
    except BaseException as publication_error:
        # The publisher links only a fully inspected copy. If it reported a
        # later failure but the strict inspector now admits the exact bytes,
        # the already coherent ISO+receipt pair is the successful outcome.
        recovered = False
        try:
            published = _strict_inspect(output)
            recovered = (
                published.get("sha256") == inspection.get("sha256")
                and published.get("bytes") == inspection.get("bytes")
            )
        except (OSError, ReleaseError):
            pass
        if recovered:
            print(
                "hosted-live-release: WARNING: publisher reported an error after "
                "the exact ISO and receipt became coherent; accepting the verified pair",
                file=sys.stderr,
            )
            return _ensure_publication_receipt(
                publication_payload,
                output=output,
                receipt=receipt,
                inspection=inspection,
                snapshot=snapshot,
            )
        if receipt_created:
            try:
                receipt.unlink()
            except OSError as rollback_error:
                raise ReleaseError(
                    "ISO publication failed and the prepublished receipt could not be "
                    f"rolled back: {receipt}: {rollback_error}"
                ) from publication_error
        raise
    return _ensure_publication_receipt(
        publication_payload,
        output=output,
        receipt=receipt,
        inspection=inspection,
        snapshot=snapshot,
    )


def _publish_verified_release(
    *,
    candidate: Path,
    output: Path,
    receipt: Path,
    inspection: dict[str, object],
    payload: dict[str, object],
    snapshot: SourceSnapshot,
) -> dict[str, object]:
    output.parent.mkdir(parents=True, exist_ok=True)
    with _publication_lock(output):
        return _publish_verified_release_locked(
            candidate=candidate,
            output=output,
            receipt=receipt,
            inspection=inspection,
            payload=payload,
            snapshot=snapshot,
        )


def _strict_inspect(path: Path) -> dict[str, object]:
    inspector = ROOT / "scripts/ostadix_capacity_iso.py"
    result = _invoke([sys.executable, str(inspector), "inspect", str(path)])
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ReleaseError("capacity ISO inspector returned invalid JSON") from error


def _complete_guest_release(
    *,
    client: MultipassClient,
    snapshot: SourceSnapshot,
    boot_objects: BootObjectSnapshot,
    temporary_root: Path,
    output: Path,
    receipt: Path,
    guest_run: Path,
    smoke_timeouts: SmokeTimeoutPolicy,
) -> dict[str, object]:
    guest_source = guest_run / "source"
    guest_archive = guest_run / "staged-source.tar"
    guest_boot_objects_archive = guest_run / "boot-objects.tar"
    guest_iso = guest_run / "output" / "ostadix-hosted-live-x86_64-uefi_VTGRUB2.iso"
    guest_receipt = guest_run / "output" / "hosted-live-release.json"
    native_paths = verify_guest_native_paths(
        client,
        [
            guest_run,
            guest_source,
            guest_archive,
            guest_boot_objects_archive,
            guest_iso,
            guest_receipt,
            Path("/home/ubuntu/.cache/ostadix/hosted-live-release/shared"),
            Path("/home/ubuntu/.cache/ostadix/capacity-host"),
        ],
    )
    client.transfer_to_guest(snapshot.archive, str(guest_archive))
    verify_guest_archive(client, guest_archive, snapshot.archive_sha256)
    client.transfer_to_guest(boot_objects.archive, str(guest_boot_objects_archive))
    verify_guest_archive(
        client,
        guest_boot_objects_archive,
        boot_objects.archive_sha256,
    )
    client.execute(
        ["tar", "-xf", str(guest_archive), "-C", str(guest_source)],
        capture_output=False,
    )
    worker = guest_source / "scripts/build-x86_64-hosted-live-linux.sh"
    client.execute(
        _guest_worker_environment(
            snapshot=snapshot,
            boot_objects=boot_objects,
            guest_boot_objects_archive=guest_boot_objects_archive,
            smoke_timeouts=smoke_timeouts,
        )
        + [
            str(worker),
            str(guest_source),
            str(guest_iso),
            str(guest_receipt),
            snapshot.tree,
            snapshot.head,
        ],
        capture_output=False,
    )

    candidate = temporary_root / "candidate.iso"
    candidate_receipt = temporary_root / "candidate.release.json"
    client.transfer_from_guest(str(guest_iso), candidate)
    client.transfer_from_guest(str(guest_receipt), candidate_receipt)
    assert_snapshot_still_current(ROOT, snapshot)
    try:
        guest_payload = json.loads(candidate_receipt.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ReleaseError("guest release receipt is unreadable or malformed") from error
    source = guest_payload.get("source", {})
    if not isinstance(source, dict) or source.get("staged_tree") != snapshot.tree:
        raise ReleaseError("guest receipt is not bound to the staged source tree")
    if source.get("base_commit") != snapshot.head:
        raise ReleaseError("guest receipt is not bound to the recorded base commit")
    if source.get("archive_sha256") != snapshot.archive_sha256:
        raise ReleaseError("guest receipt is not bound to the transferred source archive")
    if source.get("boot_objects_archive_sha256") != boot_objects.archive_sha256:
        raise ReleaseError("guest receipt is not bound to the transferred boot-object archive")
    object_receipt = source.get("boot_objects")
    if not isinstance(object_receipt, dict):
        raise ReleaseError("guest receipt omitted the boot-object store identity")
    for field in (
        "commit",
        "tree",
        "root_sha256",
        "object_count",
        "binding_count",
        "logical_bytes",
        "stored_bytes",
    ):
        if object_receipt.get(field) != boot_objects.summary.get(field):
            raise ReleaseError(f"guest boot-object receipt disagrees with host build for {field}")
    guest_payload["guest_path_boundary"] = {
        "native_paths": native_paths,
        "hermetic": False,
        "host_mounts_may_be_visible": True,
    }

    inspection = _strict_inspect(candidate)
    guest_iso_metadata = guest_payload.get("iso")
    if not isinstance(guest_iso_metadata, dict):
        raise ReleaseError("guest receipt omitted strict ISO metadata")
    if inspection.get("sha256") != guest_iso_metadata.get("sha256"):
        raise ReleaseError("host-transferred ISO hash differs from the guest receipt")
    if inspection.get("bytes") != guest_iso_metadata.get("bytes"):
        raise ReleaseError("host-transferred ISO size differs from the guest receipt")

    return _publish_verified_release(
        candidate=candidate,
        output=output,
        receipt=receipt,
        inspection=inspection,
        payload=guest_payload,
        snapshot=snapshot,
    )


def release(
    output: Path | None,
    *,
    vm_name: str = DEFAULT_VM,
    multipass_executable: str | None = None,
    runner: RunCallable | None = None,
    environment: Mapping[str, str] | None = None,
) -> dict[str, object]:
    guest_run: Path | None = None
    smoke_timeouts = resolve_smoke_timeout_policy(environment)
    with tempfile.TemporaryDirectory(prefix="ostadix-hosted-live-release.") as temporary:
        temporary_root = Path(temporary)
        snapshot = create_source_snapshot(ROOT, temporary_root / "staged-source.tar")
        if snapshot.origin != CANONICAL_REMOTE:
            raise ReleaseError(
                f"canonical release requires origin {CANONICAL_REMOTE!r}, got {snapshot.origin!r}"
            )
        if output is None:
            output = default_output_for(snapshot.tree)
        else:
            output = output.expanduser()
            if not output.is_absolute():
                output = (Path.cwd() / output).resolve()
        receipt = receipt_path_for(output)
        output.parent.mkdir(parents=True, exist_ok=True)
        with _publication_lock(output):
            adopted = _adopt_existing_pair(output, receipt, snapshot)
        if adopted is not None:
            assert_snapshot_still_current(ROOT, snapshot)
            return adopted

        boot_objects = create_boot_object_snapshot(ROOT, snapshot, temporary_root)
        assert_snapshot_still_current(ROOT, snapshot)

        multipass = multipass_executable or shutil.which("multipass")
        if not multipass:
            raise ReleaseError(
                "multipass is required for the native guest-owned Linux release route"
            )
        client = MultipassClient(multipass, vm_name, runner=runner)
        client.ensure_running()
        free_result = client.execute(
            [
                "python3",
                "-c",
                'import shutil; print(shutil.disk_usage("/home/ubuntu").free)',
            ]
        )
        try:
            free_bytes = int(free_result.stdout.strip())
        except ValueError as error:
            raise ReleaseError("could not parse free space reported by Multipass guest") from error
        if free_bytes < MIN_GUEST_FREE_BYTES:
            raise ReleaseError(
                f"Multipass guest has {free_bytes} free bytes; at least "
                f"{MIN_GUEST_FREE_BYTES} are required"
            )

        verify_guest_native_paths(
            client,
            [Path("/home/ubuntu/.cache/ostadix"), GUEST_RELEASE_BASE],
        )
        client.execute(
            [
                "sudo",
                "-n",
                "install",
                "-d",
                "-o",
                "ubuntu",
                "-g",
                "ubuntu",
                "-m",
                "0700",
                str(GUEST_RELEASE_BASE),
                str(GUEST_RELEASE_BASE / "runs"),
                str(GUEST_RELEASE_BASE / "shared"),
            ],
            capture_output=False,
        )

        run_name = f"{snapshot.tree[:12]}-{secrets.token_hex(8)}"
        guest_run = GUEST_RELEASE_BASE / "runs" / run_name
        guest_source = guest_run / "source"
        publication_succeeded = False
        try:
            client.execute(
                [
                    "install",
                    "-d",
                    "-m",
                    "0700",
                    str(guest_run),
                    str(guest_source),
                    str(guest_run / "output"),
                ],
                capture_output=False,
            )
            guest_payload = _complete_guest_release(
                client=client,
                snapshot=snapshot,
                boot_objects=boot_objects,
                temporary_root=temporary_root,
                output=output,
                receipt=receipt,
                guest_run=guest_run,
                smoke_timeouts=smoke_timeouts,
            )
            publication_succeeded = True
            return guest_payload
        finally:
            # Shared verified caches remain reusable; only the exact random run
            # is removed, on both success and failure.
            _best_effort_guest_cleanup(
                client,
                guest_run,
                publication_succeeded=publication_succeeded,
            )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Build and verify the exact staged hosted-live Ostadix ISO"
    )
    parser.add_argument("output_path", nargs="?", type=Path)
    parser.add_argument("--output", dest="output_option", type=Path)
    parser.add_argument(
        "--vm",
        default=os.environ.get("OSTADIX_HOSTED_LIVE_VM", DEFAULT_VM),
        help=argparse.SUPPRESS,
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    if arguments.output_path is not None and arguments.output_option is not None:
        print(
            "hosted-live-release: ERROR: specify OUTPUT or --output, not both",
            file=sys.stderr,
        )
        return 2
    output = arguments.output_option or arguments.output_path
    try:
        payload = release(output, vm_name=arguments.vm)
        publication = payload["host_publication"]
        assert isinstance(publication, dict)
        print(f"hosted-live-output: {publication['output']}")
        print(f"hosted-live-bytes: {publication['bytes']}")
        print(f"hosted-live-sha256: {publication['sha256']}")
        print(f"hosted-live-receipt: {publication['receipt']}")
        return 0
    except (OSError, ReleaseError) as error:
        print(f"hosted-live-release: ERROR: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
