#!/usr/bin/env python3
"""Boot the exact Hosted Live ISO and prove an interactive Openbox desktop."""

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
DEFAULT_TIMEOUT_SECONDS = 1800.0
MAX_TIMEOUT_SECONDS = 1800.0
MAX_SERIAL_BYTES = 8 * 1024 * 1024
MIN_NONBLACK_PIXELS = 20_000
# Eight raw colors plus three populated chromatic hue families distinguish the
# Openbox/Xterm palette from a monochrome, anti-aliased Linux text VT.
MIN_UNIQUE_COLORS = 8
MIN_CHROMATIC_PIXELS = 500
MIN_CHROMATIC_HUE_BUCKETS = 3
MIN_PIXELS_PER_HUE_BUCKET = 20
MIN_CHROMATIC_MAX_CHANNEL = 48
MIN_CHROMATIC_CHANNEL_SPREAD = 32
MIN_CHANGED_PIXELS = 200
MAX_MONITOR_SOCKET_PATH_BYTES = 96
INPUT_COMMAND = "echo vga-input-pass >/dev/ttyS0\n"
INPUT_MARKER = b"vga-input-pass"
DESKTOP_READY_MARKER = b"OSTADIX HOSTED DESKTOP READY: PASS"
FONT_READY_MARKER = b"OSTADIX HOSTED X11 FONT: PASS"
PTY_READY_MARKER = b"OSTADIX HOSTED PTY: PASS"
EVDEV_READY_MARKER = b"OSTADIX HOSTED EVDEV: PASS"
NOTEBOOK_GUI_READY_MARKER = b"OSTADIX HOSTED NOTEBOOK GUI READY: PASS"
VISUAL_SMOKE_SCHEMA = "ostadix.hosted-live-qemu-visual-smoke/v7"
DESKTOP_SESSION = "openbox-xterm"
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
    FONT_READY_MARKER,
    PTY_READY_MARKER,
    EVDEV_READY_MARKER,
    NOTEBOOK_GUI_READY_MARKER,
    DESKTOP_READY_MARKER,
)
FAILURE_MARKERS = (b"FAIL", b"Kernel panic", b"OSTADIX CAPACITY HOST ERROR")


class VisualSmokeError(RuntimeError):
    """The exact ISO did not establish visible, interactive desktop readiness."""


@dataclass(frozen=True)
class Frame:
    path: Path
    width: int
    height: int
    pixels: bytes
    sha256: str
    nonblack_pixels: int
    unique_colors: int
    chromatic_pixels: int
    chromatic_hue_buckets: int

    def public(self) -> dict[str, object]:
        return {
            "bytes": self.path.stat().st_size,
            "sha256": self.sha256,
            "width": self.width,
            "height": self.height,
            "nonblack_pixels": self.nonblack_pixels,
            "unique_colors": self.unique_colors,
            "chromatic_pixels": self.chromatic_pixels,
            "chromatic_hue_buckets": self.chromatic_hue_buckets,
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


def _descriptor_identity(descriptor: int, expected: os.stat_result, label: str) -> dict[str, object]:
    digest = hashlib.sha256()
    size = 0
    while chunk := os.pread(descriptor, 1024 * 1024, size):
        digest.update(chunk)
        size += len(chunk)
    if size != expected.st_size:
        raise VisualSmokeError(f"{label} size changed while hashing the pinned descriptor")
    return {"bytes": size, "sha256": digest.hexdigest()}


def _same_file(descriptor: int, expected: os.stat_result, label: str) -> None:
    current = os.fstat(descriptor)
    identity = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns)
    if identity(current) != identity(expected):
        raise VisualSmokeError(f"{label} changed during the visual smoke")


def _require_unchanged_descriptor(
    descriptor: int,
    expected_state: os.stat_result,
    expected_identity: dict[str, object],
    label: str,
) -> None:
    _same_file(descriptor, expected_state, label)
    if _descriptor_identity(descriptor, expected_state, label) != expected_identity:
        raise VisualSmokeError(f"{label} content changed during the visual smoke")


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


