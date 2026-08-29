#!/usr/bin/env python3
"""Boot the exact Hosted Live ISO and prove visible, keyboard-driven VT use."""

from __future__ import annotations

import argparse
import contextlib
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
from typing import Sequence


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_ISO = (
    ROOT
    / "target/ostadix-hosted-live/x86_64/ostadix-hosted-live-x86_64-uefi_VTGRUB2.iso"
)
DEFAULT_TIMEOUT_SECONDS = 180.0
MAX_SERIAL_BYTES = 8 * 1024 * 1024
MIN_NONBLACK_PIXELS = 2_000
# A Linux text VT may intentionally be monochrome: background plus glyph color.
MIN_UNIQUE_COLORS = 2
MIN_CHANGED_PIXELS = 200
MAX_MONITOR_SOCKET_PATH_BYTES = 96
INPUT_COMMAND = "echo vga-input-pass >/dev/ttyS0\n"
INPUT_MARKER = b"vga-input-pass"
REQUIRED_MARKERS = (
    b"OSTADIX HOSTED O SMOKE: PASS",
    b"OSTADIX HOSTED BASH: PASS",
    b"OSTADIX HOSTED SQLITE: PASS",
    b"OSTADIX HOSTED OLANGC IR: PASS",
    b"OSTADIX HOSTED O-CLI: PASS",
    b"OSTADIX HOSTED O-LINK: PASS",
    b"OSTADIX HOSTED LIVE READY",
)
FAILURE_MARKERS = (b"FAIL", b"Kernel panic", b"OSTADIX CAPACITY HOST ERROR")


class VisualSmokeError(RuntimeError):
    """The exact ISO did not establish visible, interactive VT readiness."""


@dataclass(frozen=True)
class Frame:
    path: Path
    width: int
    height: int
    pixels: bytes
    sha256: str
    nonblack_pixels: int
    unique_colors: int

    def public(self) -> dict[str, object]:
        return {
            "bytes": self.path.stat().st_size,
            "sha256": self.sha256,
            "width": self.width,
            "height": self.height,
            "nonblack_pixels": self.nonblack_pixels,
            "unique_colors": self.unique_colors,
        }


def _identity(path: Path) -> dict[str, object]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
    return {"bytes": size, "sha256": digest.hexdigest()}


def _open_regular(path: Path, label: str) -> tuple[int, os.stat_result]:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
    if not hasattr(os, "O_NOFOLLOW"):
        raise VisualSmokeError("host lacks O_NOFOLLOW for visual-smoke inputs")
    try:
        descriptor = os.open(path, flags | os.O_NOFOLLOW)
    except OSError as error:
        raise VisualSmokeError(f"cannot open {label}: {path}: {error}") from error
    state = os.fstat(descriptor)
    if not stat.S_ISREG(state.st_mode) or state.st_size <= 0:
        os.close(descriptor)
        raise VisualSmokeError(f"{label} is not a non-empty regular file: {path}")
    return descriptor, state


def _same_file(descriptor: int, expected: os.stat_result, label: str) -> None:
    current = os.fstat(descriptor)
    identity = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns)
    if identity(current) != identity(expected):
        raise VisualSmokeError(f"{label} changed during the visual smoke")


def _wait_for_path(path: Path, deadline: float, label: str) -> None:
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.02)
    raise VisualSmokeError(f"timed out waiting for {label}: {path}")


def _qemu_log_tail(path: Path, limit: int = 4096) -> str:
    if not path.exists():
        return "<QEMU log not created>"
    with path.open("rb") as stream:
        stream.seek(max(0, path.stat().st_size - limit))
        return stream.read(limit).decode("utf-8", "replace").strip()


def _wait_for_monitor(
    path: Path,
    process: subprocess.Popen[bytes],
    qemu_log: Path,
    deadline: float,
) -> None:
    while time.monotonic() < deadline:
        if path.exists():
            return
        if process.poll() is not None:
            raise VisualSmokeError(
                f"QEMU exited before creating its monitor: status={process.returncode}: "
                f"{_qemu_log_tail(qemu_log)}"
            )
        time.sleep(0.02)
    raise VisualSmokeError(f"timed out waiting for QEMU monitor socket: {path}")


