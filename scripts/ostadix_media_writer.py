#!/usr/bin/env python3
"""Confirmation-gated writer for validated OSTADIX boot-media images."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import plistlib
import platform
import re
import stat
import subprocess
import sys
import tempfile
from typing import Any, BinaryIO, Iterator


# Exact maximum outer-container size admitted by ostadix.boot-media/v1:
# 512 MiB ESP plus the fixed 1 MiB head and 1 MiB tail geometry.
MAX_IMAGE_BYTES = 538_968_064
COPY_CHUNK = 4 * 1024 * 1024
TOKEN_DOMAIN = b"OSTADIX/MEDIA-WRITE-CONFIRM/V1\0"
BOOT_MEDIA_TOOL = Path(__file__).with_name("ostadix_boot_media.py")


class WriterError(RuntimeError):
    pass


@dataclass(frozen=True)
class DeviceInfo:
    path: str
    raw_path: str
    identity: str
    bytes: int
    model: str
    transport: str
    platform: str

    def public(self) -> dict[str, object]:
        return {
            "path": self.path,
            "identity": self.identity,
            "bytes": self.bytes,
            "model": self.model,
            "transport": self.transport,
            "platform": self.platform,
        }


@dataclass(frozen=True)
class SourceIdentity:
    device: int
    inode: int
    bytes: int
    modified_ns: int
    changed_ns: int

    @classmethod
    def from_stat(cls, value: os.stat_result) -> "SourceIdentity":
        return cls(
            device=value.st_dev,
            inode=value.st_ino,
            bytes=value.st_size,
            modified_ns=value.st_mtime_ns,
            changed_ns=value.st_ctime_ns,
        )


@dataclass
class SourceSnapshot:
    """One validated, read-only image snapshot held through device mutation."""

    origin: Path
    stream: BinaryIO
    sha256: str
    bytes: int
    identity: SourceIdentity

    def assert_origin_unchanged(self) -> None:
        """Reject source growth, replacement, or content change before writing."""
        digest, identity = _hash_origin(self.origin, expected=self.identity)
        if digest != self.sha256 or identity != self.identity:
            raise WriterError("source image changed after its immutable snapshot was admitted")


def _run(command: list[str]) -> bytes:
    try:
        result = subprocess.run(command, check=False, capture_output=True)
    except OSError as error:
        raise WriterError(f"cannot execute {command[0]}: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise WriterError(f"{' '.join(command)} failed: {detail or result.returncode}")
    return result.stdout


def _open_origin(path: Path) -> BinaryIO:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise WriterError(
            f"cannot open source image without following links: {path}: {error}"
        ) from error
    return os.fdopen(descriptor, "rb")


def _checked_identity(value: os.stat_result, path: Path) -> SourceIdentity:
    if not stat.S_ISREG(value.st_mode):
        raise WriterError(f"not a regular image file: {path}")
    if value.st_size <= 0 or value.st_size > MAX_IMAGE_BYTES:
        raise WriterError(f"image size outside 1..{MAX_IMAGE_BYTES} bytes: {path}")
    return SourceIdentity.from_stat(value)


def _same_path_identity(path: Path, expected: SourceIdentity) -> None:
    try:
        current = os.stat(path, follow_symlinks=False)
    except OSError as error:
        raise WriterError(
            f"source image path disappeared or became unreadable: {path}: {error}"
        ) from error
    if not stat.S_ISREG(current.st_mode) or SourceIdentity.from_stat(current) != expected:
        raise WriterError("source image was replaced, resized, or modified during admission")


def _hash_exact(stream: BinaryIO, size: int, *, label: str) -> str:
    digest = hashlib.sha256()
    remaining = size
    while remaining:
        chunk = stream.read(min(COPY_CHUNK, remaining))
        if not chunk:
            raise WriterError(f"{label} ended before its admitted byte count")
        digest.update(chunk)
        remaining -= len(chunk)
    if stream.read(1):
        raise WriterError(f"{label} contains trailing bytes beyond its admitted byte count")
    return digest.hexdigest()


def _hash_origin(
    path: Path, *, expected: SourceIdentity | None = None
) -> tuple[str, SourceIdentity]:
    with _open_origin(path) as source:
        before = _checked_identity(os.fstat(source.fileno()), path)
        if expected is not None and before != expected:
            raise WriterError("source image was replaced, resized, or modified after admission")
        digest = _hash_exact(source, before.bytes, label="source image")
        after = _checked_identity(os.fstat(source.fileno()), path)
    if after != before:
        raise WriterError("source image changed while it was being verified")
    _same_path_identity(path, before)
    return digest, before


def _inspect_boot_media_snapshot(path: Path, digest: str, size: int) -> None:
    if not BOOT_MEDIA_TOOL.is_file():
        raise WriterError(f"boot-media inspector is missing: {BOOT_MEDIA_TOOL}")
    raw = _run([sys.executable, str(BOOT_MEDIA_TOOL), "inspect", str(path)])
    try:
        metadata = json.loads(raw)
    except json.JSONDecodeError as error:
        raise WriterError(f"boot-media inspector returned invalid JSON: {error}") from error
    if metadata.get("schema") != "ostadix.boot-media/v1":
        raise WriterError("image is not bounded OSTADIX boot-media v1")
    if metadata.get("sha256") != digest or metadata.get("bytes") != size:
        raise WriterError("immutable image snapshot disagrees with boot-media inspection")


@contextmanager
def _validated_source_snapshot(path: Path) -> Iterator[SourceSnapshot]:
    """Capture and validate one private snapshot before any device mutation.

    The source pathname is opened without following symlinks. Exactly its
    initial regular-file length is copied into a private file; early EOF,
    growth, replacement, or metadata/content change fails closed. All later
    writes consume the held read-only descriptor, never the original path.
    """
    origin = Path(os.path.abspath(os.fspath(path)))
    with tempfile.TemporaryDirectory(prefix="ostadix-media-source-") as directory:
        snapshot_path = Path(directory) / "admitted.img"
        try:
            with _open_origin(origin) as source, snapshot_path.open("x+b") as target:
                before = _checked_identity(os.fstat(source.fileno()), origin)
                digest = hashlib.sha256()
                remaining = before.bytes
                while remaining:
                    chunk = source.read(min(COPY_CHUNK, remaining))
                    if not chunk:
                        raise WriterError(
                            "source image ended while its snapshot was captured"
                        )
                    _write_all(target, chunk)
                    digest.update(chunk)
                    remaining -= len(chunk)
                if source.read(1):
                    raise WriterError("source image grew while its snapshot was captured")
                after = _checked_identity(os.fstat(source.fileno()), origin)
                if after != before:
                    raise WriterError("source image changed while its snapshot was captured")
                _same_path_identity(origin, before)
                target.flush()
                os.fsync(target.fileno())
            os.chmod(snapshot_path, 0o400)
        except PermissionError as error:
            raise WriterError(
                f"permission denied while capturing source image: {origin}"
            ) from error
        except OSError as error:
            raise WriterError(
                f"cannot capture immutable source image snapshot: {error}"
            ) from error

        captured_digest = digest.hexdigest()
        _inspect_boot_media_snapshot(snapshot_path, captured_digest, before.bytes)
        try:
            with snapshot_path.open("rb") as verification_stream:
                verified_digest = _hash_exact(
                    verification_stream,
                    before.bytes,
                    label="immutable image snapshot",
                )
        except OSError as error:
            raise WriterError(
                f"cannot verify immutable source image snapshot: {error}"
            ) from error
        if verified_digest != captured_digest:
            raise WriterError("immutable image snapshot changed during validation")

        try:
            held_stream = snapshot_path.open("rb")
        except OSError as error:
            raise WriterError(
                f"cannot hold immutable source image snapshot: {error}"
            ) from error
        with held_stream:
            snapshot = SourceSnapshot(
                origin=origin,
                stream=held_stream,
                sha256=captured_digest,
                bytes=before.bytes,
                identity=before,
            )
            snapshot.assert_origin_unchanged()
            yield snapshot


def _macos_device(path: str) -> DeviceInfo:
    if not re.fullmatch(r"/dev/(?:r)?disk[0-9]+", path):
        raise WriterError("macOS target must be a whole /dev/diskN or /dev/rdiskN device")
    canonical = "/dev/" + Path(path).name.removeprefix("r")
    info = plistlib.loads(_run(["diskutil", "info", "-plist", canonical]))
    if not info.get("Whole"):
        raise WriterError("target is not a whole disk")
    if info.get("Internal") is not False:
        raise WriterError("target must be reported as an external disk")
    if info.get("Writable") is not True or info.get("ReadOnlyMedia") is True:
        raise WriterError("target is not writable")
    identifier = str(info.get("DeviceIdentifier", ""))
    size = int(info.get("TotalSize", 0))
    if not identifier or size <= 0:
        raise WriterError("diskutil did not provide a stable device identity and size")
    root = plistlib.loads(_run(["diskutil", "info", "-plist", "/"]))
    if root.get("ParentWholeDisk") == identifier or root.get("PartOfWhole") == identifier:
        raise WriterError("refusing the disk that contains the active root filesystem")
    return DeviceInfo(
        path=f"/dev/{identifier}",
        raw_path=f"/dev/r{identifier}",
        identity=identifier,
        bytes=size,
        model=str(info.get("MediaName") or info.get("DeviceModel") or "unknown"),
        transport=str(info.get("BusProtocol") or "unknown"),
        platform="macos",
    )


def _linux_inventory() -> dict[str, Any]:
    raw = _run(
        [
            "lsblk",
            "--json",
            "--bytes",
            "--output",
            "NAME,PATH,TYPE,SIZE,RO,RM,MODEL,SERIAL,MOUNTPOINTS,PKNAME,TRAN",
        ]
    )
    try:
        return json.loads(raw)
    except json.JSONDecodeError as error:
        raise WriterError(f"lsblk returned invalid JSON: {error}") from error


def _flatten_linux(nodes: list[dict[str, Any]]) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    for node in nodes:
        result.append(node)
        result.extend(_flatten_linux(node.get("children") or []))
    return result


def _linux_flag_is_true(value: Any) -> bool:
    if value is True or value == 1:
        return True
    if isinstance(value, str):
        return value.strip().lower() in {"1", "true", "yes"}
    return False


def _linux_has_mountpoint(node: dict[str, Any]) -> bool:
    values = node.get("mountpoints")
    if values is None:
        return False
    if not isinstance(values, list):
        values = [values]
    return any(value is not None and str(value).strip() for value in values)


def _linux_device(path: str) -> DeviceInfo:
    if not re.fullmatch(r"/dev/[A-Za-z0-9._+-]+", path):
        raise WriterError("Linux target must be one exact whole /dev device")
    inventory = _linux_inventory()
    nodes = _flatten_linux(inventory.get("blockdevices") or [])
    matches = [node for node in nodes if node.get("path") == path]
    if len(matches) != 1:
        raise WriterError("target is absent or ambiguous in lsblk inventory")
    node = matches[0]
    if node.get("type") != "disk" or _linux_flag_is_true(node.get("ro")):
        raise WriterError("target must be one writable whole disk")
    nodes_by_name = {
        str(candidate.get("name")): candidate
        for candidate in nodes
        if candidate.get("name")
    }

    def descends_from_disk(candidate: dict[str, Any]) -> bool:
        parent = str(candidate.get("pkname") or "")
        visited: set[str] = set()
        while parent and parent not in visited:
            if parent == node.get("name"):
                return True
            visited.add(parent)
            parent_node = nodes_by_name.get(parent)
            if parent_node is None:
                return False
            parent = str(parent_node.get("pkname") or "")
        return False

    for candidate in nodes:
        if (candidate is node or descends_from_disk(candidate)) and _linux_has_mountpoint(
            candidate
        ):
            raise WriterError(
                "target or one of its descendants is mounted; unmount it first"
            )
    root_source = _run(["findmnt", "--noheadings", "--output", "SOURCE", "/"]).decode().strip()
    root_nodes = [candidate for candidate in nodes if candidate.get("path") == root_source]
    if root_nodes:
        root = root_nodes[0]
        if root is node or descends_from_disk(root):
            raise WriterError("refusing the disk that contains the active root filesystem")
    transport = str(node.get("tran") or "unknown").strip().lower()
    if not _linux_flag_is_true(node.get("rm")) and transport != "usb":
        raise WriterError(
            "target must be reported removable (RM=true) or use external USB transport"
        )
    size = int(node.get("size") or 0)
    if size <= 0:
        raise WriterError("lsblk did not provide a positive device size")
    identity = str(node.get("serial") or node.get("name") or "")
    if not identity:
        raise WriterError("lsblk did not provide a stable device identity")
    return DeviceInfo(
        path=path,
        raw_path=path,
        identity=identity,
        bytes=size,
        model=str(node.get("model") or "unknown").strip(),
        transport=transport,
        platform="linux",
    )


def inspect_device(path: str, system: str | None = None) -> DeviceInfo:
    system = system or platform.system()
    if system == "Darwin":
        return _macos_device(path)
    if system == "Linux":
        return _linux_device(path)
    raise WriterError(f"physical media writing is unsupported on {system}")


def confirmation_token(device: DeviceInfo, image_sha256: str, image_bytes: int) -> str:
    payload = "\0".join(
        (
            device.platform,
            device.path,
            device.identity,
            str(device.bytes),
            image_sha256,
            str(image_bytes),
        )
    ).encode("utf-8")
    suffix = hashlib.sha256(TOKEN_DOMAIN + payload).hexdigest()[:16].upper()
    return f"OSTADIX-WRITE-{suffix}"


def _require_exact_capacity(device: DeviceInfo, image_bytes: int) -> None:
    if device.bytes != image_bytes:
        raise WriterError(
            "bounded writer v1 requires device capacity to equal image bytes exactly "
            f"(device={device.bytes}, image={image_bytes}); target repacking is not implemented"
        )


def prepare(image: Path, device_path: str) -> tuple[DeviceInfo, str, int, str]:
    with _validated_source_snapshot(image) as snapshot:
        device = inspect_device(device_path)
        _require_exact_capacity(device, snapshot.bytes)
        return (
            device,
            snapshot.sha256,
            snapshot.bytes,
            confirmation_token(device, snapshot.sha256, snapshot.bytes),
        )


def _write_all(target: BinaryIO, chunk: bytes) -> None:
    view = memoryview(chunk)
    while view:
        written = target.write(view)
        if written is None or written <= 0:
            raise WriterError("target stopped accepting bytes before the image was complete")
        view = view[written:]


def _copy_and_verify(snapshot: SourceSnapshot, device: DeviceInfo) -> None:
    snapshot.stream.seek(0)
    if (
        _hash_exact(
            snapshot.stream, snapshot.bytes, label="held immutable image snapshot"
        )
        != snapshot.sha256
    ):
        raise WriterError("held immutable image snapshot digest changed before writing")
    snapshot.stream.seek(0)
    try:
        with open(device.raw_path, "r+b", buffering=0) as target:
            target.seek(0)
            remaining = snapshot.bytes
            while remaining:
                chunk = snapshot.stream.read(min(COPY_CHUNK, remaining))
                if not chunk:
                    raise WriterError(
                        "held immutable image snapshot ended during exact copy"
                    )
                _write_all(target, chunk)
                remaining -= len(chunk)
            if snapshot.stream.read(1):
                raise WriterError(
                    "held immutable image snapshot has trailing bytes after exact copy"
                )
            os.fsync(target.fileno())
            target.seek(0)
            digest = hashlib.sha256()
            remaining = snapshot.bytes
            while remaining:
                chunk = target.read(min(COPY_CHUNK, remaining))
                if not chunk:
                    raise WriterError("target ended before verification completed")
                digest.update(chunk)
                remaining -= len(chunk)
    except PermissionError as error:
        raise WriterError(
            "permission denied opening target; rerun the exact confirmed command "
            "with appropriate local privilege"
        ) from error
    except OSError as error:
        raise WriterError(f"device write or verification failed: {error}") from error
    if digest.hexdigest() != snapshot.sha256:
        raise WriterError("post-write device verification digest differs from the image")


def _emit(
    device: DeviceInfo,
    image: Path,
    digest: str,
    size: int,
    token: str,
    *,
    written: bool,
) -> None:
    result = {
        "schema": "ostadix.media-write/v1",
        "device": device.public(),
        "image": str(image.resolve()),
        "image_bytes": size,
        "image_sha256": digest,
        "confirmation": token,
        "written": written,
    }
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("prepare", "write"):
        command = commands.add_parser(name)
        command.add_argument("--image", type=Path, required=True)
        command.add_argument("--device", required=True)
        if name == "write":
            command.add_argument("--confirm", required=True)
    args = parser.parse_args(argv)
    try:
        with _validated_source_snapshot(args.image) as snapshot:
            device = inspect_device(args.device)
            _require_exact_capacity(device, snapshot.bytes)
            token = confirmation_token(device, snapshot.sha256, snapshot.bytes)
            if args.command == "prepare":
                _emit(
                    device,
                    args.image,
                    snapshot.sha256,
                    snapshot.bytes,
                    token,
                    written=False,
                )
                return 0
            if args.confirm != token:
                raise WriterError(
                    "confirmation mismatch; run prepare again and supply its exact token"
                )

            # The write invocation captures one validated snapshot and keeps
            # its read-only descriptor alive through verification. Re-probe
            # the device and re-hash the origin before the first state change.
            current = inspect_device(args.device)
            _require_exact_capacity(current, snapshot.bytes)
            if current != device:
                raise WriterError("device changed after confirmation")
            snapshot.assert_origin_unchanged()
            if current.platform == "macos":
                _run(["diskutil", "unmountDisk", current.path])

            # Unmounting can refresh macOS device metadata, so bind one final
            # probe to the same admitted identity immediately before opening
            # the raw target. Linux takes the same path without an unmount.
            final = inspect_device(args.device)
            _require_exact_capacity(final, snapshot.bytes)
            if final != current:
                raise WriterError("device changed immediately before mutation")
            _same_path_identity(snapshot.origin, snapshot.identity)
            _copy_and_verify(snapshot, final)
            _emit(
                final,
                args.image,
                snapshot.sha256,
                snapshot.bytes,
                token,
                written=True,
            )
    except WriterError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
