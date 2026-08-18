#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-world-receipt}"
MODE0_BUILD_DIR="${OCORE_MODE0_BUILD_DIR:-$ROOT/target/ocore-world-receipt-mode0}"
FIXTURE="$ROOT/tests/fixtures/world_receipt_v1.hex"
TIMEOUT_SECONDS=30
EXPECTED_BYTES=3239
EXPECTED_SHA256="1edd90bf881cd42d08e2031482baae4e7c9a95bd78cfa65f0cbe14147c0a2604"
MAX_NEW_FRAME_BYTES=8192

for tool in cargo qemu-system-x86_64 python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the World receipt smoke" >&2
    exit 127
  fi
done
if [[ ! -f "$FIXTURE" ]]; then
  echo "error: missing World receipt byte oracle: $FIXTURE" >&2
  exit 1
fi

# The hosted implementation owns the signed fixture and real Ed25519 proof.
# Native Mode 30 independently reconstructs and parses those exact bytes but
# deliberately makes no native signature-verification claim.
cargo test --quiet --manifest-path "$ROOT/Cargo.toml" --package o-lang --test world_receipt
echo "World receipt hosted Ed25519 sign/verify: PASS"
OCORE_PROBE_MODE=30 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null
OCORE_PROBE_MODE=0 OCORE_BUILD_DIR="$MODE0_BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

python3 - "$BUILD_DIR/kernel.s" "$MODE0_BUILD_DIR/kernel.s" \
  "$MAX_NEW_FRAME_BYTES" <<'PY'
import re
import sys

mode30_path, mode0_path, ceiling_text = sys.argv[1:]
ceiling = int(ceiling_text)

def collect(path, prefixes=None):
    lines = open(path, "r", encoding="utf-8").read().splitlines()
    frames = {}
    for index, line in enumerate(lines):
        label = line.strip()
        if not label.endswith(":"):
            continue
        name = label[:-1]
        if prefixes is not None and not name.startswith(prefixes):
            continue
        frame = 0
        for candidate in lines[index + 1 : index + 9]:
            match = re.fullmatch(r"\s*sub rsp, ([0-9]+)", candidate)
            if match:
                frame = int(match.group(1))
                break
            if candidate and not candidate.startswith((" ", "\t")):
                break
        frames[name] = frame
    return frames

prefixes = (
    "_O_world__identity__",
    "_O_world__receipt__",
    "_O_world__receipt_codec__",
    "_O_world__value__",
    "_O_world__value_codec__",
    "_O_world__sha256__",
    "_O_kernel__world_receipt_semantics__",
)
frames = collect(mode30_path, prefixes)
required = [
    "_O_world__identity__decode_record_with_scratch",
    "_O_world__receipt__validate_unsigned_body",
    "_O_world__receipt_codec__decode_record",
    "_O_world__receipt_codec__build_validated_signing_preimage",
    "_O_kernel__world_receipt_semantics__build_body",
]
missing = [name for name in required if name not in frames]
oversized = sorted(
    ((frame, name) for name, frame in frames.items() if frame > ceiling),
    reverse=True,
)
if missing or oversized:
    print("World receipt generated-frame ceiling: FAIL", file=sys.stderr)
    if missing:
        print("missing generated functions:", repr(missing), file=sys.stderr)
    for frame, name in oversized:
        print(f"frame {frame} > {ceiling}: {name}", file=sys.stderr)
    raise SystemExit(1)

mode30_all = collect(mode30_path)
mode0_all = collect(mode0_path)
if "kernel_main" not in mode30_all or "kernel_main" not in mode0_all:
    print("kernel_main frame is missing from Mode 30 or Mode 0", file=sys.stderr)
    raise SystemExit(1)
if mode30_all["kernel_main"] != mode0_all["kernel_main"]:
    print(
        "kernel_main frame drift: "
        f"Mode30={mode30_all['kernel_main']} Mode0={mode0_all['kernel_main']}",
        file=sys.stderr,
    )
    raise SystemExit(1)

largest = max((frame, name) for name, frame in frames.items())
print(
    "World receipt generated-frame ceiling: "
    f"PASS (largest={largest[0]} bytes, function={largest[1]}, ceiling={ceiling})"
)
print(
    "World receipt Mode0 kernel_main frame equality: "
    f"PASS ({mode30_all['kernel_main']} bytes)"
)
PY

python3 - "$BUILD_DIR/kernel.elf" "$FIXTURE" "$TIMEOUT_SECONDS" \
  "$EXPECTED_BYTES" "$EXPECTED_SHA256" <<'PY'
import hashlib
import os
import re
import selectors
import subprocess
import sys
import time

kernel, fixture_path, timeout_text, expected_bytes_text, expected_sha = sys.argv[1:]
timeout_seconds = float(timeout_text)
expected_bytes = int(expected_bytes_text)
expected_hex = open(fixture_path, "r", encoding="ascii").read().strip()
if (
    not expected_hex
    or len(expected_hex) % 2
    or re.fullmatch(r"[0-9a-f]+", expected_hex) is None
):
    print("World receipt fixture is not canonical lowercase even-length hex", file=sys.stderr)
    raise SystemExit(1)
expected = bytes.fromhex(expected_hex)
if len(expected) != expected_bytes:
    print(
        f"World receipt fixture is not the pinned {expected_bytes}-byte corpus",
        file=sys.stderr,
    )
    raise SystemExit(1)
fixture_sha = hashlib.sha256(expected).hexdigest()
if fixture_sha != expected_sha:
    print(
        f"World receipt fixture SHA mismatch: {fixture_sha} != {expected_sha}",
        file=sys.stderr,
    )
    raise SystemExit(1)

