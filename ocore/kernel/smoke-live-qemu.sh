#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m5-native}"
TIMEOUT_SECONDS=30
DIGEST="88c0db7b97f74b091407731a0be8d9bf25c86f0ca03aaf8040b2b7c007cb9fed"
IMAGE="$ROOT/target/ocore-m5-artifacts/images/root-${DIGEST}.ovfs"

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "error: qemu-system-x86_64 is not installed" >&2
  exit 127
fi

OCORE_PROBE_MODE=16 OCORE_BUILD_DIR="$BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null

if [[ ! -f "$IMAGE" ]] \
  || [[ "$(wc -c < "$IMAGE" | tr -d ' ')" != 62056 ]] \
  || [[ "$(shasum -a 256 "$IMAGE" | awk '{print $1}')" != "$DIGEST" ]]; then
  echo "error: M5 embedded OVFS artifact identity is not canonical" >&2
  exit 1
fi

# All four services must arrive only as content-addressed OVFS payload bytes.
# A source-module symbol in the kernel would bypass the native ELF loader gate.
if ! command -v nm >/dev/null 2>&1; then
  echo "error: nm is required to prove M5 services are not kernel-linked" >&2
  exit 127
fi
if ! KERNEL_SYMBOLS="$(nm "$BUILD_DIR/kernel.elf" 2>/dev/null)"; then
  echo "error: nm could not inspect the M5 kernel ELF" >&2
  exit 1
fi
if grep -Eq 'm5_(init|supervisor|pkgd|repl).*_start' <<<"$KERNEL_SYMBOLS"; then
  echo "error: M5 service was linked as kernel code" >&2
  exit 1
fi

python3 - "$BUILD_DIR/kernel.elf" "$TIMEOUT_SECONDS" "$DIGEST" <<'PY'
import os
import re
import selectors
import subprocess
import sys
import time

kernel = sys.argv[1]
timeout_seconds = float(sys.argv[2])
digest = sys.argv[3]
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
deadline = time.monotonic() + timeout_seconds
completion_seen_at = None
survived_after_completion = False

prompt = b"o> "
service_markers = [
    b"M5 init service ELF: online\n",
    b"M5 supervisor service ELF: online\n",
    b"M5 pkgd service ELF: online\n",
    b"M5 repl service ELF: online\n",
]
rejected = b"M5 serial command: rejected\n"
install_ok = b"M5 serial package install: PASS\n"
activate_ok = b"M5 serial package activation: PASS\n"
completion_bytes = b"M5 post-lifecycle timer: online\n"

malformed_sent = False
install_sent = False
activate_sent = False
input_failure = None

def send_line(line):
    global input_failure
    try:
        if process.poll() is not None:
            input_failure = "QEMU exited before an interactive command was sent"
            return False
        process.stdin.write(line)
        process.stdin.flush()
        return True
    except (BrokenPipeError, OSError) as error:
        input_failure = f"serial input failed: {error}"
        return False

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

    snapshot = bytes(stdout)
    services_online = all(marker in snapshot for marker in service_markers)
    if (
        not malformed_sent
        and services_online
        and snapshot.count(prompt) >= 1
    ):
        malformed_sent = send_line(b"install malformed 5 1\n")
    if (
        malformed_sent
        and not install_sent
        and rejected in snapshot
        and snapshot.count(prompt) >= 2
    ):
        if install_ok in snapshot or activate_ok in snapshot:
            input_failure = "malformed command published package state"
            break
        install_sent = send_line(
            f"install {digest} 5 1\n".encode("ascii")
        )
    if (
        install_sent
        and not activate_sent
        and install_ok in snapshot
        and snapshot.count(prompt) >= 3
    ):
        activate_sent = send_line(f"activate {digest}\n".encode("ascii"))
    if input_failure is not None:
        break

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

