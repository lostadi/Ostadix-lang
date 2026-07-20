#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m5-semantics}"
TIMEOUT_SECONDS=20

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "error: qemu-system-x86_64 is not installed" >&2
  exit 127
fi

OCORE_PROBE_MODE=17 OCORE_BUILD_DIR="$BUILD_DIR" \
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
completion_bytes = b"M5 semantics post-test tick: online\n"

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
transcript = output + "\n" + error

startup = "O-core kernel: serial online\n"
semantics = [
    "M5 two immutable package roots: PASS\n",
    "M5 overgrant/incomplete activation denial: PASS\n",
    "M5 failed health nonpublication: PASS\n",
    "M5 complete-set rollback + stale refs: PASS\n",
    "M5 crash/restart with unaffected state: PASS\n",
    "M5 strict serial parser corpus: PASS\n",
    "M5 supervisor semantics: PASS\n",
]
post_tick = "M5 semantics post-test tick: online\n"
required = [startup, *semantics, post_tick]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]
positions = {marker: output.find(marker) for marker in required}
phase_order_valid = not missing and all(
    positions[before] < positions[after]
    for before, after in zip(required, required[1:])
)

# The self-test runs with interrupts masked. Its final semantics marker must be
# followed by exactly one first-tick diagnostic, and only then by the explicit
# post-test marker emitted after start() observes the advancing IRQ0 counter.
timer_matches = list(re.finditer(r"(?m)^T$", output))
timer_phase_valid = (
    len(timer_matches) == 1
    and not missing
    and positions[semantics[-1]] < timer_matches[0].start()
    and timer_matches[0].start() < positions[post_tick]
)

# This gate proves only the native package/supervisor semantics corpus. Any
# marker from the independent M3/M4 or interactive M5 serial-live paths would
# make that narrower claim ambiguous and therefore fails the run closed.
forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "ISOLATION LEAK",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "M3 ",
        "M4 ",
        "M5 OVFS image import: PASS",
        "M5 four native ELF loads: PASS",
        "M5 loaded address-space W^X: PASS",
        "M5 isolated service CSpaces: PASS",
        "M5 native control plane: armed",
        "M5 init service ELF: online",
        "M5 supervisor service ELF: online",
        "M5 pkgd service ELF: online",
        "M5 repl service ELF: online",
        "M5 serial command: rejected",
        "M5 serial package install: PASS",
        "M5 serial package activation: PASS",
        "M5 package activation state: PASS",
        "M5 namespace transaction: PASS",
        "M5 resources reclaimed: PASS",
        "M5 native live system: PASS",
        "M5 post-lifecycle timer: online",
        "Linux personality",
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
    print("M5 native semantics smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not phase_order_valid:
        print("M5 semantics phase order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print(
            "exactly one standalone startup T must follow semantics and "
            "precede the post-test marker",
            file=sys.stderr,
        )
    if not survived_after_completion:
        print(
            "QEMU did not survive the one-second post-test window within "
            "the 20-second deadline",
            file=sys.stderr,
        )
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print("M5 native semantics smoke: PASS")
PY
