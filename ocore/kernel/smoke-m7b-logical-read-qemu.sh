#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m7b-logical-read-live}"
TIMEOUT_SECONDS="${OCORE_M7B_TIMEOUT_SECONDS:-120}"
IMAGE_RECORD="$BUILD_DIR/m7b-logical-read-image.path"

IMAGE_BYTES=78304
IMAGE_DIGEST=c095ca5076ae0942c21092cd0b92cb752dd736171810df163cd8a4840d0451f5
CLIENT_BYTES=37840
CLIENT_DIGEST=13ad1ea5563adafc433033e3e39c9e1f0129c8c28aebc0cbd8e98907cb5b94ea
PROVIDER_BYTES=29152
PROVIDER_DIGEST=0a56666cd378fdfe4476878be6a2624b46be3e3af23b42cd59bba14ce972205f
OBJECT_BYTES=20
OBJECT_DIGEST=59a08e13c63eb8acdae93f4caf05130733a0f5ab24e564fb1206f0f1d055809b

for tool in qemu-system-x86_64 nm python3 shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the Mode 31 M7B-1 smoke" >&2
    exit 127
  fi
done

OCORE_PROBE_MODE=31 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

if [[ ! -f "$IMAGE_RECORD" \
    || "$(wc -l < "$IMAGE_RECORD" | tr -d ' ')" != 1 ]]; then
  echo "error: Mode 31 did not record one exact embedded OVFS path" >&2
  exit 1
fi
IMAGE="$(sed -n '1p' "$IMAGE_RECORD")"
if [[ -z "$IMAGE" || ! -f "$IMAGE" \
    || "$(wc -c < "$IMAGE" | tr -d ' ')" != "$IMAGE_BYTES" \
    || "$(shasum -a 256 "$IMAGE" | awk '{print $1}')" != "$IMAGE_DIGEST" ]]; then
  echo "error: Mode 31 embedded OVFS identity is not canonical" >&2
  exit 1
fi
python3 "$ROOT/ocore/user/verify_ovfs.py" \
  --max-image-bytes 98304 "$IMAGE" >/dev/null

# Pin all three admitted objects, including the non-executable immutable value.
# The same provider payload is intentionally instantiated twice; this proves
# independent principals, not two independently packaged implementations.
python3 - \
  "$IMAGE" \
  "$CLIENT_DIGEST" "$CLIENT_BYTES" \
  "$PROVIDER_DIGEST" "$PROVIDER_BYTES" \
  "$OBJECT_DIGEST" "$OBJECT_BYTES" <<'PY'
import hashlib
import struct
import sys

raw = open(sys.argv[1], "rb").read()
expected = {
    "/bin/m7b-logical-read.elf": (3, int(sys.argv[3]), sys.argv[2]),
    "/sbin/m7b-9pd.elf": (3, int(sys.argv[5]), sys.argv[4]),
    "/objects/logical-read-v1": (1, int(sys.argv[7]), sys.argv[6]),
}
header = struct.Struct("<8sIIIIQQQ32s32s16s")
entry = struct.Struct("<HHIQQ32s64s8s")
fields = header.unpack_from(raw)
count, table_offset, entry_size = fields[4], fields[5], fields[3]
observed = {}
for index in range(count):
    record = entry.unpack_from(raw, table_offset + index * entry_size)
    path_len, _, flags, offset, size, digest, path_field, _ = record
    path = path_field[:path_len].decode("utf-8", "strict")
    payload = raw[offset : offset + size]
    observed[path] = (flags, size, digest.hex(), hashlib.sha256(payload).hexdigest())
canonical = {
    path: (flags, size, digest, digest)
    for path, (flags, size, digest) in expected.items()
}
if observed != canonical:
    raise SystemExit(
        "Mode 31 admitted-object identity mismatch: "
        f"expected {canonical!r}, observed {observed!r}"
    )
PY

if ! KERNEL_SYMBOLS="$(nm "$BUILD_DIR/kernel.elf" 2>/dev/null)"; then
  echo "error: nm could not inspect the Mode 31 kernel ELF" >&2
  exit 1
fi
if grep -Eq \
    '_O_runtime__m7b_(9p_provider|logical_read_client)__|_O_world__sha256__compute' \
    <<<"$KERNEL_SYMBOLS"; then
  echo "error: a packaged M7B-1 principal/hash implementation entered kernel code" >&2
  exit 1
fi

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
    "qemu-system-x86_64", "-machine", "q35", "-accel", "tcg",
    "-m", "128M", "-kernel", kernel, "-display", "none",
    "-serial", "stdio", "-no-reboot", "-no-shutdown",
]
process = subprocess.Popen(
    command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
    stderr=subprocess.PIPE, bufsize=0,
)
selector = selectors.DefaultSelector()
selector.register(process.stdout, selectors.EVENT_READ, "stdout")
selector.register(process.stderr, selectors.EVENT_READ, "stderr")
stdout = bytearray()
stderr = bytearray()
deadline = time.monotonic() + timeout_seconds
completion = b"M7B-1 post-lifecycle timer: online\n"
completion_seen_at = None
survived = False
while time.monotonic() < deadline:
    now = time.monotonic()
    if completion_seen_at is not None and now - completion_seen_at >= 1.0:
        survived = process.poll() is None
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

