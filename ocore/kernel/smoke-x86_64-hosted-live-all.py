#!/usr/bin/env python3
"""Run the complete serial, graphical, and direct O-core Hosted Live smoke suite."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
from typing import Callable, Iterator, Mapping, Sequence


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_ISO = (
    ROOT
    / "target/ostadix-hosted-live/x86_64/ostadix-hosted-live-x86_64-uefi_VTGRUB2.iso"
)
DEFAULT_SERIAL_SMOKE = ROOT / "ocore/kernel/smoke-x86_64-hosted-live-qemu.py"
DEFAULT_VISUAL_SMOKE = ROOT / "ocore/kernel/smoke-x86_64-hosted-live-vga-qemu.py"
DEFAULT_OCORE_SMOKE = ROOT / "ocore/kernel/smoke-x86_64-hosted-live-ocore-qemu.py"
DEFAULT_CAPACITY_RUNNER = ROOT / "ocore/kernel/run-x86_64-capacity-iso-qemu.sh"
DEFAULT_OVMF_RESOLVER = ROOT / "ocore/kernel/resolve-x86_64-ovmf-code.sh"
DEFAULT_HOSTED_TIMEOUT_SECONDS = 1800.0
DEFAULT_OCORE_TIMEOUT_SECONDS = 90.0
MAX_HOSTED_TIMEOUT_SECONDS = 1800.0
MAX_OCORE_TIMEOUT_SECONDS = 900.0
AGGREGATE_SCHEMA = "ostadix.hosted-live-qemu-smoke-all/v2"
GATE_SCHEMAS = {
    "serial": "ostadix.hosted-live-qemu-smoke/v4",
    "graphical": "ostadix.hosted-live-qemu-visual-smoke/v7",
    "ocore": "ostadix.hosted-live-ocore-qemu-smoke/v1",
}
COPY_CHUNK_BYTES = 4 * 1024 * 1024


class AggregateSmokeError(RuntimeError):
    """The complete Hosted Live smoke suite did not establish one exact result."""


@dataclass(frozen=True)
class PinnedRegular:
    descriptor: int
    path: Path
    state: os.stat_result


@dataclass(frozen=True)
class Snapshot:
    pinned: PinnedRegular
    identity: dict[str, object]

    @property
    def path(self) -> Path:
        return self.pinned.path


RunProcess = Callable[..., subprocess.CompletedProcess[str]]


def _state_identity(state: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        state.st_dev,
        state.st_ino,
        state.st_size,
        state.st_mtime_ns,
        state.st_ctime_ns,
    )


def _open_regular(path: Path, label: str) -> PinnedRegular:
    if not hasattr(os, "O_NOFOLLOW"):
        raise AggregateSmokeError("host lacks O_NOFOLLOW for exact smoke inputs")
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | os.O_NOFOLLOW,
        )
    except OSError as error:
        raise AggregateSmokeError(f"cannot open {label}: {path}: {error}") from error
    state = os.fstat(descriptor)
    if not stat.S_ISREG(state.st_mode) or state.st_size <= 0:
        os.close(descriptor)
        raise AggregateSmokeError(f"{label} is not a non-empty regular file: {path}")
    return PinnedRegular(descriptor=descriptor, path=path, state=state)


def _descriptor_identity(descriptor: int) -> dict[str, object]:
    digest = hashlib.sha256()
    size = 0
    while chunk := os.pread(descriptor, COPY_CHUNK_BYTES, size):
        digest.update(chunk)
        size += len(chunk)
    return {"bytes": size, "sha256": digest.hexdigest()}


def _require_same_pinned_file(pinned: PinnedRegular, expected: Mapping[str, object]) -> None:
    try:
        path_state = os.lstat(pinned.path)
    except OSError as error:
        raise AggregateSmokeError(
            f"private ISO snapshot disappeared during the smoke suite: {pinned.path}"
        ) from error
    current = os.fstat(pinned.descriptor)
    if stat.S_ISLNK(path_state.st_mode) or _state_identity(path_state) != _state_identity(current):
        raise AggregateSmokeError("private ISO snapshot path changed during the smoke suite")
    if _state_identity(current) != _state_identity(pinned.state):
        raise AggregateSmokeError("private ISO snapshot metadata changed during the smoke suite")
    if _descriptor_identity(pinned.descriptor) != expected:
        raise AggregateSmokeError("private ISO snapshot content changed during the smoke suite")


def _write_all(descriptor: int, payload: bytes) -> None:
    offset = 0
    while offset < len(payload):
        written = os.write(descriptor, payload[offset:])
        if written <= 0:
            raise AggregateSmokeError("short write while snapshotting the Hosted Live ISO")
        offset += written


@contextmanager
def _private_iso_snapshot(iso: Path) -> Iterator[Snapshot]:
    source = _open_regular(iso, "Hosted Live ISO")
    try:
        with tempfile.TemporaryDirectory(prefix="ostadix-hosted-live-all.") as temporary:
            temporary_path = Path(temporary)
            os.chmod(temporary_path, 0o700)
            snapshot_path = temporary_path / "hosted-live.iso"
            destination = os.open(
                snapshot_path,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
                0o600,
            )
            copied_digest = hashlib.sha256()
            copied_bytes = 0
            try:
                while chunk := os.pread(source.descriptor, COPY_CHUNK_BYTES, copied_bytes):
                    _write_all(destination, chunk)
                    copied_digest.update(chunk)
                    copied_bytes += len(chunk)
                os.fsync(destination)
            finally:
                os.close(destination)

            copied_identity: dict[str, object] = {
                "bytes": copied_bytes,
                "sha256": copied_digest.hexdigest(),
            }
            if copied_bytes != source.state.st_size:
                raise AggregateSmokeError("Hosted Live ISO size changed while snapshotting")
            if _state_identity(os.fstat(source.descriptor)) != _state_identity(source.state):
                raise AggregateSmokeError("Hosted Live ISO metadata changed while snapshotting")
            if _descriptor_identity(source.descriptor) != copied_identity:
                raise AggregateSmokeError("Hosted Live ISO content changed while snapshotting")

            os.chmod(snapshot_path, 0o400)
            snapshot_input = _open_regular(snapshot_path, "private Hosted Live ISO snapshot")
            try:
                if _descriptor_identity(snapshot_input.descriptor) != copied_identity:
                    raise AggregateSmokeError("private Hosted Live ISO snapshot hash mismatch")
                snapshot = Snapshot(pinned=snapshot_input, identity=copied_identity)
                _require_same_pinned_file(snapshot.pinned, snapshot.identity)
                try:
                    yield snapshot
                finally:
                    _require_same_pinned_file(snapshot.pinned, snapshot.identity)
            finally:
                os.close(snapshot_input.descriptor)
    finally:
        os.close(source.descriptor)


def _require_timeout(value: float, maximum: float, label: str) -> None:
    if not (1 <= value <= maximum):
        raise AggregateSmokeError(
            f"{label} timeout must be from 1 through {maximum:g} seconds"
        )


def _environment_timeout(name: str, fallback: float) -> float:
    raw = os.environ.get(name)
    if raw is None:
        return fallback
    try:
        return float(raw)
    except ValueError as error:
        raise AggregateSmokeError(f"{name} must be a number, got {raw!r}") from error


def resolve_qemu(candidate: str) -> Path:
    resolved = shutil.which(candidate)
    if resolved is None:
        raise AggregateSmokeError(f"QEMU executable was not found: {candidate}")
    try:
        path = Path(resolved).resolve(strict=True)
    except OSError as error:
        raise AggregateSmokeError(f"cannot resolve QEMU executable: {resolved}") from error
    if not path.is_file() or not os.access(path, os.X_OK):
        raise AggregateSmokeError(f"resolved QEMU is not an executable file: {path}")
    return path


def resolve_firmware(
    qemu: Path,
    *,
    explicit: Path | None = None,
    resolver: Path = DEFAULT_OVMF_RESOLVER,
    run_process: RunProcess | None = None,
) -> Path:
    if explicit is not None:
        try:
            firmware = explicit.expanduser().resolve(strict=True)
        except OSError as error:
            raise AggregateSmokeError(f"cannot resolve explicit OVMF code: {explicit}") from error
        pinned = _open_regular(firmware, "OVMF code")
        os.close(pinned.descriptor)
        return firmware

    if resolver.is_symlink() or not resolver.is_file():
        raise AggregateSmokeError(f"OVMF resolver is not a regular repository file: {resolver}")
    invoke = run_process or subprocess.run
    command = [
        "/bin/bash",
        "-c",
        'source "$1"\nresolve_ostadix_x86_64_ovmf_code "$2"',
        "ostadix-ovmf-resolver",
        str(resolver),
        str(qemu),
    ]
    try:
        completed = invoke(
            command,
            stdout=subprocess.PIPE,
            text=True,
            check=False,
            env=os.environ.copy(),
        )
    except OSError as error:
        raise AggregateSmokeError(f"OVMF discovery could not start: {error}") from error
    if completed.returncode != 0:
        raise AggregateSmokeError(
            f"OVMF discovery returned status {completed.returncode}"
        )
    output = completed.stdout.strip().splitlines()
    if len(output) != 1 or not output[0]:
        raise AggregateSmokeError("OVMF discovery returned a malformed path")
    try:
        firmware = Path(output[0]).expanduser().resolve(strict=True)
    except OSError as error:
        raise AggregateSmokeError(f"cannot resolve discovered OVMF code: {output[0]}") from error
    pinned = _open_regular(firmware, "OVMF code")
    os.close(pinned.descriptor)
    return firmware


def _require_program(path: Path, label: str, *, executable: bool = False) -> None:
    if path.is_symlink() or not path.is_file():
        raise AggregateSmokeError(f"{label} is not a regular repository file: {path}")
    if executable and not os.access(path, os.X_OK):
        raise AggregateSmokeError(f"{label} is not executable: {path}")


def _parse_identity(value: object, label: str) -> dict[str, object]:
    if not isinstance(value, dict) or set(value) != {"bytes", "sha256"}:
        raise AggregateSmokeError(f"{label} omitted an exact byte/hash identity")
    size = value.get("bytes")
    digest = value.get("sha256")
    if (
        type(size) is not int
        or size <= 0
        or not isinstance(digest, str)
        or len(digest) != 64
        or any(character not in "0123456789abcdef" for character in digest)
    ):
        raise AggregateSmokeError(f"{label} has a malformed byte/hash identity")
    return {"bytes": size, "sha256": digest}


def _reject_json_constant(value: str) -> None:
    raise ValueError(f"non-finite JSON constant {value}")


def _run_gate(
    label: str,
    command: Sequence[str],
    *,
    expected_iso: Mapping[str, object],
    expected_firmware: Mapping[str, object] | None,
    environment: Mapping[str, str],
    snapshot: Snapshot,
    run_process: RunProcess,
) -> dict[str, object]:
    try:
        completed = run_process(
            list(command),
            stdout=subprocess.PIPE,
            text=True,
            check=False,
            env=dict(environment),
        )
    except OSError as error:
        raise AggregateSmokeError(f"{label} gate could not start: {error}") from error
    finally:
        _require_same_pinned_file(snapshot.pinned, snapshot.identity)
    if completed.returncode != 0:
        raise AggregateSmokeError(f"{label} gate returned status {completed.returncode}")
    try:
        payload = json.loads(completed.stdout, parse_constant=_reject_json_constant)
    except (TypeError, ValueError, json.JSONDecodeError) as error:
        raise AggregateSmokeError(f"{label} gate returned malformed JSON") from error
    if not isinstance(payload, dict):
        raise AggregateSmokeError(f"{label} gate JSON is not an object")
    if payload.get("schema") != GATE_SCHEMAS[label]:
        raise AggregateSmokeError(f"{label} gate returned an unexpected schema")
    if _parse_identity(payload.get("iso"), f"{label} gate ISO") != dict(expected_iso):
        raise AggregateSmokeError(f"{label} gate ISO identity does not match the snapshot")
    if expected_firmware is not None and (
        _parse_identity(payload.get("firmware"), f"{label} gate firmware")
        != dict(expected_firmware)
    ):
        raise AggregateSmokeError(
            f"{label} gate firmware identity does not match the resolved OVMF code"
        )
    if payload.get("acceleration") != "tcg":
        raise AggregateSmokeError(f"{label} gate escaped the bounded TCG profile")
    if label == "serial":
        if payload.get("firmware_path") != "ovmf-through-capacity-runner":
            raise AggregateSmokeError(
                "serial gate escaped the OVMF-through-capacity-runner profile"
            )
    elif payload.get("network") != "none":
        raise AggregateSmokeError(f"{label} gate escaped the no-network profile")
    if payload.get("physical_hardware_proof") is not False:
        raise AggregateSmokeError(f"{label} gate made an invalid physical-hardware claim")
    if label in ("serial", "ocore") and payload.get("exit_code") != 0:
        raise AggregateSmokeError(f"{label} gate JSON reported a nonzero QEMU exit")
    return payload


def run_all_gates(
    iso: Path,
    *,
    qemu: Path,
    firmware: Path,
    hosted_timeout_seconds: float = DEFAULT_HOSTED_TIMEOUT_SECONDS,
    ocore_timeout_seconds: float = DEFAULT_OCORE_TIMEOUT_SECONDS,
    serial_smoke: Path = DEFAULT_SERIAL_SMOKE,
    visual_smoke: Path = DEFAULT_VISUAL_SMOKE,
    ocore_smoke: Path = DEFAULT_OCORE_SMOKE,
    capacity_runner: Path = DEFAULT_CAPACITY_RUNNER,
    run_process: RunProcess | None = None,
) -> dict[str, object]:
    """Run all three gates, in authority order, against one private ISO snapshot."""

    _require_timeout(hosted_timeout_seconds, MAX_HOSTED_TIMEOUT_SECONDS, "Hosted")
    _require_timeout(ocore_timeout_seconds, MAX_OCORE_TIMEOUT_SECONDS, "O-core")
    for path, label in (
        (serial_smoke, "serial smoke"),
        (visual_smoke, "graphical smoke"),
        (ocore_smoke, "O-core smoke"),
    ):
        _require_program(path, label)
    _require_program(capacity_runner, "capacity ISO QEMU runner", executable=True)

    firmware_input = _open_regular(firmware, "OVMF code")
    try:
        firmware_identity = _descriptor_identity(firmware_input.descriptor)
        if _state_identity(os.fstat(firmware_input.descriptor)) != _state_identity(
            firmware_input.state
        ):
            raise AggregateSmokeError("OVMF code changed while hashing")
    finally:
        os.close(firmware_input.descriptor)

    invoke = run_process or subprocess.run
    environment = os.environ.copy()
    environment["OCORE_QEMU_BIN"] = str(qemu)
    environment["OSTADIX_OVMF_CODE"] = str(firmware)

    with _private_iso_snapshot(iso) as snapshot:
        evidence_dir = snapshot.path.parent / "graphical-evidence"
        commands = (
            (
                "serial",
                [
                    sys.executable,
                    str(serial_smoke),
                    str(snapshot.path),
                    "--runner",
                    str(capacity_runner),
                    "--timeout",
                    f"{hosted_timeout_seconds:g}",
                ],
                None,
            ),
            (
                "graphical",
                [
                    sys.executable,
                    str(visual_smoke),
                    str(snapshot.path),
                    "--firmware",
                    str(firmware),
                    "--qemu",
                    str(qemu),
                    "--evidence-dir",
                    str(evidence_dir),
                    "--timeout",
                    f"{hosted_timeout_seconds:g}",
                ],
                firmware_identity,
            ),
            (
                "ocore",
                [
                    sys.executable,
                    str(ocore_smoke),
                    str(snapshot.path),
                    "--firmware",
                    str(firmware),
                    "--qemu",
                    str(qemu),
                    "--timeout",
                    f"{ocore_timeout_seconds:g}",
                ],
                firmware_identity,
            ),
        )
        results: dict[str, object] = {}
        for label, command, expected_firmware in commands:
            results[label] = _run_gate(
                label,
                command,
                expected_iso=snapshot.identity,
                expected_firmware=expected_firmware,
                environment=environment,
                snapshot=snapshot,
                run_process=invoke,
            )
        return {
            "schema": AGGREGATE_SCHEMA,
            "gate_order": ["serial", "graphical", "ocore"],
            "iso": snapshot.identity,
            "qemu": str(qemu),
            "firmware": firmware_identity,
            "timeouts": {
                "hosted_seconds": hosted_timeout_seconds,
                "ocore_seconds": ocore_timeout_seconds,
            },
            "smoke": results,
            "acceleration": "tcg",
            "network": "none",
            "physical_hardware_proof": False,
        }


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("iso", nargs="?", type=Path, default=DEFAULT_ISO)
    parser.add_argument(
        "--qemu",
        default=os.environ.get("OCORE_QEMU_BIN", "qemu-system-x86_64"),
    )
    parser.add_argument("--firmware", type=Path)
    parser.add_argument(
        "--hosted-timeout",
        "--timeout",
        dest="hosted_timeout",
        type=float,
        default=None,
    )
    parser.add_argument(
        "--ocore-timeout",
        type=float,
        default=None,
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        hosted_timeout = (
            arguments.hosted_timeout
            if arguments.hosted_timeout is not None
            else _environment_timeout(
                "OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT",
                DEFAULT_HOSTED_TIMEOUT_SECONDS,
            )
        )
        ocore_timeout = (
            arguments.ocore_timeout
            if arguments.ocore_timeout is not None
            else _environment_timeout(
                "OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT",
                DEFAULT_OCORE_TIMEOUT_SECONDS,
            )
        )
        qemu = resolve_qemu(arguments.qemu)
        firmware = resolve_firmware(qemu, explicit=arguments.firmware)
        result = run_all_gates(
            arguments.iso.expanduser().absolute(),
            qemu=qemu,
            firmware=firmware,
            hosted_timeout_seconds=hosted_timeout,
            ocore_timeout_seconds=ocore_timeout,
        )
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    except (OSError, AggregateSmokeError) as error:
        print(f"hosted-live-all-smoke: ERROR: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
