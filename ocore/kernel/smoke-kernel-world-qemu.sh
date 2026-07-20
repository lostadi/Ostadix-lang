#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel-world-objects}"
RECORD_DIR="$BUILD_DIR/kernel-world-record"
RECORD_ONE="$RECORD_DIR/kernel-world-one.record"
RECORD_TWO="$RECORD_DIR/kernel-world-two.record"
TIMEOUT_SECONDS=30
EXPECTED_RECORD_BYTES=440
EXPECTED_RECORD_SHA256="36ebffa374631fc51e70cc20e0512fd899f3703fe15d200a33e330482a707671"

for tool in qemu-system-x86_64 python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the KernelWorld object-model smoke" >&2
    exit 127
  fi
done

OCORE_PROBE_MODE=20 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

python3 - \
  "$BUILD_DIR/kernel.elf" \
  "$RECORD_ONE" \
  "$RECORD_TWO" \
  "$TIMEOUT_SECONDS" \
  "$EXPECTED_RECORD_BYTES" \
  "$EXPECTED_RECORD_SHA256" <<'PY'
import hashlib
import os
import re
import selectors
import subprocess
import sys
import time

kernel = sys.argv[1]
record_one = sys.argv[2]
record_two = sys.argv[3]
timeout_seconds = float(sys.argv[4])
expected_record_bytes = int(sys.argv[5])
expected_record_sha256 = sys.argv[6]

records = []
for path in (record_one, record_two):
    try:
        data = open(path, "rb").read()
    except OSError as error:
        print(f"KernelWorld record read failed: {path}: {error}", file=sys.stderr)
        raise SystemExit(1)
    digest = hashlib.sha256(data).hexdigest()
    if len(data) != expected_record_bytes or digest != expected_record_sha256:
        print(
            "KernelWorld record identity mismatch: "
            f"{path}: bytes={len(data)} sha256={digest}",
            file=sys.stderr,
        )
        raise SystemExit(1)
    records.append(data)

if records[0] != records[1]:
    print("KernelWorld record rebuild was not byte-identical", file=sys.stderr)
    raise SystemExit(1)

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
completion_bytes = b"KW post-object-model timer: online\n"

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

required = [
    "O-core kernel: serial online\n",
    "page protections: W^X online\n",
    "page allocator: online\n",
    "address space: online\n",
    "KW pinned hash/tamper + noncanonical-order rejection: PASS\n",
    "KW exact-byte policy + package/manifest default-deny: PASS\n",
    "KW VM/vCPU/guest-page generation + quota: PASS\n",
    "KW bounded VM pilot configured without execution: PASS\n",
    "KW exact-world revoke/reclaim; unrelated VM survives: PASS\n",
    "KW native admission + nonexecuting VM objects: PASS\n",
    "KW post-object-model timer: online\n",
]
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
    and positions[required[-2]] < timer_matches[0].start()
    and timer_matches[0].start() < positions[required[-1]]
)

# This gate stops at verified admission and a locally configured,
# nonexecuting bounded VM pilot graph.
# Any execution, firmware, interrupt, or device-assignment success marker would
# be evidence for a later milestone and therefore makes this run fail closed.
forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "guest boot: PASS",
        "VM entry: PASS",
        "VMX: PASS",
        "SVM: PASS",
        "EPT: PASS",
        "NPT: PASS",
        "firmware boot: PASS",
        "interrupt injection: PASS",
        "device assignment: PASS",
        "DMA isolation: PASS",
        "IOMMU: PASS",
        "world start: PASS",
        "health transition: PASS",
        "export binding: PASS",
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
    print("KernelWorld native admission/object-model smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not phase_order_valid:
        print("KernelWorld object-model phase order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print("exactly one standalone T must precede the post marker", file=sys.stderr)
    if not survived_after_completion:
        print("QEMU did not survive the one-second post-completion window", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print(
    "KernelWorld verified record: "
    f"{expected_record_bytes} bytes sha256={expected_record_sha256}"
)
print("KernelWorld native admission/object-model smoke: PASS")
PY