startup = "O-core kernel: serial online\n"
preflight = [
    "M7B-1 OVFS immutable corpus: PASS\n",
    "M7B-1 one provider ELF, two CPL3 instances: PASS\n",
    "M7B-1 four isolated native CSpaces/address spaces: PASS\n",
    "M7B-1 A/B identities and distinct routes admitted: PASS\n",
    "M7B-1 local 9P fragment path: armed\n",
]
a_online = "M7B-1 provider A: online\n"
client_online = "M7B-1 client LogicalRead: online\n"
witness = "M7B-1 witness unrelated native principal: online\n"
a_unavailable = "M7B-1 provider A read: unavailable\n"
a_consumed = "M7B-1 provider A Rerror consumed: PASS\n"
a_fault = "M7B-1 provider A: deliberate fault\n"
route_withdrawal = "M7B-1 provider A route withdrawal/stale generation: PASS\n"
stale = "M7B-1 stale provider A capability: PASS\n"
fallback = "M7B-1 fallback A to B after withdrawal: PASS\n"
b_activation = "M7B-1 provider B staged activation after stale proof: PASS\n"
b_online = "M7B-1 provider B: online\n"
b_client_read = "M7B-1 provider B immutable 20-byte read: PASS\n"
b_provider_read = "M7B-1 provider B immutable read: PASS\n"
sha = "M7B-1 native LogicalRead SHA-256: PASS\n"
clunk = "M7B-1 provider B fresh fid clunk: PASS\n"
final = "M7B-1 native LogicalRead fallback: PASS\n"
causal = "M7B-1 non-persisted causal attempt (not OWRECEIPT): PASS\n"
a_physical = "M7B-1 provider A physical/process cleanup: PASS\n"
b_cleanup = "M7B-1 provider B session/queues cleanup: PASS\n"
reclaim = "M7B-1 bounded mechanism resources reclaimed: PASS\n"
post_timer = "M7B-1 post-lifecycle timer: online\n"
required_once = [
    startup, *preflight, a_online, client_online, witness, a_unavailable,
    a_consumed, a_fault, route_withdrawal, stale, fallback, b_activation,
    b_online, b_client_read, b_provider_read, sha, final, causal,
    a_physical, b_cleanup, reclaim, post_timer,
]
lines = output.splitlines(keepends=True)
wrong_count = [marker for marker in required_once if lines.count(marker) != 1]
if lines.count(clunk) != 2:
    wrong_count.append(clunk)
positions = {marker: output.find(marker) for marker in required_once + [clunk]}

# These edges distinguish admission, route exclusion, stale authority,
# staged fallback, successful B data, physical A cleanup, B session cleanup,
# and whole-slice reclamation. The kernel's CAUSAL_STATE independently checks
# the same A-terminal -> withdrawal -> stale -> B-read/digest/clunk sequence.
edges = [(startup, preflight[0]), *zip(preflight, preflight[1:])]
edges.extend((preflight[-1], marker) for marker in (a_online, client_online, witness))
edges.extend([
    (a_online, a_unavailable),
    (a_online, a_consumed),
    (a_unavailable, a_fault),
    (a_consumed, a_fault),
    (a_unavailable, route_withdrawal),
    (a_consumed, route_withdrawal),
    (a_fault, route_withdrawal),
    (route_withdrawal, stale),
    (stale, fallback),
    (stale, b_activation),
    (b_activation, b_online),
    (b_online, b_client_read),
    (b_online, b_provider_read),
    (b_client_read, sha),
    (sha, clunk),
    (clunk, final),
    (final, causal),
    (causal, a_physical),
    (a_physical, b_cleanup),
    (b_cleanup, reclaim),
    (reclaim, post_timer),
])
causal_order_valid = all(
    positions[left] >= 0 and positions[left] < positions[right]
    for left, right in edges
) and output.rfind(clunk) < positions[causal]

forbidden = [
    phrase for phrase in (
        "M02 KERNEL FAULT", "M02 unexpected fault", "ISOLATION LEAK",
        "KERNEL POINTER LEAKED", "invariant violation", "Triple fault",
        "general 9P server", "general 9P namespace", "network transport",
        "writable filesystem", "persistent filesystem", "Linux boot",
        "Plan 9 boot", "foreign kernel", "foreign personality",
        "consensus", "quorum", "hardware isolation", "physical hardware",
        "DMA isolation", "IOMMU isolation",
    ) if phrase in transcript
]
diagnostic = re.findall(
    r"(?m)^(?:[DH](?=[^DH\n]|$)|[DHXSOY]+$|[DHXSOY]{2,}(?=[^DHXSOY\n]))",
    output,
)
if wrong_count or forbidden or diagnostic or not causal_order_valid \
        or not survived:
    print("Mode 31 M7B-1 LogicalRead smoke: FAIL", file=sys.stderr)
    if wrong_count:
        print("wrong marker count:", repr(wrong_count), file=sys.stderr)
    if forbidden:
        print("forbidden claims:", repr(forbidden), file=sys.stderr)
    if diagnostic:
        print("diagnostic traces:", repr(diagnostic), file=sys.stderr)
    if not causal_order_valid:
        print("causal marker order invalid", file=sys.stderr)
    if not survived:
        print("no one-second post-lifecycle survival", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print("Mode 31 M7B-1 LogicalRead smoke: PASS")
print("evidence: local QEMU/TCG, one immutable object, staged native A->B fallback")
print("nonclaims: no network, writes, persistence, foreign kernel, consensus, or hardware isolation")
PY

printf 'Mode 31 image identity: %s bytes sha256=%s\n' \
  "$IMAGE_BYTES" "$IMAGE_DIGEST"
