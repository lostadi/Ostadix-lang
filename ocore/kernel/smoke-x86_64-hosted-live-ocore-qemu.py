#!/usr/bin/env python3
"""Select and boot O-core from the exact Hosted Live UEFI ISO."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import stat
import subprocess
import sys
import time
from typing import BinaryIO, Sequence


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_ISO = (
    ROOT
    / "target/ostadix-hosted-live/x86_64/ostadix-hosted-live-x86_64-uefi_VTGRUB2.iso"
)
DEFAULT_TIMEOUT_SECONDS = 90.0
MAX_TRANSCRIPT_BYTES = 8 * 1024 * 1024
READ_CHUNK_BYTES = 64 * 1024
MENU_MARKER = b"OSTADIX O-core"
REQUIRED_MARKERS = (
    b"O-core kernel: serial online",
    b"page protections: W^X online",
    b"CPL3 native[0]: online",
    b"timer CPL3 return: online",
    b"CPL3 heartbeat: online",
)
FORBIDDEN_FRAGMENTS = (
    b"panic",
    b"fatal",
    b"triple fault",
    b"m02 kernel fault",
    b"m02 unexpected fault",
    b"leaked",
)


class OcoreSmokeError(RuntimeError):
    """The combined ISO did not select and sustain its direct O-core entry."""


@dataclass(frozen=True)
class PinnedInput:
    descriptor: int
    state: os.stat_result
    path: Path


def _open_regular(path: Path, label: str) -> PinnedInput:
    if not hasattr(os, "O_NOFOLLOW"):
        raise OcoreSmokeError("host lacks O_NOFOLLOW for exact smoke inputs")
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW,
        )
    except OSError as error:
        raise OcoreSmokeError(f"cannot open {label}: {path}: {error}") from error
    state = os.fstat(descriptor)
    if not stat.S_ISREG(state.st_mode) or state.st_size <= 0:
        os.close(descriptor)
        raise OcoreSmokeError(f"{label} is not a non-empty regular file: {path}")
    return PinnedInput(descriptor=descriptor, state=state, path=path)


def _fd_reference(descriptor: int) -> str:
    for root in (Path("/proc/self/fd"), Path("/dev/fd")):
        if root.is_dir():
            return str(root / str(descriptor))
    raise OcoreSmokeError("host exposes neither /proc/self/fd nor /dev/fd")


def _identity_state(state: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        state.st_dev,
        state.st_ino,
        state.st_size,
        state.st_mtime_ns,
        state.st_ctime_ns,
    )


def _sha256_descriptor(descriptor: int) -> str:
    digest = hashlib.sha256()
    offset = 0
    while True:
        chunk = os.pread(descriptor, 4 * 1024 * 1024, offset)
        if not chunk:
            return digest.hexdigest()
        digest.update(chunk)
        offset += len(chunk)


def _terminate(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=2)


def _diagnostic(transcript: bytearray) -> str:
    return bytes(transcript[-4096:]).decode("utf-8", "replace").strip()


def _forbidden(transcript: bytes) -> bytes | None:
    lowered = transcript.lower()
    return next((fragment for fragment in FORBIDDEN_FRAGMENTS if fragment in lowered), None)


def run_ocore_gate(
    iso: Path,
    firmware: Path,
    *,
    qemu: str = "qemu-system-x86_64",
    timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
    transcript_output: BinaryIO | None = None,
) -> dict[str, object]:
    """Boot firmware media, press GRUB hotkey ``o``, and require O-core liveness."""

    if not (1 <= timeout_seconds <= 900):
        raise OcoreSmokeError("timeout must be from 1 through 900 seconds")
    iso_input = _open_regular(iso, "Hosted Live ISO")
    firmware_input = _open_regular(firmware, "OVMF code")
    process: subprocess.Popen[bytes] | None = None
    selector: selectors.BaseSelector | None = None
    transcript = bytearray()
    digest = hashlib.sha256()
    iso_sha256 = _sha256_descriptor(iso_input.descriptor)

    def admit(chunk: bytes) -> None:
        if len(transcript) + len(chunk) > MAX_TRANSCRIPT_BYTES:
            raise OcoreSmokeError("O-core transcript exceeded eight MiB")
        transcript.extend(chunk)
        digest.update(chunk)
        if transcript_output is not None:
            transcript_output.write(chunk)
            transcript_output.flush()
        forbidden = _forbidden(bytes(transcript))
        if forbidden is not None:
            raise OcoreSmokeError(
                f"O-core transcript contains forbidden fragment {forbidden!r}"
            )

    try:
        os.set_inheritable(iso_input.descriptor, True)
        os.set_inheritable(firmware_input.descriptor, True)
        command = [
            qemu,
            "-accel", "tcg",
            "-machine", "q35",
            "-cpu", "max",
            "-smp", "2",
            "-m", "256M",
            "-drive",
            "if=pflash,unit=0,format=raw,readonly=on,file="
            + _fd_reference(firmware_input.descriptor),
            "-drive",
            "if=ide,index=2,media=cdrom,format=raw,readonly=on,file="
            + _fd_reference(iso_input.descriptor),
            "-boot", "order=d,strict=on",
            "-nodefaults",
            "-nic", "none",
            "-display", "none",
            "-serial", "mon:stdio",
            "-no-reboot",
            "-no-shutdown",
        ]
        if "-kernel" in command or command[command.index("-nic") + 1] != "none":
            raise OcoreSmokeError("O-core smoke escaped its firmware/media boundary")
        process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            bufsize=0,
            pass_fds=(iso_input.descriptor, firmware_input.descriptor),
            start_new_session=True,
        )
        assert process.stdin is not None and process.stdout is not None
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + timeout_seconds
        selected = False
        marker_index = 0
        search_offset = 0
        heartbeat_at: float | None = None
        output_eof = False

        while True:
            now = time.monotonic()
            if heartbeat_at is not None and now - heartbeat_at >= 1.0:
                if process.poll() is not None:
                    raise OcoreSmokeError("QEMU exited during post-heartbeat liveness")
                break
            if now >= deadline:
                if not selected:
                    waiting = "GRUB O-core menu"
                elif heartbeat_at is not None or marker_index >= len(REQUIRED_MARKERS):
                    waiting = "post-heartbeat liveness"
                else:
                    waiting = REQUIRED_MARKERS[marker_index].decode()
                raise OcoreSmokeError(
                    f"timed out waiting for {waiting!r}: {_diagnostic(transcript)}"
                )
            if process.poll() is not None:
                raise OcoreSmokeError(
                    f"QEMU exited before O-core readiness: status={process.returncode}: "
                    f"{_diagnostic(transcript)}"
                )
            for key, _ in selector.select(min(0.1, deadline - now)):
                chunk = os.read(key.fileobj.fileno(), READ_CHUNK_BYTES)
                if not chunk:
                    selector.unregister(key.fileobj)
                    output_eof = True
                    continue
                admit(chunk)

            if not selected and MENU_MARKER in transcript:
                process.stdin.write(b"o")
                process.stdin.flush()
                selected = True

            while selected and marker_index < len(REQUIRED_MARKERS):
                marker = REQUIRED_MARKERS[marker_index]
                position = transcript.find(marker, search_offset)
                if position < 0:
                    break
                search_offset = position + len(marker)
                marker_index += 1
                if marker_index == len(REQUIRED_MARKERS):
                    heartbeat_at = time.monotonic()
            if output_eof and heartbeat_at is None:
                raise OcoreSmokeError(
                    "QEMU output closed before O-core readiness: " + _diagnostic(transcript)
                )

        process.stdin.write(b"\x01x")
        process.stdin.flush()
        # QEMU and the guest may emit output after the last readiness marker.
        # Drain through EOF so a queued panic or fault cannot hide behind the
        # successful heartbeat and every admitted byte belongs to the receipt.
        cleanup_deadline = time.monotonic() + 5.0
        while not output_eof:
            remaining = cleanup_deadline - time.monotonic()
            if remaining <= 0:
                raise OcoreSmokeError(
                    "QEMU output did not close after the successful O-core gate"
                )
            events = selector.select(min(0.1, remaining))
            if not events:
                continue
            for key, _ in events:
                chunk = os.read(key.fileobj.fileno(), READ_CHUNK_BYTES)
                if chunk:
                    admit(chunk)
                else:
                    selector.unregister(key.fileobj)
                    output_eof = True
        try:
            exit_code = process.wait(
                timeout=max(0.001, cleanup_deadline - time.monotonic())
            )
        except subprocess.TimeoutExpired as error:
            raise OcoreSmokeError("QEMU did not exit after the successful O-core gate") from error
        if exit_code != 0:
            raise OcoreSmokeError(f"QEMU returned status {exit_code} after O-core readiness")
        if _identity_state(os.fstat(iso_input.descriptor)) != _identity_state(iso_input.state):
            raise OcoreSmokeError("Hosted Live ISO changed during O-core smoke")
        if _sha256_descriptor(iso_input.descriptor) != iso_sha256:
            raise OcoreSmokeError("Hosted Live ISO content changed during O-core smoke")
        if _identity_state(os.fstat(firmware_input.descriptor)) != _identity_state(
            firmware_input.state
        ):
            raise OcoreSmokeError("OVMF code changed during O-core smoke")
        return {
            "schema": "ostadix.hosted-live-ocore-qemu-smoke/v1",
            "selected_entry": "ocore",
            "selection_method": "grub-hotkey-o",
            "markers": [marker.decode("ascii") for marker in REQUIRED_MARKERS],
            "transcript_bytes": len(transcript),
            "transcript_sha256": digest.hexdigest(),
            "exit_code": exit_code,
            "acceleration": "tcg",
            "firmware": {
                "bytes": firmware_input.state.st_size,
                "sha256": _sha256_descriptor(firmware_input.descriptor),
            },
            "iso": {"bytes": iso_input.state.st_size, "sha256": iso_sha256},
            "network": "none",
            "physical_hardware_proof": False,
        }
    finally:
        if selector is not None:
            selector.close()
        if process is not None:
            _terminate(process)
            if process.stdin is not None:
                process.stdin.close()
            if process.stdout is not None:
                process.stdout.close()
            if process.stderr is not None:
                process.stderr.close()
        os.close(firmware_input.descriptor)
        os.close(iso_input.descriptor)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("iso", nargs="?", type=Path, default=DEFAULT_ISO)
    parser.add_argument("--firmware", required=True, type=Path)
    parser.add_argument("--qemu", default="qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument("--transcript", type=Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    stream: BinaryIO | None = None
    try:
        if arguments.transcript is not None:
            arguments.transcript.parent.mkdir(parents=True, exist_ok=True)
            stream = arguments.transcript.open("xb")
        result = run_ocore_gate(
            arguments.iso.expanduser().resolve(),
            arguments.firmware.expanduser().resolve(),
            qemu=arguments.qemu,
            timeout_seconds=arguments.timeout,
            transcript_output=stream,
        )
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    except (OSError, OcoreSmokeError) as error:
        print(f"hosted-live-ocore-smoke: ERROR: {error}", file=sys.stderr)
        return 1
    finally:
        if stream is not None:
            stream.close()


if __name__ == "__main__":
    raise SystemExit(main())
