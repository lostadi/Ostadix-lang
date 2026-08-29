#!/usr/bin/env python3
"""Bounded OVMF/QEMU readiness gate for the hosted-live capacity ISO."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
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
    / "target/ostadix-capacity-iso/x86_64/ostadix-hosted-live-x86_64-uefi.iso"
)
DEFAULT_TIMEOUT_SECONDS = 180.0
MAX_TRANSCRIPT_BYTES = 8 * 1024 * 1024
READ_CHUNK_BYTES = 64 * 1024

REQUIRED_MARKERS = (
    b"OSTADIX HOSTED O SMOKE: PASS",
    b"OSTADIX HOSTED BASH: PASS",
    b"OSTADIX HOSTED SQLITE: PASS",
    b"OSTADIX HOSTED OLANGC IR: PASS",
    b"OSTADIX HOSTED O-CLI: PASS",
    b"OSTADIX HOSTED O-LINK: PASS",
    b"OSTADIX HOSTED LIVE READY",
)
FAILURE_MARKERS = (
    b"OSTADIX HOSTED O SMOKE: FAIL",
    b"OSTADIX HOSTED BASH: FAIL",
    b"OSTADIX HOSTED SQLITE: FAIL",
    b"OSTADIX HOSTED OLANGC IR: FAIL",
    b"OSTADIX HOSTED O-CLI: FAIL",
    b"OSTADIX HOSTED O-LINK: FAIL",
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

    def public(self) -> dict[str, object]:
        return {
            "schema": "ostadix.hosted-live-qemu-smoke/v1",
            "markers": list(self.markers),
            "transcript_bytes": self.transcript_bytes,
            "transcript_sha256": self.transcript_sha256,
            "exit_code": self.exit_code,
            "acceleration": "tcg",
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


def run_marker_gate(
    command: Sequence[str],
    *,
    timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
    transcript_output: BinaryIO | None = None,
) -> SmokeResult:
    """Run one QEMU command and require the ordered hosted-live marker chain."""

    if not command:
        raise SmokeError("QEMU smoke command must not be empty")
    if not (0.05 <= timeout_seconds <= 900):
        raise SmokeError("timeout must be from 0.05 through 900 seconds")

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
        return SmokeResult(
            markers=tuple(marker.decode("ascii") for marker in REQUIRED_MARKERS),
            transcript_bytes=len(transcript),
            transcript_sha256=digest.hexdigest(),
            exit_code=exit_code,
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
