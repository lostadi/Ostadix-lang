#!/usr/bin/env bash
# Build one deterministic GPT/ESP image that boots the current x86_64 kernel
# through UEFI GRUB and the kernel's Multiboot2 entry.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MEDIA_ROOT="${OSTADIX_MEDIA_ROOT:-$ROOT/target/ostadix-media/x86_64}"
KERNEL_BUILD_DIR="${OCORE_MEDIA_KERNEL_BUILD_DIR:-$ROOT/target/ocore-media-kernel}"
OUTPUT="${1:-$MEDIA_ROOT/ostadix-x86_64-uefi.img}"
GRUB_CFG="$ROOT/ocore/kernel/x86_64/grub.cfg"
GRUB_MKSTANDALONE="${OSTADIX_GRUB_MKSTANDALONE:-}"
MFORMAT="${OSTADIX_MFORMAT:-mformat}"
MCOPY="${OSTADIX_MCOPY:-mcopy}"
PYTHON="${OSTADIX_PYTHON:-python3}"
GPT_TOOL="$ROOT/scripts/ostadix_boot_media.py"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-315532800}"
ESP_BYTES="${OSTADIX_ESP_BYTES:-67108864}"
export SOURCE_DATE_EPOCH

if [[ -z "$GRUB_MKSTANDALONE" ]]; then
  if command -v x86_64-elf-grub-mkstandalone >/dev/null 2>&1; then
    GRUB_MKSTANDALONE=x86_64-elf-grub-mkstandalone
  elif command -v grub-mkstandalone >/dev/null 2>&1; then
    GRUB_MKSTANDALONE=grub-mkstandalone
  else
    echo "error: required OSTADIX media tool is unavailable: x86_64-elf-grub-mkstandalone or grub-mkstandalone" >&2
    exit 127
  fi
fi

