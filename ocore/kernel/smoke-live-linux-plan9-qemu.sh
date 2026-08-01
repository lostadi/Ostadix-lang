#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m7-linux-plan9-live}"
TIMEOUT_SECONDS="${OCORE_M7_LINUX_PLAN9_TIMEOUT_SECONDS:-180}"
IMAGE_DIGEST="920b014cfb133f033b6761da6fe5b1d22be613bf88112c05ec0af982e1beebd9"
IMAGE_BYTES=92872
IMAGE_RECORD="$BUILD_DIR/m7-linux-plan9-image.path"
LINUX_ELF_DIGEST="06240b6a840ed4262835aceff64a94f6ebd77838666f05eb7415d9a0d1b5868d"
LINUX_ELF_BYTES=8520
DAEMON_DIGEST="55ca69a1565393a433264a50c051385d2e56297895d5ccdc4c58c9894057e31b"
DAEMON_BYTES=37240
SUPERVISOR_DIGEST="8226213011cd1c0e5d709e1328445b099cd3d9fd06474d2d5c9bc90576b21cb4"
SUPERVISOR_BYTES=10952
PLAN9_CLIENT_DIGEST="f998e6a12b2e0c5a790b01e35b0576ca32fdedb7976959846085f785db300e38"
PLAN9_CLIENT_BYTES=21008

for tool in qemu-system-x86_64 nm python3 shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the Mode 26 Linux/Plan 9 smoke" >&2
    exit 127
  fi
done

OCORE_PROBE_MODE=26 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

if [[ ! -f "$IMAGE_RECORD" ]] \
  || [[ "$(wc -l < "$IMAGE_RECORD" | tr -d ' ')" != 1 ]]; then
  echo "error: Mode 26 build did not record one exact embedded OVFS path" >&2
  exit 1
fi
IMAGE="$(sed -n '1p' "$IMAGE_RECORD")"
if [[ -z "$IMAGE" || ! -f "$IMAGE" ]] \
  || [[ "$(wc -c < "$IMAGE" | tr -d ' ')" != "$IMAGE_BYTES" ]] \
  || [[ "$(shasum -a 256 "$IMAGE" | awk '{print $1}')" != "$IMAGE_DIGEST" ]]; then
  echo "error: Mode 26 embedded OVFS artifact identity is not canonical" >&2
  exit 1
fi

OVFS_PATHS="$(
  python3 "$ROOT/ocore/user/verify_ovfs.py" \
    --max-image-bytes 98304 "$IMAGE" \
    | sed -n '/^\/bin\//p; /^\/sbin\//p'
)"
EXPECTED_PATHS=$'/bin/linux-minimal.elf: valid\n/bin/plan9-namespace-client.elf: valid\n/sbin/linux-9pd.elf: valid\n/sbin/linux-supervisord.elf: valid'
if [[ "$OVFS_PATHS" != "$EXPECTED_PATHS" ]]; then
  echo "error: Mode 26 OVFS does not contain the exact four packaged paths" >&2
  printf '%s\n' "$OVFS_PATHS" >&2
  exit 1
fi

# Pin every independently linked principal inside the image, not only the
# complete container. The strict verifier above establishes canonical table,
# path order, permissions, alignment, and per-entry digest integrity.
python3 - \
  "$IMAGE" \
  "$LINUX_ELF_DIGEST" "$LINUX_ELF_BYTES" \
  "$DAEMON_DIGEST" "$DAEMON_BYTES" \
  "$SUPERVISOR_DIGEST" "$SUPERVISOR_BYTES" \
  "$PLAN9_CLIENT_DIGEST" "$PLAN9_CLIENT_BYTES" <<'PY'
import hashlib
import struct
import sys

image = sys.argv[1]
expected = {
    "/bin/linux-minimal.elf": (3, int(sys.argv[3]), sys.argv[2]),
    "/sbin/linux-9pd.elf": (3, int(sys.argv[5]), sys.argv[4]),
    "/sbin/linux-supervisord.elf": (3, int(sys.argv[7]), sys.argv[6]),
    "/bin/plan9-namespace-client.elf": (3, int(sys.argv[9]), sys.argv[8]),
}
raw = open(image, "rb").read()
header = struct.Struct("<8sIIIIQQQ32s32s16s")
entry = struct.Struct("<HHIQQ32s64s8s")
fields = header.unpack_from(raw)
file_count, table_offset, entry_size = fields[4], fields[5], fields[3]
observed = {}
for index in range(file_count):
    record = entry.unpack_from(raw, table_offset + index * entry_size)
    path_len, _, flags, offset, size, digest, path_field, _ = record
    path = path_field[:path_len].decode("utf-8", "strict")
    payload = raw[offset : offset + size]
    observed[path] = (
        flags,
        size,
        digest.hex(),
        hashlib.sha256(payload).hexdigest(),
    )

