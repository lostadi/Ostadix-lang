#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SMOKE_ROOT="${OSTADIX_MEDIA_SMOKE_ROOT:-$ROOT/target/ostadix-media-smoke/x86_64}"
FIRST="$SMOKE_ROOT/first.img"
SECOND="$SMOKE_ROOT/second.img"
FIRST_BUILD_RECORD="$SMOKE_ROOT/first-build.txt"
SECOND_BUILD_RECORD="$SMOKE_ROOT/second-build.txt"
INSPECT_RECORD="$SMOKE_ROOT/inspect.json"
QEMU_BIN="${OCORE_QEMU_BIN:-qemu-system-x86_64}"
TIMEOUT="${OSTADIX_MEDIA_TIMEOUT_SECONDS:-12}"

if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  printf 'error: QEMU executable is unavailable: %s\n' "$QEMU_BIN" >&2
  exit 127
fi
mkdir -p "$SMOKE_ROOT"
OSTADIX_MEDIA_ROOT="$SMOKE_ROOT/build-one" \
  OCORE_MEDIA_KERNEL_BUILD_DIR="$SMOKE_ROOT/kernel-one" \
  "$ROOT/ocore/kernel/build-x86_64-uefi-media.sh" "$FIRST" >"$FIRST_BUILD_RECORD"
OSTADIX_MEDIA_ROOT="$SMOKE_ROOT/build-two" \
  OCORE_MEDIA_KERNEL_BUILD_DIR="$SMOKE_ROOT/kernel-two" \
  "$ROOT/ocore/kernel/build-x86_64-uefi-media.sh" "$SECOND" >"$SECOND_BUILD_RECORD"
if ! cmp -s "$FIRST" "$SECOND"; then
  echo "error: OSTADIX x86_64 UEFI media rebuild is not deterministic" >&2
  exit 1
fi

"$ROOT/scripts/ostadix_boot_media.py" inspect "$FIRST" | tee "$INSPECT_RECORD"
python3 - "$FIRST" "$FIRST_BUILD_RECORD" "$SECOND_BUILD_RECORD" "$INSPECT_RECORD" <<'PY'
import json
from pathlib import Path
import re
import sys

image_path, first_record_path, second_record_path, inspect_path = map(
    Path, sys.argv[1:]
)


def read_record(path):
    record = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if ": " in line:
            key, value = line.split(": ", 1)
            record[key] = value
    return record


first = read_record(first_record_path)
second = read_record(second_record_path)
for key, pattern in (
    ("fat-uuid", r"[0-9A-F]{4}-[0-9A-F]{4}"),
    ("fat-identity-sha256", r"[0-9a-f]{64}"),
):
    if key not in first or re.fullmatch(pattern, first[key]) is None:
        raise SystemExit(f"error: first build has invalid or missing {key}")
    if first[key] != second.get(key):
        raise SystemExit(f"error: deterministic rebuild changed {key}")

metadata = json.loads(inspect_path.read_text(encoding="utf-8"))
esp_offset = int(metadata["esp_first_lba"]) * int(metadata["sector_size"])
with image_path.open("rb") as stream:
    stream.seek(esp_offset + 67)
    raw_serial = stream.read(4)
if len(raw_serial) != 4:
    raise SystemExit("error: exact ESP is truncated before the FAT volume ID")
serial = int.from_bytes(raw_serial, "little")
observed_uuid = f"{serial >> 16:04X}-{serial & 0xffff:04X}"
if observed_uuid != first["fat-uuid"]:
    raise SystemExit(
        "error: built-media FAT UUID does not match the identity embedded in GRUB: "
        f"recorded={first['fat-uuid']} observed={observed_uuid}"
    )
print(f"OSTADIX x86_64 UEFI media FAT identity {observed_uuid}: PASS")
PY

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

python3 - "$QEMU_BIN" "$OSTADIX_OVMF_CODE" "$FIRST" "$TIMEOUT" <<'PY'
import subprocess
import sys

qemu, firmware, media, timeout_text = sys.argv[1:]
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
    result = subprocess.run(command, capture_output=True, timeout=float(timeout_text))
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
    "O-core kernel: serial online",
    "page protections: W^X online",
    "CPL3 native[0]: online",
    "timer CPL3 return: online",
    "CPL3 heartbeat: online",
]
missing = [marker for marker in required if marker not in output]
if missing or not timed_out:
    print(f"UEFI media smoke failed; missing={missing!r} timed_out={timed_out}", file=sys.stderr)
    print("stdout:", output, file=sys.stderr)
    print("stderr:", diagnostic, file=sys.stderr)
    raise SystemExit(1)
print(output, end="")
print("OSTADIX x86_64 UEFI media deterministic rebuild: PASS")
print("OSTADIX x86_64 UEFI media boot: PASS")
PY
