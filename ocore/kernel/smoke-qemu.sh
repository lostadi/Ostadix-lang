#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel}"

"$ROOT/ocore/kernel/build.sh" >/dev/null

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "error: qemu-system-x86_64 is not installed" >&2
  exit 127
fi

python3 - "$BUILD_DIR/kernel.elf" <<'PY'
import subprocess
import sys

kernel = sys.argv[1]
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

try:
    result = subprocess.run(command, capture_output=True, timeout=4)
    output = result.stdout.decode("utf-8", "replace")
    error = result.stderr.decode("utf-8", "replace")
except subprocess.TimeoutExpired as timeout:
    output = (timeout.stdout or b"").decode("utf-8", "replace")
    error = (timeout.stderr or b"").decode("utf-8", "replace")

expected = [
    "O-core kernel: serial online\n",
    "page allocator: online\n",
    "capability: online\n",
    "T",
]
missing = [marker for marker in expected if marker not in output]
if missing:
    print("QEMU smoke failed; missing:", repr(missing), file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print("QEMU smoke: PASS")
PY