def _allocate_monitor_socket() -> tuple[tempfile.TemporaryDirectory, Path]:
    """Allocate QEMU's AF_UNIX endpoint outside potentially deep evidence paths."""
    directory = tempfile.TemporaryDirectory(prefix="ostadix-vga-monitor.")
    path = Path(directory.name) / "qemu.sock"
    encoded_length = len(os.fsencode(path))
    if encoded_length > MAX_MONITOR_SOCKET_PATH_BYTES:
        directory.cleanup()
        raise VisualSmokeError(
            "temporary QEMU monitor socket path is too long: "
            f"{encoded_length} bytes (maximum {MAX_MONITOR_SOCKET_PATH_BYTES})"
        )
    return directory, path


def _fd_reference(descriptor: int) -> str:
    for root in (Path("/proc/self/fd"), Path("/dev/fd")):
        if root.is_dir():
            return str(root / str(descriptor))
    raise VisualSmokeError("host exposes neither /proc/self/fd nor /dev/fd")


def _read_serial(path: Path) -> bytes:
    if not path.exists():
        return b""
    size = path.stat().st_size
    if size > MAX_SERIAL_BYTES:
        raise VisualSmokeError(f"visual-smoke serial transcript exceeded {MAX_SERIAL_BYTES} bytes")
    return path.read_bytes()


def _require_ordered_markers(transcript: bytes) -> None:
    offset = 0
    for marker in REQUIRED_MARKERS:
        position = transcript.find(marker, offset)
        if position < 0:
            raise VisualSmokeError(f"serial transcript omitted marker {marker.decode()!r}")
        offset = position + len(marker)
    for marker in FAILURE_MARKERS:
        if marker in transcript:
            raise VisualSmokeError(
                f"serial transcript contains failure marker {marker.decode('ascii', 'replace')!r}"
            )


class Hmp:
    def __init__(self, path: Path, deadline: float) -> None:
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            while True:
                try:
                    self.socket.connect(str(path))
                    break
                except (FileNotFoundError, ConnectionRefusedError):
                    if time.monotonic() >= deadline:
                        raise VisualSmokeError("timed out connecting to QEMU monitor")
                    time.sleep(0.02)
            self.socket.settimeout(max(0.05, deadline - time.monotonic()))
            self._until_prompt()
        except BaseException:
            self.socket.close()
            raise

    def close(self) -> None:
        self.socket.close()

    def _until_prompt(self) -> bytes:
        response = bytearray()
        while b"(qemu)" not in response:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise VisualSmokeError("QEMU monitor closed before its prompt")
            response.extend(chunk)
            if len(response) > 1024 * 1024:
                raise VisualSmokeError("QEMU monitor response exceeded one MiB")
        return bytes(response)

    def command(self, value: str) -> bytes:
        self.socket.sendall(value.encode("ascii") + b"\n")
        response = self._until_prompt()
        if b"unknown command" in response.lower() or b"error" in response.lower():
            raise VisualSmokeError(
                f"QEMU monitor rejected {value!r}: {response.decode('utf-8', 'replace')[-1024:]}"
            )
        return response

    def quit(self) -> None:
        """Ask QEMU to exit without waiting for the prompt it will never send."""
        self.socket.sendall(b"quit\n")


def _key_name(character: str) -> str:
    if "a" <= character <= "z" or "0" <= character <= "9":
        return character
    if "A" <= character <= "Z":
        return f"shift-{character.lower()}"
    mapping = {
        " ": "spc",
        "-": "minus",
        "/": "slash",
        ">": "shift-dot",
        "\n": "ret",
    }
    try:
        return mapping[character]
    except KeyError as error:
        raise VisualSmokeError(f"input command contains unsupported key {character!r}") from error


def _type_command(monitor: Hmp, command: str) -> None:
    for character in command:
        monitor.command(f"sendkey {_key_name(character)} 35")
        time.sleep(0.045)


def _ppm_tokens(raw: bytes) -> tuple[list[bytes], int]:
    tokens: list[bytes] = []
    offset = 0
    while len(tokens) < 4:
        while offset < len(raw) and raw[offset] in b" \t\r\n":
            offset += 1
        if offset < len(raw) and raw[offset] == ord("#"):
            while offset < len(raw) and raw[offset] not in b"\r\n":
                offset += 1
            continue
        start = offset
        while offset < len(raw) and raw[offset] not in b" \t\r\n":
            offset += 1
        if start == offset:
            raise VisualSmokeError("QEMU screendump has a truncated PPM header")
        tokens.append(raw[start:offset])
    if offset >= len(raw) or raw[offset] not in b" \t\r\n":
        raise VisualSmokeError("QEMU screendump omits the PPM raster delimiter")
    offset += 2 if raw[offset : offset + 2] == b"\r\n" else 1
    return tokens, offset


