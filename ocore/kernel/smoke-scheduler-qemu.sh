#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m2}"
# One million CR3/TSS/GS/PCB/CSpace identity transactions are intentionally
# expensive under software-emulated QEMU. Keep the default comfortably above
# the observed loaded-host runtime; callers can still tighten it explicitly.
TIMEOUT_SECONDS="${OCORE_M2_TIMEOUT_SECONDS:-180}"

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "error: qemu-system-x86_64 is not installed" >&2
  exit 127
fi

OCORE_PROBE_MODE=12 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

python3 - "$BUILD_DIR/kernel.elf" "$TIMEOUT_SECONDS" <<'PY'
import subprocess
import os
import selectors
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
completion_bytes = b"M2 post-lifecycle timer: online\n"

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

startup_markers = [
    "M2 forced identity transactions: 1000000 PASS\n",
    "M2 runnable/blocked queues: online\n",
]
user_markers = [
    "M2 cpu thread 1 CPL3: online\n",
    "M2 cpu thread 2 CPL3: online\n",
    "M2 cross-thread hostile RFLAGS: PASS\n",
    "M2 cooperative yield: PASS\n",
    "M2 blocking thread 1: woke\n",
    "M2 blocking thread 2: woke\n",
]
lifecycle_markers = [
    "M2 idle thread: entered\n",
    "M2 timer preemption: PASS\n",
    "M2 blocked wake-once: PASS\n",
    "M2 priority/accounting: PASS\n",
    "M2 hostile TCB RSP: contained\n",
    "M2 exit-during-preemption: contained\n",
    "M2 sibling after exit: PASS\n",
    "M2 thread stale handles: denied\n",
    "M2 frames reclaimed: PASS\n",
    "M2 scheduler lifecycle: PASS\n",
    "M2 post-lifecycle timer: online\n",
]
required = startup_markers + user_markers + lifecycle_markers
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) > 1]

startup_positions = [output.find(marker) for marker in startup_markers]
lifecycle_positions = [output.find(marker) for marker in lifecycle_markers]
startup_order_valid = startup_positions == sorted(startup_positions)
lifecycle_order_valid = lifecycle_positions == sorted(lifecycle_positions)
phase_order_valid = not missing and (
    startup_positions[-1] < min(output.find(marker) for marker in user_markers)
    and max(output.find(marker) for marker in user_markers) < lifecycle_positions[0]
)
timer_seen = "T" in output.splitlines()

forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "M1 ISOLATION LEAK",
        "M2 ISOLATION LEAK",
        "LEAKED",
        "invariant violation",
    )
    if marker in output
]

if (
    missing
    or duplicated
    or forbidden
    or not startup_order_valid
    or not lifecycle_order_valid
    or not phase_order_valid
    or not timer_seen
    or not survived_after_completion
):
    print("M2 scheduler smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("duplicated:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not startup_order_valid:
        print("startup marker order is invalid", file=sys.stderr)
    if not lifecycle_order_valid:
        print("lifecycle marker order is invalid", file=sys.stderr)
    if not phase_order_valid:
        print("startup/user/lifecycle phase order is invalid", file=sys.stderr)
    if not timer_seen:
        print("standalone timer marker T is missing", file=sys.stderr)
    if not survived_after_completion:
        print("QEMU did not survive the post-lifecycle observation window", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print("M2 scheduler smoke: PASS")
PY
