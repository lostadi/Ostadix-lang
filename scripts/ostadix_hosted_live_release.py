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
import os
from pathlib import Path
import secrets
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from typing import Callable, Sequence


ROOT = Path(__file__).resolve().parent.parent
CANONICAL_REMOTE = "https://github.com/lostadi/Ostadix-lang.git"
DEFAULT_OUTPUT_DIRECTORY = ROOT / "target/ostadix-hosted-live/x86_64"
DEFAULT_VM = "moral-gaur"
GUEST_RELEASE_BASE = Path("/home/ubuntu/.cache/ostadix/hosted-live-release")
MIN_GUEST_FREE_BYTES = 12 * 1024 * 1024 * 1024
MIN_GRAPHICAL_NONBLACK_PIXELS = 2_000
MIN_GRAPHICAL_UNIQUE_COLORS = 2
MIN_GRAPHICAL_CHANGED_PIXELS = 200
ARCHIVE_MTIME = "@315532800"
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
REQUIRED_SMOKE_MARKERS = (
    "OSTADIX HOSTED O SMOKE: PASS",
    "OSTADIX HOSTED BASH: PASS",
    "OSTADIX HOSTED SQLITE: PASS",
    "OSTADIX HOSTED OLANGC IR: PASS",
    "OSTADIX HOSTED O-CLI: PASS",
    "OSTADIX HOSTED O-LINK: PASS",
    "OSTADIX HOSTED LIVE READY",
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
    "scripts/build-x86_64-hosted-live-linux.sh",
    "ocore/kernel/smoke-x86_64-hosted-live-qemu.py",
    "scripts/prepare-x86_64-capacity-host.sh",
    "ocore/kernel/build-x86_64-hosted-live-iso.sh",
    "evidence/hosted_live_physical_iso.toml",
    "evidence/hosted_live_physical_apk_packages.txt",
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


RunCallable = Callable[..., subprocess.CompletedProcess[str]]


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
    if payload.get("schema") != "ostadix.hosted-live-release/v2":
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

    def identity(value: object, label: str) -> dict[str, object]:
        if not isinstance(value, dict):
            raise ReleaseError(f"existing receipt omitted {label} identity")
        size = value.get("bytes")
        digest = value.get("sha256")
        if type(size) is not int or size <= 0 or not isinstance(digest, str) \
                or len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
            raise ReleaseError(f"existing receipt has an invalid {label} identity")
        return value

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
    entries = inspection.get("entries")
    if not isinstance(entries, list) or [
        entry.get("id") for entry in entries if isinstance(entry, dict)
    ] != ["hosted"]:
        raise ReleaseError("existing receipt omitted the single physical hosted boot entry")
    artifacts = inspection.get("artifacts")
    if not isinstance(artifacts, list) or len(artifacts) != 2:
        raise ReleaseError("existing receipt omitted the exact two physical ISO artifacts")
    artifact_closure = {
        (artifact.get("iso_path"), artifact.get("role"))
        for artifact in artifacts
        if isinstance(artifact, dict)
    }
    if artifact_closure != {
        ("/boot/hosted/vmlinuz-lts", "linux-kernel"),
        ("/boot/hosted/initramfs.cpio.gz", "linux-initrd"),
    }:
        raise ReleaseError("existing receipt has the wrong physical ISO artifact closure")
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
    ):
        raise ReleaseError("existing receipt omitted the exact hosted build/toolchain lock")
    identity(build.get("sysroot_manifest"), "sysroot manifest")
    identity(build.get("hosted_live_package_lock"), "hosted-live package lock")
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

    binaries = payload.get("binaries")
    if not isinstance(binaries, dict) or set(binaries) != {"O", "o-cli", "olangc", "o-link"}:
        raise ReleaseError("existing receipt omitted the exact hosted binary set")
    for name in sorted(binaries):
        identity(binaries[name], f"hosted binary {name}")
    identity(payload.get("initramfs"), "hosted initramfs")

    if payload.get("boot_profile") != {
        "kind": "physical-hosted-live",
        "kernel_flavor": "alpine-lts",
        "preferred_console": "tty0",
        "panic_timeout_seconds": 0,
        "ventoy_mode": "grub2-filename-suffix",
    }:
        raise ReleaseError("existing receipt omitted the exact physical boot profile")

    smoke = payload.get("smoke")
    if not isinstance(smoke, dict) or smoke.get("schema") != "ostadix.hosted-live-boot-gates/v2":
        raise ReleaseError("existing receipt omitted the combined serial and graphical boot gates")
    serial_smoke = smoke.get("serial")
    if not isinstance(serial_smoke, dict) or (
        serial_smoke.get("schema") != "ostadix.hosted-live-qemu-smoke/v1"
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
    graphical = smoke.get("graphical")
    if not isinstance(graphical, dict) or (
        graphical.get("schema") != "ostadix.hosted-live-qemu-visual-smoke/v1"
        or graphical.get("markers") != list(REQUIRED_SMOKE_MARKERS)
        or graphical.get("input_marker") != "vga-input-pass"
        or graphical.get("acceleration") != "tcg"
        or graphical.get("display_device") != "VGA"
        or graphical.get("input_device") != "usb-kbd"
        or graphical.get("network") != "none"
        or graphical.get("physical_hardware_proof") is not False
        or type(graphical.get("changed_pixels")) is not int
        or graphical["changed_pixels"] < MIN_GRAPHICAL_CHANGED_PIXELS
    ):
        raise ReleaseError("existing receipt omitted the interactive graphical boot gate")
    identity(graphical.get("serial"), "QEMU graphical serial transcript")
    identity(graphical.get("firmware"), "QEMU graphical firmware")
    for label in ("frame_before", "frame_after"):
        frame = identity(graphical.get(label), f"QEMU graphical {label}")
        if (
            type(frame.get("width")) is not int
            or type(frame.get("height")) is not int
            or type(frame.get("nonblack_pixels")) is not int
            or type(frame.get("unique_colors")) is not int
            or frame["width"] < 320
            or frame["height"] < 200
            or frame["nonblack_pixels"] < MIN_GRAPHICAL_NONBLACK_PIXELS
            or frame["unique_colors"] < MIN_GRAPHICAL_UNIQUE_COLORS
        ):
            raise ReleaseError("existing receipt contains invalid graphical frame evidence")
    claim = payload.get("claim_boundary")
    if not isinstance(claim, dict) or (
        not isinstance(claim.get("substrate"), str)
        or not claim["substrate"]
        or claim.get("physical_hardware_proof") is not False
        or claim.get("secure_boot_proof") is not False
        or claim.get("hermetic") is not False
        or claim.get("host_mounts_may_be_visible") is not True
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
    temporary_root: Path,
    output: Path,
    receipt: Path,
    guest_run: Path,
) -> dict[str, object]:
    guest_source = guest_run / "source"
    guest_archive = guest_run / "staged-source.tar"
    guest_iso = guest_run / "output" / "ostadix-hosted-live-x86_64-uefi_VTGRUB2.iso"
    guest_receipt = guest_run / "output" / "hosted-live-release.json"
    native_paths = verify_guest_native_paths(
        client,
        [
            guest_run,
            guest_source,
            guest_archive,
            guest_iso,
            guest_receipt,
            Path("/home/ubuntu/.cache/ostadix/hosted-live-release/shared"),
            Path("/home/ubuntu/.cache/ostadix/capacity-host"),
        ],
    )
    client.transfer_to_guest(snapshot.archive, str(guest_archive))
    verify_guest_archive(client, guest_archive, snapshot.archive_sha256)
    client.execute(
        ["tar", "-xf", str(guest_archive), "-C", str(guest_source)],
        capture_output=False,
    )
    worker = guest_source / "scripts/build-x86_64-hosted-live-linux.sh"
    client.execute(
        [
            "env",
            f"OSTADIX_HOSTED_LIVE_ARCHIVE_SHA256={snapshot.archive_sha256}",
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
) -> dict[str, object]:
    guest_run: Path | None = None
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
                temporary_root=temporary_root,
                output=output,
                receipt=receipt,
                guest_run=guest_run,
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
