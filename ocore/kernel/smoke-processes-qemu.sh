#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "error: qemu-system-x86_64 is not installed" >&2
  exit 127
fi

for scenario in "10 exit" "11 fault"; do
  mode="${scenario%% *}"
  name="${scenario#* }"
  build_dir="$ROOT/target/ocore-m1/$name"
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
    result = subprocess.run(command, capture_output=True, timeout=2.5)
    timed_out = False
    output = result.stdout.decode("utf-8", "replace")
    error = result.stderr.decode("utf-8", "replace")
except subprocess.TimeoutExpired as timeout:
    timed_out = True
    output = (timeout.stdout or b"").decode("utf-8", "replace")
    error = (timeout.stderr or b"").decode("utf-8", "replace")

process_one_result = (
    "M1 process 1: exited\n"
    if name == "exit"
    else "M1 process 1: fault-contained\n"
)
markers = [
    "M1 address spaces: independent\n",
    "M1 context transaction p1: PASS\n",
    "M1 process 1 CPL3: online\n",
    process_one_result,
]
if name == "fault":
    markers.append("M1 faulted sibling: continuing\n")
markers.extend([
    "M1 sibling private VA: isolated\n",
    "M1 teardown p1: refs dropped\n",
    "M1 teardown p1: AS freed\n",
    "M1 teardown p1: CSpace freed\n",
    "M1 process 1: reaped\n",
    "M1 stale handles: denied\n",
    "M1 context transaction p2: PASS\n",
    "M1 process 2 CPL3: online\n",
    "M1 sibling survival: PASS\n",
    "M1 process 2: exited\n",
    "M1 frames reclaimed: PASS\n",
    "M1 lifecycle: PASS\n",
    "M1 post-lifecycle timer: online\n",
])

missing = [marker for marker in markers if marker not in output]
duplicated = [marker for marker in markers if output.count(marker) > 1]
positions = [output.find(marker) for marker in markers]
forbidden = [
    marker
    for marker in (
        "M1 ISOLATION LEAK",
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "M1 process 1: fault-contained\n" if name == "exit" else "M1 process 1: exited\n",
    )
    if marker in output
]
timer_seen = "T" in output.splitlines()
if (
    missing
    or duplicated
    or forbidden
    or positions != sorted(positions)
    or not timer_seen
    or not timed_out
):
    print(f"M1 {name} lifecycle: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("duplicated:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if positions != sorted(positions):
        print("marker order is invalid", file=sys.stderr)
    if not timer_seen:
        print("standalone timer marker T is missing", file=sys.stderr)
    if not timed_out:
        print("QEMU stopped before the survival window", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(f"M1 {name} lifecycle: PASS")
PY
done

echo "M1 process isolation and teardown matrix: PASS"
