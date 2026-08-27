#!/usr/bin/env bash
# Rebuild the ISO twice, prove byte identity, then boot the exact first image
# through OVMF/QEMU TCG without -kernel, writable media, or networking.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SMOKE_ROOT="${OSTADIX_ISO_SMOKE_ROOT:-$ROOT/target/ostadix-iso-smoke/x86_64}"
FIRST="$SMOKE_ROOT/first.iso"
SECOND="$SMOKE_ROOT/second.iso"
BUILD_SCRIPT="${OSTADIX_ISO_BUILD_SCRIPT:-$ROOT/ocore/kernel/build-x86_64-uefi-iso.sh}"
INSPECTOR="${OSTADIX_ISO_INSPECTOR:-$ROOT/scripts/ostadix_boot_iso.py}"
QEMU_BIN="${OCORE_QEMU_BIN:-qemu-system-x86_64}"
TIMEOUT="${OSTADIX_ISO_TIMEOUT_SECONDS:-12}"
RECORD_DIR=""

cleanup() {
  if [[ -n "$RECORD_DIR" ]]; then
    rm -rf -- "$RECORD_DIR"
  fi
}
trap cleanup EXIT INT TERM

if [[ $# -ne 0 ]]; then
  echo "usage: smoke-x86_64-uefi-iso-qemu.sh" >&2
  exit 2
fi
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  printf 'error: QEMU executable is unavailable: %s\n' "$QEMU_BIN" >&2
  exit 127
fi
if [[ ! "$TIMEOUT" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  echo "error: OSTADIX_ISO_TIMEOUT_SECONDS must be a positive number" >&2
  exit 2
fi
if ! python3 - "$TIMEOUT" <<'PY'
import sys

raise SystemExit(0 if float(sys.argv[1]) > 0 else 1)
PY
then
  echo "error: OSTADIX_ISO_TIMEOUT_SECONDS must be greater than zero" >&2
  exit 2
fi
for script in "$BUILD_SCRIPT" "$INSPECTOR"; do
  if [[ -L "$script" || ! -f "$script" || ! -x "$script" ]]; then
    printf 'error: required OSTADIX ISO script is not an executable non-symlink file: %s\n' \
      "$script" >&2
    exit 1
  fi
done

if [[ -L "$SMOKE_ROOT" ]]; then
  printf 'error: refusing OSTADIX ISO smoke directory symlink: %s\n' "$SMOKE_ROOT" >&2
  exit 1
fi
mkdir -p "$SMOKE_ROOT"
if [[ -L "$SMOKE_ROOT" || ! -d "$SMOKE_ROOT" ]]; then
  echo "error: OSTADIX ISO smoke root is not a non-symlink directory" >&2
  exit 1
fi
RECORD_DIR="$(mktemp -d "$SMOKE_ROOT/.smoke-records.XXXXXX")"
FIRST_BUILD_RECORD="$RECORD_DIR/first-build.txt"
SECOND_BUILD_RECORD="$RECORD_DIR/second-build.txt"
FIRST_INSPECT_RECORD="$RECORD_DIR/first-inspect.json"
SECOND_INSPECT_RECORD="$RECORD_DIR/second-inspect.json"

OSTADIX_ISO_ROOT="$SMOKE_ROOT/build-one" \
  OCORE_ISO_KERNEL_BUILD_DIR="$SMOKE_ROOT/kernel-one" \
  "$BUILD_SCRIPT" "$FIRST" >"$FIRST_BUILD_RECORD"
OSTADIX_ISO_ROOT="$SMOKE_ROOT/build-two" \
  OCORE_ISO_KERNEL_BUILD_DIR="$SMOKE_ROOT/kernel-two" \
  "$BUILD_SCRIPT" "$SECOND" >"$SECOND_BUILD_RECORD"
if ! cmp -s "$FIRST" "$SECOND"; then
  echo "error: OSTADIX x86_64 UEFI ISO rebuild is not byte-identical" >&2
  exit 1
fi

"$INSPECTOR" inspect "$FIRST" | tee "$FIRST_INSPECT_RECORD"
"$INSPECTOR" inspect "$SECOND" >"$SECOND_INSPECT_RECORD"
if ! cmp -s "$FIRST_INSPECT_RECORD" "$SECOND_INSPECT_RECORD"; then
  echo "error: byte-identical OSTADIX ISOs produced different inspection records" >&2
  exit 1
fi
python3 - "$FIRST_BUILD_RECORD" "$SECOND_BUILD_RECORD" "$FIRST_INSPECT_RECORD" <<'PY'
import json
from pathlib import Path
import re
import sys

first_record_path, second_record_path, inspect_path = map(Path, sys.argv[1:])


def record(path: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if ": " in line:
            key, value = line.split(": ", 1)
            result[key] = value
    return result


first = record(first_record_path)
second = record(second_record_path)
for key in (
    "iso-sha256",
    "kernel-sha256",
    "efi-boot-image-sha256",
    "efi-bootloader-sha256",
):
    if re.fullmatch(r"[0-9a-f]{64}", first.get(key, "")) is None:
        raise SystemExit(f"error: first ISO build has invalid or missing {key}")
    if second.get(key) != first[key]:
        raise SystemExit(f"error: deterministic ISO rebuild changed {key}")
metadata = json.loads(inspect_path.read_text(encoding="utf-8"))
if metadata.get("schema") != "ostadix.boot-iso/v1":
    raise SystemExit("error: ISO inspector returned the wrong schema")
if metadata.get("sha256") != first["iso-sha256"]:
    raise SystemExit("error: build record and strict ISO inspection disagree")
if metadata.get("el_torito_platform_id") != 239:
    raise SystemExit("error: strict ISO inspection did not admit UEFI platform 0xef")
if metadata.get("el_torito_media_type") != 0:
    raise SystemExit("error: strict ISO inspection did not admit no-emulation media")
print("OSTADIX x86_64 UEFI ISO strict structure: PASS")
PY

# shellcheck source=resolve-x86_64-ovmf-code.sh
source "$ROOT/ocore/kernel/resolve-x86_64-ovmf-code.sh"
OSTADIX_OVMF_CODE="$(resolve_ostadix_x86_64_ovmf_code "$QEMU_BIN")"

python3 - "$QEMU_BIN" "$OSTADIX_OVMF_CODE" "$FIRST" "$TIMEOUT" "$INSPECTOR" \
  "$FIRST_INSPECT_RECORD" <<'PY'
import importlib.util
import json
import os
from pathlib import Path
import selectors
import stat
import subprocess
import sys
import time

(
    qemu,
    firmware_text,
    media_text,
    timeout_text,
    inspector_text,
    expected_metadata_text,
) = sys.argv[1:]
media = Path(media_text)
expected_metadata = json.loads(
    Path(expected_metadata_text).read_text(encoding="utf-8")
)
inspector_spec = importlib.util.spec_from_file_location("ostadix_boot_iso", inspector_text)
if inspector_spec is None or inspector_spec.loader is None:
    raise SystemExit("error: cannot load the OSTADIX ISO inspector")
inspector = importlib.util.module_from_spec(inspector_spec)
inspector_spec.loader.exec_module(inspector)


def identity(value: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        value.st_dev,
        value.st_ino,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


media_descriptor = -1
firmware_descriptor = -1
process: subprocess.Popen[bytes] | None = None
selector: selectors.BaseSelector | None = None
stdout_capture = bytearray()
stderr_capture = bytearray()
capture_error = ""
sustained_liveness = False
try:
    media_descriptor = inspector._open_pinned_regular(
        media, nofollow=True, require_no_write_bits=True
    )
    firmware_descriptor = inspector._open_pinned_regular(
        Path(firmware_text), nofollow=False
    )
    before = os.fstat(media_descriptor)
    metadata = inspector.inspect_descriptor(media_descriptor, str(media))
    if metadata != expected_metadata:
        raise SystemExit(
            "error: descriptor-pinned smoke ISO differs from private inspection"
        )
    before_digest = metadata["sha256"]

    media_fd_path = f"/dev/fd/{media_descriptor}"
    firmware_fd_path = f"/dev/fd/{firmware_descriptor}"
    if not os.path.exists(media_fd_path) or not os.path.exists(firmware_fd_path):
        raise SystemExit("error: this host does not expose inherited descriptors via /dev/fd")
    command = [
        qemu,
        "-accel", "tcg",
        "-machine", "q35",
        "-m", "128M",
        "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={firmware_fd_path}",
        "-drive", f"if=ide,index=2,media=cdrom,format=raw,readonly=on,file={media_fd_path}",
        "-boot", "order=d,strict=on",
        "-nodefaults",
        "-nic", "none",
        "-display", "none",
        "-serial", "stdio",
        "-monitor", "none",
        "-no-reboot",
        "-no-shutdown",
    ]
    if (
        "-kernel" in command
        or command[command.index("-nic") + 1] != "none"
        or media_text in command
        or firmware_text in command
    ):
        raise SystemExit(
            "error: ISO smoke command escaped its pinned-firmware/no-network boundary"
        )

    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        pass_fds=(media_descriptor, firmware_descriptor),
    )
    assert process.stdout is not None and process.stderr is not None
    selector = selectors.DefaultSelector()
    for stream, name in ((process.stdout, "stdout"), (process.stderr, "stderr")):
        os.set_blocking(stream.fileno(), False)
        selector.register(stream, selectors.EVENT_READ, name)

    timeout = float(timeout_text)
    start = time.monotonic()
    deadline = start + timeout
    liveness_window = min(1.0, timeout / 4.0)
    completion_at: float | None = None
    completion_marker = b"CPL3 heartbeat: online"
    maximum_capture = 4 * 1024 * 1024
    while True:
        now = time.monotonic()
        if completion_at is not None and now >= completion_at + liveness_window:
            sustained_liveness = process.poll() is None
            break
        if now >= deadline or process.poll() is not None:
            break
        observation_deadline = deadline
        if completion_at is not None:
            observation_deadline = min(deadline, completion_at + liveness_window)
        events = selector.select(max(0.0, min(0.1, observation_deadline - now)))
        for key, _mask in events:
            try:
                chunk = os.read(key.fileobj.fileno(), 65536)
            except BlockingIOError:
                continue
            if not chunk:
                selector.unregister(key.fileobj)
                continue
            target = stdout_capture if key.data == "stdout" else stderr_capture
            if len(target) + len(chunk) > maximum_capture:
                capture_error = f"{key.data} exceeded the bounded capture limit"
                break
            target.extend(chunk)
        if capture_error:
            break
        if completion_at is None and completion_marker in stdout_capture:
            completion_at = time.monotonic()

    if process.poll() is None:
        process.terminate()
    try:
        tail_stdout, tail_stderr = process.communicate(timeout=1.0)
    except subprocess.TimeoutExpired:
        process.kill()
        tail_stdout, tail_stderr = process.communicate()
    if len(stdout_capture) + len(tail_stdout) <= 4 * 1024 * 1024:
        stdout_capture.extend(tail_stdout)
    else:
        capture_error = capture_error or "stdout exceeded the bounded capture limit"
    if len(stderr_capture) + len(tail_stderr) <= 4 * 1024 * 1024:
        stderr_capture.extend(tail_stderr)
    else:
        capture_error = capture_error or "stderr exceeded the bounded capture limit"

    after = os.fstat(media_descriptor)
    after_metadata = inspector.inspect_descriptor(media_descriptor, str(media))
    if identity(before) != identity(after) or metadata != after_metadata:
        raise SystemExit("error: exact pinned read-only ISO changed during QEMU boot")

    output = stdout_capture.decode("utf-8", "replace")
    diagnostic = stderr_capture.decode("utf-8", "replace")
    issues = inspector.validate_smoke_output(output, sustained_liveness)
    if capture_error:
        issues.append(capture_error)
    if issues:
        print(f"UEFI ISO smoke failed; issues={issues!r}", file=sys.stderr)
        print("stdout:", output, file=sys.stderr)
        print("stderr:", diagnostic, file=sys.stderr)
        raise SystemExit(1)
    print(output, end="")
    print(f"OSTADIX x86_64 UEFI ISO exact sha256={before_digest}")
    print(
        f"OSTADIX x86_64 UEFI ISO post-heartbeat liveness={liveness_window:.3f}s"
    )
    print("OSTADIX x86_64 UEFI ISO deterministic rebuild: PASS")
    print("OSTADIX x86_64 UEFI ISO boot: PASS")
finally:
    if selector is not None:
        selector.close()
    if process is not None and process.poll() is None:
        process.kill()
        process.wait()
    if firmware_descriptor >= 0:
        os.close(firmware_descriptor)
    if media_descriptor >= 0:
        os.close(media_descriptor)
PY
