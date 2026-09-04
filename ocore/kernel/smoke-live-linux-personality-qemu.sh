#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m6-linux-live-personality}"
TIMEOUT_SECONDS="${OCORE_M6_LINUX_LIVE_TIMEOUT_SECONDS:-180}"
DIGEST="b380e5cbbe50403bd58bdafb11c54f2201f0cc742fc898487fa08ba26e2886e8"
IMAGE_BYTES=60104
IMAGE_RECORD="$BUILD_DIR/m6-linux-image.path"
LINUX_ELF_DIGEST="06240b6a840ed4262835aceff64a94f6ebd77838666f05eb7415d9a0d1b5868d"
LINUX_ELF_BYTES=8520

for tool in qemu-system-x86_64 nm python3 shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the M6 live Linux-personality smoke" >&2
    exit 127
  fi
done

OCORE_PROBE_MODE=25 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

if [[ ! -f "$IMAGE_RECORD" ]] \
  || [[ "$(wc -l < "$IMAGE_RECORD" | tr -d ' ')" != 1 ]]; then
  echo "error: Mode 25 build did not record one exact embedded OVFS path" >&2
  exit 1
fi
IMAGE="$(sed -n '1p' "$IMAGE_RECORD")"
if [[ -z "$IMAGE" || ! -f "$IMAGE" ]] \
  || [[ "$(wc -c < "$IMAGE" | tr -d ' ')" != "$IMAGE_BYTES" ]] \
  || [[ "$(shasum -a 256 "$IMAGE" | awk '{print $1}')" != "$DIGEST" ]]; then
  echo "error: M6 live Linux embedded OVFS artifact identity is not canonical" >&2
  exit 1
fi

OVFS_PATHS="$(
  python3 "$ROOT/ocore/user/verify_ovfs.py" "$IMAGE" \
    | sed -n '/^\/bin\//p; /^\/sbin\//p'
)"
EXPECTED_PATHS=$'/bin/linux-minimal.elf: valid\n/sbin/linux-observer.elf: valid\n/sbin/linux-personalityd.elf: valid\n/sbin/linux-supervisord.elf: valid'
if [[ "$OVFS_PATHS" != "$EXPECTED_PATHS" ]]; then
  echo "error: M6 live Linux OVFS does not contain the exact four packaged ELF paths" >&2
  printf '%s\n' "$OVFS_PATHS" >&2
  exit 1
fi

# Pin the exact foreign corpus inside the image, independently of the complete
# image digest. The strict OVFS verifier above has already established table,
# payload, path-order, permission, and per-entry digest integrity.
python3 - "$IMAGE" "$LINUX_ELF_DIGEST" "$LINUX_ELF_BYTES" <<'PY'
import hashlib
import struct
import sys

image, expected_digest, expected_size = sys.argv[1], sys.argv[2], int(sys.argv[3])
raw = open(image, "rb").read()
header = struct.Struct("<8sIIIIQQQ32s32s16s")
entry = struct.Struct("<HHIQQ32s64s8s")
fields = header.unpack_from(raw)
file_count, table_offset, entry_size = fields[4], fields[5], fields[3]
matches = []
for index in range(file_count):
    record = entry.unpack_from(raw, table_offset + index * entry_size)
    path_len, _, flags, offset, size, digest, path_field, _ = record
    path = path_field[:path_len].decode("utf-8", "strict")
    if path == "/bin/linux-minimal.elf":
        payload = raw[offset : offset + size]
        matches.append((flags, size, digest.hex(), hashlib.sha256(payload).hexdigest()))

expected = (3, expected_size, expected_digest, expected_digest)
if matches != [expected]:
    raise SystemExit(
        "M6 live Linux embedded corpus identity mismatch: "
        f"expected {[expected]!r}, observed {matches!r}"
    )
PY

# All four CPL3 principals must enter the kernel as bytes in the canonical
# OVFS image. Linked source-module symbols would bypass the loader and package
# boundary, even if the same marker strings happened to appear at runtime.
if ! KERNEL_SYMBOLS="$(nm "$BUILD_DIR/kernel.elf" 2>/dev/null)"; then
  echo "error: nm could not inspect the M6 live Linux kernel ELF" >&2
  exit 1
