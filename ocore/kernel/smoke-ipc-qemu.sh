#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m3-live}"
TIMEOUT_SECONDS=20

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "error: qemu-system-x86_64 is not installed" >&2
  exit 127
fi

OCORE_PROBE_MODE=14 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

python3 - "$BUILD_DIR/kernel.elf" "$TIMEOUT_SECONDS" <<'PY'
import os
import re
import selectors
import subprocess
import sys
import time

kernel = sys.argv[1]
timeout_seconds = float(sys.argv[2])
command = [
    "qemu-system-x86_64",
    "-machine", "q35",
    "-m", "128M",
    "-kernel", kernel,
    "-display", "none",
    "-serial", "stdio",
    "-no-reboot",
    "-no-shutdown",
]

process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
selector = selectors.DefaultSelector()
selector.register(process.stdout, selectors.EVENT_READ, "stdout")
selector.register(process.stderr, selectors.EVENT_READ, "stderr")
stdout = bytearray()
stderr = bytearray()
deadline = time.monotonic() + timeout_seconds
completion_seen_at = None
survived_after_completion = False
completion_bytes = b"M3 post-lifecycle timer: online\n"

while time.monotonic() < deadline:
    now = time.monotonic()
    if completion_seen_at is not None and now - completion_seen_at >= 1.0:
        survived_after_completion = process.poll() is None
        break
    if process.poll() is not None:
        break
    for key, _ in selector.select(timeout=0.1):
        chunk = os.read(key.fileobj.fileno(), 4096)
        if not chunk:
            selector.unregister(key.fileobj)
        elif key.data == "stdout":
            stdout.extend(chunk)
            if completion_seen_at is None and completion_bytes in stdout:
                completion_seen_at = time.monotonic()
        else:
            stderr.extend(chunk)

if process.poll() is None:
    process.terminate()
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
for stream, destination in ((process.stdout, stdout), (process.stderr, stderr)):
    remainder = stream.read()
    if remainder:
        destination.extend(remainder)
selector.close()
output = stdout.decode("utf-8", "replace").replace("\r\n", "\n")
error = stderr.decode("utf-8", "replace").replace("\r\n", "\n")

client_online = "M3 client CPL3 endpoint path: online\n"
transfer_abort_pass = "M3 transfer abort exhaustion recovery: PASS\n"
service_online = "M3 healthy personality CPL3: online\n"
observer_online = "M3 unrelated observer CPL3: online\n"
crash_online = "M3 crashing personality CPL3: fault now\n"
cross_pass = "M3 cross-domain request/reply: PASS\n"
service_pass = "M3 personality service FIFO: PASS\n"
client_pass = "M3 client composed 5 replies: PASS\n"

management = [
    "M3 public CPL3 endpoint syscalls: PASS\n",
    "M3 bounded blocking FIFO/wake-once: PASS\n",
    "M3 attenuated capability transfer: PASS\n",
    "M3 bounded transfer abort/recovery: PASS (16/16)\n",
    "M3 automatic sender-death cleanup: PASS\n",
    "M3 personality crash containment: PASS\n",
    "M3 unrelated world after crash: PASS\n",
    "M3 same/cross-domain composition: PASS\n",
    "M3 resources reclaimed: PASS\n",
    "M3 native live substrate: PASS\n",
    "M3 post-lifecycle timer: online\n",
]
required = [
    client_online,
    service_online,
    observer_online,
    crash_online,
    transfer_abort_pass,
    cross_pass,
    service_pass,
    client_pass,
    *management,
]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]

positions = {marker: output.find(marker) for marker in required}
phase_edges = [
    (client_online, crash_online),
    (crash_online, transfer_abort_pass),
    (client_online, cross_pass),
    (observer_online, cross_pass),
    (crash_online, cross_pass),
    (transfer_abort_pass, cross_pass),
    (service_online, service_pass),
    (cross_pass, service_pass),
    (cross_pass, client_pass),
    (client_pass, management[0]),
    (service_pass, management[0]),
]
phase_edges.extend(zip(management, management[1:]))
phase_order_valid = not missing and all(
    positions[before] < positions[after]
    for before, after in phase_edges
)

# Mode 14 needs IRQ0 to run the preemptive CPL3 proof, so the timer module's
# one-shot `T` honestly appears before user execution.  The separate final
# marker proves another tick after full lifecycle teardown.
timer_matches = list(re.finditer(r"(?m)^T$", output))
online_positions = [positions[marker] for marker in (
    client_online,
    service_online,
    observer_online,
    crash_online,
)]
startup_position = output.find("O-core kernel: serial online\n")
timer_phase_valid = (
    len(timer_matches) == 1
    and startup_position >= 0
    and not missing
    and startup_position < timer_matches[0].start() < min(online_positions)
)

forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "M1 ISOLATION LEAK",
        "M2 ISOLATION LEAK",
        "M3 ISOLATION LEAK",
        "KERNEL POINTER LEAKED",
        "LEAKED",
        "invariant violation",
        "M3 foundation",
        "M3 IPC foundation",
        "M3 completion entered",
        "Milestone 4: PASS",
        "Linux personality: PASS",
    )
    if marker in output
]

if (
    missing
    or duplicated
    or forbidden
    or not phase_order_valid
    or not timer_phase_valid
    or not survived_after_completion
):
    print("M3 native IPC smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not phase_order_valid:
        print("M3 causal phase order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print(
            "exactly one standalone startup T must precede all CPL3 markers",
            file=sys.stderr,
        )
    if not survived_after_completion:
        print(
            "QEMU did not survive the one-second post-lifecycle window "
            "within the 20-second deadline",
            file=sys.stderr,
        )
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print("M3 native IPC smoke: PASS")
PY
