#!/usr/bin/env bash
# Build one deterministic ISO9660/El Torito image that boots the current
# x86_64 O-core kernel through UEFI GRUB and Multiboot2.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ISO_ROOT="${OSTADIX_ISO_ROOT:-$ROOT/target/ostadix-iso/x86_64}"
KERNEL_BUILD_DIR="${OCORE_ISO_KERNEL_BUILD_DIR:-$ROOT/target/ocore-iso-kernel}"
OUTPUT="${1:-$ISO_ROOT/ostadix-x86_64-uefi.iso}"
GRUB_MKRESCUE="${OSTADIX_GRUB_MKRESCUE:-}"
GRUB_EFI_DIRECTORY="${OSTADIX_GRUB_EFI_DIRECTORY:-}"
XORRISO="${OSTADIX_XORRISO:-}"
PYTHON="${OSTADIX_PYTHON:-python3}"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-315532800}"
GRUB_CFG="$ROOT/ocore/kernel/x86_64/grub-iso.cfg"
ISO_TOOL="$ROOT/scripts/ostadix_boot_iso.py"
XORRISO_WRAPPER="$ROOT/scripts/ostadix_xorriso_reproducible.py"
KERNEL_BUILD_SCRIPT="$ROOT/ocore/kernel/build.sh"
WORK_DIR=""
export LC_ALL=C SOURCE_DATE_EPOCH TZ=UTC

cleanup() {
  if [[ -n "$WORK_DIR" ]]; then
    rm -rf -- "$WORK_DIR"
  fi
}
trap cleanup EXIT INT TERM