canonical = {
    path: (flags, size, digest, digest)
    for path, (flags, size, digest) in expected.items()
}
if observed != canonical:
    raise SystemExit(
        "Mode 26 embedded principal identity mismatch: "
        f"expected {canonical!r}, observed {observed!r}"
    )
PY

# The three native CPL3 principals and the foreign Linux ELF must enter only
# as bytes in the canonical OVFS image. Linking their source modules into the
# kernel would bypass the loader, CSpace, and package boundaries under test.
if ! KERNEL_SYMBOLS="$(nm "$BUILD_DIR/kernel.elf" 2>/dev/null)"; then
  echo "error: nm could not inspect the Mode 26 kernel ELF" >&2
  exit 1
fi
if grep -Eq \
    '_O_runtime__m7_(linux_9pd|linux_supervisord|plan9_client)__|_O_runtime__linux_minimal_guest__' \
    <<<"$KERNEL_SYMBOLS"; then
  echo "error: Mode 26 packaged user principal was linked as kernel code" >&2
  exit 1
fi

python3 - \
  "$BUILD_DIR/kernel.elf" "$TIMEOUT_SECONDS" \
  "$IMAGE_DIGEST" "$IMAGE_BYTES" \
  "$LINUX_ELF_DIGEST" "$LINUX_ELF_BYTES" \
  "$DAEMON_DIGEST" "$DAEMON_BYTES" \
  "$SUPERVISOR_DIGEST" "$SUPERVISOR_BYTES" \
  "$PLAN9_CLIENT_DIGEST" "$PLAN9_CLIENT_BYTES" <<'PY'
import os
import re
import selectors
import subprocess
import sys
import time

kernel = sys.argv[1]
timeout_seconds = float(sys.argv[2])
image_digest, image_bytes = sys.argv[3], int(sys.argv[4])
linux_digest, linux_bytes = sys.argv[5], int(sys.argv[6])
daemon_digest, daemon_bytes = sys.argv[7], int(sys.argv[8])
supervisor_digest, supervisor_bytes = sys.argv[9], int(sys.argv[10])
plan9_digest, plan9_bytes = sys.argv[11], int(sys.argv[12])
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
completion_bytes = b"M7 Linux/9P post-lifecycle timer: online\n"

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
    "M7 Linux/9P OVFS image import: PASS\n",
    "M7 Linux provider + Plan 9 CPL3 loads: PASS\n",
    "M7 Linux/9P address-space W^X: PASS\n",
    "M7 Linux/9P isolated principal CSpaces: PASS\n",
    "M7 9P private-before-health publication: PASS\n",
    "M7 Linux-to-9P namespace path: armed\n",
]
client_online = "M7 Plan 9 client ELF: online\n"
g1_online = "M7 Linux 9P daemon g1: online\n"
stdout_line = "o-core linux stdout\n"
g1_snapshot = "M7 Linux 9P snapshot g1: PASS\n"
g1_service = "M7 Linux 9P service g1: PASS\n"
client_g1 = "M7 9P2000 Linux namespace g1: PASS\n"
g1_fault = "M7 Linux 9P daemon g1: deliberate fault\n"
fault_withdrawal = "M7 provider fault withdrew 9P namespace: PASS\n"
survivors = "M7 Linux and Plan 9 clients survived fault: PASS\n"
g2_online = "M7 Linux 9P daemon g2: online\n"
stale_daemon = "M7 Linux 9P stale g1: PASS\n"
stderr_line = "o-core linux stderr\n"
g2_snapshot = "M7 Linux 9P snapshot g2: PASS\n"
stale_client = "M7 stale 9P call capability denial: PASS\n"
g2_service = "M7 Linux 9P service g2: PASS\n"
client_g2 = "M7 9P2000 Linux namespace g2: PASS\n"
g2_stop = "M7 Linux 9P daemon g2: stop ready\n"
policy = "M7 Linux supervisor policy loop: PASS\n"
enosys = "M7 Linux provider exact -ENOSYS return: PASS\n"
exit_42 = "M7 Linux provider direct exit_group(42): PASS\n"
completion = [
    "M7 Linux/9P generation cleanup: PASS\n",
    "M7 Linux/9P resources reclaimed: PASS\n",
    "M7 Linux/9P post-lifecycle timer: online\n",
]
required = [
    startup,
    *preflight,
    client_online,
    g1_online,
    stdout_line,
    g1_snapshot,
    g1_service,
    client_g1,
    g1_fault,
    fault_withdrawal,
    survivors,
    g2_online,
    stale_daemon,
    stderr_line,
    g2_snapshot,
    stale_client,
    g2_service,
    client_g2,
    g2_stop,
    policy,
    enosys,
    exit_42,
    *completion,
]
output_lines = output.splitlines(keepends=True)
marker_counts = {marker: output_lines.count(marker) for marker in required}
missing = [marker for marker in required if marker_counts[marker] == 0]
wrong_count = [marker for marker in required if marker_counts[marker] != 1]
positions = {marker: -1 for marker in required}
line_offset = 0
for line in output_lines:
    if line in positions and positions[line] < 0:
        positions[line] = line_offset
    line_offset += len(line)

