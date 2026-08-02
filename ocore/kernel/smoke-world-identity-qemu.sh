#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-world-identity}"
FIXTURE="$ROOT/tests/fixtures/world_identity_v1.hex"
TIMEOUT_SECONDS=30

for tool in cargo qemu-system-x86_64 python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the World identity smoke" >&2
    exit 127
  fi
done
if [[ ! -f "$FIXTURE" ]]; then
  echo "error: missing World identity byte oracle: $FIXTURE" >&2
  exit 1
fi

# The convergence marker is admitted only after the independent Rust producer
# passes and the native serial corpus is checked against its exact hex oracle.
cargo test --quiet --manifest-path "$ROOT/Cargo.toml" --test world_identity_wire
OCORE_PROBE_MODE=27 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

python3 - "$BUILD_DIR/kernel.elf" "$FIXTURE" "$TIMEOUT_SECONDS" <<'PY'
import os
import re
import selectors
import subprocess
import sys
import time

kernel, fixture_path, timeout_text = sys.argv[1:]
timeout_seconds = float(timeout_text)
expected_hex = open(fixture_path, "r", encoding="ascii").read().strip()
if not expected_hex or len(expected_hex) % 2 or re.fullmatch(r"[0-9a-f]+", expected_hex) is None:
    print("World identity fixture is not canonical lowercase even-length hex", file=sys.stderr)
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
completion = b"World identity post-test timer: online\n"
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
hex_matches = re.findall(r"(?m)^WORLD_IDENTITY_V1_HEX=([0-9a-f]+)$", output)
actual_hex = hex_matches[0] if len(hex_matches) == 1 else ""

required = [
    "O-core kernel: serial online\n",
    "page protections: W^X online\n",
    "page allocator: online\n",
    "address space: online\n",
    "World identity Rust/.oc exact-byte convergence: PASS\n",
    "World identity malformed/nonzero/stale rejection: PASS\n",
    "World identity v1 native smoke: PASS\n",
    "World identity post-test timer: online\n",
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
        "G0: PASS",
        "authority transfer: PASS",
        "general protocol: PASS",
    )
    if marker in transcript
]

hex_valid = len(hex_matches) == 1 and actual_hex == expected_hex
if not hex_valid and actual_hex:
    common = 0
    for common, (actual, expected) in enumerate(zip(actual_hex, expected_hex)):
        if actual != expected:
            break
    else:
        common = min(len(actual_hex), len(expected_hex))
    print(
        "World identity corpus mismatch: "
        f"hex_offset={common} byte_offset={common // 2} "
        f"native_digits={len(actual_hex)} fixture_digits={len(expected_hex)}",
        file=sys.stderr,
    )

if (
    not hex_valid
    or missing
    or wrong_count
    or not ordered
    or not timer_valid
    or forbidden
    or not survived_after_completion
):
    print("World identity v1 QEMU smoke: FAIL", file=sys.stderr)
    if len(hex_matches) != 1:
        print("expected exactly one native hex corpus", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if wrong_count:
        print("wrong marker count:", repr(wrong_count), file=sys.stderr)
    if not ordered:
        print("World identity marker order is invalid", file=sys.stderr)
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
    "World identity exact corpus: "
    f"{len(expected_hex) // 2} bytes / {len(expected_hex)} lowercase hex digits"
)
print("World identity v1 QEMU smoke: PASS")
PY