def read_frame(path: Path) -> Frame:
    raw = path.read_bytes()
    tokens, offset = _ppm_tokens(raw)
    if tokens[0] != b"P6" or not re.fullmatch(rb"[0-9]+", tokens[1]) \
            or not re.fullmatch(rb"[0-9]+", tokens[2]) or tokens[3] != b"255":
        raise VisualSmokeError("QEMU screendump is not a bounded 8-bit P6 PPM")
    width, height = int(tokens[1]), int(tokens[2])
    if not (320 <= width <= 4096 and 200 <= height <= 2160):
        raise VisualSmokeError(f"QEMU screendump dimensions are implausible: {width}x{height}")
    pixels = raw[offset:]
    if len(pixels) != width * height * 3:
        raise VisualSmokeError("QEMU screendump pixel payload has the wrong length")
    colors = {pixels[index : index + 3] for index in range(0, len(pixels), 3)}
    nonblack = sum(
        1 for index in range(0, len(pixels), 3) if max(pixels[index : index + 3]) > 8
    )
    return Frame(
        path=path,
        width=width,
        height=height,
        pixels=pixels,
        sha256=hashlib.sha256(raw).hexdigest(),
        nonblack_pixels=nonblack,
        unique_colors=len(colors),
    )


def validate_visible_frame(frame: Frame) -> None:
    if frame.nonblack_pixels < MIN_NONBLACK_PIXELS:
        raise VisualSmokeError(
            f"graphical console is effectively black: {frame.nonblack_pixels} nonblack pixels"
        )
    if frame.unique_colors < MIN_UNIQUE_COLORS:
        raise VisualSmokeError(
            f"graphical console lacks text/color diversity: {frame.unique_colors} colors"
        )


def changed_pixel_count(before: Frame, after: Frame) -> int:
    if (before.width, before.height) != (after.width, after.height):
        raise VisualSmokeError("graphical console dimensions changed during input proof")
    changed = sum(
        1
        for index in range(0, len(before.pixels), 3)
        if before.pixels[index : index + 3] != after.pixels[index : index + 3]
    )
    if changed < MIN_CHANGED_PIXELS:
        raise VisualSmokeError(
            f"graphical console did not visibly react to input: {changed} pixels changed"
        )
    return changed


def _capture(monitor: Hmp, path: Path, deadline: float) -> Frame:
    path.unlink(missing_ok=True)
    monitor.command(f"screendump {path}")
    _wait_for_path(path, deadline, "QEMU screendump")
    frame = read_frame(path)
    validate_visible_frame(frame)
    return frame


def _terminate(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=2)