def _raise_for_failure_markers(transcript: bytes) -> None:
    for marker in FAILURE_MARKERS:
        if marker in transcript:
            raise VisualSmokeError(
                f"serial transcript contains failure marker {marker.decode('ascii', 'replace')!r}"
            )


def _require_ordered_markers(transcript: bytes) -> None:
    _raise_for_failure_markers(transcript)
    offset = 0
    positions = []
    for marker in REQUIRED_MARKERS:
        position = transcript.find(marker, offset)
        if position < 0:
            raise VisualSmokeError(f"serial transcript omitted marker {marker.decode()!r}")
        positions.append(position)
        offset = position + len(marker)
    _, entropy_position = _validated_entropy_identity(transcript)
    entropy_slot = REQUIRED_MARKERS.index(ENTROPY_ORDERED_MARKER)
    if positions[entropy_slot] != entropy_position:
        raise VisualSmokeError(
            "full Hosted entropy marker did not occupy its ordered position"
        )
    _, _, wasm_materialization_position, wasm_artifact_position = (
        _parse_wasm_identity(transcript)
    )
    if positions[REQUIRED_MARKERS.index(WASM_MATERIALIZATION_PREFIX.rstrip())] \
            != wasm_materialization_position:
        raise VisualSmokeError(
            "full Olangc materialization marker did not occupy its ordered position"
        )
    if positions[REQUIRED_MARKERS.index(WASM_ARTIFACT_PREFIX.rstrip())] \
            != wasm_artifact_position:
        raise VisualSmokeError(
            "full Olangc WASM artifact marker did not occupy its ordered position"
        )


def _parse_rootfs_identity(transcript: bytes) -> dict[str, object]:
    candidates = [
        line
        for line in transcript.splitlines()
        if line.startswith(ROOTFS_IDENTITY_PREFIX)
    ]
    if len(candidates) != 1:
        raise VisualSmokeError(
            "completed transcript must contain exactly one full Hosted rootfs identity marker"
        )
    match = ROOTFS_IDENTITY_RE.fullmatch(candidates[0])
    if match is None:
        raise VisualSmokeError(
            "completed transcript must contain exactly one full Hosted rootfs identity marker"
        )
    return {
        "bytes": int(match.group(1)),
        "sha256": match.group(2).decode("ascii"),
    }


def _validated_entropy_identity(
    transcript: bytes,
) -> tuple[dict[str, object], int]:
    candidates: list[tuple[int, bytes]] = []
    offset = 0
    for raw_line in transcript.splitlines(keepends=True):
        line = raw_line.rstrip(b"\r\n")
        if line.startswith(ENTROPY_IDENTITY_PREFIX):
            candidates.append((offset, line))
        offset += len(raw_line)
    if len(candidates) != 1:
        raise VisualSmokeError(
            "completed transcript must contain exactly one full Hosted entropy marker"
        )
    position, line = candidates[0]
    match = ENTROPY_IDENTITY_RE.fullmatch(line)
    if match is None:
        raise VisualSmokeError(
            "completed transcript must contain exactly one full Hosted entropy marker"
        )
    available = int(match.group(1))
    if available < MIN_ENTROPY_BITS:
        raise VisualSmokeError(
            f"Hosted entropy marker reported only {available} available bits"
        )
    return (
        {
            "device": "virtio-rng-pci",
            "crng_bytes": 32,
            "available": available,
        },
        position,
    )


def _parse_entropy_identity(transcript: bytes) -> dict[str, object]:
    return _validated_entropy_identity(transcript)[0]


