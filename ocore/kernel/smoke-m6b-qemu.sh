#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m6b-bounded}"
TIMEOUT_SECONDS=30

for tool in qemu-system-x86_64 python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the M6B bounded-mechanism smoke" >&2
    exit 127
  fi
done

OCORE_PROBE_MODE=19 OCORE_BUILD_DIR="$BUILD_DIR" \
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
completion_bytes = b"M6B post-mechanism timer: online\n"

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
transcript = output + "\n" + error

startup = "O-core kernel: serial online\n"
mechanism = [
    "M6B pre-dispatch bounds/generation/rights/quota: PASS\n",
    "M6B bounded snapshot + attenuated view caps: PASS\n",
    "M6B staged commit exposes written prefix only: PASS\n",
    "M6B revoke-before-terminal + wake-once paths: PASS\n",
    "M6B five delegated lease classes revoke: PASS\n",
    "M6B request-wide revoke + unrelated-scope survival: PASS\n",
    "M6B reply cleanup + drain-safe teardown: PASS\n",
    "M6B bounded native mechanism slice: PASS\n",
]
post_tick = "M6B post-mechanism timer: online\n"
required = [startup, *mechanism, post_tick]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]
positions = {marker: output.find(marker) for marker in required}
phase_order_valid = not missing and all(
    positions[before] < positions[after]
    for before, after in zip(required, required[1:])
)

timer_matches = list(re.finditer(r"(?m)^T$", output))
timer_phase_valid = (
    len(timer_matches) == 1
    and not missing
    and positions[mechanism[-1]] < timer_matches[0].start()
    and timer_matches[0].start() < positions[post_tick]
)

# This is a bounded-copy mechanism gate, not the later pinned-window, signal,
# Linux-ABI, or user-space personality-service completion. Any such marker
# would broaden the evidence and therefore fails this run closed.
forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "M6A ",
        "pinned window: PASS",
        "signal restart oracle: PASS",
        "Linux personality",
        "Linux ABI",
        "IOMMU",
        "DMA isolation: PASS",
    )
    if marker in transcript
]

if (
    missing
    or duplicated
    or forbidden
    or not phase_order_valid
    or not timer_phase_valid
    or not survived_after_completion
):
    print("M6B bounded native mechanism smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not phase_order_valid:
        print("M6B mechanism phase order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print("exactly one standalone T must precede the post marker", file=sys.stderr)
    if not survived_after_completion:
        print("QEMU did not survive the one-second post-completion window", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print("M6B bounded native mechanism smoke: PASS")
PY
