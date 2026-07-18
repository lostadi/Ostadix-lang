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
    timed_out = False
    output = result.stdout.decode("utf-8", "replace")
    error = result.stderr.decode("utf-8", "replace")
except subprocess.TimeoutExpired as timeout:
    timed_out = True
    output = (timeout.stdout or b"").decode("utf-8", "replace")
    error = (timeout.stderr or b"").decode("utf-8", "replace")

expected = [
    "O-core kernel: serial online\n",
    "page allocator: online\n",
    "capability: online\n",
    "CPL3 native[0]: online\n",
    "user zero-fill: online\n",
    "capability bounds: denied\n",
    "forged capability: denied\n",
    "stale capability: denied\n",
    "wrong rights: denied\n",
    "wrong type: denied\n",
    "closed capability: denied\n",
    "user ranges: denied\n",
    "kernel pointer: denied\n",
    "unknown syscall: denied\n",
    "RFLAGS sanitization: online\n",
    "timer CPL3 return: online\n",
    "yield hook: online\n",
    "CPL3 heartbeat: online\n",
]
missing = [marker for marker in expected if marker not in output]
lines = output.splitlines()
if "T" not in lines:
    missing.append("standalone timer marker T")
if missing:
    print("QEMU smoke failed; missing:", repr(missing), file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

timer_line = lines.index("T")
return_line = lines.index("timer CPL3 return: online")
heartbeat_line = lines.index("CPL3 heartbeat: online")
if not timer_line < return_line < heartbeat_line:
    print("QEMU smoke failed; timer/return/heartbeat order is invalid", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

if "LEAKED" in output:
    print("QEMU smoke failed; a denied payload reached serial output", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

if not timed_out:
    print("QEMU smoke failed; kernel stopped before the observation window", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print("QEMU smoke: PASS")
PY
