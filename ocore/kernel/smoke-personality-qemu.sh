#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m6a-personality}"
TIMEOUT_SECONDS=30
DIGEST="c2699a2eadae2b406a0b48ecec424fda0cb36402f7cac7324441d98aff73c4e7"
IMAGE_BYTES=62104
IMAGE="$ROOT/target/ocore-m6-artifacts/images/root-${DIGEST}.ovfs"

for tool in qemu-system-x86_64 nm python3 shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the M6A personality smoke" >&2
    exit 127
  fi
done

OCORE_PROBE_MODE=18 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

if [[ ! -f "$IMAGE" ]] \
  || [[ "$(wc -c < "$IMAGE" | tr -d ' ')" != "$IMAGE_BYTES" ]] \
  || [[ "$(shasum -a 256 "$IMAGE" | awk '{print $1}')" != "$DIGEST" ]]; then
  echo "error: M6A embedded OVFS artifact identity is not canonical" >&2
  exit 1
fi

OVFS_PATHS="$(
  python3 "$ROOT/ocore/user/verify_ovfs.py" "$IMAGE" | sed -n '/^\/sbin\//p'
)"
EXPECTED_PATHS=$'/sbin/m6-client.elf: valid\n/sbin/m6-observer.elf: valid\n/sbin/m6-personalityd.elf: valid\n/sbin/m6-supervisord.elf: valid'
if [[ "$OVFS_PATHS" != "$EXPECTED_PATHS" ]]; then
  echo "error: M6A OVFS does not contain the exact four packaged ELF paths" >&2
  printf '%s\n' "$OVFS_PATHS" >&2
  exit 1
fi

# The four principals must enter the kernel only as bytes in the verified OVFS
# image. Source-module symbols in kernel.elf would bypass the package and loader
# boundary that this gate claims.
if ! KERNEL_SYMBOLS="$(nm "$BUILD_DIR/kernel.elf" 2>/dev/null)"; then
  echo "error: nm could not inspect the M6A kernel ELF" >&2
  exit 1
fi
if grep -Eq '_O_runtime__m6_(client|personalityd|supervisord|observer)__' \
    <<<"$KERNEL_SYMBOLS"; then
  echo "error: M6A user principal was linked as kernel code" >&2
  exit 1
fi

python3 - "$BUILD_DIR/kernel.elf" "$TIMEOUT_SECONDS" "$DIGEST" \
  "$IMAGE_BYTES" <<'PY'
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
completion_bytes = b"M6A post-lifecycle timer: online\n"

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

startup = "O-core kernel: serial online\n"
preflight = [
    "M6A OVFS image import: PASS\n",
    "M6A four packaged ELF loads: PASS\n",
    "M6A loaded address-space W^X: PASS\n",
    "M6A isolated personality CSpaces: PASS\n",
    "M6A private-before-health publication: PASS\n",
    "M6A scalar personality RPC: armed\n",
]
initial_principals = [
    "M6A client ELF: online\n",
    "M6A personality RPC daemon ELF g1: online\n",
    "M6A supervisor RPC daemon ELF: online\n",
    "M6A unrelated observer ELF: online\n",
]
g1 = [
    "M6A supervisor health RPC g1: PASS\n",
    "M6A supervisor published g1: PASS\n",
    "M6A personality g1 RPC loop: PASS\n",
    "M6A g1 scalar RPC corpus: PASS\n",
    "M6A supervisor cancellation decision: issued\n",
    "M6A supervisor cancellation result: PASS\n",
    "M6A late cancelled reply rejection: PASS\n",
    "M6A timeout result: PASS\n",
    "M6A late timeout reply rejection: PASS\n",
    "M6A personality RPC daemon ELF g1: deliberate fault\n",
]
fault_completion = [
    "M6A personality fault containment: PASS\n",
    "M6A terminal arbitration/wake-once: PASS\n",
    "M6A unrelated world survived: PASS\n",
]
restart = [
    "M6A crash failure result: PASS\n",
    "M6A supervisor endpoint-close event: PASS\n",
    "M6A supervisor restart decision g1->g2: issued\n",
]
g2 = [
    "M6A personality RPC daemon ELF g2: online\n",
    "M6A prior-generation reply rejection: PASS\n",
    "M6A supervisor health RPC g2: PASS\n",
    "M6A supervisor published g2: PASS\n",
    "M6A client observed g2 rebind: PASS\n",
    "M6A duplicate reply rejection: PASS\n",
    "M6A personality g2 RPC loop: PASS\n",
    "M6A g2 scalar RPC corpus: PASS\n",
    "M6A personality g2 cooperative stop: ready\n",
    "M6A supervisor stop decision g2: issued\n",
    "M6A supervisor policy loop: PASS\n",
]
completion = [
    "M6A g2 replacement/rebind: PASS\n",
    "M6A supervisor-owned lifecycle: PASS\n",
    "M6A stale/late/duplicate rejection: PASS\n",
    "M6A resources reclaimed: PASS\n",
    "M6A live personality substrate: PASS\n",
    "M6A post-lifecycle timer: online\n",
]
required = [
    startup,
    *preflight,
    *initial_principals,
    *g1,
    *fault_completion,
    *restart,
    *g2,
    *completion,
]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]
positions = {marker: output.find(marker) for marker in required}