fi
if grep -Eq \
    '_O_runtime__(m6_)?linux(_live)?_(personalityd|supervisord|observer)__|_O_runtime__linux_minimal_guest__' \
    <<<"$KERNEL_SYMBOLS"; then
  echo "error: M6 live Linux user principal was linked as kernel code" >&2
  exit 1
fi

python3 - "$BUILD_DIR/kernel.elf" "$TIMEOUT_SECONDS" "$DIGEST" \
  "$IMAGE_BYTES" "$LINUX_ELF_DIGEST" "$LINUX_ELF_BYTES" <<'PY'
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
linux_elf_digest = sys.argv[5]
linux_elf_bytes = int(sys.argv[6])
qmp_dir = tempfile.mkdtemp(prefix="o25q.", dir="/tmp")
qmp_socket = os.path.join(qmp_dir, "qmp.sock")

# These are data-symbol identities, not fixed addresses.  Failure capture asks
# nm for the exact just-built kernel layout, validates each bounded object size,
# then reads the identity-mapped physical bytes only after QEMU is paused.
physical_probe_groups = {
    "thread": [
        ("thread.states", "_O_runtime__thread__STATES", 4, 1, True),
        ("thread.queue_kinds", "_O_runtime__thread__QUEUE_KINDS", 4, 1, True),
        ("thread.runnable_queue", "_O_runtime__thread__RUNNABLE_QUEUE", 32, 8, True),
        ("thread.runnable_head", "_O_runtime__thread__RUNNABLE_HEAD", 8, 8, True),
        ("thread.runnable_length", "_O_runtime__thread__RUNNABLE_LENGTH", 8, 8, True),
        ("thread.blocked_queue", "_O_runtime__thread__BLOCKED_QUEUE", 32, 8, True),
        ("thread.blocked_head", "_O_runtime__thread__BLOCKED_HEAD", 8, 8, True),
        ("thread.blocked_length", "_O_runtime__thread__BLOCKED_LENGTH", 8, 8, True),
        ("thread.current", "_O_runtime__thread__CURRENT_THREAD", 8, 8, True),
        ("thread.prepared", "_O_runtime__thread__PREPARED_THREAD", 8, 8, True),
        ("thread.run_ticks", "_O_runtime__thread__RUN_TICKS", 32, 8, True),
        ("thread.switch_counts", "_O_runtime__thread__SWITCH_COUNTS", 32, 8, True),
        ("thread.frames", "_O_runtime__thread__FRAMES", 704, 8, False),
    ],
    "scheduler": [
        ("scheduler.threads", "_O_runtime__scheduler__THREADS", 32, 8, True),
        ("scheduler.active", "_O_runtime__scheduler__ACTIVE", 1, 1, True),
        ("scheduler.failed", "_O_runtime__scheduler__FAILED", 1, 1, True),
        ("scheduler.kind", "_O_runtime__scheduler__SCHEDULER_KIND", 1, 1, True),
        ("scheduler.completion_kind", "_O_runtime__scheduler__COMPLETION_KIND", 1, 1, True),
        ("scheduler.last_tick", "_O_runtime__scheduler__LAST_TICK", 8, 8, True),
    ],
    "m6": [
        ("m6.active_mode", "_O_kernel__m6__ACTIVE_MODE", 8, 8, True),
        ("m6.processes", "_O_kernel__m6__PROCESSES", 32, 8, True),
        ("m6.threads", "_O_kernel__m6__THREADS", 32, 8, True),
        ("m6.physicals", "_O_kernel__m6__PHYSICALS", 32, 8, True),
        ("m6.supervision", "_O_kernel__m6__SUPERVISION", 8, 8, True),
        ("m6.service_fault_recorded", "_O_kernel__m6__SERVICE_FAULT_RECORDED", 1, 1, True),
    ],
    "mode25_diagnostics": [
        (
            "mode25_diagnostics.post_s2",
            "_O_kernel__m6_mode25_diagnostics__POST_S2_DIAGNOSTICS",
            1,
            1,
            True,
        ),
    ],
    "supervision": [
        ("supervision.states", "_O_runtime__personality_supervision__STATES", 8, 1, True),
        ("supervision.generations", "_O_runtime__personality_supervision__GENERATIONS", 32, 4, True),
        ("supervision.monitor_arms_per_binding", "_O_runtime__personality_supervision__MONITOR_ARMS_PER_BINDING", 64, 8, True),
        ("supervision.crashes_per_binding", "_O_runtime__personality_supervision__CRASHES_PER_BINDING", 64, 8, True),
        ("supervision.restart_requests_per_binding", "_O_runtime__personality_supervision__RESTART_REQUESTS_PER_BINDING", 64, 8, True),
        ("supervision.replacements_per_binding", "_O_runtime__personality_supervision__REPLACEMENTS_PER_BINDING", 64, 8, True),
        ("supervision.publishes", "_O_runtime__personality_supervision__PUBLISHES", 8, 8, True),
        ("supervision.monitor_arms", "_O_runtime__personality_supervision__MONITOR_ARMS", 8, 8, True),
        ("supervision.crashes", "_O_runtime__personality_supervision__CRASHES", 8, 8, True),
        ("supervision.restart_requests", "_O_runtime__personality_supervision__RESTART_REQUESTS", 8, 8, True),
        ("supervision.replacement_installs", "_O_runtime__personality_supervision__REPLACEMENT_INSTALLS", 8, 8, True),
        ("supervision.stop_requests", "_O_runtime__personality_supervision__STOP_REQUESTS", 8, 8, True),
        ("supervision.denials", "_O_runtime__personality_supervision__DENIALS", 8, 8, True),
    ],
}


