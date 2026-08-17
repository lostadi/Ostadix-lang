#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m6a-personality}"
# On Apple Silicon hosts this x86-64 TCG scenario can cross 30 seconds after a
# long aggregate run.  Keep the evidence assertions unchanged while giving the
# fixed lifecycle enough wall-clock budget to finish under host contention.
TIMEOUT_SECONDS="${OCORE_M6A_TIMEOUT_SECONDS:-180}"
DIGEST="f5924eeb64b5a3d332e20b5d0fae7b233ae2714eb58b72ea07f08a4d26334417"
IMAGE_BYTES=62104
IMAGE="$ROOT/target/ocore-m6-artifacts/images/root-${DIGEST}.ovfs"

for tool in qemu-system-x86_64 nm python3 shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the M6A personality smoke" >&2
    exit 127
  fi
done

OCORE_PROBE_MODE=18 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

if [[ ! -f "$IMAGE" ]] \
  || [[ "$(wc -c < "$IMAGE" | tr -d ' ')" != "$IMAGE_BYTES" ]] \
  || [[ "$(shasum -a 256 "$IMAGE" | awk '{print $1}')" != "$DIGEST" ]]; then
  echo "error: M6A embedded OVFS artifact identity is not canonical" >&2
  exit 1
fi

OVFS_PATHS="$(
  python3 "$ROOT/ocore/user/verify_ovfs.py" "$IMAGE" | sed -n '/^\/sbin\//p'
)"
EXPECTED_PATHS=$'/sbin/m6-client.elf: valid\n/sbin/m6-observer.elf: valid\n/sbin/m6-personalityd.elf: valid\n/sbin/m6-supervisord.elf: valid'
if [[ "$OVFS_PATHS" != "$EXPECTED_PATHS" ]]; then
  echo "error: M6A OVFS does not contain the exact four packaged ELF paths" >&2
  printf '%s\n' "$OVFS_PATHS" >&2
  exit 1
fi

# The four principals must enter the kernel only as bytes in the verified OVFS
# image. Source-module symbols in kernel.elf would bypass the package and loader
# boundary that this gate claims.
if ! KERNEL_SYMBOLS="$(nm "$BUILD_DIR/kernel.elf" 2>/dev/null)"; then
  echo "error: nm could not inspect the M6A kernel ELF" >&2
  exit 1
fi
if grep -Eq '_O_runtime__m6_(client|personalityd|supervisord|observer)__' \
    <<<"$KERNEL_SYMBOLS"; then
  echo "error: M6A user principal was linked as kernel code" >&2
  exit 1
fi

python3 - "$BUILD_DIR/kernel.elf" "$TIMEOUT_SECONDS" "$DIGEST" \
  "$IMAGE_BYTES" <<'PY'
import atexit
import json
import os
import re
import selectors
import shutil
import socket
import subprocess
import sys
import tempfile
import time

kernel = sys.argv[1]
timeout_seconds = float(sys.argv[2])
digest = sys.argv[3]
image_bytes = int(sys.argv[4])
qmp_dir = tempfile.mkdtemp(prefix="o18q.", dir="/tmp")
qmp_socket = os.path.join(qmp_dir, "qmp.sock")


def cleanup_qmp_dir():
    shutil.rmtree(qmp_dir, ignore_errors=True)


