#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel-world-live}"
RECORD_DIR="$BUILD_DIR/kernel-world-record"
RECORD_ONE="$RECORD_DIR/kernel-world-one.record"
RECORD_TWO="$RECORD_DIR/kernel-world-two.record"
TIMEOUT_SECONDS=30
EXPECTED_RECORD_BYTES=459
EXPECTED_RECORD_SHA256="0ece5f7f37ebe203d03cc7e5213dc8f9257a9a225a73e52d37d1f718424b9232"

for tool in qemu-system-x86_64 python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the KernelWorld live-service smoke" >&2
    exit 127
  fi
done

OCORE_PROBE_MODE=22 OCORE_BUILD_DIR="$BUILD_DIR" \
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
    "-accel", "tcg",
    "-cpu", "max",
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
completion = b"KW post-boot-service timer: online\n"
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
    "KW pinned hash/tamper + noncanonical-order rejection: PASS\n",
    "KW exact SVM/NPT requirement binding: PASS\n",
    "KW exact-byte policy + package/manifest default-deny: PASS\n",
    "KW exact export authority + typed rights: PASS\n",
    "KW VM/vCPU/guest-page generation + quota: PASS\n",
    "KW boot supervisor staged configured worlds: PASS\n",
    "KW health-before-publication + typed CSpace lookup: PASS\n",
    "KW capability-backed status/reset broker: PASS\n",
    "KW failure unpublishes before exact VM-graph revoke: PASS\n",
    "KW unrelated live service survived primary failure: PASS\n",
    "KW declared on-failure restart policy enforced: PASS\n",
    "KW generation-2 restart/rebind + stale denial: PASS\n",
    "KW live boot-service lifecycle + reclamation: PASS\n",
    "KW post-boot-service timer: online\n",
]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]
positions = [output.find(marker) for marker in required]
ordered = not missing and positions == sorted(positions)

# Mode 22 proves only the native lifecycle, publication, broker, and revocation
# mechanisms while O-core itself runs under QEMU TCG. It does not enter a guest,
# boot a foreign kernel, assign hardware, or establish a DMA/IOMMU boundary.
forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "KW bounded VM pilot configured without execution: PASS",
        "KW exact-world revoke/reclaim; unrelated VM survives: PASS",
        "KW native admission + nonexecuting VM objects: PASS",
        "KW post-object-model timer: online",
        "KW virtualization capability detected: PASS",
        "KW second-stage guest mappings: PASS",
        "KW vCPU entered guest mode: PASS",
        "KW guest computation result: PASS",
        "KW controlled hypercall exit: PASS",
        "KW virtual interrupt delivery: PASS",
        "KW unauthorized guest memory denied: PASS",
        "KW exact-world NPT teardown: PASS",
        "KW vCPU stop/restart generation: PASS",
        "KW unrelated VM survived: PASS",
        "KW first executable VM substrate: PASS",
        "KW post-execution timer: online",
        "guest boot: PASS",
        "firmware boot: PASS",
        "Linux boot: PASS",
        "Plan 9 boot: PASS",
        "guest agent: PASS",
        "shared ring: PASS",
        "9P: PASS",
        "PCI assignment: PASS",
        "device assignment: PASS",
        "DMA isolation: PASS",
        "IOMMU: PASS",
        "hardware device reset: PASS",
    )
    if marker in transcript
]

if (
    missing
    or duplicated
    or forbidden
    or not ordered
    or not survived_after_completion
):
    print("KernelWorld TCG live boot-service smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not ordered:
        print("Mode 22 lifecycle phase order is invalid", file=sys.stderr)
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
print("KernelWorld TCG live boot-service smoke: PASS")
PY
