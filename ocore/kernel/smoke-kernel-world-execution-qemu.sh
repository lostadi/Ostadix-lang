#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel-world-execution}"
TIMEOUT_SECONDS=30

for tool in qemu-system-x86_64 python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the KernelWorld execution smoke" >&2
    exit 127
  fi
done
if [[ ! -r /dev/kvm || ! -w /dev/kvm ]]; then
  echo "error: read/write access to /dev/kvm is required for Mode 21" >&2
  exit 126
fi
if ! grep -qw svm /proc/cpuinfo || ! grep -qw npt /proc/cpuinfo; then
  echo "error: Mode 21 currently requires AMD SVM with nested paging" >&2
  exit 126
fi

OCORE_PROBE_MODE=21 OCORE_BUILD_DIR="$BUILD_DIR" \
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
    "-accel", "kvm",
    "-cpu", "host",
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
completion = b"KW post-execution timer: online\n"

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
    "KW virtualization capability detected: PASS\n",
    "KW second-stage guest mappings: PASS\n",
    "KW vCPU entered guest mode: PASS\n",
    "KW guest computation result: PASS\n",
    "KW controlled hypercall exit: PASS\n",
    "KW virtual interrupt delivery: PASS\n",
    "KW unauthorized guest memory denied: PASS\n",
    "KW exact-world NPT teardown: PASS\n",
    "KW vCPU stop/restart generation: PASS\n",
    "KW unrelated VM survived: PASS\n",
    "KW first executable VM substrate: PASS\n",
    "KW post-execution timer: online\n",
]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]
positions = [output.find(marker) for marker in required]
ordered = not missing and positions == sorted(positions)
timer_matches = list(re.finditer(r"(?m)^T$", output))
timer_valid = (
    len(timer_matches) == 1
    and positions[-2] < timer_matches[0].start() < positions[-1]
)
forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "guest boot: PASS",
        "firmware boot: PASS",
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
    or not ordered
    or not timer_valid
    or not survived_after_completion
):
    print("KernelWorld first vCPU execution smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not ordered:
        print("Mode 21 phase order is invalid", file=sys.stderr)
    if not timer_valid:
        print("exactly one standalone T must precede the post marker", file=sys.stderr)
    if not survived_after_completion:
        print("QEMU did not survive the one-second post-completion window", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print("KernelWorld first generation-bound vCPU execution smoke: PASS")
PY