atexit.register(cleanup_qmp_dir)
command = [
    "qemu-system-x86_64",
    "-machine", "q35",
    "-m", "128M",
    "-kernel", kernel,
    "-display", "none",
    "-serial", "stdio",
    "-qmp", f"unix:{qmp_socket},server=on,wait=off",
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
selector = selectors.DefaultSelector()
selector.register(process.stdout, selectors.EVENT_READ, "stdout")
selector.register(process.stderr, selectors.EVENT_READ, "stderr")
stdout = bytearray()
stderr = bytearray()
deadline = time.monotonic() + timeout_seconds
completion_seen_at = None
survived_after_completion = False
completion_bytes = b"M6A post-lifecycle timer: online\n"

# Failure diagnostics are host-only. The just-built ELF supplies exact symbol
# addresses and sizes; no diagnostic state or marker is linked into the guest.
physical_probe_groups = {
    "thread": [
        ("thread.states", "_O_runtime__thread__STATES", 4, 1, True),
        ("thread.queue_kinds", "_O_runtime__thread__QUEUE_KINDS", 4, 1, True),
        ("thread.sleep_epochs", "_O_runtime__thread__SLEEP_EPOCHS", 16, 4, True),
        ("thread.wake_epochs", "_O_runtime__thread__WAKE_EPOCHS", 16, 4, True),
        ("thread.wake_reasons", "_O_runtime__thread__WAKE_REASONS", 4, 1, True),
        ("thread.run_ticks", "_O_runtime__thread__RUN_TICKS", 32, 8, True),
        ("thread.switch_counts", "_O_runtime__thread__SWITCH_COUNTS", 32, 8, True),
        ("thread.frames", "_O_runtime__thread__FRAMES", 704, 8, False),
        ("thread.runnable_queue", "_O_runtime__thread__RUNNABLE_QUEUE", 32, 8, True),
        ("thread.runnable_head", "_O_runtime__thread__RUNNABLE_HEAD", 8, 8, True),
        ("thread.runnable_length", "_O_runtime__thread__RUNNABLE_LENGTH", 8, 8, True),
        ("thread.blocked_queue", "_O_runtime__thread__BLOCKED_QUEUE", 32, 8, True),
        ("thread.blocked_head", "_O_runtime__thread__BLOCKED_HEAD", 8, 8, True),
        ("thread.blocked_length", "_O_runtime__thread__BLOCKED_LENGTH", 8, 8, True),
        ("thread.current", "_O_runtime__thread__CURRENT_THREAD", 8, 8, True),
        ("thread.prepared", "_O_runtime__thread__PREPARED_THREAD", 8, 8, True),
    ],
    "scheduler": [
        ("scheduler.active", "_O_runtime__scheduler__ACTIVE", 1, 1, True),
        ("scheduler.complete", "_O_runtime__scheduler__COMPLETE", 1, 1, True),
        ("scheduler.failed", "_O_runtime__scheduler__FAILED", 1, 1, True),
        ("scheduler.last_tick", "_O_runtime__scheduler__LAST_TICK", 8, 8, True),
        ("scheduler.kind", "_O_runtime__scheduler__SCHEDULER_KIND", 1, 1, True),
        ("scheduler.completion_kind", "_O_runtime__scheduler__COMPLETION_KIND", 1, 1, True),
    ],
    "personality_rpc": [
        ("rpc.states", "_O_runtime__personality_rpc__STATES", 4, 1, True),
        ("rpc.caller_threads", "_O_runtime__personality_rpc__CALLER_THREADS", 32, 8, True),
        ("rpc.request_frames", "_O_runtime__personality_rpc__REQUEST_FRAMES", 32, 8, True),
        ("rpc.deadlines", "_O_runtime__personality_rpc__DEADLINES", 32, 8, True),
        ("rpc.wait_requests", "_O_runtime__personality_rpc__WAIT_REQUESTS", 32, 8, True),
        ("rpc.terminal_kinds", "_O_runtime__personality_rpc__TERMINAL_KINDS", 4, 1, True),
        ("rpc.terminal_results", "_O_runtime__personality_rpc__TERMINAL_RESULTS", 32, 8, True),
        ("rpc.dispatches", "_O_runtime__personality_rpc__DISPATCHES", 8, 8, True),
        (
            "rpc.service_request_wakes",
            "_O_runtime__personality_rpc__SERVICE_REQUEST_WAKES",
            8,
            8,
            True,
        ),
        (
            "rpc.terminal_transitions",
            "_O_runtime__personality_rpc__TERMINAL_TRANSITIONS",
            8,
            8,
            True,
        ),
        ("rpc.terminal_wakes", "_O_runtime__personality_rpc__TERMINAL_WAKES", 8, 8, True),
        ("rpc.replies", "_O_runtime__personality_rpc__REPLIES", 8, 8, True),
        ("rpc.deadline_expiries", "_O_runtime__personality_rpc__DEADLINE_EXPIRIES", 8, 8, True),
        ("rpc.consumed_results", "_O_runtime__personality_rpc__CONSUMED_RESULTS", 8, 8, True),
        ("rpc.stale_replies", "_O_runtime__personality_rpc__STALE_REPLIES", 8, 8, True),
        ("rpc.duplicate_replies", "_O_runtime__personality_rpc__DUPLICATE_REPLIES", 8, 8, True),
        ("rpc.late_replies", "_O_runtime__personality_rpc__LATE_REPLIES", 8, 8, True),
    ],
    "m6": [
        ("m6.active_mode", "_O_kernel__m6__ACTIVE_MODE", 8, 8, True),
        ("m6.physicals", "_O_kernel__m6__PHYSICALS", 32, 8, True),
        ("m6.threads", "_O_kernel__m6__THREADS", 32, 8, True),
    ],
}


def capture_qmp_failure_state(path, budget_seconds=2.0):
    """Best-effort bounded snapshot; it never changes the frozen verdict."""
    captured = []
    captured_bytes = 0
    capture_deadline = time.monotonic() + budget_seconds
    receive_buffer = bytearray()
    max_message_bytes = 128 * 1024
    max_capture_bytes = 512 * 1024
    max_capture_events = 96

    def record(value):
        nonlocal captured_bytes
        if len(captured) >= max_capture_events or captured_bytes >= max_capture_bytes:
            return
        encoded = value.encode("utf-8", "replace")
        remaining = max_capture_bytes - captured_bytes
        if len(encoded) > remaining:
            encoded = encoded[:remaining]
            value = encoded.decode("utf-8", "replace")
        captured.append(value)
        captured_bytes += len(encoded)

    def receive_matching(connection, expected_id=None, greeting=False):
        while time.monotonic() < capture_deadline:
            while True:
                newline = receive_buffer.find(b"\n")
                if newline < 0:
                    break
                raw = bytes(receive_buffer[:newline]).strip()
                del receive_buffer[: newline + 1]
                if not raw:
                    continue
                message = json.loads(raw.decode("utf-8", "replace"))
                if greeting and "QMP" in message:
                    return message
                if expected_id is not None and message.get("id") == expected_id:
                    return message
                record("qmp-async=" + json.dumps(message, sort_keys=True)[:65536])
            remaining = capture_deadline - time.monotonic()
            if remaining <= 0:
                break
            connection.settimeout(min(0.25, remaining))
            try:
                chunk = connection.recv(65536)
            except socket.timeout:
                continue
            if not chunk:
                raise RuntimeError("QMP socket closed before response")
            receive_buffer.extend(chunk)
            if len(receive_buffer) > max_message_bytes:
                raise RuntimeError("QMP response exceeded the message limit")
        raise TimeoutError("QMP diagnostic capture budget exhausted")

    def qmp_execute(connection, command_id, execute, arguments=None):
        request = {"execute": execute, "id": command_id}
        if arguments is not None:
            request["arguments"] = arguments
        remaining = capture_deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("QMP diagnostic capture budget exhausted")
        connection.settimeout(min(0.25, remaining))
        connection.sendall((json.dumps(request) + "\n").encode("utf-8"))
        return receive_matching(connection, command_id)

    def resolve_physical_symbols():
        required_names = {
            symbol
            for specs in physical_probe_groups.values()
            for _, symbol, _, _, _ in specs
        }
        remaining = capture_deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("QMP diagnostic capture budget exhausted")
        result = subprocess.run(
            ["nm", "-S", "-n", "--defined-only", kernel],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
            timeout=max(0.05, remaining),
        )
        if result.returncode != 0:
            raise RuntimeError(
                "nm physical-symbol resolution failed: "
                + result.stderr.strip()[:1024]
            )
        resolved = {}
        for line in result.stdout.splitlines():
            fields = line.split(None, 3)
            if len(fields) != 4 or fields[3] not in required_names:
                continue
            try:
                address = int(fields[0], 16)
                size = int(fields[1], 16)
            except ValueError:
                continue
            if fields[3] in resolved:
                raise RuntimeError("duplicate nm symbol: " + fields[3])
            resolved[fields[3]] = (address, size)
        missing_symbols = sorted(required_names - resolved.keys())
        if missing_symbols:
            raise RuntimeError("missing nm physical symbols: " + repr(missing_symbols))
        return resolved

    def hmp_physical_bytes(response, expected_size):
        if "error" in response or not isinstance(response.get("return"), str):
            raise RuntimeError("QMP physical read failed: " + repr(response))
        tokens = re.findall(
            r"(?<![0-9A-Fa-f])0x([0-9A-Fa-f]{2})(?![0-9A-Fa-f])",
            response["return"],
        )
        raw = bytes(int(token, 16) for token in tokens)
        if len(raw) != expected_size:
            raise RuntimeError(
                f"QMP physical read returned {len(raw)} bytes, "
                f"expected {expected_size}"
            )
        return raw

    def decode_units(raw, unit):
        if unit not in (1, 4, 8) or len(raw) % unit != 0:
            raise RuntimeError(f"invalid physical decode unit={unit} size={len(raw)}")
        return [
            int.from_bytes(raw[offset : offset + unit], "little")
            for offset in range(0, len(raw), unit)
        ]

    def capture_physical_symbols(connection):
        resolved = resolve_physical_symbols()
        values = {}
        for group_name, specs in physical_probe_groups.items():
            for label, symbol, expected_size, _, _ in specs:
                address, actual_size = resolved[symbol]
                if actual_size != expected_size:
                    raise RuntimeError(
                        f"nm size mismatch for {label}: "
                        f"expected={expected_size} actual={actual_size}"
                    )
                if address <= 0 or address + actual_size > 128 * 1024 * 1024:
                    raise RuntimeError(
                        f"nm physical address outside guest RAM for {label}: "
                        f"address=0x{address:x} size={actual_size}"
                    )
            group_start = min(resolved[symbol][0] for _, symbol, _, _, _ in specs)
            group_end = max(
                resolved[symbol][0] + resolved[symbol][1]
                for _, symbol, _, _, _ in specs
            )
            group_size = group_end - group_start
            if group_size <= 0 or group_size > 4096:
                raise RuntimeError(
                    f"unbounded physical probe group {group_name}: "
                    f"address=0x{group_start:x} size={group_size}"
                )
            response = qmp_execute(
                connection,
                "hmp-xp-" + group_name,
                "human-monitor-command",
                {"command-line": f"xp /{group_size}bx 0x{group_start:x}"},
            )
            group_raw = hmp_physical_bytes(response, group_size)
            record(
                f"qmp-physical-group={group_name} "
                f"address=0x{group_start:x} size={group_size}"
            )
            for label, symbol, _, unit, emit in specs:
                address, size = resolved[symbol]
                offset = address - group_start
                raw = group_raw[offset : offset + size]
                values[label] = (address, raw, unit)
                if emit:
                    record(
                        f"qmp-physical-symbol={label} "
                        f"address=0x{address:x} size={size} "
                        f"values={decode_units(raw, unit)!r}"
                    )

        m6_threads = decode_units(values["m6.threads"][1], 8)
        frames_address, frames_raw, _ = values["thread.frames"]
        frame_size = 22 * 8
        for principal_index, principal_name in enumerate(
            ("client", "service", "supervisor", "observer")
        ):
            handle = m6_threads[principal_index]
            slot = handle & 0x00FF_FFFF
            if slot >= 4:
                raise RuntimeError(
                    f"invalid {principal_name} thread handle for frame capture: "
                    f"handle=0x{handle:x} slot={slot}"
                )
            frame_offset = slot * frame_size
            frame = frames_raw[frame_offset : frame_offset + frame_size]
            record(
                f"qmp-physical-symbol=thread.{principal_name}_saved_frame "
                f"handle=0x{handle:x} slot={slot} "
                f"address=0x{frames_address + frame_offset:x} size={frame_size} "
                f"values={decode_units(frame, 8)!r}"
            )

        m6_physicals = decode_units(values["m6.physicals"][1], 8)
        for principal_name, physical in zip(
            ("client", "service", "supervisor", "observer"), m6_physicals
        ):
            config_size = 80
            if physical <= 0 or physical + config_size > 128 * 1024 * 1024:
                raise RuntimeError(
                    f"invalid {principal_name} config physical address: "
                    f"0x{physical:x}"
                )
            response = qmp_execute(
                connection,
                "hmp-xp-config-" + principal_name,
                "human-monitor-command",
                {"command-line": f"xp /{config_size}bx 0x{physical:x}"},
            )
            raw = hmp_physical_bytes(response, config_size)
            record(
                f"qmp-physical-config={principal_name} "
                f"address=0x{physical:x} size={config_size} "
                f"values={decode_units(raw, 8)!r}"
            )

    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            remaining = capture_deadline - time.monotonic()
            connection.settimeout(max(0.05, min(0.5, remaining)))
            connection.connect(path)
            greeting = receive_matching(connection, greeting=True)
            record("qmp-greeting=" + json.dumps(greeting, sort_keys=True)[:65536])
            capabilities = qmp_execute(
                connection, "capabilities", "qmp_capabilities"
            )
            record(
                "qmp-capabilities="
                + json.dumps(capabilities, sort_keys=True)[:65536]
            )
            status_before = qmp_execute(
                connection, "query-status-before-stop", "query-status"
            )
            record(
                "qmp-status-before-stop="
                + json.dumps(status_before, sort_keys=True)[:65536]
            )
            cpus_before = qmp_execute(
                connection, "query-cpus-fast-before-stop", "query-cpus-fast"
            )
            record(
                "qmp-cpus-before-stop="
                + json.dumps(cpus_before, sort_keys=True)[:65536]
            )
            stop_response = qmp_execute(
                connection, "stop-before-diagnostics", "stop"
            )
            record("qmp-stop=" + json.dumps(stop_response, sort_keys=True)[:65536])
            status_after = qmp_execute(
                connection, "query-status-after-stop", "query-status"
            )
            record(
                "qmp-status-after-stop="
                + json.dumps(status_after, sort_keys=True)[:65536]
            )
            stopped = status_after.get("return", {}).get("status") == "paused"
            record("qmp-stop-confirmed=" + repr(stopped))
            for command_id, execute, arguments in (
                ("query-cpus-fast-after-stop", "query-cpus-fast", None),
                (
                    "hmp-info-registers",
                    "human-monitor-command",
                    {"command-line": "info registers"},
                ),
                (
                    "hmp-info-pic",
                    "human-monitor-command",
                    {"command-line": "info pic"},
                ),
                (
                    "hmp-info-irq",
                    "human-monitor-command",
                    {"command-line": "info irq"},
                ),
            ):
                response = qmp_execute(connection, command_id, execute, arguments)
                record(
                    command_id + "=" + json.dumps(response, sort_keys=True)[:65536]
                )
            if stopped:
                try:
                    capture_physical_symbols(connection)
                except Exception as error:
                    record(
                        "qmp-physical-capture-failure="
                        + type(error).__name__
                        + ": "
                        + str(error)
                    )
            else:
                record("qmp-physical-capture-skipped=QEMU stop was not confirmed")
    except Exception as error:
        record("qmp-capture-failure=" + type(error).__name__ + ": " + str(error))
    return captured

while time.monotonic() < deadline:
    now = time.monotonic()
    if completion_seen_at is not None and now - completion_seen_at >= 1.0:
        survived_after_completion = process.poll() is None
        break
    if process.poll() is not None:
        break
    for key, _ in selector.select(timeout=0.05):
        chunk = os.read(key.fileobj.fileno(), 4096)
        if not chunk:
            selector.unregister(key.fileobj)
        elif key.data == "stdout":
            stdout.extend(chunk)
            if completion_seen_at is None and completion_bytes in stdout:
                completion_seen_at = time.monotonic()
        else:
            stderr.extend(chunk)

# A failed liveness observation freezes its transcript before QMP. A successful
# completion window first stops QEMU, then drains the finite pipe tails so a
# queued duplicate or forbidden line cannot escape the semantic verdict.
pre_cleanup_returncode = process.poll()
alive_before_qmp = pre_cleanup_returncode is None


def drain_finite_pipe_tails():
    if process.poll() is None:
        return
    for stream, destination in ((process.stdout, stdout), (process.stderr, stderr)):
        remainder = stream.read()
        if remainder:
            destination.extend(remainder)


def cleanup_process():
    action = "none"
    if process.poll() is not None:
        return action, False
    action = "terminate"
    try:
        process.terminate()
    except ProcessLookupError:
        action = "exited-before-terminate"
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        action = "kill"
        try:
            process.kill()
        except ProcessLookupError:
            action = "exited-before-kill"
        else:
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                action = "kill-timeout"
    return action, process.poll() is None


if not alive_before_qmp:
    drain_finite_pipe_tails()

if survived_after_completion:
    qmp_diagnostics = ["qmp-capture=not-requested-after-liveness-success"]
    alive_after_qmp = process.poll() is None
    cleanup_action, cleanup_failed = cleanup_process()
    drain_finite_pipe_tails()
    frozen_stdout = bytes(stdout)
    frozen_stderr = bytes(stderr)
    verdict_frozen_before_qmp = False
else:
    # No byte observed after the fixed deadline may repair the failed verdict.
    frozen_stdout = bytes(stdout)
    frozen_stderr = bytes(stderr)
    verdict_frozen_before_qmp = True
    if alive_before_qmp:
        qmp_diagnostics = capture_qmp_failure_state(qmp_socket)
    else:
        qmp_diagnostics = ["qmp-capture-unavailable=qemu-not-alive"]
    alive_after_qmp = process.poll() is None
    cleanup_action, cleanup_failed = cleanup_process()
    drain_finite_pipe_tails()

if cleanup_failed:
    qmp_diagnostics.append(
        "process-cleanup-failure=QEMU remained alive for two seconds after "
        "SIGKILL"
    )
for stream in (process.stdout, process.stderr):
    if process.poll() is None:
        stream.close()
selector.close()
cleanup_qmp_dir()

output = frozen_stdout.decode("utf-8", "replace").replace("\r\n", "\n")
error = frozen_stderr.decode("utf-8", "replace").replace("\r\n", "\n")
transcript = output + "\n" + error

startup = "O-core kernel: serial online\n"
preflight = [
    "M6A OVFS image import: PASS\n",
    "M6A four packaged ELF loads: PASS\n",
    "M6A loaded address-space W^X: PASS\n",
    "M6A isolated personality CSpaces: PASS\n",
    "M6A private-before-health publication: PASS\n",
    "M6A scalar personality RPC: armed\n",
]
initial_principals = [
    "M6A client ELF: online\n",
    "M6A personality RPC daemon ELF g1: online\n",
    "M6A supervisor RPC daemon ELF: online\n",
    "M6A unrelated observer ELF: online\n",
]
g1 = [
    "M6A supervisor health RPC g1: PASS\n",
    "M6A supervisor published g1: PASS\n",
    "M6A personality g1 RPC loop: PASS\n",
    "M6A g1 scalar RPC corpus: PASS\n",
    "M6A supervisor cancellation decision: issued\n",
    "M6A supervisor cancellation result: PASS\n",
    "M6A late cancelled reply rejection: PASS\n",
    "M6A timeout result: PASS\n",
    "M6A late timeout reply rejection: PASS\n",
    "M6A personality RPC daemon ELF g1: deliberate fault\n",
]
fault_completion = [
    "M6A personality fault containment: PASS\n",
    "M6A terminal arbitration/wake-once: PASS\n",
    "M6A unrelated world survived: PASS\n",
]
restart = [
    "M6A crash failure result: PASS\n",
    "M6A supervisor endpoint-close event: PASS\n",
    "M6A supervisor restart decision g1->g2: issued\n",
]
g2 = [
    "M6A personality RPC daemon ELF g2: online\n",
    "M6A prior-generation reply rejection: PASS\n",
    "M6A supervisor health RPC g2: PASS\n",
    "M6A supervisor published g2: PASS\n",
    "M6A client observed g2 rebind: PASS\n",
    "M6A duplicate reply rejection: PASS\n",
    "M6A personality g2 RPC loop: PASS\n",
    "M6A g2 scalar RPC corpus: PASS\n",
    "M6A personality g2 cooperative stop: ready\n",
    "M6A supervisor stop decision g2: issued\n",
    "M6A supervisor policy loop: PASS\n",
]
completion = [
    "M6A g2 replacement/rebind: PASS\n",
    "M6A supervisor-owned lifecycle: PASS\n",
    "M6A stale/late/duplicate rejection: PASS\n",
    "M6A resources reclaimed: PASS\n",
    "M6A live personality substrate: PASS\n",
    "M6A post-lifecycle timer: online\n",
]
required = [
    startup,
    *preflight,
    *initial_principals,
    *g1,
    *fault_completion,
    *restart,
    *g2,
    *completion,
]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]
positions = {marker: output.find(marker) for marker in required}

