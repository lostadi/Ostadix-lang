#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SMOKE_ROOT="${OSTADIX_BOOT_INFO_SMOKE_ROOT:-$ROOT/target/ostadix-boot-info-smoke}"
QEMU_BIN="${OCORE_QEMU_BIN:-qemu-system-x86_64}"
TIMEOUT="${OSTADIX_BOOT_INFO_TIMEOUT_SECONDS:-10}"
FIRST="$SMOKE_ROOT/first.img"
SECOND="$SMOKE_ROOT/second.img"
MODE0="$SMOKE_ROOT/challenged-mode0.img"
MODE0_TRANSCRIPT="$SMOKE_ROOT/challenged-mode0.serial"

if [[ $# -ne 0 ]]; then
  echo "usage: smoke-x86_64-boot-info-qemu.sh" >&2
  exit 2
fi
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  printf 'error: QEMU executable is unavailable: %s\n' "$QEMU_BIN" >&2
  exit 127
fi
if [[ -z "${OSTADIX_BOOT_CHALLENGE:-}" ]]; then
  OSTADIX_BOOT_CHALLENGE="$(python3 - <<'PY'
import secrets
print(secrets.token_hex(32))
PY
)"
fi
if [[ ! "$OSTADIX_BOOT_CHALLENGE" =~ ^[0-9a-f]{64}$ ]]; then
  echo "error: OSTADIX_BOOT_CHALLENGE must be exactly 64 lowercase hexadecimal digits" >&2
  exit 2
fi
SOURCE_COMMIT="$(git -C "$ROOT" rev-parse --verify 'HEAD^{commit}')"
if [[ ! "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: BootInfo smoke requires a 40-hex Git source commit" >&2
  exit 1
fi

if [[ -z "${OSTADIX_OVMF_CODE:-}" ]]; then
  for candidate in \
    /opt/homebrew/opt/qemu/share/qemu/edk2-x86_64-code.fd \
    /usr/local/opt/qemu/share/qemu/edk2-x86_64-code.fd \
    /usr/share/OVMF/OVMF_CODE.fd \
    /usr/share/edk2/x64/OVMF_CODE.fd; do
    if [[ -f "$candidate" ]]; then
      OSTADIX_OVMF_CODE="$candidate"
      break
    fi
  done
fi
if [[ -z "${OSTADIX_OVMF_CODE:-}" || ! -f "$OSTADIX_OVMF_CODE" ]]; then
  echo "error: UEFI firmware not found; set OSTADIX_OVMF_CODE" >&2
  exit 127
fi

mkdir -p "$SMOKE_ROOT"
OCORE_MEDIA_PROBE_MODE=33 \
OSTADIX_BOOT_CHALLENGE="$OSTADIX_BOOT_CHALLENGE" \
OSTADIX_MEDIA_ROOT="$SMOKE_ROOT/build-one" \
OCORE_MEDIA_KERNEL_BUILD_DIR="$SMOKE_ROOT/kernel-one" \
  "$ROOT/ocore/kernel/build-x86_64-uefi-media.sh" "$FIRST" \
  >"$SMOKE_ROOT/first-build.txt"
OCORE_MEDIA_PROBE_MODE=33 \
OSTADIX_BOOT_CHALLENGE="$OSTADIX_BOOT_CHALLENGE" \
OSTADIX_MEDIA_ROOT="$SMOKE_ROOT/build-two" \
OCORE_MEDIA_KERNEL_BUILD_DIR="$SMOKE_ROOT/kernel-two" \
  "$ROOT/ocore/kernel/build-x86_64-uefi-media.sh" "$SECOND" \
  >"$SMOKE_ROOT/second-build.txt"
if ! cmp -s "$FIRST" "$SECOND"; then
  echo "error: identical BootInfo inputs produced different media bytes" >&2
  exit 1
fi

python3 - \
  "$QEMU_BIN" \
  "$OSTADIX_OVMF_CODE" \
  "$FIRST" \
  "$TIMEOUT" \
  "$OSTADIX_BOOT_CHALLENGE" \
  "$SOURCE_COMMIT" <<'PY'
import hashlib
from pathlib import Path
import re
import subprocess
import sys

qemu, firmware, media, timeout_text, challenge, source_commit = sys.argv[1:]
command = [
    qemu,
    "-accel", "tcg",
    "-machine", "q35",
    "-m", "128M",
    "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={firmware}",
    "-drive", f"if=none,id=ostadix,format=raw,readonly=on,file={media}",
    "-device", "virtio-blk-pci,drive=ostadix",
    "-nodefaults",
    "-nic", "none",
    "-display", "none",
    "-serial", "stdio",
    "-monitor", "none",
    "-no-reboot",
    "-no-shutdown",
]
try:
    result = subprocess.run(
        command,
        capture_output=True,
        timeout=float(timeout_text),
        check=False,
    )
    timed_out = False
    stdout = result.stdout
    stderr = result.stderr
except subprocess.TimeoutExpired as error:
    timed_out = True
    stdout = error.stdout or b""
    stderr = error.stderr or b""

output = stdout.decode("utf-8", "replace")
diagnostic = stderr.decode("utf-8", "replace")
required = [
    "O-core kernel: serial online\n",
    "BootInfoV1: malformed fixture rejected\n",
    "BootInfoV1: source pointer and temporary aperture released\n",
    "BootInfoV1: Multiboot2 normalized\n",
    "BootInfoV1: ACPI status valid\n",
    "BootInfoV1: EFI64 boot services exited\n",
    "page protections: W^X online\n",
    "page allocator: online\n",
    "BootInfoV1: firmware allocator window admitted\n",
    f"OSTADIX boot challenge: {challenge}\n",
    f"OSTADIX source commit: {source_commit}\n",
    "BootInfoV1: boot handoff PASS\n",
]
missing = [marker for marker in required if marker not in output]
regions = re.search(r"BootInfoV1 usable regions: ([0-9]+)\n", output)
usable_bytes = re.search(r"BootInfoV1 usable bytes: ([0-9]+)\n", output)
allocator_start = re.search(r"BootInfoV1 allocator start: ([0-9]+)\n", output)
allocator_end = re.search(r"BootInfoV1 allocator end: ([0-9]+)\n", output)
if regions is None or int(regions.group(1)) == 0:
    missing.append("positive normalized usable-region count")
if usable_bytes is None or int(usable_bytes.group(1)) == 0:
    missing.append("positive normalized usable-byte count")
if allocator_start is None or allocator_end is None:
    missing.append("normalized allocator interval")
else:
    start = int(allocator_start.group(1))
    end = int(allocator_end.group(1))
    if not (
        0x0040_0000 <= start < end <= 0x0100_0000
        and start % 4096 == 0
        and end % 4096 == 0
        and end - start >= 0x0040_0000
    ):
        missing.append("bounded page-aligned allocator interval")
if "BootInfoV1: rejected" in output or "BootInfoV1 rejection code:" in output:
    missing.append("no BootInfo rejection marker")
if output.count(f"OSTADIX boot challenge: {challenge}\n") != 1:
    missing.append("exactly one challenge echo")
if output.count(f"OSTADIX source commit: {source_commit}\n") != 1:
    missing.append("exactly one source-commit echo")
if output.count(
    "BootInfoV1: source pointer and temporary aperture released\n"
) != 1:
    missing.append("exactly one source-pointer/aperture release assertion")
positions = [output.find(marker) for marker in required]
if positions != sorted(positions):
    missing.append("causal marker order")
if missing or not timed_out:
    print(
        f"BootInfo QEMU smoke failed; missing={missing!r} timed_out={timed_out}",
        file=sys.stderr,
    )
    print("stdout:", output, file=sys.stderr)
    print("stderr:", diagnostic, file=sys.stderr)
    raise SystemExit(1)

image_digest = hashlib.sha256(Path(media).read_bytes()).hexdigest()
print(output, end="")
print(f"OSTADIX boot challenge {challenge}")
print(f"OSTADIX source commit {source_commit}")
print(f"OSTADIX BootInfo media SHA-256 {image_digest}")
print("OSTADIX bounded freestanding BootInfo: PASS")
PY

# A physical evidence transcript uses the ordinary mode-0 kernel, not the
# focused mode-33 halt. Prove the same exact challenge survives through W^X,
# the admitted allocator, CPL3 entry, timer return, and heartbeat.
OCORE_MEDIA_PROBE_MODE=0 \
OSTADIX_BOOT_CHALLENGE="$OSTADIX_BOOT_CHALLENGE" \
OSTADIX_MEDIA_ROOT="$SMOKE_ROOT/build-mode0" \
OCORE_MEDIA_KERNEL_BUILD_DIR="$SMOKE_ROOT/kernel-mode0" \
  "$ROOT/ocore/kernel/build-x86_64-uefi-media.sh" "$MODE0" \
  >"$SMOKE_ROOT/mode0-build.txt"

python3 - \
  "$QEMU_BIN" \
  "$OSTADIX_OVMF_CODE" \
  "$MODE0" \
  "$TIMEOUT" \
  "$OSTADIX_BOOT_CHALLENGE" \
  "$SOURCE_COMMIT" \
  "$MODE0_TRANSCRIPT" <<'PY'
import hashlib
from pathlib import Path
import subprocess
import sys

qemu, firmware, media, timeout_text, challenge, source_commit, transcript = sys.argv[1:]
command = [
    qemu,
    "-accel", "tcg",
    "-machine", "q35",
    "-m", "128M",
    "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={firmware}",
    "-drive", f"if=none,id=ostadix,format=raw,readonly=on,file={media}",
    "-device", "virtio-blk-pci,drive=ostadix",
    "-nodefaults",
    "-nic", "none",
    "-display", "none",
    "-serial", "stdio",
    "-monitor", "none",
    "-no-reboot",
    "-no-shutdown",
]
try:
    result = subprocess.run(
        command,
        capture_output=True,
        timeout=float(timeout_text),
        check=False,
    )
    timed_out = False
    stdout = result.stdout
    stderr = result.stderr
except subprocess.TimeoutExpired as error:
    timed_out = True
    stdout = error.stdout or b""
    stderr = error.stderr or b""
output = stdout.decode("utf-8", "replace")
diagnostic = stderr.decode("utf-8", "replace")
required = [
    "BootInfoV1: source pointer and temporary aperture released\n",
    "BootInfoV1: Multiboot2 normalized\n",
    "page protections: W^X online\n",
    "page allocator: online\n",
    "BootInfoV1: firmware allocator window admitted\n",
    f"OSTADIX boot challenge: {challenge}\n",
    f"OSTADIX source commit: {source_commit}\n",
    "CPL3 native[0]: online\n",
    "timer CPL3 return: online\n",
    "CPL3 heartbeat: online\n",
]
missing = [marker for marker in required if marker not in output]
positions = [output.find(marker) for marker in required]
if positions != sorted(positions):
    missing.append("challenged mode-0 causal marker order")
if "BootInfoV1: rejected" in output or "BootInfoV1 rejection code:" in output:
    missing.append("no BootInfo rejection marker")
if output.count(f"OSTADIX boot challenge: {challenge}\n") != 1:
    missing.append("exactly one mode-0 challenge echo")
if output.count(f"OSTADIX source commit: {source_commit}\n") != 1:
    missing.append("exactly one mode-0 source-commit echo")
if output.count(
    "BootInfoV1: source pointer and temporary aperture released\n"
) != 1:
    missing.append("exactly one mode-0 source-pointer/aperture release assertion")
if missing or not timed_out:
    print(
        f"challenged mode-0 QEMU smoke failed; missing={missing!r} "
        f"timed_out={timed_out}",
        file=sys.stderr,
    )
    print("stdout:", output, file=sys.stderr)
    print("stderr:", diagnostic, file=sys.stderr)
    raise SystemExit(1)
image_digest = hashlib.sha256(Path(media).read_bytes()).hexdigest()
Path(transcript).write_bytes(stdout)
print(output, end="")
print(f"OSTADIX challenged mode-0 media SHA-256 {image_digest}")
print("OSTADIX challenged mode-0 CPL3 lifecycle: PASS")
PY

# Exercise the exact transcript grammar shared with authority-free physical
# observations. This is explicitly a QEMU-context check, not physical proof.
"$ROOT/scripts/ostadix_physical_evidence.py" check-transcript \
  --context qemu-tcg \
  --transcript "$MODE0_TRANSCRIPT" \
  --challenge "$OSTADIX_BOOT_CHALLENGE" \
  --source-commit "$SOURCE_COMMIT" \
  --expected-cpus 1 \
  >"$SMOKE_ROOT/mode0-transcript-check.json"

first_nibble="${OSTADIX_BOOT_CHALLENGE:0:1}"
if [[ "$first_nibble" == 0 ]]; then
  wrong_challenge="1${OSTADIX_BOOT_CHALLENGE:1}"
else
  wrong_challenge="0${OSTADIX_BOOT_CHALLENGE:1}"
fi
if "$ROOT/scripts/ostadix_physical_evidence.py" check-transcript \
  --context qemu-tcg \
  --transcript "$MODE0_TRANSCRIPT" \
  --challenge "$wrong_challenge" \
  --source-commit "$SOURCE_COMMIT" \
  --expected-cpus 1 \
  >"$SMOKE_ROOT/wrong-challenge.stdout" \
  2>"$SMOKE_ROOT/wrong-challenge.stderr"; then
  echo "error: shared transcript grammar accepted the wrong challenge" >&2
  exit 1
fi
printf '%s\n' "OSTADIX challenged transcript grammar: PASS"