def _parse_wasm_identity(
    transcript: bytes,
) -> tuple[dict[str, object], dict[str, object], int, int]:
    materializations: list[tuple[int, bytes]] = []
    artifacts: list[tuple[int, bytes]] = []
    offset = 0
    for raw_line in transcript.splitlines(keepends=True):
        line = raw_line.rstrip(b"\r\n")
        if line.startswith(WASM_MATERIALIZATION_PREFIX):
            materializations.append((offset, line))
        if line.startswith(WASM_ARTIFACT_PREFIX):
            artifacts.append((offset, line))
        offset += len(raw_line)
    if len(materializations) != 1 or len(artifacts) != 1:
        raise VisualSmokeError(
            "completed transcript must contain exactly one full Olangc WASM identity chain"
        )
    materialization_position, materialization_line = materializations[0]
    artifact_position, artifact_line = artifacts[0]
    materialization_match = WASM_MATERIALIZATION_RE.fullmatch(materialization_line)
    artifact_match = WASM_ARTIFACT_RE.fullmatch(artifact_line)
    if materialization_match is None or artifact_match is None:
        raise VisualSmokeError(
            "completed transcript contains a malformed Olangc WASM identity chain"
        )
    return (
        {
            "root_sha256": materialization_match.group(1).decode("ascii"),
        },
        {
            "staged_tree": artifact_match.group(1).decode("ascii"),
            "bytes": int(artifact_match.group(2)),
            "sha256": artifact_match.group(3).decode("ascii"),
            "materialized_project_sha256": materialization_match.group(1).decode(
                "ascii"
            ),
        },
        materialization_position,
        artifact_position,
    )


def _input_marker_after_desktop(transcript: bytes) -> bool:
    desktop = transcript.find(DESKTOP_READY_MARKER)
    return desktop >= 0 and transcript.find(INPUT_MARKER, desktop + len(DESKTOP_READY_MARKER)) >= 0


def _remaining_before_deadline(deadline: float, action: str) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise VisualSmokeError(f"timed out {action}")
    return remaining


class Hmp:
    def __init__(self, path: Path, deadline: float) -> None:
        self.deadline = deadline
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self._connect(path)
            self._until_prompt("waiting for QEMU monitor prompt")
        except BaseException:
            self.socket.close()
            raise

    def close(self) -> None:
        self.socket.close()

    def _remaining(self, action: str) -> float:
        return _remaining_before_deadline(self.deadline, action)

    def _arm(self, action: str) -> None:
        self.socket.settimeout(self._remaining(action))

    def _sleep(self, seconds: float, action: str) -> None:
        time.sleep(min(seconds, self._remaining(action)))
        self._remaining(action)

    def _connect(self, path: Path) -> None:
        action = "connecting to QEMU monitor"
        while True:
            try:
                self._arm(action)
                self.socket.connect(str(path))
            except socket.timeout as error:
                raise VisualSmokeError(f"timed out {action}") from error
            except (FileNotFoundError, ConnectionRefusedError):
                self._sleep(0.02, action)
                continue
            self._remaining(action)
            return

    def _send(self, payload: bytes, action: str) -> None:
        try:
            self._arm(action)
            self.socket.sendall(payload)
        except socket.timeout as error:
            raise VisualSmokeError(f"timed out {action}") from error
        self._remaining(action)

    def _recv(self, action: str) -> bytes:
        try:
            self._arm(action)
            chunk = self.socket.recv(4096)
        except socket.timeout as error:
            raise VisualSmokeError(f"timed out {action}") from error
        self._remaining(action)
        return chunk

    def _until_prompt(self, action: str) -> bytes:
        response = bytearray()
        while b"(qemu)" not in response:
            chunk = self._recv(action)
            if not chunk:
                raise VisualSmokeError("QEMU monitor closed before its prompt")
            response.extend(chunk)
            if len(response) > 1024 * 1024:
                raise VisualSmokeError("QEMU monitor response exceeded one MiB")
        return bytes(response)

    def command(self, value: str) -> bytes:
        self._send(
            value.encode("ascii") + b"\n",
            "sending a QEMU monitor command",
        )
        response = self._until_prompt("waiting for a QEMU monitor command response")
        if b"unknown command" in response.lower() or b"error" in response.lower():
            raise VisualSmokeError(
                f"QEMU monitor rejected {value!r}: {response.decode('utf-8', 'replace')[-1024:]}"
            )
        return response

    def quit(self) -> None:
        """Ask QEMU to exit without waiting for the prompt it will never send."""
        self._send(b"quit\n", "sending QEMU monitor quit")


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
        monitor._sleep(0.045, "typing the graphical input command")


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
    chromatic = 0
    hue_counts = [0] * 6
    for index in range(0, len(pixels), 3):
        red, green, blue = pixels[index : index + 3]
        maximum = max(red, green, blue)
        minimum = min(red, green, blue)
        chroma = maximum - minimum
        if (
            maximum < MIN_CHROMATIC_MAX_CHANNEL
            or chroma < MIN_CHROMATIC_CHANNEL_SPREAD
        ):
            continue
        chromatic += 1
        if maximum == minimum:
            continue
        if maximum == red:
            hue = (green - blue) / chroma
        elif maximum == green:
            hue = 2.0 + (blue - red) / chroma
        else:
            hue = 4.0 + (red - green) / chroma
        hue_counts[int((hue % 6.0))] += 1
    populated_hues = sum(count >= MIN_PIXELS_PER_HUE_BUCKET for count in hue_counts)
    return Frame(
        path=path,
        width=width,
        height=height,
        pixels=pixels,
        sha256=hashlib.sha256(raw).hexdigest(),
        nonblack_pixels=nonblack,
        unique_colors=len(colors),
        chromatic_pixels=chromatic,
        chromatic_hue_buckets=populated_hues,
    )


