#!/usr/bin/env bash
# Build the hardware-oriented x86_64 UEFI Hosted Live plus capacity ISO.
# Hosted, O-core, and Alpine boot directly; foreign media use nested QEMU TCG.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
ISO_ROOT=${OSTADIX_HOSTED_LIVE_ISO_ROOT:-"$ROOT/target/ostadix-hosted-live/x86_64"}
OUTPUT=${1:-"$ISO_ROOT/ostadix-hosted-live-x86_64-uefi_VTGRUB2.iso"}
PROFILE=${OSTADIX_HOSTED_LIVE_ISO_PROFILE:-"$ROOT/evidence/hosted_live_physical_iso.toml"}
KERNEL=${OSTADIX_HOSTED_LIVE_KERNEL:-"${XDG_DATA_HOME:-$HOME/.local/share}/ostadix/hosted-live/vmlinuz-lts"}
INITRAMFS=${OSTADIX_HOSTED_LIVE_INITRAMFS:-"$ROOT/target/ostadix-hosted-live/x86_64/initramfs.cpio.gz"}
ROOTFS=${OSTADIX_HOSTED_LIVE_ROOTFS:-"$ROOT/target/ostadix-hosted-live/x86_64/rootfs.squashfs"}
VENTOY_MODLOOP=${OSTADIX_HOSTED_LIVE_VENTOY_MODLOOP:-"$ROOT/target/ostadix-hosted-live/x86_64/modloop-lts"}
OCORE_KERNEL=${OSTADIX_HOSTED_LIVE_OCORE_KERNEL:-"$ROOT/target/ocore-kernel/kernel.elf"}
GUEST_ROOT=${OSTADIX_GUEST_ROOT:-"${XDG_DATA_HOME:-$HOME/.local/share}/ostadix/guests"}
CAPACITY_HOST_KERNEL=${OSTADIX_HOSTED_LIVE_CAPACITY_HOST_KERNEL:-"$GUEST_ROOT/alpine-3.24.1-x86_64/vmlinuz-virt"}
CAPACITY_HOST_INITRAMFS=${OSTADIX_HOSTED_LIVE_CAPACITY_HOST_INITRAMFS:-"$ROOT/target/ostadix-capacity-host/x86_64/initramfs.cpio.gz"}
ALPINE_INITRAMFS=${OSTADIX_HOSTED_LIVE_ALPINE_INITRAMFS:-"$GUEST_ROOT/alpine-3.24.1-x86_64/initramfs-virt"}
FOREIGN_LAB=${OSTADIX_FOREIGN_KERNEL_LAB:-"$ROOT/scripts/foreign_kernel_lab.py"}
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

Build the seven-entry hardware-oriented OSTADIX Hosted Live UEFI ISO. Hosted,
O-core, and Alpine are direct boot entries. Guix, OpenBSD, 9front, and Redox are
explicitly labeled nested QEMU TCG entries using the on-disc capacity host.
All 14 typed artifacts must already be fetched and verified. Hosted remains the
first and default entry. This command downloads no inputs and never replaces an
existing output.
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
for source in "$PROFILE" "$KERNEL" "$INITRAMFS" "$ROOTFS" "$VENTOY_MODLOOP" \
  "$OCORE_KERNEL" "$CAPACITY_HOST_KERNEL" "$CAPACITY_HOST_INITRAMFS" \
  "$ALPINE_INITRAMFS" "$FOREIGN_LAB" "$ISO_TOOL" \
  "$XORRISO_WRAPPER"; do
  if [[ -L "$source" || ! -f "$source" ]]; then
    die "required hosted-live ISO input is missing or a symlink: $source"
  fi
done
for executable in "$FOREIGN_LAB" "$ISO_TOOL" "$XORRISO_WRAPPER"; do
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
for module in modinfo.sh normal.mod multiboot2.mod linux.mod part_gpt.mod fat.mod iso9660.mod; do
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

"$PYTHON" "$FOREIGN_LAB" --guest-dir "$GUEST_ROOT" verify \
  --guest linux-alpine-3.24.1-x86_64 \
  --guest guix-system-1.5.0-x86_64 \
  --guest plan9-9front-11983-amd64 \
  --guest redox-0.9.0-server-x86_64 \
  --guest openbsd-7.9-amd64 >/dev/null

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
install_artifact "$ROOTFS" boot/hosted/rootfs.squashfs
install_artifact "$VENTOY_MODLOOP" boot/modloop-lts
install_artifact "$OCORE_KERNEL" boot/ocore/kernel.elf
install_artifact "$CAPACITY_HOST_KERNEL" boot/capacity-host/vmlinuz-virt
install_artifact "$CAPACITY_HOST_INITRAMFS" boot/capacity-host/initramfs.cpio.gz
install_artifact "$ALPINE_INITRAMFS" boot/entry/010-alpine/initramfs-virt
install_artifact "$GUEST_ROOT/guix-1.5.0-x86_64/linux-libre-6.17.12-bzImage" \
  ostadix/guix/linux-libre-6.17.12-bzimage
install_artifact "$GUEST_ROOT/guix-1.5.0-x86_64/guix-1.5.0-initrd.cpio.gz" \
  ostadix/guix/guix-1.5.0-initrd.cpio.gz
install_artifact "$GUEST_ROOT/guix-1.5.0-x86_64/guix-system-install-1.5.0.x86_64-linux.iso" \
  ostadix/guix/guix-system-install-1.5.0.x86_64-linux.iso
install_artifact "$GUEST_ROOT/openbsd-7.9-amd64/install79.iso" \
  ostadix/openbsd/install79.iso
install_artifact "$GUEST_ROOT/9front-11983-amd64/9front-11983.amd64.qcow2" \
  ostadix/9front/9front-11983.amd64.qcow2
install_artifact "$GUEST_ROOT/redox-0.9.0-server-x86_64/redox_server_x86_64_2024-09-07_1225_livedisk.iso" \
  ostadix/redox/redox-server-0.9.0-livedisk.iso

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