if [[ $# -gt 1 ]]; then
  echo "usage: build-x86_64-uefi-media.sh [OUTPUT]" >&2
  exit 2
fi
if [[ ! "$ESP_BYTES" =~ ^[0-9]+$ ]] || (( ESP_BYTES < 33554432 || ESP_BYTES > 536870912 || ESP_BYTES % 512 != 0 )); then
  echo "error: OSTADIX_ESP_BYTES must be a 512-byte-aligned integer from 33554432 through 536870912" >&2
  exit 2
fi
for tool in "$GRUB_MKSTANDALONE" "$MFORMAT" "$MCOPY" "$PYTHON"; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'error: required OSTADIX media tool is unavailable: %s\n' "$tool" >&2
    exit 127
  fi
done
for source in "$GRUB_CFG" "$GPT_TOOL" "$ROOT/ocore/kernel/build.sh"; do
  if [[ ! -f "$source" ]]; then
    printf 'error: required OSTADIX media source is missing: %s\n' "$source" >&2
    exit 1
  fi
done

mkdir -p "$MEDIA_ROOT" "$(dirname "$OUTPUT")"
WORK_DIR="$(mktemp -d "$MEDIA_ROOT/.media-build.XXXXXX")"
cleanup() {
  rm -rf -- "$WORK_DIR"
}
trap cleanup EXIT INT TERM

STAGE="$WORK_DIR/stage"
ESP="$WORK_DIR/esp.fat"
RENDERED_GRUB_CFG="$WORK_DIR/grub.cfg"
FAT_IDENTITY="$WORK_DIR/fat-identity.txt"
mkdir -p "$STAGE/EFI/BOOT" "$STAGE/boot"

OCORE_PROBE_MODE=0 OCORE_BUILD_DIR="$KERNEL_BUILD_DIR" \
  "$ROOT/ocore/kernel/build.sh" >/dev/null
KERNEL="$KERNEL_BUILD_DIR/kernel.elf"
if [[ ! -f "$KERNEL" ]]; then
  printf 'error: kernel build completed without producing %s\n' "$KERNEL" >&2
  exit 1
fi

cp "$KERNEL" "$STAGE/boot/kernel.elf"
"$PYTHON" - "$KERNEL" "$GRUB_CFG" "$RENDERED_GRUB_CFG" "$FAT_IDENTITY" <<'PY'
from hashlib import sha256
from pathlib import Path
import sys

kernel_path, template_path, rendered_path, identity_path = map(Path, sys.argv[1:])
kernel = kernel_path.read_bytes()
template = template_path.read_bytes()
marker = b"@OSTADIX_FAT_UUID@"
if template.count(marker) != 1:
    raise SystemExit(
        "error: GRUB configuration must contain exactly one "
        "@OSTADIX_FAT_UUID@ marker"
    )

# This is the versioned identity preimage, not a digest of the completed ESP:
# the completed ESP contains BOOTX64.EFI, which itself embeds this identity.
preimage = (
    b"OSTADIX/FAT-IDENTITY/V1\0"
    + len(kernel).to_bytes(8, "big")
    + kernel
    + len(template).to_bytes(8, "big")
    + template
)
identity_digest = sha256(preimage).digest()
serial = int.from_bytes(identity_digest[:4], "big")
if serial == 0:
    serial = 1

# GRUB reads the little-endian FAT volume ID into a host integer, then renders
# that integer's high and low 16-bit words (0x12345678 becomes 1234-5678).
fat_uuid = f"{serial >> 16:04X}-{serial & 0xffff:04X}"
rendered_path.write_bytes(template.replace(marker, fat_uuid.encode("ascii")))
identity_path.write_text(
    f"{serial:08X}\n{fat_uuid}\n{identity_digest.hex()}\n",
    encoding="ascii",
)
PY
FAT_SERIAL="$(sed -n '1p' "$FAT_IDENTITY")"
FAT_UUID="$(sed -n '2p' "$FAT_IDENTITY")"
FAT_IDENTITY_SHA256="$(sed -n '3p' "$FAT_IDENTITY")"
if [[ ! "$FAT_SERIAL" =~ ^[0-9A-F]{8}$ ]] \
  || [[ ! "$FAT_UUID" =~ ^[0-9A-F]{4}-[0-9A-F]{4}$ ]] \
  || [[ ! "$FAT_IDENTITY_SHA256" =~ ^[0-9a-f]{64}$ ]]; then
  echo "error: derived OSTADIX FAT identity has an invalid representation" >&2
  exit 1
fi

SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" "$GRUB_MKSTANDALONE" \
  --format=x86_64-efi \
  --output="$STAGE/EFI/BOOT/BOOTX64.EFI" \
  --locales="" \
  --fonts="" \
  --modules="part_gpt fat normal search search_fs_uuid multiboot2 serial configfile" \
  "boot/grub/grub.cfg=$RENDERED_GRUB_CFG"

# FAT timestamps cannot represent dates before 1980.  Preserve this exact
# epoch during recursive mcopy so directory entries remain reproducible.
find "$STAGE" -exec touch -t 198001010000 {} +
"$PYTHON" - "$ESP" "$ESP_BYTES" <<'PY'
from pathlib import Path
import sys

with Path(sys.argv[1]).open("wb") as stream:
    stream.truncate(int(sys.argv[2]))
PY
"$MFORMAT" -i "$ESP" -F -v OSTADIX -N "0x$FAT_SERIAL" ::
"$PYTHON" - "$ESP" "$FAT_SERIAL" <<'PY'
from pathlib import Path
import sys

esp_path = Path(sys.argv[1])
expected = int(sys.argv[2], 16)
with esp_path.open("rb") as stream:
    boot_sector = stream.read(512)
if len(boot_sector) != 512 or boot_sector[66] != 0x29:
    raise SystemExit("error: mformat did not produce the expected FAT32 volume-ID field")
observed = int.from_bytes(boot_sector[67:71], "little")
if observed != expected:
    raise SystemExit(
        f"error: FAT volume ID mismatch: expected {expected:08X}, observed {observed:08X}"
    )
PY
"$MCOPY" -smp -i "$ESP" "$STAGE/EFI" "$STAGE/boot" ::/

PACK_METADATA="$WORK_DIR/pack.json"
"$PYTHON" "$GPT_TOOL" pack --esp "$ESP" --output "$OUTPUT" >"$PACK_METADATA"
"$PYTHON" "$GPT_TOOL" inspect "$OUTPUT" >"$WORK_DIR/inspect.json"
if ! cmp -s "$PACK_METADATA" "$WORK_DIR/inspect.json"; then
  echo "error: packed and independently inspected OSTADIX media metadata differ" >&2
  exit 1
fi

"$PYTHON" - "$OUTPUT" "$PACK_METADATA" <<'PY'
import json
from pathlib import Path
import sys

image = Path(sys.argv[1]).resolve()
metadata = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
print(f"media: {image}")
print(f"media-bytes: {metadata['bytes']}")
print(f"media-sha256: {metadata['sha256']}")
print(f"esp-sha256: {metadata['esp_sha256']}")
print(f"disk-guid: {metadata['disk_guid']}")
PY
printf 'fat-uuid: %s\n' "$FAT_UUID"
printf 'fat-identity-sha256: %s\n' "$FAT_IDENTITY_SHA256"
