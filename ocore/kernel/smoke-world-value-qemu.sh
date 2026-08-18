#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-world-value}"
FIXTURE="$ROOT/tests/fixtures/world_value_v1.hex"
TIMEOUT_SECONDS=30
EXPECTED_BYTES=928
EXPECTED_SHA256="264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc"
MAX_NEW_FRAME_BYTES=8192

for tool in cargo qemu-system-x86_64 python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the World value smoke" >&2
    exit 127
  fi
done
if [[ ! -f "$FIXTURE" ]]; then
  echo "error: missing World value byte oracle: $FIXTURE" >&2
  exit 1
fi

# The independent hosted implementation owns the pinned fixture.  Native
# convergence is admitted only after both that oracle and the freestanding
# Mode-29 implementation pass in the same invocation.
cargo test --quiet --manifest-path "$ROOT/Cargo.toml" --package o-lang --test world_value
OCORE_PROBE_MODE=29 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

python3 - "$BUILD_DIR/kernel.s" "$MAX_NEW_FRAME_BYTES" <<'PY'
import re
import sys

assembly_path, ceiling_text = sys.argv[1:]
ceiling = int(ceiling_text)
lines = open(assembly_path, "r", encoding="utf-8").read().splitlines()
prefixes = (
    "_O_world__value__",
    "_O_world__value_codec__",
    "_O_world__sha256__",
    "_O_kernel__world_value_semantics__",
)
frames = {}
for index, line in enumerate(lines):
    label = line.strip()
    if not label.endswith(":") or not label.startswith(prefixes):
        continue
    name = label[:-1]
    frame = 0
    for candidate in lines[index + 1 : index + 8]:
        match = re.fullmatch(r"\s*sub rsp, ([0-9]+)", candidate)
        if match:
            frame = int(match.group(1))
            break
        if candidate and not candidate.startswith((" ", "\t")):
            break
    frames[name] = frame

required = [
    "_O_world__sha256__compute",
    "_O_world__value_codec__encode_record",
    "_O_world__value_codec__begin_node",
    "_O_world__value_codec__decode_record",
]
missing = [name for name in required if name not in frames]
oversized = sorted(
    ((frame, name) for name, frame in frames.items() if frame > ceiling),
    reverse=True,
)
if missing or oversized:
    print("World value generated-frame ceiling: FAIL", file=sys.stderr)
    if missing:
        print("missing generated functions:", repr(missing), file=sys.stderr)
    for frame, name in oversized:
        print(f"frame {frame} > {ceiling}: {name}", file=sys.stderr)
    raise SystemExit(1)
largest = max((frame, name) for name, frame in frames.items())
print(
    "World value generated-frame ceiling: "
    f"PASS (largest={largest[0]} bytes, function={largest[1]}, ceiling={ceiling})"
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
    print("World value fixture is not canonical lowercase even-length hex", file=sys.stderr)
    raise SystemExit(1)
expected = bytes.fromhex(expected_hex)
if len(expected) != expected_bytes:
    print(
        f"World value fixture is not the pinned {expected_bytes}-byte corpus",
        file=sys.stderr,
    )
    raise SystemExit(1)
fixture_sha = hashlib.sha256(expected).hexdigest()
if fixture_sha != expected_sha:
    print(
        f"World value fixture SHA mismatch: {fixture_sha} != {expected_sha}",
        file=sys.stderr,
    )
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
completion = b"World value post-test timer: online\n"
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
hex_matches = re.findall(r"(?m)^WORLD_VALUE_V1_HEX=([0-9a-f]+)$", output)
sha_matches = re.findall(r"(?m)^WORLD_VALUE_V1_SHA256=([0-9a-f]{64})$", output)
actual_hex = hex_matches[0] if len(hex_matches) == 1 else ""
actual_sha = sha_matches[0] if len(sha_matches) == 1 else ""

required = [
    "O-core kernel: serial online\n",
    "page protections: W^X online\n",
    "page allocator: online\n",
    "address space: online\n",
    "World value Rust/.oc exact-byte convergence: PASS\n",
    "World value canonical SHA-256 convergence: PASS\n",
    "World value core/extension canonical round-trip: PASS\n",
    "World value authority/capsule rejection: PASS\n",
    "World value bounded malformed rejection: PASS\n",
    "World value v1 native smoke: PASS\n",
    "World value post-test timer: online\n",
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
        "P29",
        "G0: PASS",
        "authority transfer: PASS",
        "network transport: PASS",
        "live negotiation: PASS",
        "Linux boot: PASS",
        "Plan 9 boot: PASS",
        "hardware isolation: PASS",
    )
    if marker in transcript
]

hex_valid = len(hex_matches) == 1 and actual_hex == expected_hex
sha_valid = len(sha_matches) == 1 and actual_sha == expected_sha
if not hex_valid and actual_hex:
    common = 0
    for common, (actual, expected_digit) in enumerate(zip(actual_hex, expected_hex)):
        if actual != expected_digit:
            break
    else:
        common = min(len(actual_hex), len(expected_hex))
    print(
        "World value corpus mismatch: "
        f"hex_offset={common} byte_offset={common // 2} "
        f"native_digits={len(actual_hex)} fixture_digits={len(expected_hex)}",
        file=sys.stderr,
    )

if (
    not hex_valid
    or not sha_valid
    or missing
    or wrong_count
    or not ordered
    or not timer_valid
    or forbidden
    or not survived_after_completion
):
    print("World value v1 QEMU smoke: FAIL", file=sys.stderr)
    if len(hex_matches) != 1:
        print("expected exactly one native hex corpus", file=sys.stderr)
    if len(sha_matches) != 1:
        print("expected exactly one native SHA-256", file=sys.stderr)
    if not sha_valid and actual_sha:
        print(f"native SHA mismatch: {actual_sha} != {expected_sha}", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if wrong_count:
        print("wrong marker count:", repr(wrong_count), file=sys.stderr)
    if not ordered:
        print("World value marker order is invalid", file=sys.stderr)
    if not timer_valid:
        print("exactly one standalone timer T must follow native completion", file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not survived_after_completion:
        print("QEMU did not survive the post-completion window", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", error, file=sys.stderr)
    raise SystemExit(1)

print(output, end="")
print(
    "World value exact corpus: "
    f"{len(expected)} bytes / {len(expected_hex)} lowercase hex digits"
)
print(f"World value corpus SHA-256: {expected_sha}")
print("World value v1 QEMU smoke: PASS")
PY