records = []
offset = 0
while offset < len(expected):
    if offset + 24 > len(expected):
        print("World receipt fixture has a truncated header", file=sys.stderr)
        raise SystemExit(1)
    total = int.from_bytes(expected[offset + 12 : offset + 16], "big")
    body_length = int.from_bytes(expected[offset + 16 : offset + 20], "big")
    if total < 120 or offset + total > len(expected) or total != 24 + body_length + 96:
        print("World receipt fixture has an invalid envelope length", file=sys.stderr)
        raise SystemExit(1)
    records.append(expected[offset : offset + total])
    offset += total
if offset != len(expected) or [len(record) for record in records] != [1634, 1605]:
    print("World receipt fixture does not contain the two pinned records", file=sys.stderr)
    raise SystemExit(1)

domain = b"OSTADIX/OWRECEIPT/V1\0"
preimages = []
for record in records:
    body_length = int.from_bytes(record[16:20], "big")
    body = record[24 : 24 + body_length]
    key_id = record[24 + body_length : 24 + body_length + 32]
    preimages.append(
        domain + record[8:10] + record[10:12] + record[16:20] + key_id + body
    )
expected_preimage = b"".join(preimages)
if [len(value) for value in preimages] != [1575, 1546]:
    print("World receipt derived preimage lengths are not pinned", file=sys.stderr)
    raise SystemExit(1)

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
completion = b"World receipt post-test timer: online\n"
completion_seen_at = None
survived_after_completion = False

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
            if completion_seen_at is None and completion in stdout.replace(b"\r\n", b"\n"):
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
hex_matches = re.findall(r"(?m)^WORLD_RECEIPT_V1_HEX=([0-9a-f]+)$", output)
sha_matches = re.findall(r"(?m)^WORLD_RECEIPT_V1_SHA256=([0-9a-f]{64})$", output)
preimage_matches = re.findall(
    r"(?m)^WORLD_RECEIPT_V1_PREIMAGE_HEX=([0-9a-f]+)$", output
)
actual_hex = hex_matches[0] if len(hex_matches) == 1 else ""
actual_sha = sha_matches[0] if len(sha_matches) == 1 else ""
actual_preimage = bytes.fromhex(preimage_matches[0]) if len(preimage_matches) == 1 else b""

required = [
    "O-core kernel: serial online\n",
    "page protections: W^X online\n",
    "page allocator: online\n",
    "address space: online\n",
    "World receipt Rust/.oc exact-byte convergence: PASS\n",
    "World receipt canonical signing-preimage convergence: PASS\n",
    "World receipt signature-envelope structural rejection: PASS\n",
    "World receipt identity/generation/commit-field binding: PASS\n",
    "World receipt descriptive capability boundary: PASS\n",
    "World receipt bounded malformed rejection: PASS\n",
    "World receipt v1 native smoke: PASS\n",
    "World receipt post-test timer: online\n",
]
missing = [marker for marker in required if marker not in output]
wrong_count = [marker for marker in required if output.count(marker) != 1]
positions = {marker: output.find(marker) for marker in required}
ordered = not missing and all(
    positions[left] < positions[right]
    for left, right in zip(required, required[1:])
)
timer_matches = list(re.finditer(r"(?m)^T$", output))
timer_valid = (
    len(timer_matches) == 1
    and not missing
    and positions[required[-2]] < timer_matches[0].start() < positions[required[-1]]
)
forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "Triple fault",
        "P30",
        "G0: PASS",
        "authority transfer: PASS",
        "network transport: PASS",
        "live negotiation: PASS",
        "Linux boot: PASS",
        "Plan 9 boot: PASS",
        "Ed25519 native verification: PASS",
        "hardware isolation: PASS",
    )
    if marker in transcript
]

valid = (
    len(hex_matches) == 1
    and actual_hex == expected_hex
    and len(sha_matches) == 1
    and actual_sha == expected_sha
    and len(preimage_matches) == 1
    and actual_preimage == expected_preimage
    and not missing
    and not wrong_count
    and ordered
    and timer_valid
    and not forbidden
    and survived_after_completion
)
if not valid:
    print("World receipt v1 QEMU smoke: FAIL", file=sys.stderr)
    if actual_hex and actual_hex != expected_hex:
        mismatch = next(
            (index for index, pair in enumerate(zip(actual_hex, expected_hex)) if pair[0] != pair[1]),
            min(len(actual_hex), len(expected_hex)),
        )
        print(f"native corpus mismatch at byte {mismatch // 2}", file=sys.stderr)
    if actual_sha and actual_sha != expected_sha:
        print(f"native SHA mismatch: {actual_sha} != {expected_sha}", file=sys.stderr)
    if actual_preimage and actual_preimage != expected_preimage:
        print("native canonical signing preimages differ", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if wrong_count:
        print("wrong marker count:", repr(wrong_count), file=sys.stderr)
    if not ordered:
        print("World receipt marker order is invalid", file=sys.stderr)
    if not timer_valid:
        print("standalone timer T ordering is invalid", file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not survived_after_completion:
        print("QEMU did not survive the post-completion window", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print(
    "World receipt exact corpus: "
    f"{len(expected)} bytes / {len(expected_hex)} lowercase hex digits"
)
print(f"World receipt corpus SHA-256: {expected_sha}")
print(
    "World receipt canonical preimages: "
    f"{len(expected_preimage)} bytes across {len(preimages)} records"
)
print("World receipt v1 QEMU smoke: PASS")
PY