# Preserve scheduler freedom between independent principals while requiring the
# semantic edges that distinguish supervised policy from a scripted transcript.
edges = [(startup, preflight[0]), *zip(preflight, preflight[1:])]
edges.extend((preflight[-1], marker) for marker in initial_principals)
edges.extend(
    [
        (initial_principals[1], g1[0]),
        (initial_principals[2], g1[0]),
        (g1[0], g1[1]),
        (g1[0], g1[2]),
        (g1[0], g1[3]),
        (g1[1], g1[4]),
        (g1[3], g1[4]),
        (g1[4], g1[5]),
        (g1[5], g1[6]),
        (g1[5], g1[7]),
        (g1[2], g1[6]),
        (g1[6], g1[8]),
        (g1[7], g1[8]),
        (g1[8], g1[9]),
        (g1[2], g1[9]),
        (g1[9], restart[0]),
        (g1[9], restart[1]),
        (restart[1], restart[2]),
        (restart[0], fault_completion[0]),
        (restart[2], fault_completion[0]),
        *zip(fault_completion, fault_completion[1:]),
        (fault_completion[-1], g2[0]),
        (g2[0], g2[1]),
        (g2[1], g2[2]),
        (g2[2], g2[3]),
        (g2[2], g2[4]),
        (g2[4], g2[5]),
        (g2[5], g2[6]),
        (g2[5], g2[7]),
        (g2[6], g2[8]),
        (g2[6], g2[9]),
        (g2[7], g2[8]),
        (g2[7], g2[9]),
        (g2[9], g2[10]),
        *((marker, completion[0]) for marker in g2),
        *zip(completion, completion[1:]),
    ]
)
# Missing markers are already a hard failure. Diagnose causal ordering only for
# edges whose two endpoints were actually observed so a timeout does not also
# manufacture unrelated order errors for absent suffixes.
causal_order_valid = all(
    positions[before] < positions[after]
    for before, after in edges
    if positions[before] >= 0 and positions[after] >= 0
)

