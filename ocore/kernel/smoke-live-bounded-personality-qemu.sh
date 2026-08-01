#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m6b-live-personality}"
TIMEOUT_SECONDS="${OCORE_M6B_LIVE_TIMEOUT_SECONDS:-180}"
DIGEST="5b9d2526da2abd75ec90b4770ded5923d856132fad736fb13f241c34f1579887"
IMAGE_BYTES=65152
IMAGE="$ROOT/target/ocore-m6b-live-artifacts/images/root-${DIGEST}.ovfs"

for tool in qemu-system-x86_64 nm python3 shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the M6B live bounded-personality smoke" >&2
    exit 127
  fi
done

OCORE_PROBE_MODE=24 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

if [[ ! -f "$IMAGE" ]] \
  || [[ "$(wc -c < "$IMAGE" | tr -d ' ')" != "$IMAGE_BYTES" ]] \
  || [[ "$(shasum -a 256 "$IMAGE" | awk '{print $1}')" != "$DIGEST" ]]; then
  echo "error: M6B live embedded OVFS artifact identity is not canonical" >&2
  exit 1
fi

OVFS_PATHS="$(
  python3 "$ROOT/ocore/user/verify_ovfs.py" "$IMAGE" | sed -n '/^\/sbin\//p'
)"
EXPECTED_PATHS=$'/sbin/m6-client.elf: valid\n/sbin/m6-observer.elf: valid\n/sbin/m6-personalityd.elf: valid\n/sbin/m6-supervisord.elf: valid'
if [[ "$OVFS_PATHS" != "$EXPECTED_PATHS" ]]; then
  echo "error: M6B live OVFS does not contain the exact four packaged ELF paths" >&2
  printf '%s\n' "$OVFS_PATHS" >&2
  exit 1
fi

# The four CPL3 principals enter the kernel only as bytes in the canonical
# OVFS image. Linked source-module symbols would bypass the claimed loader and
# package boundary.
if ! KERNEL_SYMBOLS="$(nm "$BUILD_DIR/kernel.elf" 2>/dev/null)"; then
  echo "error: nm could not inspect the M6B live kernel ELF" >&2
  exit 1
fi
if grep -Eq '_O_runtime__m6b_live_(client|personalityd|supervisord|observer)__' \
    <<<"$KERNEL_SYMBOLS"; then
  echo "error: M6B live user principal was linked as kernel code" >&2
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
completion_bytes = b"M6B live post-lifecycle timer: online\n"

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
    "M6B live OVFS image import: PASS\n",
    "M6B live four packaged ELF loads: PASS\n",
    "M6B live loaded address-space W^X: PASS\n",
    "M6B live isolated personality CSpaces: PASS\n",
    "M6B live private-before-health publication: PASS\n",
    "M6B live bounded RPC package: armed\n",
]
initial_principals = [
    "M6B live unrelated observer ELF: online\n",
    "M6B live personality RPC daemon ELF g1: online\n",
]
g1 = [
    "M6B live delegated view close/copy denial: PASS\n",
    "M6B live direct saved-RAX result/no reissue: PASS\n",
    "M6B live personality g1 bounded RPC loop: PASS\n",
    "M6B live late cancelled reply rejection: PASS\n",
    "M6B live late timeout reply rejection: PASS\n",
    "M6B live personality RPC daemon ELF g1: deliberate fault\n",
]
fault_completion = [
    "M6B live bounded fault containment: PASS\n",
    "M6B live unrelated world survived: PASS\n",
]
g2 = [
    "M6B live personality RPC daemon ELF g2: online\n",
    "M6B live prior-generation reply rejection: PASS\n",
    "M6B live duplicate reply rejection: PASS\n",
    "M6B live personality g2 bounded RPC loop: PASS\n",
    "M6B live personality g2 cooperative stop: ready\n",
    "M6B live supervisor policy loop: PASS\n",
]
completion = [
    "M6B live bounded terminal matrix + g2 rebind: PASS\n",
    "M6B live CPL3 requests/admin lifecycle dispositions: PASS\n",
    "M6B live bounded authority cleanup: PASS\n",
    "M6B live post-lifecycle timer: online\n",
]
required = [
    startup,
    *preflight,
    *initial_principals,
    *g1,
    *fault_completion,
    *g2,
    *completion,
]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]
positions = {marker: output.find(marker) for marker in required}

# Require semantic dependencies while leaving the observer and daemon free to
# interleave according to the real M6A scheduler.
edges = [(startup, preflight[0]), *zip(preflight, preflight[1:])]
edges.extend((preflight[-1], marker) for marker in initial_principals)
edges.extend(
    [
        (initial_principals[1], g1[0]),
        (initial_principals[1], g1[1]),
        (g1[0], g1[2]),
        (g1[1], g1[2]),
        *zip(g1[2:], g1[3:]),
        (g1[-1], fault_completion[0]),
        (initial_principals[0], fault_completion[1]),
        *zip(fault_completion, fault_completion[1:]),
        (fault_completion[-1], g2[0]),
        *zip(g2, g2[1:]),
        *zip(completion, completion[1:]),
        *((marker, completion[0]) for marker in g2),
    ]
)
causal_order_valid = all(
    positions[before] < positions[after]
    for before, after in edges
    if positions[before] >= 0 and positions[after] >= 0
)

# As in the earlier loader gates, the first IRQ0 marker must occur after the
# bounded router is armed and before either output-producing packaged principal.
timer_matches = list(re.finditer(r"(?m)^T$", output))
observed_initial_positions = [
    positions[marker]
    for marker in initial_principals
    if positions[marker] >= 0
]
timer_phase_valid = (
    len(timer_matches) == 1
    and positions[preflight[-1]] >= 0
    and positions[preflight[-1]] < timer_matches[0].start()
    and all(
        timer_matches[0].start() < principal_position
        for principal_position in observed_initial_positions
    )
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
        "M6A ",
        "M6B pre-dispatch bounds/generation/rights/quota: PASS",
        "M6B bounded native mechanism slice: PASS",
        "M6B minimal Linux fd/classification kernel-admin semantics: PASS",
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
    print("M6B live bounded personality smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not causal_order_valid:
        print("M6B live causal phase order is invalid", file=sys.stderr)
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
print(f"M6B live artifact identity: {image_bytes} bytes sha256={digest}")
print("M6B live bounded personality smoke: PASS")
PY