startup = "O-core kernel: serial online\n"
preflight = [
    "M5 OVFS image import: PASS\n",
    "M5 four native ELF loads: PASS\n",
    "M5 loaded address-space W^X: PASS\n",
    "M5 isolated service CSpaces: PASS\n",
    "M5 native control plane: armed\n",
]
services = [marker.decode("ascii") for marker in service_markers]
interaction = [
    rejected.decode("ascii"),
    install_ok.decode("ascii"),
    activate_ok.decode("ascii"),
]
fault_sequence = [
    "M5 pkgd service ELF: deliberate fault\n",
    "M5 pkgd fault: contained\n",
]
completion = [
    "M5 package activation state: PASS\n",
    "M5 pkgd fault containment: PASS\n",
    "M5 unrelated services survived: PASS\n",
    "M5 stale generation rejection: PASS\n",
    "M5 pkgd restart: armed\n",
    "M5 pkgd service ELF: restarted\n",
    "M5 pkgd restart generation: PASS\n",
    "M5 supervisor crash/restart: PASS\n",
    "M5 control plane deactivated: PASS\n",
    "M5 namespace transaction: PASS\n",
    "M5 resources reclaimed: PASS\n",
    "M5 native live system: PASS\n",
    "M5 post-lifecycle timer: online\n",
]
required = [
    startup,
    *preflight,
    *services,
    *interaction,
    *fault_sequence,
    *completion,
]
missing = [marker for marker in required if marker not in output]
duplicated = [marker for marker in required if output.count(marker) != 1]
positions = {marker: output.find(marker) for marker in required}
prompts = [match.start() for match in re.finditer(re.escape("o> "), output)]
timer_matches = list(re.finditer(r"(?m)^T$", output))

phase_order_valid = not missing and all(
    positions[before] < positions[after]
    for before, after in [
        (startup, preflight[0]),
        *zip(preflight, preflight[1:]),
        *((preflight[-1], service) for service in services),
        *((service, interaction[1]) for service in services),
        (services[3], interaction[0]),
        (interaction[0], interaction[1]),
        (interaction[1], interaction[2]),
        (interaction[2], fault_sequence[0]),
        *zip(fault_sequence, fault_sequence[1:]),
        (fault_sequence[-1], completion[0]),
        *zip(completion, completion[1:]),
    ]
)

prompt_order_valid = (
    len(prompts) == 3
    and not missing
    and positions[services[3]] < prompts[0]
    and prompts[0] < positions[interaction[0]] < prompts[1]
    and prompts[1] < positions[interaction[1]] < prompts[2]
    and prompts[2] < positions[interaction[2]]
)

timer_phase_valid = (
    len(timer_matches) == 1
    and not missing
    and positions[preflight[-1]] < timer_matches[0].start()
    and timer_matches[0].start() < min(positions[service] for service in services)
)

forbidden = [
    marker
    for marker in (
        "M02 KERNEL FAULT",
        "M02 unexpected fault",
        "M4 ISOLATION LEAK",
        "KERNEL POINTER LEAKED",
        "invariant violation",
        "M4 native loader/VFS: PASS",
        "M3 native live substrate: PASS",
        "Linux personality: PASS",
    )
    if marker in output
]

commands_complete = malformed_sent and install_sent and activate_sent
if (
    missing
    or duplicated
    or forbidden
    or not phase_order_valid
    or not prompt_order_valid
    or not timer_phase_valid
    or not commands_complete
    or input_failure is not None
    or not survived_after_completion
):
    print("M5 native live smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if duplicated:
        print("wrong marker count:", repr(duplicated), file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if not phase_order_valid:
        print("M5 causal phase order is invalid", file=sys.stderr)
    if not prompt_order_valid:
        print("M5 prompt/command order is invalid", file=sys.stderr)
    if not timer_phase_valid:
        print(
            "exactly one standalone startup T must precede all loaded ELFs",
            file=sys.stderr,
        )
    if not commands_complete:
        print("not all three serial commands were sent", file=sys.stderr)
    if input_failure is not None:
        print(input_failure, file=sys.stderr)
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
print("M5 native live smoke: PASS")
PY
