#!/usr/bin/env bash
# Build the hardware-oriented x86_64 UEFI Hosted Live ISO. The seven-entry
# absorbed-capacity laboratory image remains a separate serial/QEMU artifact.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
ISO_ROOT=${OSTADIX_HOSTED_LIVE_ISO_ROOT:-"$ROOT/target/ostadix-hosted-live/x86_64"}
OUTPUT=${1:-"$ISO_ROOT/ostadix-hosted-live-x86_64-uefi_VTGRUB2.iso"}
PROFILE=${OSTADIX_HOSTED_LIVE_ISO_PROFILE:-"$ROOT/evidence/hosted_live_physical_iso.toml"}
KERNEL=${OSTADIX_HOSTED_LIVE_KERNEL:-"${XDG_DATA_HOME:-$HOME/.local/share}/ostadix/hosted-live/vmlinuz-lts"}
INITRAMFS=${OSTADIX_HOSTED_LIVE_INITRAMFS:-"$ROOT/target/ostadix-hosted-live/x86_64/initramfs.cpio.gz"}
ISO_TOOL=${OSTADIX_CAPACITY_ISO_TOOL:-"$ROOT/scripts/ostadix_capacity_iso.py"}
XORRISO_WRAPPER=${OSTADIX_XORRISO_WRAPPER:-"$ROOT/scripts/ostadix_xorriso_reproducible.py"}
GRUB_MKRESCUE=${OSTADIX_GRUB_MKRESCUE:-}
GRUB_EFI_DIRECTORY=${OSTADIX_GRUB_EFI_DIRECTORY:-}
XORRISO=${OSTADIX_XORRISO:-xorriso}
PYTHON=${OSTADIX_PYTHON:-python3}
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-315532800}
WORK_DIR=
export LC_ALL=C SOURCE_DATE_EPOCH TZ=UTC

cleanup() {
  if [[ -n "$WORK_DIR" && -d "$WORK_DIR" ]]; then
    rm -rf -- "$WORK_DIR"
  fi
}
trap cleanup EXIT INT TERM

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'USAGE'
Usage: build-x86_64-hosted-live-iso.sh [OUTPUT]

Build the single-entry hardware-oriented OSTADIX Hosted Live UEFI ISO from a
pinned Alpine LTS kernel and a prepared hosted-live initramfs. This command
downloads no inputs and never replaces an existing output.
USAGE
}