if [[ $# -gt 1 ]]; then
  echo "usage: build-x86_64-uefi-iso.sh [OUTPUT]" >&2
  exit 2
fi
if [[ ! "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]] \
    || (( SOURCE_DATE_EPOCH < 315532800 || SOURCE_DATE_EPOCH > 2147483647 )); then
  echo "error: SOURCE_DATE_EPOCH must be an integer from 315532800 through 2147483647" >&2
  exit 2
fi

if [[ -L "$OUTPUT" ]]; then
  printf 'error: refusing OSTADIX ISO output symlink: %s\n' "$OUTPUT" >&2
  exit 1
fi
if [[ -e "$OUTPUT" && ! -f "$OUTPUT" ]]; then
  printf 'error: OSTADIX ISO output exists and is not a regular file: %s\n' "$OUTPUT" >&2
  exit 1
fi
if [[ -L "$ISO_ROOT" || -L "$KERNEL_BUILD_DIR" ]]; then
  echo "error: OSTADIX ISO build directories must not be symlinks" >&2
  exit 1
fi
for source in "$GRUB_CFG" "$ISO_TOOL" "$XORRISO_WRAPPER" "$KERNEL_BUILD_SCRIPT"; do
  if [[ -L "$source" || ! -f "$source" ]]; then
    printf 'error: required OSTADIX ISO source is not a regular non-symlink file: %s\n' \
      "$source" >&2
    exit 1
  fi
done
if [[ ! -x "$ISO_TOOL" || ! -x "$XORRISO_WRAPPER" ]]; then
  echo "error: OSTADIX ISO inspector and xorriso wrapper must be executable" >&2
  exit 1
fi

if [[ -z "$GRUB_MKRESCUE" ]]; then
  if command -v x86_64-elf-grub-mkrescue >/dev/null 2>&1; then
    GRUB_MKRESCUE=x86_64-elf-grub-mkrescue
  elif command -v grub-mkrescue >/dev/null 2>&1; then
    GRUB_MKRESCUE=grub-mkrescue
  else
    echo "error: required OSTADIX ISO tool is unavailable: x86_64-elf-grub-mkrescue or grub-mkrescue" >&2
    exit 127
  fi
fi
if [[ -z "$XORRISO" ]]; then
  XORRISO=xorriso
fi
for tool in "$GRUB_MKRESCUE" "$XORRISO" "$PYTHON"; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'error: required OSTADIX ISO tool is unavailable: %s\n' "$tool" >&2
    exit 127
  fi
done

if [[ -z "$GRUB_EFI_DIRECTORY" ]]; then
  for candidate in \
    /opt/homebrew/opt/x86_64-elf-grub/lib/x86_64-elf/grub/x86_64-efi \
    /usr/local/opt/x86_64-elf-grub/lib/x86_64-elf/grub/x86_64-efi \
    /usr/lib/grub/x86_64-efi; do
    if [[ -d "$candidate" ]]; then
      GRUB_EFI_DIRECTORY="$candidate"
      break
    fi
  done
fi
if [[ -z "$GRUB_EFI_DIRECTORY" || -L "$GRUB_EFI_DIRECTORY" \
    || ! -d "$GRUB_EFI_DIRECTORY" ]]; then
  echo "error: x86_64-efi GRUB platform directory is unavailable; set OSTADIX_GRUB_EFI_DIRECTORY" >&2
  exit 127
fi
for module in modinfo.sh normal.mod multiboot2.mod; do
  if [[ -L "$GRUB_EFI_DIRECTORY/$module" || ! -f "$GRUB_EFI_DIRECTORY/$module" ]]; then
    printf 'error: x86_64-efi GRUB platform input is missing or a symlink: %s\n' \
      "$GRUB_EFI_DIRECTORY/$module" >&2
    exit 1
  fi
done

OUTPUT_DIR="$(dirname "$OUTPUT")"
if [[ -L "$OUTPUT_DIR" ]]; then
  printf 'error: refusing OSTADIX ISO output directory symlink: %s\n' "$OUTPUT_DIR" >&2
  exit 1
fi
mkdir -p "$ISO_ROOT" "$KERNEL_BUILD_DIR" "$OUTPUT_DIR"
if [[ -L "$ISO_ROOT" || ! -d "$ISO_ROOT" \
    || -L "$KERNEL_BUILD_DIR" || ! -d "$KERNEL_BUILD_DIR" \
    || -L "$OUTPUT_DIR" || ! -d "$OUTPUT_DIR" ]]; then
  echo "error: OSTADIX ISO build directories must be non-symlink directories" >&2
  exit 1
fi
WORK_DIR="$(mktemp -d "$ISO_ROOT/.iso-build.XXXXXX")"
STAGE="$WORK_DIR/stage"
CANDIDATE="$WORK_DIR/candidate.iso"
CANDIDATE_METADATA="$WORK_DIR/candidate.json"
PUBLISHED_METADATA="$WORK_DIR/published.json"
mkdir -p "$STAGE/boot/grub"

OCORE_BOOT_INFO_ENABLED=1 OCORE_PROBE_MODE=0 OCORE_BUILD_DIR="$KERNEL_BUILD_DIR" \
  "$KERNEL_BUILD_SCRIPT" >/dev/null
KERNEL="$KERNEL_BUILD_DIR/kernel.elf"
if [[ -L "$KERNEL" || ! -f "$KERNEL" ]]; then
  printf 'error: kernel build did not produce a regular non-symlink file: %s\n' \
    "$KERNEL" >&2
  exit 1
fi
cp "$KERNEL" "$STAGE/boot/kernel.elf"
cp "$GRUB_CFG" "$STAGE/boot/grub/grub.cfg"
find "$STAGE" -exec touch -t 198001010000 {} +

ISO_DATE="$($PYTHON - "$SOURCE_DATE_EPOCH" <<'PY'
from datetime import datetime, timezone
import sys

value = datetime.fromtimestamp(int(sys.argv[1]), timezone.utc)
print(value.strftime("%Y%m%d%H%M%S00"))
PY
)"
OSTADIX_REAL_XORRISO="$XORRISO" "$GRUB_MKRESCUE" \
  --directory="$GRUB_EFI_DIRECTORY" \
  --xorriso="$XORRISO_WRAPPER" \
  --output="$CANDIDATE" \
  --compress=no \
  --fonts="" \
  --locales="" \
  --themes="" \
  "$STAGE" \
  --modification-date="$ISO_DATE" \
  -V OSTADIX >/dev/null

if [[ -L "$CANDIDATE" || ! -f "$CANDIDATE" ]]; then
  echo "error: grub-mkrescue did not produce a regular private ISO candidate" >&2
  exit 1
fi
"$PYTHON" "$ISO_TOOL" inspect "$CANDIDATE" >"$CANDIDATE_METADATA"
"$PYTHON" "$ISO_TOOL" publish --source "$CANDIDATE" --output "$OUTPUT" \
  >"$PUBLISHED_METADATA"
if ! cmp -s "$CANDIDATE_METADATA" "$PUBLISHED_METADATA"; then
  echo "error: private and published OSTADIX ISO metadata differ" >&2
  exit 1
fi

"$PYTHON" - "$OUTPUT" "$PUBLISHED_METADATA" <<'PY'
import json
from pathlib import Path
import sys

image = Path(sys.argv[1]).absolute()
metadata = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
print(f"iso: {image}")
print(f"iso-bytes: {metadata['bytes']}")
print(f"iso-sha256: {metadata['sha256']}")
print(f"kernel-sha256: {metadata['kernel_sha256']}")
print(f"efi-boot-image-sha256: {metadata['efi_boot_image_sha256']}")
print(f"efi-bootloader-sha256: {metadata['efi_bootloader_sha256']}")
print("boot-contract: ISO9660 El-Torito UEFI no-emulation")
PY