def cleanup_qmp_dir():
    shutil.rmtree(qmp_dir, ignore_errors=True)


atexit.register(cleanup_qmp_dir)
command = [
    "qemu-system-x86_64",
    "-machine", "q35",
    "-accel", "tcg",
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
launch_at = time.monotonic()
deadline = launch_at + timeout_seconds
completion_seen_at = None
survived_after_completion = False
completion_bytes = b"M6 Linux post-lifecycle timer: online\n"
arrival_events = []
stream_fragments = {"stdout": bytearray(), "stderr": bytearray()}


def capture_qmp_failure_state(path, budget_seconds=2.0):
    """Best-effort diagnostics only; never changes the smoke verdict."""
    captured = []
    capture_deadline = time.monotonic() + budget_seconds
    receive_buffer = bytearray()

    def qmp_execute(connection, command_id, execute, arguments=None):
        request = {"execute": execute, "id": command_id}
        if arguments is not None:
            request["arguments"] = arguments
        connection.sendall((json.dumps(request) + "\n").encode("utf-8"))
        return receive_matching(connection, command_id)

    def resolve_physical_symbols():
        required_names = {
            symbol
            for specs in physical_probe_groups.values()
            for _, symbol, _, _, _ in specs
        }
        result = subprocess.run(
            ["nm", "-S", "-n", "--defined-only", kernel],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
            timeout=max(0.05, capture_deadline - time.monotonic()),
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
        missing = sorted(required_names - resolved.keys())
        if missing:
            raise RuntimeError("missing nm physical symbols: " + repr(missing))
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
            raise RuntimeError(
                f"invalid physical decode unit={unit} size={len(raw)}"
            )
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
            command_id = "hmp-xp-" + group_name
            response = qmp_execute(
                connection,
                command_id,
                "human-monitor-command",
                {"command-line": f"xp /{group_size}bx 0x{group_start:x}"},
            )
            group_raw = hmp_physical_bytes(response, group_size)
            captured.append(
                f"qmp-physical-group={group_name} "
                f"address=0x{group_start:x} size={group_size}"
            )
            for label, symbol, _, unit, emit in specs:
                address, size = resolved[symbol]
                offset = address - group_start
                raw = group_raw[offset : offset + size]
                values[label] = (address, raw, unit)
                if emit:
                    captured.append(
                        f"qmp-physical-symbol={label} "
                        f"address=0x{address:x} size={size} "
                        f"values={decode_units(raw, unit)!r}"
                    )

        m6_thread_values = decode_units(values["m6.threads"][1], 8)
        supervisor_handle = m6_thread_values[2]
        supervisor_slot = supervisor_handle & 0x00FF_FFFF
        frames_address, frames_raw, _ = values["thread.frames"]
        frame_size = 22 * 8
        if supervisor_slot >= 4:
            raise RuntimeError(
                "invalid supervisor thread handle for frame capture: "
                f"handle=0x{supervisor_handle:x} slot={supervisor_slot}"
            )
        frame_offset = supervisor_slot * frame_size
        supervisor_frame = frames_raw[frame_offset : frame_offset + frame_size]
        captured.append(
            "qmp-physical-symbol=thread.supervisor_saved_frame "
            f"handle=0x{supervisor_handle:x} slot={supervisor_slot} "
            f"address=0x{frames_address + frame_offset:x} size={frame_size} "
            f"values={decode_units(supervisor_frame, 8)!r}"
        )

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
                try:
                    message = json.loads(raw.decode("utf-8", "replace"))
                except json.JSONDecodeError as error:
                    captured.append(
                        "qmp-decode-error="
                        + repr(str(error))
                        + " raw="
                        + repr(raw[:256])
                    )
                    continue
                if greeting and "QMP" in message:
                    return message
                if expected_id is not None and message.get("id") == expected_id:
                    return message
                captured.append(
                    "qmp-async=" + json.dumps(message, sort_keys=True)[:65536]
                )
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
        raise TimeoutError("QMP diagnostic capture budget exhausted")

    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            remaining = capture_deadline - time.monotonic()
            connection.settimeout(max(0.05, min(0.5, remaining)))
            connection.connect(path)
            greeting_message = receive_matching(connection, greeting=True)
            captured.append(
                "qmp-greeting="
                + json.dumps(greeting_message, sort_keys=True)[:65536]
            )

            capabilities_id = "capabilities"
            capabilities = qmp_execute(
                connection, capabilities_id, "qmp_capabilities"
            )
            captured.append(
                "qmp-capabilities="
                + json.dumps(capabilities, sort_keys=True)[:65536]
            )

            status_before_id = "query-status-before-stop"
            status_before = qmp_execute(
                connection, status_before_id, "query-status"
            )
            captured.append(
                "qmp-status-before-stop="
                + json.dumps(status_before, sort_keys=True)[:65536]
            )

            cpus_before_id = "query-cpus-fast-before-stop"
            cpus_before = qmp_execute(
                connection, cpus_before_id, "query-cpus-fast"
            )
            captured.append(
                "qmp-cpus-before-stop="
                + json.dumps(cpus_before, sort_keys=True)[:65536]
            )

            stop_id = "stop-before-diagnostics"
            stop_response = qmp_execute(connection, stop_id, "stop")
            captured.append(
                "qmp-stop="
                + json.dumps(stop_response, sort_keys=True)[:65536]
            )

            status_id = "query-status-after-stop"
            status_response = qmp_execute(connection, status_id, "query-status")
            stopped_status = status_response.get("return", {}).get("status")
            captured.append(
                "qmp-status-after-stop="
                + json.dumps(status_response, sort_keys=True)[:65536]
            )
            captured.append(
                "qmp-stop-confirmed=" + repr(stopped_status == "paused")
            )

            commands = [
                (
                    "query-cpus-fast-after-stop",
                    "query-cpus-fast",
                    None,
                ),
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
            ]
            for command_id, execute, arguments in commands:
                response = qmp_execute(
                    connection, command_id, execute, arguments
                )
                captured.append(
                    command_id
                    + "="
                    + json.dumps(response, sort_keys=True)[:65536]
                )
            if stopped_status == "paused":
                try:
                    capture_physical_symbols(connection)
                except Exception as error:
                    captured.append(
                        "qmp-physical-capture-failure="
                        + type(error).__name__
                        + ": "
                        + str(error)
                    )
            else:
                captured.append(
                    "qmp-physical-capture-skipped=QEMU stop was not confirmed"
                )
    except Exception as error:
        captured.append(
            "qmp-capture-failure="
            + type(error).__name__
            + ": "
            + str(error)
        )
    return captured


def record_chunk(stream_name, destination, chunk, observed_at):
    destination.extend(chunk)
    pending = stream_fragments[stream_name]
    pending.extend(chunk)
    while True:
        newline = pending.find(b"\n")
        if newline < 0:
            break
        raw_line = bytes(pending[: newline + 1])
        del pending[: newline + 1]
        normalized = raw_line.replace(b"\r\n", b"\n").decode(
            "utf-8", "replace"
        )
        arrival_events.append((observed_at - launch_at, stream_name, normalized))

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
            record_chunk("stdout", stdout, chunk, time.monotonic())
            if completion_seen_at is None and (
                stdout.startswith(completion_bytes)
                or b"\n" + completion_bytes in stdout
            ):
                completion_seen_at = time.monotonic()
        else:
            record_chunk("stderr", stderr, chunk, time.monotonic())

observed_elapsed = time.monotonic() - launch_at
pre_cleanup_returncode = process.poll()
alive_before_cleanup = pre_cleanup_returncode is None
if completion_seen_at is None:
    if alive_before_cleanup:
        harness_classification = "completion-not-seen/qemu-alive"
    else:
        harness_classification = "early-exit"
elif survived_after_completion:
    harness_classification = "success"
elif alive_before_cleanup:
    harness_classification = "completion-window-incomplete/qemu-alive"
else:
    harness_classification = "post-window-death"

if harness_classification != "success" and alive_before_cleanup:
    qmp_diagnostics = capture_qmp_failure_state(qmp_socket)
elif harness_classification != "success":
    qmp_diagnostics = ["qmp-capture-unavailable=qemu-not-alive"]
else:
    qmp_diagnostics = ["qmp-capture=not-requested-on-success"]

cleanup_action = "none"
alive_after_qmp = process.poll() is None
if alive_before_cleanup and not alive_after_qmp:
    cleanup_action = "exited-during-qmp-capture"
if alive_after_qmp:
    cleanup_action = "terminate"
    try:
        process.terminate()
    except ProcessLookupError:
        cleanup_action = "exited-before-terminate"
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        cleanup_action = "kill"
        try:
            process.kill()
        except ProcessLookupError:
            cleanup_action = "exited-before-kill"
        else:
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                cleanup_action = "kill-timeout"
                qmp_diagnostics.append(
                    "process-cleanup-failure=QEMU remained alive for two "
                    "seconds after SIGKILL; original verdict unchanged"
                )
for stream_name, stream, destination in (
    ("stdout", process.stdout, stdout),
    ("stderr", process.stderr, stderr),
):
    if process.poll() is None:
        stream.close()
    else:
        remainder = stream.read()
        if remainder:
            record_chunk(stream_name, destination, remainder, time.monotonic())
selector.close()
cleanup_qmp_dir()
for stream_name, pending in stream_fragments.items():
    if pending:
        arrival_events.append(
            (
                time.monotonic() - launch_at,
                stream_name,
                bytes(pending).decode("utf-8", "replace"),
            )
        )

output = stdout.decode("utf-8", "replace").replace("\r\n", "\n")
error = stderr.decode("utf-8", "replace").replace("\r\n", "\n")
transcript = output + "\n" + error
final_returncode = process.poll()

startup = "O-core kernel: serial online\n"
preflight = [
    "M6 Linux OVFS image import: PASS\n",
    "M6 Linux four packaged ELF loads: PASS\n",
    "M6 Linux loaded address-space W^X: PASS\n",
    "M6 Linux isolated personality CSpaces: PASS\n",
    "M6 Linux private-before-health publication: PASS\n",
    "M6 Linux bounded write personality: armed\n",
]
observer_online = "M6 Linux unrelated observer ELF: online\n"
g1_online = "M6 Linux personality daemon ELF g1: online\n"
failure_matrix = (
    "M6 Linux pre-reply failure matrix: PASS\n"
)
stdout_line = "o-core linux stdout\n"
stderr_line = "o-core linux stderr\n"
stdout_complete = "M6 Linux stdout bounded completion g1: PASS\n"
g1_fault = "M6 Linux personality daemon ELF g1: deliberate fault\n"
fault_contained = "M6 Linux bounded fault containment: PASS\n"
observer_survived = "M6 Linux unrelated world survived: PASS\n"
g2_online = "M6 Linux personality daemon ELF g2: online\n"
stale_g1 = "M6 Linux prior-generation request rejection: PASS\n"
stderr_complete = "M6 Linux stderr bounded completion g2: PASS\n"
g2_shutdown = [
    "M6 Linux personality g2 cooperative stop: ready\n",
    "M6 Linux supervisor policy loop: PASS\n",
]
enosys = "M6 Linux exact -ENOSYS return: PASS\n"
exit_42 = "M6 Linux direct exit_group(42): PASS\n"
completion = [
    "M6 Linux bridge/fd generation cleanup: PASS\n",
    "M6 Linux resources reclaimed: PASS\n",
    "M6 Linux post-lifecycle timer: online\n",
]
required = [
    startup,
    *preflight,
    observer_online,
    g1_online,
    failure_matrix,
    stdout_line,
    stdout_complete,
    g1_fault,
    fault_contained,
    observer_survived,
    g2_online,
    stale_g1,
    stderr_line,
    stderr_complete,
    *g2_shutdown,
    enosys,
    exit_42,
    *completion,
]
output_lines = output.splitlines(keepends=True)
diagnostic_descriptions = {
    "M25D:S1": "service-fault handler entered",
    "M25D:S2": "service-fault state committed",
    "M25D:S3": "supervisor restart request accepted",
    "M25D:S4": "crash continuation entered",
    "M25D:S5": "generation-one call capability revoked",
    "M25D:S6": "generation-one endpoints closed",
    "M25D:S7": "crashed service generation reaped",
    "M25D:S8": "replacement service validated",
    "M25D:S9": "replacement scheduler armed",
    "M25D:S10": "terminal continuation entered",
    "M25D:S11": "terminal capabilities and endpoints closed",
    "M25D:S12": "mount namespaces drained",
    "M25D:S13": "threads, processes, and image reaped",
    "M25D:S14": "post-cleanup timer wait armed",
    "M25D:R0": "generation-one monitor-arm milestone queried after S2",
    "M25D:R0B": "post-S2 monitor-arm milestone invariant failed",
    "M25D:R0S": "generation-one monitor-arm milestone observed after S2",
    "M25D:R1": "generation-one crash-monitor query entered after S2",
    "M25D:R2": "generation-one crash-monitor milestone observed",
    "M25D:R3": "generation-one restart transition entered",
    "M25D:C1": "crash-continuation entry guard failed",
    "M25D:C2": "generation-one call-capability revoke failed",
    "M25D:C3": "Plan-9 residual call capability remained",
    "M25D:C4": "generation-one endpoint closure failed",
    "M25D:C5": "crashed service thread reap failed",
    "M25D:C6": "crashed service process unbind failed",
    "M25D:C7": "crashed service address-space reap failed",
    "M25D:C8": "replacement creation or validation failed",
    "M25D:C9": "replacement scheduler arm failed",
    "M25D:C10": "supervisor restart request rejected",
    "M25D:D1": "Linux write dispatch unexpectedly reported consumed",
    "M25D:D2": "Linux write wait tag or commit invariant failed",
    "M25D:D3": "Linux write wait abort invariant failed",
    "M25D:D4": "Linux pre-reply rollback, stream accounting, or emission failed",
    "M25D:D5": "generation-two publish transition failed",
    "M25D:F1": "terminal-continuation entry guard failed",
    "M25D:F2": "terminal capability or endpoint closure failed",
    "M25D:F3": "mount-namespace teardown failed",
    "M25D:F4": "terminal thread or process reap failed",
    "M25D:F5": "OVFS image destruction failed",
    "M25D:F6": "namespace finalization failed",
    "M25D:F7": "domain, frame, or global invariant cleanup failed",
}
fatal_diagnostic_codes = {
    code
    for code in diagnostic_descriptions
    if code == "M25D:R0B" or code.startswith(
        ("M25D:C", "M25D:D", "M25D:F")
    )
}
native_diagnostics = [
    line.rstrip("\n")
    for line in output_lines
    if line.startswith("M25D:")
]
unknown_native_diagnostics = [
    code for code in native_diagnostics if code not in diagnostic_descriptions
]
fatal_native_diagnostics = [
    code for code in native_diagnostics if code in fatal_diagnostic_codes
]
required_mode25_progress = [
    *(f"M25D:S{index}" for index in range(1, 15)),
    "M25D:R1",
    "M25D:R2",
    "M25D:R3",
]
mode25_progress_counts = {
    code: native_diagnostics.count(code) for code in required_mode25_progress
}
wrong_mode25_progress_counts = {
    code: count for code, count in mode25_progress_counts.items() if count != 1
}
r0_count = native_diagnostics.count("M25D:R0")
r0_success_count = native_diagnostics.count("M25D:R0S")
r0_pair_valid = (r0_count, r0_success_count) in ((0, 0), (1, 1))
ordered_mode25_progress = ["M25D:S2"]
if r0_count == 1 and r0_success_count == 1:
    ordered_mode25_progress.extend(("M25D:R0", "M25D:R0S"))
ordered_mode25_progress.extend(
    ("M25D:R1", "M25D:R2", "M25D:R3", "M25D:S3")
)
mode25_progress_order_valid = (
    not wrong_mode25_progress_counts
    and r0_pair_valid
    and all(
        native_diagnostics.index(before) < native_diagnostics.index(after)
        for before, after in zip(
            ordered_mode25_progress, ordered_mode25_progress[1:]
        )
    )
)
marker_counts = {marker: output_lines.count(marker) for marker in required}
missing = [marker for marker in required if marker_counts[marker] == 0]
duplicated = [marker for marker in required if marker_counts[marker] != 1]
positions = {marker: -1 for marker in required}
line_offset = 0
for line in output_lines:
    if line in positions and positions[line] < 0:
        positions[line] = line_offset
    line_offset += len(line)

# Assert only semantic dependencies. The observer, supervisor, and foreign
# process remain free to interleave according to the real single-CPU scheduler.
edges = [(startup, preflight[0]), *zip(preflight, preflight[1:])]
edges.extend((preflight[-1], marker) for marker in (observer_online, g1_online))
edges.extend(
    [
        (g1_online, failure_matrix),
        (failure_matrix, stdout_line),
        (stdout_line, stdout_complete),
        (stdout_complete, g1_fault),
        (g1_fault, fault_contained),
        (g1_fault, g2_online),
        (observer_online, observer_survived),
        (fault_contained, observer_survived),
        (g2_online, stale_g1),
        (stale_g1, stderr_line),
        (stderr_line, stderr_complete),
        (stderr_complete, g2_shutdown[0]),
        *zip(g2_shutdown, g2_shutdown[1:]),
        (stderr_line, enosys),
        (enosys, exit_42),
        (observer_survived, completion[0]),
        (g2_shutdown[-1], completion[0]),
        (exit_42, completion[0]),
        *zip(completion, completion[1:]),
    ]
)
causal_order_valid = all(
    positions[before] < positions[after]
    for before, after in edges
    if positions[before] >= 0 and positions[after] >= 0
)

timer_matches = list(re.finditer(r"(?m)^T$", output))
initial_positions = [positions[observer_online], positions[g1_online]]
timer_phase_valid = (
    len(timer_matches) == 1
    and positions[preflight[-1]] >= 0
    and positions[preflight[-1]] < timer_matches[0].start()
    and all(
        timer_matches[0].start() < principal_position
        for principal_position in initial_positions
        if principal_position >= 0
    )
)

forbidden = [
    phrase
    for phrase in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "ISOLATION LEAK",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "M6B live bounded personality smoke: PASS",
        "KernelWorld TCG supervised execution-device smoke: PASS",
        "Linux kernel boot",
        "Linux boot",
        "Plan 9 boot",
        "Plan9 boot",
        "general Linux ABI",
        "general foreign ABI",
        "KVM evidence",
        "hardware execution",
        "hardware isolation",
        "physical hardware",
        "PCI assignment",
        "device assignment",
        "physical-device evidence",
        "DMA isolation",
        "DMA mapping",
        "IOMMU isolation",
    )
    if phrase in transcript
]
temporary_diagnostic_traces = re.findall(r"(?m)^[LF][1-8]$", output)
tracked_arrivals = []
required_set = set(required)
required_arrival_ordinals = {
    marker: ordinal for ordinal, marker in enumerate(required, start=1)
}
diagnostic_arrival_ordinals = {
    code: ordinal
    for ordinal, code in enumerate(diagnostic_descriptions, start=1)
}
for elapsed, stream_name, line in arrival_events:
    code = line.rstrip("\n")
    if line in required_set:
        tracked_arrivals.append(
            (
                elapsed,
                stream_name,
                "canonical",
                required_arrival_ordinals[line],
                "canonical evidence marker",
            )
        )
    elif code in diagnostic_descriptions:
        tracked_arrivals.append(
            (
                elapsed,
                stream_name,
                "native-diagnostic",
                diagnostic_arrival_ordinals[code],
                diagnostic_descriptions[code],
            )
        )


def print_harness_diagnostics(destination):
    print(
        "M6 Linux harness diagnostic: "
        f"classification={harness_classification} "
        f"pid={process.pid} elapsed={observed_elapsed:.6f}s "
        f"completion_seen={completion_seen_at is not None} "
        f"alive_before_cleanup={alive_before_cleanup} "
        f"alive_after_qmp={alive_after_qmp} "
        f"pre_cleanup_returncode={pre_cleanup_returncode!r} "
        f"cleanup_action={cleanup_action} final_returncode={final_returncode!r}",
        file=destination,
    )
    for diagnostic in qmp_diagnostics:
        print(
            "M6 Linux QMP diagnostic: " + diagnostic,
            file=destination,
        )
    for elapsed, stream_name, marker_kind, ordinal, description in tracked_arrivals:
        print(
            "M6 Linux marker arrival: "
            f"t={elapsed:.6f}s stream={stream_name} "
            f"kind={marker_kind} ordinal={ordinal:02d} meaning={description}",
            file=destination,
        )

if (
    missing
    or duplicated
    or forbidden
    or temporary_diagnostic_traces
    or unknown_native_diagnostics
    or fatal_native_diagnostics
    or wrong_mode25_progress_counts
    or not r0_pair_valid
    or not mode25_progress_order_valid
    or not causal_order_valid
    or not timer_phase_valid
    or harness_classification != "success"
    or final_returncode is None
):
    print("M6 live Linux personality smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if temporary_diagnostic_traces:
        print(
            "temporary diagnostic traces:",
            repr(temporary_diagnostic_traces),
            file=sys.stderr,
        )
    if unknown_native_diagnostics:
        print(
            "unknown native diagnostics:",
            repr(unknown_native_diagnostics),
            file=sys.stderr,
        )
    if fatal_native_diagnostics:
        print(
            "fatal native diagnostics:",
            repr(
                [
                    (code, diagnostic_descriptions[code])
                    for code in fatal_native_diagnostics
                ]
            ),
            file=sys.stderr,
        )
    if wrong_mode25_progress_counts:
        print(
            "wrong Mode25 progress marker count:",
            repr(wrong_mode25_progress_counts),
            file=sys.stderr,
        )
    if not r0_pair_valid:
        print(
            "invalid Mode25 late monitor-arm pair: "
            f"R0={r0_count} R0S={r0_success_count}",
            file=sys.stderr,
        )
    if not mode25_progress_order_valid:
        print(
            "Mode25 progress order invalid:",
            repr(ordered_mode25_progress),
            file=sys.stderr,
        )
    if not causal_order_valid:
        print("M6 live Linux causal phase order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print(
            "exactly one standalone startup T must follow router arming "
            "and precede both output-producing packaged principals",
            file=sys.stderr,
        )
    if harness_classification != "success":
        print(
            "QEMU lifecycle classification: "
            f"{harness_classification} within the "
            f"{timeout_seconds:g}-second deadline",
            file=sys.stderr,
        )
    if final_returncode is None:
        print(
            "QEMU cleanup failure: process remained unreaped after the "
            "bounded SIGTERM/SIGKILL waits",
            file=sys.stderr,
        )
    print_harness_diagnostics(sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print_harness_diagnostics(sys.stdout)
print(
    "M6 live Linux corpus identity: "
    f"{linux_elf_bytes} bytes sha256={linux_elf_digest}"
)
print(f"M6 live Linux artifact identity: {image_bytes} bytes sha256={digest}")
print("M6 live Linux personality smoke: PASS")
PY
