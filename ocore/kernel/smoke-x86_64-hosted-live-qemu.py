#!/usr/bin/env python3
"""Bounded OVMF/QEMU readiness gate for the hosted-live capacity ISO."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import sys
import time
from typing import BinaryIO, Sequence


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_RUNNER = ROOT / "ocore/kernel/run-x86_64-capacity-iso-qemu.sh"
DEFAULT_ISO = (
    ROOT
    / "target/ostadix-hosted-live/x86_64/ostadix-hosted-live-x86_64-uefi_VTGRUB2.iso"
)
DEFAULT_TIMEOUT_SECONDS = 1800.0
MAX_TIMEOUT_SECONDS = 1800.0
MAX_TRANSCRIPT_BYTES = 8 * 1024 * 1024
READ_CHUNK_BYTES = 64 * 1024
SMOKE_SCHEMA = "ostadix.hosted-live-qemu-smoke/v4"
ISO_IDENTITY_RE = re.compile(
    rb"^OSTADIX ISO IDENTITY bytes=([1-9][0-9]*) sha256=([0-9a-f]{64})$",
    re.MULTILINE,
)
ROOTFS_IDENTITY_PREFIX = b"OSTADIX HOSTED ROOTFS: PASS "
ROOTFS_IDENTITY_RE = re.compile(
    rb"OSTADIX HOSTED ROOTFS: PASS bytes=([1-9][0-9]*) sha256=([0-9a-f]{64})"
)
ENTROPY_IDENTITY_PREFIX = b"OSTADIX HOSTED ENTROPY: PASS "
ENTROPY_IDENTITY_RE = re.compile(
    rb"OSTADIX HOSTED ENTROPY: PASS device=virtio-rng-pci "
    rb"crng_bytes=32 available=([1-9][0-9]*)"
)
ENTROPY_ORDERED_MARKER = b"OSTADIX HOSTED ENTROPY: PASS"
WASM_MATERIALIZATION_PREFIX = b"OSTADIX HOSTED OLANGC MATERIALIZATION: PASS "
WASM_MATERIALIZATION_RE = re.compile(
    rb"OSTADIX HOSTED OLANGC MATERIALIZATION: PASS root_sha256=([0-9a-f]{64})"
)
WASM_ARTIFACT_PREFIX = b"OSTADIX HOSTED OLANGC WASM ARTIFACT: PASS "
WASM_ARTIFACT_RE = re.compile(
    rb"OSTADIX HOSTED OLANGC WASM ARTIFACT: PASS "
    rb"tree=([0-9a-f]{40}) bytes=([1-9][0-9]*) sha256=([0-9a-f]{64})"
)
MIN_ENTROPY_BITS = 128

REQUIRED_MARKERS = (
    b"OSTADIX HOSTED ROOTFS: PASS bytes=",
    b"OSTADIX HOSTED ROOTFS OVERLAY: PASS",
    b"OSTADIX HOSTED READ-ONLY TREES: PASS",
    b"OSTADIX HOSTED LOOPBACK: PASS",
    b"OSTADIX HOSTED O SMOKE: PASS",
    b"OSTADIX HOSTED BASH: PASS",
    b"OSTADIX HOSTED APK: PASS",
    b"OSTADIX HOSTED SQLITE: PASS",
    b"OSTADIX HOSTED OLANGC IR: PASS",
    b"OSTADIX HOSTED O-CLI: PASS",
    b"OSTADIX HOSTED O-LINK: PASS",
    b"OSTADIX HOSTED RUSTC: PASS",
    b"OSTADIX HOSTED CARGO: PASS",
    b"OSTADIX HOSTED RUSTFMT: PASS",
    b"OSTADIX HOSTED CLIPPY: PASS",
    b"OSTADIX HOSTED CARGO HELLO: PASS",
    ENTROPY_ORDERED_MARKER,
    b"OSTADIX HOSTED O-NODE: PASS",
    b"OSTADIX HOSTED NOTEBOOK: PASS",
    b"OSTADIX HOSTED STANDARD BINARIES: PASS",
    b"OSTADIX HOSTED DECLARED ROOT BINARIES: PASS",
    b"OSTADIX HOSTED UNIFIED ROUTES: PASS",
    b"OSTADIX HOSTED SOURCE SNAPSHOT: PASS",
    WASM_MATERIALIZATION_PREFIX.rstrip(),
    WASM_ARTIFACT_PREFIX.rstrip(),
    b"OSTADIX HOSTED RUST WASM: PASS",
    b"OSTADIX HOSTED WASM RUNTIME: PASS",
    b"OSTADIX HOSTED OLANGC WASM EXECUTION: PASS",
    b"OSTADIX HOSTED WEBASSEMBLY BACKEND: PASS",
    b"OSTADIX HOSTED MCP: PASS",
    b"OSTADIX BOOT OBJECTS: PASS",
    b"OSTADIX HOSTED SOURCE OBJECT CLOSURE: PASS",
    b"OSTADIX HOSTED LIVE READY",
)
FAILURE_MARKERS = (
    b"OSTADIX HOSTED BOOTSTRAP: FAIL",
    b"OSTADIX HOSTED ROOTFS OVERLAY: FAIL",
    b"OSTADIX HOSTED READ-ONLY TREES: FAIL",
    b"OSTADIX HOSTED LOOPBACK: FAIL",
    b"OSTADIX HOSTED O SMOKE: FAIL",
    b"OSTADIX HOSTED BASH: FAIL",
    b"OSTADIX HOSTED APK: FAIL",
    b"OSTADIX HOSTED SQLITE: FAIL",
    b"OSTADIX HOSTED OLANGC IR: FAIL",
    b"OSTADIX HOSTED O-CLI: FAIL",
    b"OSTADIX HOSTED O-LINK: FAIL",
    b"OSTADIX HOSTED RUSTC: FAIL",
    b"OSTADIX HOSTED CARGO: FAIL",
    b"OSTADIX HOSTED RUSTFMT: FAIL",
    b"OSTADIX HOSTED CLIPPY: FAIL",
    b"OSTADIX HOSTED CARGO HELLO: FAIL",
    b"OSTADIX HOSTED ENTROPY: FAIL",
    b"OSTADIX HOSTED O-NODE: FAIL",
    b"OSTADIX HOSTED NOTEBOOK: FAIL",
    b"OSTADIX HOSTED STANDARD BINARIES: FAIL",
    b"OSTADIX HOSTED DECLARED ROOT BINARIES: FAIL",
    b"OSTADIX HOSTED UNIFIED ROUTES: FAIL",
    b"OSTADIX HOSTED SOURCE SNAPSHOT: FAIL",
    b"OSTADIX HOSTED OLANGC MATERIALIZATION: FAIL",
    b"OSTADIX HOSTED OLANGC WASM ARTIFACT: FAIL",
    b"OSTADIX HOSTED RUST WASM: FAIL",
    b"OSTADIX HOSTED WASM RUNTIME: FAIL",
    b"OSTADIX HOSTED OLANGC WASM EXECUTION: FAIL",
    b"OSTADIX HOSTED WEBASSEMBLY BACKEND: FAIL",
    b"OSTADIX HOSTED MCP: FAIL",
    b"OSTADIX BOOT OBJECTS: FAIL",
    b"OSTADIX HOSTED SOURCE OBJECT CLOSURE: FAIL",
    b"OSTADIX CAPACITY HOST ERROR",
    b"Kernel panic",
)


class SmokeError(RuntimeError):
    """The exact ISO did not establish bounded hosted-live readiness."""


@dataclass(frozen=True)
class SmokeResult:
    markers: tuple[str, ...]
    transcript_bytes: int
    transcript_sha256: str
    exit_code: int
    iso_bytes: int
    iso_sha256: str
    rootfs_bytes: int
    rootfs_sha256: str
    entropy_available: int
    wasm_project_sha256: str
    wasm_tree: str
    wasm_bytes: int
    wasm_sha256: str

    def public(self) -> dict[str, object]:
        return {
            "schema": SMOKE_SCHEMA,
            "markers": list(self.markers),
            "transcript_bytes": self.transcript_bytes,
            "transcript_sha256": self.transcript_sha256,
            "exit_code": self.exit_code,
            "iso": {"bytes": self.iso_bytes, "sha256": self.iso_sha256},
            "rootfs": {
                "bytes": self.rootfs_bytes,
                "sha256": self.rootfs_sha256,
            },
            "acceleration": "tcg",
            "entropy": {
                "device": "virtio-rng-pci",
                "crng_bytes": 32,
                "available": self.entropy_available,
            },
            "olangc_wasm": {
                "staged_tree": self.wasm_tree,
                "bytes": self.wasm_bytes,
                "sha256": self.wasm_sha256,
                "materialized_project_sha256": self.wasm_project_sha256,
            },
            "firmware_path": "ovmf-through-capacity-runner",
            "physical_hardware_proof": False,
        }


def _terminate_process_group(process: subprocess.Popen[bytes]) -> None:
    process_group = process.pid
    permission_error: PermissionError | None = None
    try:
        os.killpg(process_group, signal.SIGTERM)
    except ProcessLookupError:
        pass
    except PermissionError as error:
        permission_error = error
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        pass
    # A runner can exit while descendants keep the inherited QEMU pipes open.
    # Always address the original process group, even after the leader is reaped.
    try:
        os.killpg(process_group, signal.SIGKILL)
    except ProcessLookupError:
        pass
    except PermissionError as error:
        permission_error = error
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired as error:
        raise SmokeError("could not reap the hosted-live QEMU process group") from error
    if permission_error is not None:
        raise SmokeError(
            "permission denied while terminating the hosted-live QEMU process group"
        ) from permission_error


def _diagnostic(transcript: bytearray) -> str:
    tail = bytes(transcript[-4096:]).decode("utf-8", "replace")
    return tail.strip() or "no serial output captured"


def _parse_iso_identity(transcript: bytearray) -> tuple[int, str]:
    matches = list(ISO_IDENTITY_RE.finditer(transcript))
    if len(matches) != 1:
        raise SmokeError(
            "capacity runner must emit exactly one valid pinned ISO identity before boot"
        )
    first_marker = transcript.find(REQUIRED_MARKERS[0])
    if first_marker < 0 or matches[0].start() > first_marker:
        raise SmokeError("capacity runner emitted the pinned ISO identity after guest boot")
    return int(matches[0].group(1)), matches[0].group(2).decode("ascii")


def _parse_rootfs_identity(transcript: bytes | bytearray) -> tuple[int, str]:
    candidates = [
        line
        for line in bytes(transcript).splitlines()
        if line.startswith(ROOTFS_IDENTITY_PREFIX)
    ]
    if len(candidates) != 1:
        raise SmokeError(
            "completed transcript must contain exactly one full Hosted rootfs identity marker"
        )
    match = ROOTFS_IDENTITY_RE.fullmatch(candidates[0])
    if match is None:
        raise SmokeError(
            "completed transcript must contain exactly one full Hosted rootfs identity marker"
        )
    return int(match.group(1)), match.group(2).decode("ascii")


def _validated_entropy_identity(
    transcript: bytes | bytearray,
) -> tuple[int, int]:
    candidates: list[tuple[int, bytes]] = []
    offset = 0
    for raw_line in bytes(transcript).splitlines(keepends=True):
        line = raw_line.rstrip(b"\r\n")
        if line.startswith(ENTROPY_IDENTITY_PREFIX):
            candidates.append((offset, line))
        offset += len(raw_line)
    if len(candidates) != 1:
        raise SmokeError(
            "completed transcript must contain exactly one full Hosted entropy marker"
        )
    position, line = candidates[0]
    match = ENTROPY_IDENTITY_RE.fullmatch(line)
    if match is None:
        raise SmokeError(
            "completed transcript must contain exactly one full Hosted entropy marker"
        )
    available = int(match.group(1))
    if available < MIN_ENTROPY_BITS:
        raise SmokeError(
            f"Hosted entropy marker reported only {available} available bits"
        )
    return available, position


def _parse_entropy_identity(transcript: bytes | bytearray) -> int:
    return _validated_entropy_identity(transcript)[0]


def _validated_wasm_identity(
    transcript: bytes | bytearray,
) -> tuple[str, str, int, str, int, int]:
    materializations: list[tuple[int, bytes]] = []
    artifacts: list[tuple[int, bytes]] = []
    offset = 0
    for raw_line in bytes(transcript).splitlines(keepends=True):
        line = raw_line.rstrip(b"\r\n")
        if line.startswith(WASM_MATERIALIZATION_PREFIX):
            materializations.append((offset, line))
        if line.startswith(WASM_ARTIFACT_PREFIX):
            artifacts.append((offset, line))
        offset += len(raw_line)
    if len(materializations) != 1 or len(artifacts) != 1:
        raise SmokeError(
            "completed transcript must contain exactly one full Olangc WASM identity chain"
        )
    materialization_position, materialization_line = materializations[0]
    artifact_position, artifact_line = artifacts[0]
    materialization_match = WASM_MATERIALIZATION_RE.fullmatch(materialization_line)
    artifact_match = WASM_ARTIFACT_RE.fullmatch(artifact_line)
    if materialization_match is None or artifact_match is None:
        raise SmokeError(
            "completed transcript contains a malformed Olangc WASM identity chain"
        )
    return (
        materialization_match.group(1).decode("ascii"),
        artifact_match.group(1).decode("ascii"),
        int(artifact_match.group(2)),
        artifact_match.group(3).decode("ascii"),
        materialization_position,
        artifact_position,
    )


def run_marker_gate(
    command: Sequence[str],
    *,
    timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
    transcript_output: BinaryIO | None = None,
) -> SmokeResult:
    """Run one QEMU command and require the ordered hosted-live marker chain."""

    if not command:
        raise SmokeError("QEMU smoke command must not be empty")
    if not (0.05 <= timeout_seconds <= MAX_TIMEOUT_SECONDS):
        raise SmokeError(
            f"timeout must be from 0.05 through {MAX_TIMEOUT_SECONDS:g} seconds"
        )

    process = subprocess.Popen(
        list(command),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        bufsize=0,
        start_new_session=True,
    )
    assert process.stdout is not None
    transcript = bytearray()
    digest = hashlib.sha256()
    marker_index = 0
    marker_positions: list[int] = []
    search_offset = 0
    deadline = time.monotonic() + timeout_seconds
    selector: selectors.BaseSelector | None = None
    failure_line = bytearray()

    def admit(chunk: bytes) -> None:
        nonlocal failure_line
        if len(transcript) + len(chunk) > MAX_TRANSCRIPT_BYTES:
            raise SmokeError(
                f"hosted-live QEMU transcript exceeded {MAX_TRANSCRIPT_BYTES} bytes"
            )
        transcript.extend(chunk)
        digest.update(chunk)
        if transcript_output is not None:
            transcript_output.write(chunk)
            transcript_output.flush()

        for failure in FAILURE_MARKERS:
            if failure in transcript:
                raise SmokeError(
                    f"guest emitted failure marker {failure.decode()!r}: "
                    f"{_diagnostic(transcript)}"
                )
        failure_line.extend(chunk)
        lines = failure_line.split(b"\n")
        failure_line = bytearray(lines.pop())
        for line in [*lines, bytes(failure_line)]:
            upper = line.upper()
            if b"OSTADIX" in upper and b"FAIL" in upper:
                raise SmokeError(
                    "guest emitted an OSTADIX failure line: "
                    f"{line.decode('utf-8', 'replace')}: {_diagnostic(transcript)}"
                )

    try:
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        while marker_index < len(REQUIRED_MARKERS):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise SmokeError(
                    "hosted-live QEMU smoke timed out before marker "
                    f"{REQUIRED_MARKERS[marker_index].decode()!r}: {_diagnostic(transcript)}"
                )
            events = selector.select(min(remaining, 0.25))
            if not events:
                if process.poll() is not None:
                    raise SmokeError(
                        "QEMU exited before hosted-live readiness: "
                        f"status={process.returncode}: {_diagnostic(transcript)}"
                    )
                continue

            chunk = os.read(process.stdout.fileno(), READ_CHUNK_BYTES)
            if not chunk:
                if process.poll() is not None:
                    raise SmokeError(
                        "QEMU output closed before hosted-live readiness: "
                        f"status={process.returncode}: {_diagnostic(transcript)}"
                    )
                continue
            admit(chunk)

            while marker_index < len(REQUIRED_MARKERS):
                marker = REQUIRED_MARKERS[marker_index]
                position = transcript.find(marker, search_offset)
                if position < 0:
                    break
                marker_positions.append(position)
                search_offset = position + len(marker)
                marker_index += 1

        # The capacity runner exposes QEMU's monitor escape on stdio. Exit only
        # after the final guest readiness marker, then require a bounded cleanup.
        if process.stdin is not None:
            try:
                process.stdin.write(b"\x01x")
                process.stdin.flush()
            except BrokenPipeError:
                pass

        # Continue admitting output until EOF. A failure printed immediately
        # after READY is still a boot failure, and every admitted byte belongs
        # in the transcript identity.
        cleanup_deadline = time.monotonic() + 5.0
        output_eof = False
        while not output_eof:
            remaining = cleanup_deadline - time.monotonic()
            if remaining <= 0:
                raise SmokeError("QEMU output did not close after the readiness marker")
            events = selector.select(min(remaining, 0.25))
            if not events:
                continue
            chunk = os.read(process.stdout.fileno(), READ_CHUNK_BYTES)
            if chunk:
                admit(chunk)
            else:
                selector.unregister(process.stdout)
                output_eof = True
        try:
            exit_code = process.wait(
                timeout=max(0.001, cleanup_deadline - time.monotonic())
            )
        except subprocess.TimeoutExpired as error:
            raise SmokeError("QEMU did not exit after the verified readiness marker") from error
        if exit_code != 0:
            raise SmokeError(
                f"QEMU returned status {exit_code} after readiness: {_diagnostic(transcript)}"
            )
        iso_bytes, iso_sha256 = _parse_iso_identity(transcript)
        rootfs_bytes, rootfs_sha256 = _parse_rootfs_identity(transcript)
        entropy_available, entropy_position = _validated_entropy_identity(transcript)
        entropy_slot = REQUIRED_MARKERS.index(ENTROPY_ORDERED_MARKER)
        if marker_positions[entropy_slot] != entropy_position:
            raise SmokeError(
                "full Hosted entropy marker did not occupy its ordered position"
            )
        (
            wasm_project_sha256,
            wasm_tree,
            wasm_bytes,
            wasm_sha256,
            wasm_materialization_position,
            wasm_artifact_position,
        ) = _validated_wasm_identity(transcript)
        if marker_positions[REQUIRED_MARKERS.index(WASM_MATERIALIZATION_PREFIX.rstrip())] \
                != wasm_materialization_position:
            raise SmokeError(
                "full Olangc materialization marker did not occupy its ordered position"
            )
        if marker_positions[REQUIRED_MARKERS.index(WASM_ARTIFACT_PREFIX.rstrip())] \
                != wasm_artifact_position:
            raise SmokeError(
                "full Olangc WASM artifact marker did not occupy its ordered position"
            )
        return SmokeResult(
            markers=tuple(marker.decode("ascii") for marker in REQUIRED_MARKERS),
            transcript_bytes=len(transcript),
            transcript_sha256=digest.hexdigest(),
            exit_code=exit_code,
            iso_bytes=iso_bytes,
            iso_sha256=iso_sha256,
            rootfs_bytes=rootfs_bytes,
            rootfs_sha256=rootfs_sha256,
            entropy_available=entropy_available,
            wasm_project_sha256=wasm_project_sha256,
            wasm_tree=wasm_tree,
            wasm_bytes=wasm_bytes,
            wasm_sha256=wasm_sha256,
        )
    finally:
        primary_failure = sys.exc_info()[0] is not None
        try:
            if selector is not None:
                selector.close()
            try:
                _terminate_process_group(process)
            except SmokeError:
                if not primary_failure:
                    raise
        finally:
            if process.stdin is not None:
                process.stdin.close()
            process.stdout.close()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Boot an exact hosted-live ISO under OVMF/QEMU and require readiness markers"
    )
    parser.add_argument("iso", nargs="?", type=Path, default=DEFAULT_ISO)
    parser.add_argument("--runner", type=Path, default=DEFAULT_RUNNER)
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_SECONDS)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    iso = arguments.iso.expanduser().resolve()
    runner = arguments.runner.expanduser().resolve()
    try:
        if iso.is_symlink() or not iso.is_file():
            raise SmokeError(f"ISO must be a regular non-symlink file: {iso}")
        if runner.is_symlink() or not runner.is_file() or not os.access(runner, os.X_OK):
            raise SmokeError(f"runner must be an executable non-symlink file: {runner}")
        stream = getattr(sys.stderr, "buffer", sys.stderr)
        result = run_marker_gate(
            [str(runner), str(iso)],
            timeout_seconds=arguments.timeout,
            transcript_output=stream,
        )
        print(json.dumps(result.public(), sort_keys=True, separators=(",", ":")))
        return 0
    except (OSError, SmokeError) as error:
        print(f"hosted-live-qemu-smoke: ERROR: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
