#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-world-project-receipt}"
TIMEOUT_SECONDS="${OCORE_WORLD_PROJECT_RECEIPT_TIMEOUT:-30}"
MAX_RECORD_BYTES=4096
MAX_NEW_FRAME_BYTES=8192

if (( $# != 2 )); then
  echo "usage: $0 RECORD_HEX_FILE EXPECTED_SEMANTIC_SHA256" >&2
  exit 2
fi
RECORD_HEX_FILE="$1"
EXPECTED_SEMANTIC_SHA256="$2"

for tool in cargo qemu-system-x86_64 python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the World project receipt smoke" >&2
    exit 127
  fi
done
if [[ ! -f "$RECORD_HEX_FILE" ]]; then
  echo "error: missing World project receipt hex record: $RECORD_HEX_FILE" >&2
  exit 1
fi
if [[ ! "$EXPECTED_SEMANTIC_SHA256" =~ ^[0-9a-f]{64}$ ]]; then
  echo "error: expected semantic digest must be 64 lowercase hex digits" >&2
  exit 2
fi

# Preflight the caller-supplied vector and independently derive the exact
# domain-separated semantic digest expected from native Mode 32.
python3 - "$RECORD_HEX_FILE" "$EXPECTED_SEMANTIC_SHA256" \
  "$MAX_RECORD_BYTES" <<'PY'
import hashlib
import re
import sys

path, expected, maximum_text = sys.argv[1:]
maximum = int(maximum_text)
text = open(path, "r", encoding="ascii").read()
if text.endswith("\n"):
    text = text[:-1]
if not text or re.fullmatch(r"[0-9a-f]+", text) is None or len(text) % 2:
    print("World project receipt input is not one canonical lowercase hex line", file=sys.stderr)
    raise SystemExit(1)
record = bytes.fromhex(text)
if len(record) < 120 or len(record) > maximum:
    print(
        f"World project receipt is {len(record)} bytes; Mode 32 accepts 120..{maximum}",
        file=sys.stderr,
    )
    raise SystemExit(1)
if record[:8] != b"OWRCPT\0\0":
    print("World project receipt has the wrong envelope magic", file=sys.stderr)
    raise SystemExit(1)
total = int.from_bytes(record[12:16], "big")
body_length = int.from_bytes(record[16:20], "big")
if total != len(record) or total != 24 + body_length + 96:
    print("World project receipt envelope lengths are inconsistent", file=sys.stderr)
    raise SystemExit(1)
body = record[24 : 24 + body_length]
domain = b"OSTADIX/PROJECT-RECEIPT-SEMANTICS/V1\0"
actual = hashlib.sha256(domain + body_length.to_bytes(4, "big") + body).hexdigest()
if actual != expected:
    print(
        f"World project receipt semantic digest mismatch: derived {actual}, expected {expected}",
        file=sys.stderr,
    )
    raise SystemExit(1)
print(
    "World project receipt semantic digest preflight: "
    f"PASS ({len(record)} bytes, body={body_length}, sha256={actual})"
)
PY

OCORE_PROBE_MODE=32 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

# Keep large parser carriers in caller-owned static storage. A generated stack
# frame above this bounded ceiling is a regression even if the QEMU run passes.
python3 - "$BUILD_DIR/kernel.s" "$MAX_NEW_FRAME_BYTES" <<'PY'
import re
import sys

path, ceiling_text = sys.argv[1:]
ceiling = int(ceiling_text)
lines = open(path, "r", encoding="utf-8").read().splitlines()
frames = {}
prefixes = (
    "_O_world__identity__",
    "_O_world__receipt__",
    "_O_world__receipt_codec__",
    "_O_world__value__",
    "_O_world__value_codec__",
    "_O_world__sha256__",
    "_O_kernel__world_project_receipt_semantics__",
)
for index, line in enumerate(lines):
    label = line.strip()
    if not label.endswith(":"):
        continue
    name = label[:-1]
    if not name.startswith(prefixes):
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

required = [
    "_O_world__receipt__validate_unsigned_body",
    "_O_world__receipt__validated_terminal_and_commit",
    "_O_world__receipt_codec__decode_record",
    "_O_world__receipt_codec__build_validated_signing_preimage",
    "_O_kernel__world_project_receipt_semantics__read_canonical_hex_line",
    "_O_kernel__world_project_receipt_semantics__semantic_digest",
]
missing = [name for name in required if name not in frames]
oversized = sorted(
    ((frame, name) for name, frame in frames.items() if frame > ceiling),
    reverse=True,
)
if missing or oversized:
    print("World project receipt generated-frame ceiling: FAIL", file=sys.stderr)
    if missing:
        print("missing generated functions:", repr(missing), file=sys.stderr)
    for frame, name in oversized:
        print(f"frame {frame} > {ceiling}: {name}", file=sys.stderr)
    raise SystemExit(1)
largest = max((frame, name) for name, frame in frames.items())
print(
    "World project receipt generated-frame ceiling: "
    f"PASS (largest={largest[0]} bytes, function={largest[1]}, ceiling={ceiling})"
)
PY

python3 - "$BUILD_DIR/kernel.elf" "$RECORD_HEX_FILE" \
  "$EXPECTED_SEMANTIC_SHA256" "$TIMEOUT_SECONDS" <<'PY'
import os
import re
import selectors
import subprocess
import sys
import time

kernel, record_path, expected_digest, timeout_text = sys.argv[1:]
timeout_seconds = float(timeout_text)
record_hex = open(record_path, "r", encoding="ascii").read()
if record_hex.endswith("\n"):
    record_hex = record_hex[:-1]
record = bytes.fromhex(record_hex)

ready = "World project receipt native probe: ready\n"
rejected = "World project receipt native probe: REJECT\n"
post_tick = "World project receipt post-test timer: online\n"

def run_case(payload_hex, success):
    command = [
        "qemu-system-x86_64",
        "-machine", "q35",
        "-m", "128M",
        "-kernel", kernel,
        "-display", "none",
        "-monitor", "none",
        "-serial", "stdio",
        "-no-reboot",
        "-no-shutdown",
    ]
    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=0,
    )
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    stdout = bytearray()
    stderr = bytearray()
    sent = False
    survived = False
    deadline = time.monotonic() + timeout_seconds
    target = post_tick if success else rejected
    try:
        while time.monotonic() < deadline:
            for key, _ in selector.select(timeout=0.1):
                chunk = os.read(key.fileobj.fileno(), 4096)
                if not chunk:
                    selector.unregister(key.fileobj)
                elif key.data == "stdout":
                    stdout.extend(chunk)
                else:
                    stderr.extend(chunk)
            normalized = stdout.decode("utf-8", "replace").replace("\r\n", "\n")
            if not sent and ready in normalized:
                process.stdin.write(payload_hex.encode("ascii") + b"\n")
                process.stdin.flush()
                sent = True
            if sent and (target in normalized or (success and rejected in normalized)):
                survived = process.poll() is None
                break
            if process.poll() is not None:
                break
    finally:
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
    return output, error, sent, survived

output, error, sent, survived = run_case(record_hex, True)
transcript = output + "\n" + error
required = [
    "O-core kernel: serial online\n",
    "page protections: W^X online\n",
    "page allocator: online\n",
    "address space: online\n",
    ready,
    "World project receipt canonical lowercase hex: PASS\n",
    "World project receipt full native decode: PASS\n",
    "World project receipt exact canonical reencode: PASS\n",
    "World project receipt validated signing preimage: PASS\n",
    "World project receipt uncommitted fence: PASS\n",
    "World project receipt semantic structure SHA-256: PASS\n",
    "World project receipt native canonical/semantic comparison: PASS\n",
    "World project receipt stale tag reset: PASS\n",
    "World project receipt boundary: canonical structure only; no native execution or Ed25519 verification\n",
    "World project receipt native probe: PASS\n",
    post_tick,
]
missing = [marker for marker in required if marker not in output]
wrong_count = [marker for marker in required if output.count(marker) != 1]
positions = [output.find(marker) for marker in required]
ordered = not missing and positions == sorted(positions)
digests = re.findall(
    r"(?m)^WORLD_PROJECT_RECEIPT_SEMANTIC_SHA256=([0-9a-f]{64})$",
    output,
)
forbidden = [
    marker
    for marker in (
        "Ed25519 native verification: PASS",
        "native project execution: PASS",
        "Acceptance A: PASS",
        "G1: PASS",
        "Governor commitment: PASS",
        "hardware isolation: PASS",
        "Linux boot: PASS",
    )
    if marker in transcript
]
valid = (
    sent
    and survived
    and not missing
    and not wrong_count
    and ordered
    and digests == [expected_digest]
    and rejected not in output
    and not forbidden
)
if not valid:
    print("World project receipt native semantic comparison: FAIL", file=sys.stderr)
    if not sent:
        print("record was not sent after the native ready marker", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if wrong_count:
        print("wrong marker count:", repr(wrong_count), file=sys.stderr)
    if not ordered:
        print("marker ordering is invalid", file=sys.stderr)
    if digests != [expected_digest]:
        print("semantic digest output:", repr(digests), file=sys.stderr)
    if forbidden:
        print("forbidden overclaims:", repr(forbidden), file=sys.stderr)
    if not survived:
        print("QEMU did not remain live through the post-test tick", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

# A canonical lowercase line with a nonzero reserved envelope byte must reach
# the native parser and fail closed before any decode/reencode/digest claim.
malformed = bytearray(record)
malformed[20] = 1
bad_output, bad_error, bad_sent, bad_survived = run_case(malformed.hex(), False)
bad_transcript = bad_output + "\n" + bad_error
bad_forbidden = [
    marker
    for marker in (
        "World project receipt full native decode: PASS",
        "World project receipt exact canonical reencode: PASS",
        "WORLD_PROJECT_RECEIPT_SEMANTIC_SHA256=",
        "World project receipt native probe: PASS",
    )
    if marker in bad_transcript
]
if (
    not bad_sent
    or not bad_survived
    or bad_output.count(ready) != 1
    or bad_output.count(rejected) != 1
    or bad_forbidden
):
    print("World project receipt malformed native rejection: FAIL", file=sys.stderr)
    if bad_forbidden:
        print("forbidden malformed-case markers:", repr(bad_forbidden), file=sys.stderr)
    print("stdout:", bad_output, file=sys.stderr)
    print("stderr:", bad_error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print("World project receipt malformed native rejection: PASS")
print(
    "World project receipt native semantic comparison: PASS "
    "(canonical structure only; no Ed25519 verification or native project execution)"
)
PY