def validate_visible_frame(frame: Frame) -> None:
    if frame.nonblack_pixels < MIN_NONBLACK_PIXELS:
        raise VisualSmokeError(
            f"graphical console is effectively black: {frame.nonblack_pixels} nonblack pixels"
        )
    if frame.unique_colors < MIN_UNIQUE_COLORS:
        raise VisualSmokeError(
            f"desktop lacks raw color diversity: {frame.unique_colors} colors"
        )
    if frame.chromatic_pixels < MIN_CHROMATIC_PIXELS:
        raise VisualSmokeError(
            "desktop lacks chromatic area: "
            f"{frame.chromatic_pixels} chromatic pixels"
        )
    if frame.chromatic_hue_buckets < MIN_CHROMATIC_HUE_BUCKETS:
        raise VisualSmokeError(
            "desktop lacks independent hue families: "
            f"{frame.chromatic_hue_buckets} populated hue buckets"
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


def _validate_timeout_seconds(timeout_seconds: float) -> None:
    if not (1 <= timeout_seconds <= MAX_TIMEOUT_SECONDS):
        raise VisualSmokeError(
            f"timeout must be from 1 through {MAX_TIMEOUT_SECONDS:g} seconds"
        )


def run_visual_gate(
    iso: Path,
    firmware: Path,
    *,
    qemu: str,
    evidence_dir: Path,
    timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
) -> dict[str, object]:
    _validate_timeout_seconds(timeout_seconds)
    evidence_dir.mkdir(parents=True, exist_ok=False)
    resources = contextlib.ExitStack()
    process: subprocess.Popen[bytes] | None = None
    monitor: Hmp | None = None
    try:
        iso_fd, iso_state = _open_regular(iso, "Hosted Live ISO")
        resources.callback(os.close, iso_fd)
        iso_identity = _descriptor_identity(iso_fd, iso_state, "Hosted Live ISO")
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
                "-m", "4096M",
                "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={_fd_reference(firmware_fd)}",
                "-drive", f"if=ide,index=2,media=cdrom,format=raw,readonly=on,file={_fd_reference(iso_fd)}",
                "-boot", "order=d,strict=on",
                "-nodefaults",
                # Model physical entropy without relaxing the no-network gate.
                "-object", "rng-random,filename=/dev/urandom,id=ostadix_rng",
                "-device", "virtio-rng-pci,rng=ostadix_rng",
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
                _raise_for_failure_markers(transcript)
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
                _raise_for_failure_markers(transcript)
                if _input_marker_after_desktop(transcript):
                    break
                if process.poll() is not None:
                    raise VisualSmokeError("QEMU exited before graphical keyboard proof")
                time.sleep(0.05)
            else:
                raise VisualSmokeError("focused desktop Xterm did not accept the emulated USB keyboard")
            after = _capture(monitor, after_path, deadline)
            changed_pixels = changed_pixel_count(before, after)
            _same_file(iso_fd, iso_state, "Hosted Live ISO")
            _same_file(firmware_fd, firmware_state, "OVMF code")
            monitor.quit()
            exit_action = "waiting for QEMU to exit after successful visual proof"
            exit_wait_seconds = _remaining_before_deadline(deadline, exit_action)
            try:
                exit_code = process.wait(timeout=min(5.0, exit_wait_seconds))
            except subprocess.TimeoutExpired as error:
                raise VisualSmokeError("QEMU did not exit after successful visual proof") from error
            _remaining_before_deadline(deadline, exit_action)
            if exit_code != 0:
                raise VisualSmokeError(f"QEMU returned status {exit_code} after visual proof")
            # Read once more after QEMU has exited and flushed the serial file.
            # A desktop failure emitted after LIVE READY or even after the input
            # marker remains fatal to the exact-session proof.
            transcript = _read_serial(serial)
            _require_ordered_markers(transcript)
            rootfs_identity = _parse_rootfs_identity(transcript)
            entropy_identity = _parse_entropy_identity(transcript)
            _, wasm_identity, _, _ = _parse_wasm_identity(transcript)
            if not _input_marker_after_desktop(transcript):
                raise VisualSmokeError("serial input marker did not follow desktop readiness")
            _require_unchanged_descriptor(
                iso_fd,
                iso_state,
                iso_identity,
                "Hosted Live ISO",
            )
            _same_file(firmware_fd, firmware_state, "OVMF code")
            firmware_identity = _descriptor_identity(
                firmware_fd, firmware_state, "OVMF code"
            )
            serial_identity = _identity(serial)
            return {
                "schema": VISUAL_SMOKE_SCHEMA,
                "markers": [marker.decode("ascii") for marker in REQUIRED_MARKERS],
                "font_marker": FONT_READY_MARKER.decode("ascii"),
                "pty_marker": PTY_READY_MARKER.decode("ascii"),
                "evdev_marker": EVDEV_READY_MARKER.decode("ascii"),
                "notebook_gui_marker": NOTEBOOK_GUI_READY_MARKER.decode("ascii"),
                "desktop_marker": DESKTOP_READY_MARKER.decode("ascii"),
                "input_marker": INPUT_MARKER.decode("ascii"),
                "session": DESKTOP_SESSION,
                "iso": iso_identity,
                "rootfs": rootfs_identity,
                "serial": serial_identity,
                "frame_before": before.public(),
                "frame_after": after.public(),
                "changed_pixels": changed_pixels,
                "acceleration": "tcg",
                "firmware": firmware_identity,
                "display_device": "VGA",
                "input_device": "usb-kbd",
                "entropy": entropy_identity,
                "olangc_wasm": wasm_identity,
                "network": "none",
                "visual_thresholds": {
                    "minimum_nonblack_pixels": MIN_NONBLACK_PIXELS,
                    "minimum_unique_colors": MIN_UNIQUE_COLORS,
                    "minimum_chromatic_pixels": MIN_CHROMATIC_PIXELS,
                    "minimum_chromatic_hue_buckets": MIN_CHROMATIC_HUE_BUCKETS,
                    "minimum_pixels_per_hue_bucket": MIN_PIXELS_PER_HUE_BUCKET,
                    "minimum_chromatic_max_channel": MIN_CHROMATIC_MAX_CHANNEL,
                    "minimum_chromatic_channel_spread": MIN_CHROMATIC_CHANNEL_SPREAD,
                    "minimum_changed_pixels": MIN_CHANGED_PIXELS,
                },
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