# Preserve scheduler freedom between independent principals while requiring the
# semantic edges that distinguish supervised policy from a scripted transcript.
edges = [(startup, preflight[0]), *zip(preflight, preflight[1:])]
edges.extend((preflight[-1], marker) for marker in initial_principals)
edges.extend(
    [
        (initial_principals[1], g1[0]),
        (initial_principals[2], g1[0]),
        (g1[0], g1[1]),
        (g1[0], g1[2]),
        (g1[0], g1[3]),
        (g1[1], g1[4]),
        (g1[3], g1[4]),
        (g1[4], g1[5]),
        (g1[5], g1[6]),
        (g1[5], g1[7]),
        (g1[2], g1[6]),
        (g1[6], g1[8]),
        (g1[7], g1[8]),
        (g1[8], g1[9]),
        (g1[2], g1[9]),
        (g1[9], restart[0]),
        (g1[9], restart[1]),
        (restart[1], restart[2]),
        (restart[0], fault_completion[0]),
        (restart[2], fault_completion[0]),
        *zip(fault_completion, fault_completion[1:]),
        (fault_completion[-1], g2[0]),
        (g2[0], g2[1]),
        (g2[1], g2[2]),
        (g2[2], g2[3]),
        (g2[2], g2[4]),
        (g2[4], g2[5]),
        (g2[5], g2[6]),
        (g2[5], g2[7]),
        (g2[6], g2[8]),
        (g2[6], g2[9]),
        (g2[7], g2[8]),
        (g2[7], g2[9]),
        (g2[9], g2[10]),
        *((marker, completion[0]) for marker in g2),
        *zip(completion, completion[1:]),
    ]
)
causal_order_valid = not missing and all(
    positions[before] < positions[after] for before, after in edges
)

# As in the M4/M5 loader gates, the first IRQ0 marker must occur after the
# router is armed and before any packaged principal publishes user output.
timer_matches = list(re.finditer(r"(?m)^T$", output))
timer_phase_valid = (
    len(timer_matches) == 1
    and not missing
    and positions[preflight[-1]] < timer_matches[0].start()
    and timer_matches[0].start()
    < min(positions[marker] for marker in initial_principals)
)

forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "ISOLATION LEAK",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "M3 native live substrate: PASS",
        "M4 native loader/VFS: PASS",
        "M5 native live system: PASS",
        "M6 complete",
        "Milestone 6 complete",
        "Linux personality",
        "personality memory view: PASS",
        "foreign memory view: PASS",
    )
    if marker in transcript
]

if (
    missing
    or duplicated
    or forbidden
    or not causal_order_valid
    or not timer_phase_valid
    or not survived_after_completion
):
    print("M6A scalar personality smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not causal_order_valid:
        print("M6A causal phase order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print(
            "exactly one standalone startup T must follow router arming "
            "and precede all packaged principals",
            file=sys.stderr,
        )
    if not survived_after_completion:
        print(
            "QEMU did not survive the one-second post-lifecycle window "
            "within the 30-second deadline",
            file=sys.stderr,
        )
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print(
    f"M6A artifact identity: {image_bytes} bytes sha256={digest}"
)
print("M6A scalar personality smoke: PASS")
PY