# As in the M4/M5 loader gates, the first IRQ0 marker must occur after the
# router is armed and before any packaged principal publishes user output.
timer_matches = list(re.finditer(r"(?m)^T$", output))
observed_initial_positions = [
    positions[marker]
    for marker in initial_principals
    if positions[marker] >= 0
]
timer_phase_valid = (
    len(timer_matches) == 1
    and positions[preflight[-1]] >= 0
    and positions[preflight[-1]] < timer_matches[0].start()
    and all(
        timer_matches[0].start() < principal_position
        for principal_position in observed_initial_positions
    )
)

forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "ISOLATION LEAK",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "M3 native live substrate: PASS",
        "M4 native loader/VFS: PASS",
        "M5 native live system: PASS",
        "M6 complete",
        "Milestone 6 complete",
        "Linux personality",
        "personality memory view: PASS",
        "foreign memory view: PASS",
    )
    if marker in transcript
]

observation_failed = (
    missing
    or duplicated
    or forbidden
    or not causal_order_valid
    or not timer_phase_valid
    or not survived_after_completion
    or cleanup_failed
)

if observation_failed:
    print("M6A scalar personality smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not causal_order_valid:
        print("M6A causal phase order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print(
            "exactly one standalone startup T must follow router arming "
            "and precede all packaged principals",
            file=sys.stderr,
        )
    if not survived_after_completion:
        print(
            "QEMU did not survive the one-second post-lifecycle window "
            f"within the {timeout_seconds:g}-second deadline",
            file=sys.stderr,
        )
    if cleanup_failed:
        print("bounded QEMU cleanup failed", file=sys.stderr)
    print(
        "M6A QMP diagnostic context: "
        f"verdict_frozen_before_qmp={verdict_frozen_before_qmp} "
        f"alive_before_qmp={alive_before_qmp} "
        f"alive_after_qmp={alive_after_qmp} "
        f"cleanup_action={cleanup_action}",
        file=sys.stderr,
    )
    for diagnostic in qmp_diagnostics:
        print("M6A QMP diagnostic: " + diagnostic, file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print(
    f"M6A artifact identity: {image_bytes} bytes sha256={digest}"
)
print("M6A scalar personality smoke: PASS")
PY
