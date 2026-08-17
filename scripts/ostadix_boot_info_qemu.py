#!/usr/bin/env python3
"""Run the challenged BootInfo mode-0 QEMU observation.

The guest evidence contract is intentionally unchanged.  This host harness
streams serial output until the final lifecycle marker, proves that QEMU stays
alive for one bounded post-lifecycle window, and captures best-effort QMP state
only after a failure verdict has already been fixed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import selectors
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from typing import Any, Sequence


DIAGNOSTIC_SCHEMA = "ostadix.boot-info-mode0-qemu-diagnostic/v1"
COMPLETION_MARKER = "CPL3 heartbeat: online\n"
MAX_CAPTURE_BYTES = 4 * 1024 * 1024
MAX_ARRIVAL_EVENTS = 512
MAX_ARRIVAL_LINE_BYTES = 1024
MAX_QMP_MESSAGE_BYTES = 128 * 1024
MAX_QMP_CAPTURE_BYTES = 512 * 1024
MAX_QMP_CAPTURE_EVENTS = 64
REQUIRED_MARKERS = (
    "BootInfoV1: source pointer and temporary aperture released\n",
    "BootInfoV1: Multiboot2 normalized\n",
    "page protections: W^X online\n",
    "page allocator: online\n",
    "BootInfoV1: firmware allocator window admitted\n",
    "OSTADIX boot challenge: {challenge}\n",
    "OSTADIX source commit: {source_commit}\n",
    "CPL3 native[0]: online\n",
    "timer CPL3 return: online\n",
    COMPLETION_MARKER,
)


class _TerminationSignal(BaseException):
    """Turn a catchable host termination signal into orderly unwinding."""

    def __init__(self, signum: int) -> None:
        super().__init__(f"received host signal {signum}")
        self.signum = signum


class _RuntimeResources:
    """Own every host resource that must be reclaimed on every exit path."""

    def __init__(self) -> None:
        self.process: subprocess.Popen[bytes] | None = None
        self.selector: selectors.BaseSelector | None = None
        self.qmp_dir: str | None = None

    def cleanup(self, timeout_seconds: float) -> None:
        process = self.process
        if process is not None and process.poll() is None:
            try:
                _cleanup_process(process, timeout_seconds)
            except BaseException:
                # Cleanup is best effort and must not mask the original failure.
                pass
        if self.selector is not None:
            try:
                self.selector.close()
            except BaseException:
                pass
        if process is not None:
            for stream in (process.stdout, process.stderr):
                if stream is not None and not stream.closed:
                    try:
                        stream.close()
                    except BaseException:
                        pass
        if self.qmp_dir is not None:
            shutil.rmtree(self.qmp_dir, ignore_errors=True)


def _install_termination_handlers() -> dict[int, Any]:
    """Install temporary handlers where Python permits main-thread signals."""

    previous: dict[int, Any] = {}

    def raise_signal(signum: int, _frame: Any) -> None:
        raise _TerminationSignal(signum)

    for name in ("SIGTERM", "SIGHUP"):
        signum = getattr(signal, name, None)
        if signum is None:
            continue
        try:
            old_handler = signal.getsignal(signum)
            signal.signal(signum, raise_signal)
        except (OSError, ValueError):
            continue
        previous[signum] = old_handler
    return previous


def _restore_signal_handlers(previous: dict[int, Any]) -> None:
    for signum, handler in previous.items():
        try:
            signal.signal(signum, handler)
        except (OSError, ValueError):
            pass


def _sha256_file(path: Path) -> tuple[int, str]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while True:
            chunk = stream.read(1024 * 1024)
            if not chunk:
                break
            size += len(chunk)
            digest.update(chunk)
    return size, digest.hexdigest()


def validate_mode0_output(
    output: str, challenge: str, source_commit: str
) -> list[str]:
    """Return exact guest-contract violations without changing their verdict."""

    lines = output.replace("\r\n", "\n").replace("\r", "\n").splitlines()
    required = [
        marker.format(challenge=challenge, source_commit=source_commit).removesuffix("\n")
        for marker in REQUIRED_MARKERS
    ]
    issues: list[str] = []
    missing = [marker for marker in required if lines.count(marker) == 0]
    if missing:
        issues.append("missing=" + repr(missing))

    wrong_counts = [marker for marker in required if lines.count(marker) != 1]
    if wrong_counts:
        issues.append("wrong-marker-count=" + repr(wrong_counts))

    if not missing:
        positions = [lines.index(marker) for marker in required]
        if positions != sorted(positions):
            issues.append("challenged mode-0 causal marker order")

    if any(
        line.startswith(("BootInfoV1: rejected", "BootInfoV1 rejection code:"))
        for line in lines
    ):
        issues.append("BootInfo rejection marker reached output")
    return issues


def _qmp_failure_capture(socket_path: str, budget_seconds: float) -> list[dict[str, Any]]:
    """Pause and inspect a still-live failed VM without changing its verdict."""

    captured: list[dict[str, Any]] = []
    captured_bytes = 0
    deadline = time.monotonic() + budget_seconds
    receive_buffer = bytearray()

    def append_capture(entry: dict[str, Any]) -> None:
        nonlocal captured_bytes
        if len(captured) >= MAX_QMP_CAPTURE_EVENTS:
            raise RuntimeError("QMP diagnostic event limit exceeded")
        encoded_size = len(
            json.dumps(entry, separators=(",", ":"), sort_keys=True).encode("utf-8")
        )
        if captured_bytes + encoded_size > MAX_QMP_CAPTURE_BYTES:
            raise RuntimeError("QMP diagnostic byte limit exceeded")
        captured.append(entry)
        captured_bytes += encoded_size

    def receive_matching(
        connection: socket.socket,
        *,
        expected_id: str | None = None,
        greeting: bool = False,
    ) -> dict[str, Any]:
        while time.monotonic() < deadline:
            while True:
                newline = receive_buffer.find(b"\n")
                if newline < 0:
                    break
                raw = bytes(receive_buffer[:newline]).strip()
                del receive_buffer[: newline + 1]
                if not raw:
                    continue
                if len(raw) > MAX_QMP_MESSAGE_BYTES:
                    raise RuntimeError("QMP response exceeded the message limit")
                message = json.loads(raw.decode("utf-8", "replace"))
                if greeting and "QMP" in message:
                    return message
                if expected_id is not None and message.get("id") == expected_id:
                    return message
                append_capture({"id": "async", "response": message})
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            connection.settimeout(max(0.01, min(0.25, remaining)))
            try:
                chunk = connection.recv(65536)
            except socket.timeout:
                continue
            if not chunk:
                raise RuntimeError("QMP socket closed before response")
            receive_buffer.extend(chunk)
            if (
                len(receive_buffer) > MAX_QMP_MESSAGE_BYTES
                and b"\n" not in receive_buffer
            ):
                raise RuntimeError("QMP response exceeded the message limit")
        raise TimeoutError("QMP diagnostic capture budget exhausted")

    def execute(
        connection: socket.socket,
        command_id: str,
        command: str,
        arguments: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        request: dict[str, Any] = {"execute": command, "id": command_id}
        if arguments is not None:
            request["arguments"] = arguments
        connection.sendall((json.dumps(request) + "\n").encode("utf-8"))
        response = receive_matching(connection, expected_id=command_id)
        append_capture({"id": command_id, "response": response})
        return response

    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            remaining = deadline - time.monotonic()
            connection.settimeout(max(0.01, min(0.5, remaining)))
            connection.connect(socket_path)
            append_capture(
                {
                    "id": "greeting",
                    "response": receive_matching(connection, greeting=True),
                }
            )
            execute(connection, "capabilities", "qmp_capabilities")
            execute(connection, "status-before-stop", "query-status")
            execute(connection, "cpus-before-stop", "query-cpus-fast")
            execute(connection, "stop", "stop")
            status = execute(connection, "status-after-stop", "query-status")
            append_capture(
                {
                    "id": "stop-confirmed",
                    "response": status.get("return", {}).get("status") == "paused",
                }
            )
            execute(connection, "cpus-after-stop", "query-cpus-fast")
            for command_id, monitor_command in (
                ("registers", "info registers"),
                ("pic", "info pic"),
                ("irq", "info irq"),
            ):
                execute(
                    connection,
                    command_id,
                    "human-monitor-command",
                    {"command-line": monitor_command},
                )
    except Exception as error:  # Diagnostics must never replace the frozen verdict.
        captured.append(
            {
                "id": "capture-failure",
                "error": f"{type(error).__name__}: {error}",
            }
        )
    return captured


def _cleanup_process(
    process: subprocess.Popen[bytes], timeout_seconds: float
) -> tuple[str, int | None]:
    if process.poll() is not None:
        return "already-exited", process.returncode
    action = "terminate"
    try:
        process.terminate()
    except ProcessLookupError:
        return "exited-before-terminate", process.poll()
    try:
        process.wait(timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        action = "kill"
        try:
            process.kill()
        except ProcessLookupError:
            return "exited-before-kill", process.poll()
        try:
            process.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            return "kill-timeout", None
    return action, process.returncode


def _run_challenged_mode0(
    *,
    resources: _RuntimeResources,
    qemu: str,
    firmware: Path,
    media: Path,
    kernel: Path,
    challenge: str,
    source_commit: str,
    completion_timeout_seconds: float,
    post_lifecycle_seconds: float,
    transcript_path: Path,
    stderr_path: Path,
    diagnostic_path: Path,
    qmp_budget_seconds: float = 2.0,
    cleanup_timeout_seconds: float = 2.0,
) -> dict[str, Any]:
    """Run one bounded observation and persist its complete host diagnostics."""

    for label, value, upper in (
        ("completion timeout", completion_timeout_seconds, 300.0),
        ("post-lifecycle window", post_lifecycle_seconds, 30.0),
        ("QMP budget", qmp_budget_seconds, 30.0),
        ("cleanup timeout", cleanup_timeout_seconds, 30.0),
    ):
        if not math.isfinite(value) or value <= 0.0 or value > upper:
            raise ValueError(f"{label} must be finite and within 0..{upper}")

    media_size, media_digest = _sha256_file(media)
    kernel_size, kernel_digest = _sha256_file(kernel)
    transcript_path.parent.mkdir(parents=True, exist_ok=True)
    stderr_path.parent.mkdir(parents=True, exist_ok=True)
    diagnostic_path.parent.mkdir(parents=True, exist_ok=True)

    qmp_dir = tempfile.mkdtemp(prefix="ostadix-mode0-qmp.")
    resources.qmp_dir = qmp_dir
    qmp_socket = os.path.join(qmp_dir, "qmp.sock")
    command = [
        qemu,
        "-accel",
        "tcg",
        "-machine",
        "q35",
        "-m",
        "128M",
        "-drive",
        f"if=pflash,unit=0,format=raw,readonly=on,file={firmware}",
        "-drive",
        f"if=none,id=ostadix,format=raw,readonly=on,file={media}",
        "-device",
        "virtio-blk-pci,drive=ostadix",
        "-nodefaults",
        "-nic",
        "none",
        "-display",
        "none",
        "-serial",
        "stdio",
        "-monitor",
        "none",
        "-qmp",
        f"unix:{qmp_socket},server=on,wait=off",
        "-no-reboot",
        "-no-shutdown",
    ]
    process = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=0,
    )
    resources.process = process
    assert process.stdout is not None
    assert process.stderr is not None

    selector = selectors.DefaultSelector()
    resources.selector = selector
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    stdout = bytearray()
    stderr = bytearray()
    line_fragments = {"stdout": bytearray(), "stderr": bytearray()}
    arrivals: list[dict[str, Any]] = []
    arrival_truncated = False
    launch_at = time.monotonic()
    deadline = launch_at + completion_timeout_seconds
    completion_seen_at: float | None = None
    survived_after_completion = False
    capture_overflow = False

    def record_chunk(stream_name: str, destination: bytearray, chunk: bytes) -> None:
        nonlocal arrival_truncated, capture_overflow, completion_seen_at
        remaining_capacity = MAX_CAPTURE_BYTES - len(destination)
        admitted = chunk[: max(0, remaining_capacity)]
        destination.extend(admitted)
        if len(chunk) > len(admitted):
            capture_overflow = True
        pending = line_fragments[stream_name]
        pending.extend(admitted)
        while True:
            newline = pending.find(b"\n")
            if newline < 0:
                break
            raw_line = bytes(pending[: newline + 1])
            del pending[: newline + 1]
            normalized_line = raw_line.replace(b"\r\n", b"\n")
            if (
                stream_name == "stdout"
                and completion_seen_at is None
                and normalized_line == COMPLETION_MARKER.encode("ascii")
            ):
                completion_seen_at = time.monotonic()
            if len(arrivals) >= MAX_ARRIVAL_EVENTS:
                arrival_truncated = True
                continue
            arrivals.append(
                {
                    "elapsed_seconds": round(time.monotonic() - launch_at, 6),
                    "stream": stream_name,
                    "line": raw_line[:MAX_ARRIVAL_LINE_BYTES].decode(
                        "utf-8", "replace"
                    ),
                    "line_truncated": len(raw_line) > MAX_ARRIVAL_LINE_BYTES,
                }
            )

    while True:
        now = time.monotonic()
        if completion_seen_at is None and now >= deadline:
            break
        if completion_seen_at is not None and now - completion_seen_at >= post_lifecycle_seconds:
            survived_after_completion = process.poll() is None
            break
        if process.poll() is not None or capture_overflow:
            break
        phase_deadline = (
            deadline
            if completion_seen_at is None
            else completion_seen_at + post_lifecycle_seconds
        )
        select_timeout = max(0.0, min(0.05, phase_deadline - now))
        for key, _ in selector.select(timeout=select_timeout):
            chunk = os.read(key.fileobj.fileno(), 4096)
            if not chunk:
                selector.unregister(key.fileobj)
                continue
            destination = stdout if key.data == "stdout" else stderr
            record_chunk(key.data, destination, chunk)

    observed_elapsed = time.monotonic() - launch_at
    capture_overflow_at_freeze = capture_overflow
    frozen_stdout_bytes = len(stdout)
    frozen_output = bytes(stdout).decode("utf-8", "replace")
    frozen_marker_issues = validate_mode0_output(
        frozen_output, challenge, source_commit
    )
    pre_cleanup_returncode = process.poll()
    alive_before_qmp = pre_cleanup_returncode is None
    if capture_overflow:
        liveness_classification = "capture-overflow"
    elif completion_seen_at is None and alive_before_qmp:
        liveness_classification = "completion-deadline/qemu-alive"
    elif completion_seen_at is None:
        liveness_classification = "pre-completion-death"
    elif not survived_after_completion:
        liveness_classification = "post-completion-window-failed"
    else:
        liveness_classification = "success"

    # The complete guest/liveness verdict is frozen before QMP can perturb QEMU.
    liveness_failed = liveness_classification != "success"
    if liveness_failed:
        classification = liveness_classification
    elif frozen_marker_issues:
        classification = "semantic-invalid"
    else:
        classification = "success"
    observation_failed = classification != "success"
    if observation_failed and alive_before_qmp:
        qmp_diagnostics = _qmp_failure_capture(qmp_socket, qmp_budget_seconds)
    elif observation_failed:
        qmp_diagnostics = [
            {"id": "capture-unavailable", "error": "QEMU was not alive"}
        ]
    else:
        qmp_diagnostics = [{"id": "not-requested-on-success"}]

    cleanup_action, final_returncode = _cleanup_process(
        process, cleanup_timeout_seconds
    )

    def drain_nonblocking(
        stream_name: str, stream: Any, destination: bytearray
    ) -> None:
        try:
            os.set_blocking(stream.fileno(), False)
        except OSError:
            return
        while True:
            remaining_capacity = MAX_CAPTURE_BYTES - len(destination)
            try:
                remainder = os.read(
                    stream.fileno(), min(4096, max(1, remaining_capacity + 1))
                )
            except (BlockingIOError, OSError):
                break
            if not remainder:
                break
            record_chunk(stream_name, destination, remainder)
            if len(destination) >= MAX_CAPTURE_BYTES:
                break

    for stream_name, stream, destination in (
        ("stdout", process.stdout, stdout),
        ("stderr", process.stderr, stderr),
    ):
        if process.poll() is not None:
            drain_nonblocking(stream_name, stream, destination)

    for stream_name, pending in line_fragments.items():
        if not pending:
            continue
        if len(arrivals) >= MAX_ARRIVAL_EVENTS:
            arrival_truncated = True
            break
        arrivals.append(
            {
                "elapsed_seconds": round(time.monotonic() - launch_at, 6),
                "stream": stream_name,
                "line": bytes(pending[:MAX_ARRIVAL_LINE_BYTES]).decode(
                    "utf-8", "replace"
                ),
                "line_truncated": len(pending) > MAX_ARRIVAL_LINE_BYTES,
            }
        )

    transcript_path.write_bytes(stdout)
    stderr_path.write_bytes(stderr)
    issues = list(frozen_marker_issues)
    if liveness_failed:
        issues.insert(0, f"liveness={liveness_classification}")
    if capture_overflow and not capture_overflow_at_freeze:
        issues.append("serial/stderr capture overflow after frozen observation")
    if final_returncode is None:
        issues.append("QEMU cleanup did not reap the process")

    diagnostic: dict[str, Any] = {
        "schema": DIAGNOSTIC_SCHEMA,
        "classification": classification,
        "liveness_classification": liveness_classification,
        "passed": not issues,
        "issues": issues,
        "challenge": challenge,
        "source_commit": source_commit,
        "completion_timeout_seconds": completion_timeout_seconds,
        "post_lifecycle_seconds": post_lifecycle_seconds,
        "observed_elapsed_seconds": round(observed_elapsed, 6),
        "completion_seen_seconds": (
            None
            if completion_seen_at is None
            else round(completion_seen_at - launch_at, 6)
        ),
        "survived_after_completion": survived_after_completion,
        "pid": process.pid,
        "alive_before_qmp": alive_before_qmp,
        "pre_cleanup_returncode": pre_cleanup_returncode,
        "cleanup_action": cleanup_action,
        "final_returncode": final_returncode,
        "verdict_frozen_before_qmp": True,
        "frozen_stdout_bytes": frozen_stdout_bytes,
        "post_freeze_stdout_bytes": len(stdout) - frozen_stdout_bytes,
        "stdout_bytes": len(stdout),
        "stderr_bytes": len(stderr),
        "capture_limit_bytes_per_stream": MAX_CAPTURE_BYTES,
        "capture_overflow": capture_overflow,
        "arrival_events_truncated": arrival_truncated,
        "arrival_events": arrivals,
        "qmp": qmp_diagnostics,
        "media": {
            "path": str(media.resolve()),
            "bytes": media_size,
            "sha256": media_digest,
        },
        "kernel": {
            "path": str(kernel.resolve()),
            "bytes": kernel_size,
            "sha256": kernel_digest,
        },
    }
    diagnostic_path.write_text(
        json.dumps(diagnostic, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return diagnostic


def run_challenged_mode0(
    *,
    qemu: str,
    firmware: Path,
    media: Path,
    kernel: Path,
    challenge: str,
    source_commit: str,
    completion_timeout_seconds: float,
    post_lifecycle_seconds: float,
    transcript_path: Path,
    stderr_path: Path,
    diagnostic_path: Path,
    qmp_budget_seconds: float = 2.0,
    cleanup_timeout_seconds: float = 2.0,
) -> dict[str, Any]:
    resources = _RuntimeResources()
    previous_handlers = _install_termination_handlers()
    try:
        return _run_challenged_mode0(
            resources=resources,
            qemu=qemu,
            firmware=firmware,
            media=media,
            kernel=kernel,
            challenge=challenge,
            source_commit=source_commit,
            completion_timeout_seconds=completion_timeout_seconds,
            post_lifecycle_seconds=post_lifecycle_seconds,
            transcript_path=transcript_path,
            stderr_path=stderr_path,
            diagnostic_path=diagnostic_path,
            qmp_budget_seconds=qmp_budget_seconds,
            cleanup_timeout_seconds=cleanup_timeout_seconds,
        )
    finally:
        resources.cleanup(cleanup_timeout_seconds)
        _restore_signal_handlers(previous_handlers)


def _positive_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or parsed <= 0.0:
        raise argparse.ArgumentTypeError("must be a finite positive number")
    return parsed


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--qemu", required=True)
    parser.add_argument("--firmware", type=Path, required=True)
    parser.add_argument("--media", type=Path, required=True)
    parser.add_argument("--kernel", type=Path, required=True)
    parser.add_argument("--challenge", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--completion-timeout-seconds", type=_positive_float, required=True)
    parser.add_argument("--post-lifecycle-seconds", type=_positive_float, default=1.0)
    parser.add_argument("--transcript", type=Path, required=True)
    parser.add_argument("--stderr", type=Path, required=True)
    parser.add_argument("--diagnostic", type=Path, required=True)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        diagnostic = run_challenged_mode0(
            qemu=args.qemu,
            firmware=args.firmware,
            media=args.media,
            kernel=args.kernel,
            challenge=args.challenge,
            source_commit=args.source_commit,
            completion_timeout_seconds=args.completion_timeout_seconds,
            post_lifecycle_seconds=args.post_lifecycle_seconds,
            transcript_path=args.transcript,
            stderr_path=args.stderr,
            diagnostic_path=args.diagnostic,
        )
    except _TerminationSignal as error:
        print(f"challenged mode-0 harness interrupted by signal {error.signum}", file=sys.stderr)
        return 128 + error.signum
    output = args.transcript.read_text(encoding="utf-8", errors="replace")
    if not diagnostic["passed"]:
        print(
            "challenged mode-0 QEMU smoke failed; "
            f"classification={diagnostic['classification']} "
            f"issues={diagnostic['issues']!r}",
            file=sys.stderr,
        )
        print("stdout:", output, file=sys.stderr)
        print(
            "stderr:",
            args.stderr.read_text(encoding="utf-8", errors="replace"),
            file=sys.stderr,
        )
        print(f"diagnostic: {args.diagnostic}", file=sys.stderr)
        return 1

    print(output, end="")
    print(f"OSTADIX challenged mode-0 media SHA-256 {diagnostic['media']['sha256']}")
    print("OSTADIX challenged mode-0 post-lifecycle window: PASS")
    print("OSTADIX challenged mode-0 CPL3 lifecycle: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
