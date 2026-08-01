#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m6-linux-live-personality}"
TIMEOUT_SECONDS="${OCORE_M6_LINUX_LIVE_TIMEOUT_SECONDS:-180}"
DIGEST="b380e5cbbe50403bd58bdafb11c54f2201f0cc742fc898487fa08ba26e2886e8"
IMAGE_BYTES=60104
IMAGE_RECORD="$BUILD_DIR/m6-linux-image.path"
LINUX_ELF_DIGEST="06240b6a840ed4262835aceff64a94f6ebd77838666f05eb7415d9a0d1b5868d"
LINUX_ELF_BYTES=8520

for tool in qemu-system-x86_64 nm python3 shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the M6 live Linux-personality smoke" >&2
    exit 127
  fi
done

OCORE_PROBE_MODE=25 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

if [[ ! -f "$IMAGE_RECORD" ]] \
  || [[ "$(wc -l < "$IMAGE_RECORD" | tr -d ' ')" != 1 ]]; then
  echo "error: Mode 25 build did not record one exact embedded OVFS path" >&2
  exit 1
fi
IMAGE="$(sed -n '1p' "$IMAGE_RECORD")"
if [[ -z "$IMAGE" || ! -f "$IMAGE" ]] \
  || [[ "$(wc -c < "$IMAGE" | tr -d ' ')" != "$IMAGE_BYTES" ]] \
  || [[ "$(shasum -a 256 "$IMAGE" | awk '{print $1}')" != "$DIGEST" ]]; then
  echo "error: M6 live Linux embedded OVFS artifact identity is not canonical" >&2
  exit 1
fi

OVFS_PATHS="$(
  python3 "$ROOT/ocore/user/verify_ovfs.py" "$IMAGE" \
    | sed -n '/^\/bin\//p; /^\/sbin\//p'
)"
EXPECTED_PATHS=$'/bin/linux-minimal.elf: valid\n/sbin/linux-observer.elf: valid\n/sbin/linux-personalityd.elf: valid\n/sbin/linux-supervisord.elf: valid'
if [[ "$OVFS_PATHS" != "$EXPECTED_PATHS" ]]; then
  echo "error: M6 live Linux OVFS does not contain the exact four packaged ELF paths" >&2
  printf '%s\n' "$OVFS_PATHS" >&2
  exit 1
fi

# Pin the exact foreign corpus inside the image, independently of the complete
# image digest. The strict OVFS verifier above has already established table,
# payload, path-order, permission, and per-entry digest integrity.
python3 - "$IMAGE" "$LINUX_ELF_DIGEST" "$LINUX_ELF_BYTES" <<'PY'
import hashlib
import struct
import sys

image, expected_digest, expected_size = sys.argv[1], sys.argv[2], int(sys.argv[3])
raw = open(image, "rb").read()
header = struct.Struct("<8sIIIIQQQ32s32s16s")
entry = struct.Struct("<HHIQQ32s64s8s")
fields = header.unpack_from(raw)
file_count, table_offset, entry_size = fields[4], fields[5], fields[3]
matches = []
for index in range(file_count):
    record = entry.unpack_from(raw, table_offset + index * entry_size)
    path_len, _, flags, offset, size, digest, path_field, _ = record
    path = path_field[:path_len].decode("utf-8", "strict")
    if path == "/bin/linux-minimal.elf":
        payload = raw[offset : offset + size]
        matches.append((flags, size, digest.hex(), hashlib.sha256(payload).hexdigest()))

expected = (3, expected_size, expected_digest, expected_digest)
if matches != [expected]:
    raise SystemExit(
        "M6 live Linux embedded corpus identity mismatch: "
        f"expected {[expected]!r}, observed {matches!r}"
    )
PY

# All four CPL3 principals must enter the kernel as bytes in the canonical
# OVFS image. Linked source-module symbols would bypass the loader and package
# boundary, even if the same marker strings happened to appear at runtime.
if ! KERNEL_SYMBOLS="$(nm "$BUILD_DIR/kernel.elf" 2>/dev/null)"; then
  echo "error: nm could not inspect the M6 live Linux kernel ELF" >&2
  exit 1
fi
if grep -Eq \
    '_O_runtime__(m6_)?linux(_live)?_(personalityd|supervisord|observer)__|_O_runtime__linux_minimal_guest__' \
    <<<"$KERNEL_SYMBOLS"; then
  echo "error: M6 live Linux user principal was linked as kernel code" >&2
  exit 1
fi

python3 - "$BUILD_DIR/kernel.elf" "$TIMEOUT_SECONDS" "$DIGEST" \
  "$IMAGE_BYTES" "$LINUX_ELF_DIGEST" "$LINUX_ELF_BYTES" <<'PY'
import os
import re
import selectors
import subprocess
import sys
import time

