#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m3}"
TIMEOUT_SECONDS="${OCORE_QEMU_TIMEOUT_SECONDS:-15}"

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "error: qemu-system-x86_64 is not installed" >&2
  exit 127
fi

OCORE_PROBE_MODE=13 OCORE_BUILD_DIR="$BUILD_DIR" \
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
completion_bytes = b"M3 foundation post-lifecycle timer: online\n"

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
output = stdout.decode("utf-8", "replace")
error = stderr.decode("utf-8", "replace")

required = [
    "M3 foundation shared mapping: PASS\n",
    "M3 foundation bounded FIFO/cancel: PASS\n",
    "M3 foundation waiter-record cleanup: PASS\n",
    "M3 foundation endpoint generations: PASS\n",
    "M3 foundation attenuating transfer: PASS\n",
    "M3 foundation dead-sender rejection: PASS\n",
    "M3 foundation resources reclaimed: PASS\n",
    "M3 foundation lifecycle: PASS\n",
    "M3 foundation post-lifecycle timer: online\n",
]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) > 1]
positions = [output.find(marker) for marker in required]
marker_order_valid = not missing and positions == sorted(positions)

timer_matches = list(re.finditer(r"(?m)^T\r?$", output))
lifecycle_position = output.find("M3 foundation lifecycle: PASS\n")
post_timer_position = output.find(completion_bytes.decode("ascii"))
standalone_timer_order_valid = (
    len(timer_matches) == 1
    and lifecycle_position >= 0
    and post_timer_position >= 0
    and lifecycle_position < timer_matches[0].start() < post_timer_position
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
        "M3 foundation waiter cleanup: PASS",
        "M3 IPC: PASS",
        "M3 full IPC: PASS",
        "M3 blocking IPC: PASS",
        "M3 ping-pong: PASS",
        "M3 personality crash containment: PASS",
        "M3 complete",
        "M3 COMPLETE",
        "Milestone 3 complete",
        "Milestone 3: PASS",
    )
    if marker in output
]

if (
    missing
    or duplicated
    or forbidden
    or not marker_order_valid
    or not standalone_timer_order_valid
    or not survived_after_completion
):
    print("M3 IPC foundation smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("duplicated:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not marker_order_valid:
        print("M3 foundation marker order is invalid", file=sys.stderr)
    if not standalone_timer_order_valid:
        print(
            "exactly one standalone timer marker T must occur between "
            "lifecycle and post-lifecycle markers",
            file=sys.stderr,
        )
    if not survived_after_completion:
        print(
            "QEMU did not survive the one-second post-lifecycle window "
            f"within the {timeout_seconds:g}-second deadline",
            file=sys.stderr,
        )
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print("M3 IPC foundation smoke: PASS")
PY
