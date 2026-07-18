#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "error: qemu-system-x86_64 is not installed" >&2
  exit 127
fi

for probe in \
  "1 divide" \
  "2 invalid-opcode" \
  "3 non-present" \
  "4 supervisor-read" \
  "5 guard-stack" \
  "6 nx-rip" \
  "7 noncanonical" \
  "8 bad-syscall-return"
do
  mode="${probe%% *}"
  name="${probe#* }"
  build_dir="$ROOT/target/ocore-m02/$name"
  OCORE_PROBE_MODE="$mode" OCORE_BUILD_DIR="$build_dir" \
    "$ROOT/ocore/kernel/build.sh" >/dev/null

  python3 - "$build_dir/kernel.elf" "$name" <<'PY'
import subprocess
import sys

kernel, name = sys.argv[1:]
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
    result = subprocess.run(command, capture_output=True, timeout=1.5)
    timed_out = False
    output = result.stdout.decode("utf-8", "replace")
    error = result.stderr.decode("utf-8", "replace")
except subprocess.TimeoutExpired as timeout:
    timed_out = True
    output = (timeout.stdout or b"").decode("utf-8", "replace")
    error = (timeout.stderr or b"").decode("utf-8", "replace")

trap_marker = (
    "M02 invalid syscall return: contained\n"
    if name == "bad-syscall-return"
    else "M02 trap: expected CPL3 fault\n"
)
markers = [
    "M02 probe: armed CPL3\n",
    trap_marker,
    "M02 process: faulted current=none\n",
    "M02 post-fault timer: online\n",
    "M02 fault containment: PASS\n",
]
missing = [marker for marker in markers if marker not in output]
bad = [
    marker
    for marker in ("M02 KERNEL FAULT", "M02 unexpected fault", "CPL3 native[0]")
    if marker in output
]
positions = [output.find(marker) for marker in markers]
ordered = positions == sorted(positions)
if missing or bad or not ordered or not timed_out:
    print(f"M02 {name}: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if bad:
        print("forbidden:", repr(bad), file=sys.stderr)
    if not ordered:
        print("marker order is invalid", file=sys.stderr)
    if not timed_out:
        print("QEMU stopped before the survival window", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(f"M02 {name}: PASS")
PY
done

copy_build_dir="$ROOT/target/ocore-m02/user-copy"
OCORE_PROBE_MODE=9 OCORE_BUILD_DIR="$copy_build_dir" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

python3 - "$copy_build_dir/kernel.elf" <<'PY'
import subprocess
import sys

command = [
    "qemu-system-x86_64",
    "-machine", "q35",
    "-m", "128M",
    "-kernel", sys.argv[1],
    "-display", "none",
    "-serial", "stdio",
    "-no-reboot",
    "-no-shutdown",
]
try:
    result = subprocess.run(command, capture_output=True, timeout=1.5)
    timed_out = False
    output = result.stdout.decode("utf-8", "replace")
    error = result.stderr.decode("utf-8", "replace")
except subprocess.TimeoutExpired as timeout:
    timed_out = True
    output = (timeout.stdout or b"").decode("utf-8", "replace")
    error = (timeout.stderr or b"").decode("utf-8", "replace")

markers = [
    "M02 probe: armed CPL3\n",
    "user copy syscall: recovered\n",
    "copy-fault heartbeat: online\n",
]
missing = [marker for marker in markers if marker not in output]
positions = [output.find(marker) for marker in markers]
forbidden = [
    marker
    for marker in (
        "M02 trap:",
        "M02 process: faulted",
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
    )
    if marker in output
]
if missing or forbidden or positions != sorted(positions) or not timed_out:
    print("M02 recoverable user-copy: FAIL", file=sys.stderr)
    print("missing:", repr(missing), file=sys.stderr)
    print("forbidden:", repr(forbidden), file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)
print("M02 recoverable user-copy: PASS")
PY

echo "M02 fault and recovery matrix: PASS"