kernel = sys.argv[1]
timeout_seconds = float(sys.argv[2])
digest = sys.argv[3]
image_bytes = int(sys.argv[4])
linux_elf_digest = sys.argv[5]
linux_elf_bytes = int(sys.argv[6])
command = [
    "qemu-system-x86_64",
    "-machine", "q35",
    "-accel", "tcg",
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
completion_bytes = b"M6 Linux post-lifecycle timer: online\n"

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
            if completion_seen_at is None and (
                stdout.startswith(completion_bytes)
                or b"\n" + completion_bytes in stdout
            ):
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
preflight = [
    "M6 Linux OVFS image import: PASS\n",
    "M6 Linux four packaged ELF loads: PASS\n",
    "M6 Linux loaded address-space W^X: PASS\n",
    "M6 Linux isolated personality CSpaces: PASS\n",
    "M6 Linux private-before-health publication: PASS\n",
    "M6 Linux bounded write personality: armed\n",
]
observer_online = "M6 Linux unrelated observer ELF: online\n"
g1_online = "M6 Linux personality daemon ELF g1: online\n"
stdout_line = "o-core linux stdout\n"
stderr_line = "o-core linux stderr\n"
stdout_complete = "M6 Linux stdout bounded completion g1: PASS\n"
g1_fault = "M6 Linux personality daemon ELF g1: deliberate fault\n"
fault_contained = "M6 Linux bounded fault containment: PASS\n"
observer_survived = "M6 Linux unrelated world survived: PASS\n"
g2_online = "M6 Linux personality daemon ELF g2: online\n"
stale_g1 = "M6 Linux prior-generation request rejection: PASS\n"
stderr_complete = "M6 Linux stderr bounded completion g2: PASS\n"
g2_shutdown = [
    "M6 Linux personality g2 cooperative stop: ready\n",
    "M6 Linux supervisor policy loop: PASS\n",
]
enosys = "M6 Linux exact -ENOSYS return: PASS\n"
exit_42 = "M6 Linux direct exit_group(42): PASS\n"
completion = [
    "M6 Linux bridge/fd generation cleanup: PASS\n",
    "M6 Linux resources reclaimed: PASS\n",
    "M6 Linux post-lifecycle timer: online\n",
]
required = [
    startup,
    *preflight,
    observer_online,
    g1_online,
    stdout_line,
    stdout_complete,
    g1_fault,
    fault_contained,
    observer_survived,
    g2_online,
    stale_g1,
    stderr_line,
    stderr_complete,
    *g2_shutdown,
    enosys,
    exit_42,
    *completion,
]
output_lines = output.splitlines(keepends=True)
marker_counts = {marker: output_lines.count(marker) for marker in required}
missing = [marker for marker in required if marker_counts[marker] == 0]
duplicated = [marker for marker in required if marker_counts[marker] != 1]
positions = {marker: -1 for marker in required}
line_offset = 0
for line in output_lines:
    if line in positions and positions[line] < 0:
        positions[line] = line_offset
    line_offset += len(line)

# Assert only semantic dependencies. The observer, supervisor, and foreign
# process remain free to interleave according to the real single-CPU scheduler.
edges = [(startup, preflight[0]), *zip(preflight, preflight[1:])]
edges.extend((preflight[-1], marker) for marker in (observer_online, g1_online))
edges.extend(
    [
        (g1_online, stdout_line),
        (stdout_line, stdout_complete),
        (stdout_complete, g1_fault),
        (g1_fault, fault_contained),
        (g1_fault, g2_online),
        (observer_online, observer_survived),
        (fault_contained, observer_survived),
        (g2_online, stale_g1),
        (stale_g1, stderr_line),
        (stderr_line, stderr_complete),
        (stderr_complete, g2_shutdown[0]),
        *zip(g2_shutdown, g2_shutdown[1:]),
        (stderr_line, enosys),
        (enosys, exit_42),
        (observer_survived, completion[0]),
        (g2_shutdown[-1], completion[0]),
        (exit_42, completion[0]),
        *zip(completion, completion[1:]),
    ]
)
causal_order_valid = all(
    positions[before] < positions[after]
    for before, after in edges
    if positions[before] >= 0 and positions[after] >= 0
)

timer_matches = list(re.finditer(r"(?m)^T$", output))
initial_positions = [positions[observer_online], positions[g1_online]]
timer_phase_valid = (
    len(timer_matches) == 1
    and positions[preflight[-1]] >= 0
    and positions[preflight[-1]] < timer_matches[0].start()
    and all(
        timer_matches[0].start() < principal_position
        for principal_position in initial_positions
        if principal_position >= 0
    )
)

forbidden = [
    phrase
    for phrase in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "ISOLATION LEAK",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "M6B live bounded personality smoke: PASS",
        "KernelWorld TCG supervised execution-device smoke: PASS",
        "Linux kernel boot",
        "Linux boot",
        "Plan 9 boot",
        "Plan9 boot",
        "general Linux ABI",
        "general foreign ABI",
        "KVM evidence",
        "hardware execution",
        "hardware isolation",
        "physical hardware",
        "PCI assignment",
        "device assignment",
        "physical-device evidence",
        "DMA isolation",
        "DMA mapping",
        "IOMMU isolation",
    )
    if phrase in transcript
]
diagnostic_traces = re.findall(r"(?m)^[LF][1-8]$", output)

if (
    missing
    or duplicated
    or forbidden
    or diagnostic_traces
    or not causal_order_valid
    or not timer_phase_valid
    or not survived_after_completion
):
    print("M6 live Linux personality smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if diagnostic_traces:
        print(
            "temporary diagnostic traces:",
            repr(diagnostic_traces),
            file=sys.stderr,
        )
    if not causal_order_valid:
        print("M6 live Linux causal phase order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print(
            "exactly one standalone startup T must follow router arming "
            "and precede both output-producing packaged principals",
            file=sys.stderr,
        )
    if not survived_after_completion:
        print(
            "QEMU did not survive the one-second post-lifecycle window "
            f"within the {timeout_seconds:g}-second deadline",
            file=sys.stderr,
        )
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print(
    "M6 live Linux corpus identity: "
    f"{linux_elf_bytes} bytes sha256={linux_elf_digest}"
)
print(f"M6 live Linux artifact identity: {image_bytes} bytes sha256={digest}")
print("M6 live Linux personality smoke: PASS")
PY
