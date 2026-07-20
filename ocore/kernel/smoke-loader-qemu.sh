#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m4-native}"
TIMEOUT_SECONDS=20

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "error: qemu-system-x86_64 is not installed" >&2
  exit 127
fi

OCORE_PROBE_MODE=15 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

# The personalities must arrive only as OVFS payload bytes. A kernel symbol
# for either source module would mean the native-loader gate was bypassed.
if ! command -v nm >/dev/null 2>&1; then
  echo "error: nm is required to prove M4 personalities are not kernel-linked" >&2
  exit 127
fi
if ! KERNEL_SYMBOLS="$(nm "$BUILD_DIR/kernel.elf" 2>/dev/null)"; then
  echo "error: nm could not inspect the M4 kernel ELF" >&2
  exit 1
fi
if grep -Eq 'm4_personality_(alpha|beta).*_start' <<<"$KERNEL_SYMBOLS"; then
  echo "error: M4 personality was linked as kernel code" >&2
  exit 1
fi

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
completion_bytes = b"M4 post-lifecycle timer: online\n"

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

startup = "O-core kernel: serial online\n"
alpha = "M4 personality alpha ELF: online\n"
beta = "M4 personality beta ELF: online\n"
preflight = [
    "M4 OVFS image import: PASS\n",
    "M4 ELF rejection corpus: PASS\n",
    "M4 two native ELF loads: PASS\n",
    "M4 loaded address-space W^X: PASS\n",
    "M4 capability service lookup: PASS\n",
]
completion = [
    "M4 namespace transaction: PASS\n",
    "M4 resources reclaimed: PASS\n",
    "M4 native loader/VFS: PASS\n",
    "M4 post-lifecycle timer: online\n",
]
required = [startup, *preflight, alpha, beta, *completion]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]
positions = {marker: output.find(marker) for marker in required}

edges = [(startup, preflight[0]), *zip(preflight, preflight[1:])]
edges.extend((preflight[-1], marker) for marker in (alpha, beta))
edges.extend((marker, completion[0]) for marker in (alpha, beta))
edges.extend(zip(completion, completion[1:]))
phase_order_valid = not missing and all(
    positions[before] < positions[after] for before, after in edges
)

# IRQ0 must first preempt after the loader has armed two independently loaded
# CPL3 contexts. The final marker proves another interrupt after full teardown.
timer_matches = list(re.finditer(r"(?m)^T$", output))
timer_phase_valid = (
    len(timer_matches) == 1
    and not missing
    and positions[startup] < timer_matches[0].start()
    and timer_matches[0].start() < min(positions[alpha], positions[beta])
)

forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "M4 ISOLATION LEAK",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "M3 native live substrate: PASS",
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
    print("M4 native loader smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not phase_order_valid:
        print("M4 causal phase order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print(
            "exactly one standalone startup T must precede both loaded ELFs",
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
print("M4 native loader smoke: PASS")
PY