# Assert semantic dependencies while allowing the Linux provider, Plan-9-style
# consumer, service, and supervisor to interleave on the real scheduler.
edges = [(startup, preflight[0]), *zip(preflight, preflight[1:])]
edges.extend((preflight[-1], marker) for marker in (client_online, g1_online))
edges.extend(
    [
        (g1_online, stdout_line),
        (stdout_line, g1_snapshot),
        (g1_snapshot, g1_service),
        (g1_snapshot, client_g1),
        (g1_service, g1_fault),
        (client_g1, fault_withdrawal),
        (g1_fault, fault_withdrawal),
        (fault_withdrawal, survivors),
        (survivors, g2_online),
        (g2_online, stale_daemon),
        (g2_online, stale_client),
        (g2_online, stderr_line),
        (stderr_line, g2_snapshot),
        (g2_snapshot, g2_service),
        (g2_snapshot, client_g2),
        (stale_client, client_g2),
        (g2_service, g2_stop),
        (client_g2, g2_stop),
        (g2_stop, policy),
        (stderr_line, enosys),
        (enosys, exit_42),
        (policy, completion[0]),
        (client_g2, completion[0]),
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
initial_positions = [positions[client_online], positions[g1_online]]
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
        "M6 Linux OVFS image import: PASS",
        "M6B live bounded personality smoke: PASS",
        "KernelWorld TCG supervised execution-device smoke: PASS",
        "Linux kernel boot",
        "Linux boot",
        "Plan 9 kernel boot",
        "Plan 9 boot",
        "Plan9 boot",
        "distribution boot",
        "general Linux ABI",
        "general Plan 9 ABI",
        "general foreign ABI",
        "general 9P server",
        "general guest agent",
        "root filesystem",
        "dynamic loader",
        "arbitrary Linux binary",
        "Plan 9 syscall compatibility",
        "KVM evidence",
        "SVM evidence",
        "hardware execution",
        "hardware isolation",
        "physical hardware",
        "PCI assignment",
        "device assignment",
        "physical-device evidence",
        "DMA isolation",
        "DMA mapping",
        "IOMMU isolation",
        "interrupt remapping",
        "hardware reset",
        "shared ring",
        "shared queue",
    )
    if phrase in transcript
]
diagnostic_traces = re.findall(
    r"(?m)^(?:[DH](?=[^DH\n]|$)|[DHXSOY]+$|[DHXSOY]{2,}(?=[^DHXSOY\n]))",
    output,
)

if (
    missing
    or wrong_count
    or forbidden
    or diagnostic_traces
    or not causal_order_valid
    or not timer_phase_valid
    or not survived_after_completion
):
    print("Mode 26 live Linux/Plan 9 smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if wrong_count:
        print("wrong marker count:", repr(wrong_count), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if diagnostic_traces:
        print("temporary diagnostic traces:", repr(diagnostic_traces), file=sys.stderr)
    if not causal_order_valid:
        print("Mode 26 causal phase order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print(
            "exactly one standalone startup T must follow namespace arming "
            "and precede both packaged output-producing principals",
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
    "Mode 26 minimal Linux corpus identity: "
    f"{linux_bytes} bytes sha256={linux_digest}"
)
print(
    "Mode 26 Linux 9P daemon identity: "
    f"{daemon_bytes} bytes sha256={daemon_digest}"
)
print(
    "Mode 26 Linux supervisor identity: "
    f"{supervisor_bytes} bytes sha256={supervisor_digest}"
)
print(
    "Mode 26 Plan 9 namespace client identity: "
    f"{plan9_bytes} bytes sha256={plan9_digest}"
)
print(
    "Mode 26 Linux/Plan 9 OVFS identity: "
    f"{image_bytes} bytes sha256={image_digest}"
)
print("live Linux/Plan 9 9P2000 lifecycle: PASS")
PY
