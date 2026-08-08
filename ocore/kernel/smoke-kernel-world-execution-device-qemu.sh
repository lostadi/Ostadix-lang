#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel-world-execution-device}"
RECORD_DIR="$BUILD_DIR/kernel-world-record"
RECORD_ONE="$RECORD_DIR/kernel-world-one.record"
RECORD_TWO="$RECORD_DIR/kernel-world-two.record"
TIMEOUT_SECONDS=30
EXPECTED_RECORD_BYTES=459
EXPECTED_RECORD_SHA256="0ece5f7f37ebe203d03cc7e5213dc8f9257a9a225a73e52d37d1f718424b9232"

for tool in qemu-system-x86_64 python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the KernelWorld execution-device smoke" >&2
    exit 127
  fi
done

qemu_version_line="$(qemu-system-x86_64 --version | head -n 1)"
qemu_version="$(
  sed -nE 's/^QEMU emulator version ([0-9]+)\.([0-9]+)(\.[0-9]+)?.*/\1 \2/p' \
    <<<"$qemu_version_line"
)"
if [[ ! "$qemu_version" =~ ^[0-9]+\ [0-9]+$ ]]; then
  echo "error: could not parse QEMU version from: $qemu_version_line" >&2
  exit 2
fi
read -r qemu_major qemu_minor <<<"$qemu_version"
if (( qemu_major < 9 || (qemu_major == 9 && qemu_minor < 2) )); then
  echo "error: Mode 23 requires the supported QEMU 9.2+ real-mode NPT floor; found: $qemu_version_line" >&2
  echo "error: Ubuntu 24.04's QEMU 8.2.2 omits this NPT walk (upstream fix b56617bb)" >&2
  exit 2
fi
printf 'Mode 23 emulator prerequisite: %s\n' "$qemu_version_line"

OCORE_PROBE_MODE=23 OCORE_BUILD_DIR="$BUILD_DIR" \
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

# TCG gives this portable gate an emulated AMD SVM/NPT execution substrate.
# It is deliberately not evidence of KVM acceleration or physical hardware
# assignment/isolation.
command = [
    "qemu-system-x86_64",
    "-accel", "tcg,thread=single",
    "-cpu", "max",
    "-smp", "1",
    "-machine", "q35",
    "-m", "128M",
    "-nodefaults",
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
completion = b"KW post-execution-device timer: online\n"
completion_seen_at = None
survived_after_completion = False

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
            if completion_seen_at is None and completion in stdout:
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
    "KW pinned record hash/tamper rejection: PASS\n",
    "KW exact SVM/NPT requirement binding: PASS\n",
    "KW exact-byte policy + package/manifest binding: PASS\n",
    "KW exact export authority + typed rights: PASS\n",
    "KW VM/vCPU/guest-page generation + quota: PASS\n",
    "KW exact boot-to-SVM/device binding: PASS\n",
    "KW VMEXIT-derived health publication: PASS\n",
    "KW guest PIO exit + virtual endpoint reply: PASS\n",
    "KW client reset dispatched to virtual endpoint: PASS\n",
    "KW NPF-driven quiesce before supervisor failure: PASS\n",
    "KW unrelated live service survived VMEXIT failure: PASS\n",
    "KW generation-2 guest/device rebind + stale denial: PASS\n",
    "KW supervised execution-device reclamation: PASS\n",
    "KW post-execution-device timer: online\n",
]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]
positions = [output.find(marker) for marker in required]
ordered = not missing and positions == sorted(positions)

timer_matches = list(re.finditer(r"(?m)^T$", output))
timer_valid = (
    len(timer_matches) == 1
    and not missing
    and positions[-2] < timer_matches[0].start() < positions[-1]
)

# These strings would overstate the bounded virtual-PIO mechanism proved here.
forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "hardware execution: PASS",
        "hardware isolation: PASS",
        "hardware passthrough: PASS",
        "physical hardware: PASS",
        "KVM: PASS",
        "KVM acceleration: PASS",
        "KVM execution: PASS",
        "PCI assignment: PASS",
        "physical PCI assignment: PASS",
        "PCI passthrough: PASS",
        "device assignment: PASS",
        "physical device assignment: PASS",
        "hardware device assignment: PASS",
        "DMA: PASS",
        "DMA isolation: PASS",
        "IOMMU: PASS",
        "hardware reset: PASS",
        "hardware device reset: PASS",
        "guest boot: PASS",
        "Linux boot: PASS",
        "Plan 9 boot: PASS",
        "Plan9 boot: PASS",
        "firmware boot: PASS",
        "9P: PASS",
        "9p: PASS",
        "shared ring: PASS",
        "shared queue: PASS",
        "shared-memory ring: PASS",
        "shared-memory queue: PASS",
        "queue transport: PASS",
        "guest agent: PASS",
        "general guest agent: PASS",
        "general-purpose guest agent: PASS",
    )
    if marker in transcript
]

if (
    missing
    or duplicated
    or forbidden
    or not ordered
    or not timer_valid
    or not survived_after_completion
):
    print("KernelWorld TCG execution-device smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not ordered:
        print("Mode 23 execution-device phase order is invalid", file=sys.stderr)
    if not timer_valid:
        print(
            "exactly one standalone T must separate reclamation and the post marker",
            file=sys.stderr,
        )
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
print("KernelWorld TCG supervised execution-device smoke: PASS")
PY