def run_visual_gate(
    iso: Path,
    firmware: Path,
    *,
    qemu: str,
    evidence_dir: Path,
    timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
) -> dict[str, object]:
    if not (1 <= timeout_seconds <= 900):
        raise VisualSmokeError("timeout must be from 1 through 900 seconds")
    evidence_dir.mkdir(parents=True, exist_ok=False)
    resources = contextlib.ExitStack()
    process: subprocess.Popen[bytes] | None = None
    monitor: Hmp | None = None
    try:
        iso_fd, iso_state = _open_regular(iso, "Hosted Live ISO")
        resources.callback(os.close, iso_fd)
        firmware_fd, firmware_state = _open_regular(firmware, "OVMF code")
        resources.callback(os.close, firmware_fd)
        serial = evidence_dir / "serial.log"
        qemu_log = evidence_dir / "qemu.log"
        monitor_directory, monitor_path = _allocate_monitor_socket()
        resources.callback(monitor_directory.cleanup)
        before_path = evidence_dir / "before.ppm"
        after_path = evidence_dir / "after.ppm"
        deadline = time.monotonic() + timeout_seconds
        with qemu_log.open("wb") as log:
            command = [
                qemu,
                "-accel", "tcg",
                "-machine", "q35",
                "-cpu", "max",
                "-smp", "2",
                "-m", "2048M",
                "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={_fd_reference(firmware_fd)}",
                "-drive", f"if=ide,index=2,media=cdrom,format=raw,readonly=on,file={_fd_reference(iso_fd)}",
                "-boot", "order=d,strict=on",
                "-nodefaults",
                "-device", "VGA",
                "-device", "qemu-xhci,id=xhci",
                "-device", "usb-kbd,bus=xhci.0",
                "-nic", "none",
                "-display", "none",
                "-serial", f"file:{serial}",
                "-monitor", f"unix:{monitor_path},server=on,wait=off",
                "-no-reboot",
                "-no-shutdown",
            ]
            process = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
                pass_fds=(iso_fd, firmware_fd),
                start_new_session=True,
            )
            _wait_for_monitor(monitor_path, process, qemu_log, deadline)
            monitor = Hmp(monitor_path, deadline)
            transcript = b""
            while time.monotonic() < deadline:
                transcript = _read_serial(serial)
                if REQUIRED_MARKERS[-1] in transcript:
                    break
                if process.poll() is not None:
                    raise VisualSmokeError(
                        f"QEMU exited before graphical readiness: status={process.returncode}"
                    )
                time.sleep(0.05)
            else:
                raise VisualSmokeError("timed out before Hosted Live graphical readiness")
            _require_ordered_markers(transcript)
            before = _capture(monitor, before_path, deadline)
            _type_command(monitor, INPUT_COMMAND)
            while time.monotonic() < deadline:
                transcript = _read_serial(serial)
                if INPUT_MARKER in transcript:
                    break
                if process.poll() is not None:
                    raise VisualSmokeError("QEMU exited before graphical keyboard proof")
                time.sleep(0.05)
            else:
                raise VisualSmokeError("visible VT shell did not accept the emulated USB keyboard")
            after = _capture(monitor, after_path, deadline)
            changed_pixels = changed_pixel_count(before, after)
            _same_file(iso_fd, iso_state, "Hosted Live ISO")
            _same_file(firmware_fd, firmware_state, "OVMF code")
            monitor.quit()
            try:
                exit_code = process.wait(timeout=5)
            except subprocess.TimeoutExpired as error:
                raise VisualSmokeError("QEMU did not exit after successful visual proof") from error
            if exit_code != 0:
                raise VisualSmokeError(f"QEMU returned status {exit_code} after visual proof")
            serial_identity = _identity(serial)
            return {
                "schema": "ostadix.hosted-live-qemu-visual-smoke/v1",
                "markers": [marker.decode("ascii") for marker in REQUIRED_MARKERS],
                "input_marker": INPUT_MARKER.decode("ascii"),
                "serial": serial_identity,
                "frame_before": before.public(),
                "frame_after": after.public(),
                "changed_pixels": changed_pixels,
                "acceleration": "tcg",
                "firmware": _identity(firmware),
                "display_device": "VGA",
                "input_device": "usb-kbd",
                "network": "none",
                "physical_hardware_proof": False,
            }
    finally:
        try:
            if monitor is not None:
                monitor.close()
        finally:
            try:
                if process is not None:
                    _terminate(process)
            finally:
                resources.close()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("iso", nargs="?", type=Path, default=DEFAULT_ISO)
    parser.add_argument("--firmware", required=True, type=Path)
    parser.add_argument("--qemu", default="qemu-system-x86_64")
    parser.add_argument("--evidence-dir", type=Path)
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_SECONDS)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    iso = arguments.iso.expanduser().resolve()
    firmware = arguments.firmware.expanduser().resolve()
    evidence_dir = arguments.evidence_dir
    try:
        if evidence_dir is None:
            with tempfile.TemporaryDirectory(prefix="ostadix-hosted-vga-smoke.") as temporary:
                result = run_visual_gate(
                    iso,
                    firmware,
                    qemu=arguments.qemu,
                    evidence_dir=Path(temporary) / "evidence",
                    timeout_seconds=arguments.timeout,
                )
        else:
            result = run_visual_gate(
                iso,
                firmware,
                qemu=arguments.qemu,
                evidence_dir=evidence_dir.resolve(),
                timeout_seconds=arguments.timeout,
            )
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    except (OSError, VisualSmokeError) as error:
        print(f"hosted-live-vga-smoke: ERROR: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