if [[ $# -gt 1 ]]; then
  usage >&2
  exit 2
fi
if [[ ! "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]] \
    || (( SOURCE_DATE_EPOCH < 315532800 || SOURCE_DATE_EPOCH > 2147483647 )); then
  die "SOURCE_DATE_EPOCH must be an integer from 315532800 through 2147483647"
fi
for source in "$PROFILE" "$KERNEL" "$INITRAMFS" "$ISO_TOOL" "$XORRISO_WRAPPER"; do
  if [[ -L "$source" || ! -f "$source" ]]; then
    die "required hosted-live ISO input is missing or a symlink: $source"
  fi
done
for executable in "$ISO_TOOL" "$XORRISO_WRAPPER"; do
  [[ -x "$executable" ]] || die "required hosted-live ISO script is not executable: $executable"
done
for tool in "$PYTHON" "$XORRISO"; do
  command -v "$tool" >/dev/null 2>&1 || die "required hosted-live ISO tool is unavailable: $tool"
done

if [[ -z "$GRUB_MKRESCUE" ]]; then
  if command -v x86_64-elf-grub-mkrescue >/dev/null 2>&1; then
    GRUB_MKRESCUE=x86_64-elf-grub-mkrescue
  elif command -v grub-mkrescue >/dev/null 2>&1; then
    GRUB_MKRESCUE=grub-mkrescue
  else
    die "x86_64-elf-grub-mkrescue or grub-mkrescue is required"
  fi
fi
command -v "$GRUB_MKRESCUE" >/dev/null 2>&1 \
  || die "GRUB rescue builder is unavailable: $GRUB_MKRESCUE"
if [[ -z "$GRUB_EFI_DIRECTORY" ]]; then
  for candidate in \
    /opt/homebrew/opt/x86_64-elf-grub/lib/x86_64-elf/grub/x86_64-efi \
    /usr/local/opt/x86_64-elf-grub/lib/x86_64-elf/grub/x86_64-efi \
    /usr/lib/grub/x86_64-efi; do
    if [[ -d "$candidate" && ! -L "$candidate" ]]; then
      GRUB_EFI_DIRECTORY=$candidate
      break
    fi
  done
fi
[[ -n "$GRUB_EFI_DIRECTORY" && -d "$GRUB_EFI_DIRECTORY" && ! -L "$GRUB_EFI_DIRECTORY" ]] \
  || die "x86_64-efi GRUB platform directory is unavailable"
for module in modinfo.sh normal.mod linux.mod part_gpt.mod fat.mod iso9660.mod; do
  if [[ -L "$GRUB_EFI_DIRECTORY/$module" || ! -f "$GRUB_EFI_DIRECTORY/$module" ]]; then
    die "required hosted-live ISO GRUB module is missing or a symlink: $GRUB_EFI_DIRECTORY/$module"
  fi
done

if [[ -L "$OUTPUT" || ( -e "$OUTPUT" && ! -f "$OUTPUT" ) ]]; then
  die "hosted-live ISO output is a symlink or non-regular path: $OUTPUT"
fi
if [[ -e "$OUTPUT" ]]; then
  die "refusing to clobber existing hosted-live ISO output: $OUTPUT"
fi
mkdir -p -- "$ISO_ROOT" "$(dirname -- "$OUTPUT")"
WORK_DIR=$(mktemp -d "$ISO_ROOT/.hosted-live-iso-build.XXXXXX")
chmod 0700 "$WORK_DIR"
STAGE="$WORK_DIR/stage"
CANDIDATE="$WORK_DIR/candidate.iso"
mkdir -m 0700 "$STAGE"

install_artifact() {
  local source=$1 destination=$2
  [[ -f "$source" && ! -L "$source" ]] \
    || die "hosted-live artifact is missing or a symlink: $source"
  mkdir -p -- "$(dirname -- "$STAGE/$destination")"
  install -m 0444 "$source" "$STAGE/$destination"
}

install_artifact "$KERNEL" boot/hosted/vmlinuz-lts
install_artifact "$INITRAMFS" boot/hosted/initramfs.cpio.gz

"$PYTHON" "$ISO_TOOL" create-lock --stage "$STAGE" --profile "$PROFILE" \
  >"$WORK_DIR/hosted-live-lock.json"
"$PYTHON" - "$STAGE" "$SOURCE_DATE_EPOCH" <<'PY'
import os
from pathlib import Path
import sys

root = Path(sys.argv[1])
timestamp = int(sys.argv[2])
for path in sorted(root.rglob("*"), reverse=True):
    os.utime(path, (timestamp, timestamp), follow_symlinks=False)
os.utime(root, (timestamp, timestamp), follow_symlinks=False)
PY

ISO_DATE=$($PYTHON - "$SOURCE_DATE_EPOCH" <<'PY'
from datetime import datetime, timezone
import sys

print(datetime.fromtimestamp(int(sys.argv[1]), timezone.utc).strftime("%Y%m%d%H%M%S00"))
PY
)
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
  -V OSTADIX_CAPACITY >/dev/null

[[ -f "$CANDIDATE" && ! -L "$CANDIDATE" ]] \
  || die "grub-mkrescue did not produce a private hosted-live ISO candidate"
"$PYTHON" "$ISO_TOOL" inspect "$CANDIDATE" >"$WORK_DIR/candidate-inspection.json"
"$PYTHON" "$ISO_TOOL" publish --source "$CANDIDATE" --output "$OUTPUT" >/dev/null
"$PYTHON" "$ISO_TOOL" inspect "$OUTPUT" >"$WORK_DIR/published-inspection.json"
"$PYTHON" - "$OUTPUT" "$WORK_DIR/published-inspection.json" <<'PY'
import json
from pathlib import Path
import sys

metadata = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
print(f"hosted-live-iso-output: {sys.argv[1]}")
print(f"hosted-live-iso-bytes: {metadata['bytes']}")
print(f"hosted-live-iso-sha256: {metadata['sha256']}")
print(f"hosted-live-lock-sha256: {metadata['capacity_lock_sha256']}")
print(f"hosted-live-entry-count: {len(metadata['entries'])}")
PY
