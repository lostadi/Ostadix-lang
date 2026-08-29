#!/usr/bin/env bash
# Build the opt-in x86_64 UEFI ISO containing O-core plus absorbed foreign
# systems. Foreign disk media is launched by the on-disc Linux capacity host.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
ISO_ROOT=${OSTADIX_CAPACITY_ISO_ROOT:-"$ROOT/target/ostadix-capacity-iso/x86_64"}
KERNEL_BUILD_DIR=${OCORE_CAPACITY_ISO_KERNEL_BUILD_DIR:-"$ROOT/target/ocore-capacity-iso-kernel"}
OUTPUT=${1:-"$ISO_ROOT/ostadix-absorbed-capacity-x86_64-uefi.iso"}
PROFILE=${OSTADIX_CAPACITY_ISO_PROFILE:-"$ROOT/evidence/absorbed_capacity_iso.toml"}
GUEST_ROOT=${OSTADIX_GUEST_ROOT:-"${XDG_DATA_HOME:-$HOME/.local/share}/ostadix/guests"}
CAPACITY_HOST_INITRAMFS=${OSTADIX_CAPACITY_HOST_INITRAMFS:-"$ROOT/target/ostadix-capacity-host/x86_64/initramfs.cpio.gz"}
FOREIGN_LAB=${OSTADIX_FOREIGN_KERNEL_LAB:-"$ROOT/scripts/foreign_kernel_lab.py"}
ISO_TOOL=${OSTADIX_CAPACITY_ISO_TOOL:-"$ROOT/scripts/ostadix_capacity_iso.py"}
XORRISO_WRAPPER=${OSTADIX_XORRISO_WRAPPER:-"$ROOT/scripts/ostadix_xorriso_reproducible.py"}
KERNEL_BUILD_SCRIPT=${OCORE_BUILD_SCRIPT:-"$ROOT/ocore/kernel/build.sh"}
GRUB_MKRESCUE=${OSTADIX_GRUB_MKRESCUE:-}
GRUB_EFI_DIRECTORY=${OSTADIX_GRUB_EFI_DIRECTORY:-}
XORRISO=${OSTADIX_XORRISO:-xorriso}
PYTHON=${OSTADIX_PYTHON:-python3}
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-315532800}
WORK_DIR=
export CARGO_NET_OFFLINE=true LC_ALL=C SOURCE_DATE_EPOCH TZ=UTC

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
Usage: build-x86_64-capacity-iso.sh [OUTPUT]

Build the opt-in OSTADIX absorbed-capacity UEFI ISO. Required pinned artifacts
must already be fetched by scripts/foreign_kernel_lab.py, and the capacity-host
initramfs must already be prepared. Cargo is forced offline, so its
dependencies must already be cached; this command downloads no guest media.
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
for source in "$PROFILE" "$FOREIGN_LAB" "$ISO_TOOL" "$XORRISO_WRAPPER" \
  "$KERNEL_BUILD_SCRIPT" "$CAPACITY_HOST_INITRAMFS"; do
  if [[ -L "$source" || ! -f "$source" ]]; then
    die "required capacity ISO input is missing or a symlink: $source"
  fi
done
for executable in "$FOREIGN_LAB" "$ISO_TOOL" "$XORRISO_WRAPPER" "$KERNEL_BUILD_SCRIPT"; do
  [[ -x "$executable" ]] || die "required capacity ISO script is not executable: $executable"
done
for tool in "$PYTHON" "$XORRISO"; do
  command -v "$tool" >/dev/null 2>&1 || die "required capacity ISO tool is unavailable: $tool"
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
    die "required capacity ISO GRUB module is missing or a symlink: $GRUB_EFI_DIRECTORY/$module"
  fi
done

if [[ -L "$OUTPUT" || ( -e "$OUTPUT" && ! -f "$OUTPUT" ) ]]; then
  die "capacity ISO output is a symlink or non-regular path: $OUTPUT"
fi
if [[ -e "$OUTPUT" ]]; then
  die "refusing to clobber existing capacity ISO output: $OUTPUT"
fi
mkdir -p -- "$ISO_ROOT" "$KERNEL_BUILD_DIR" "$(dirname -- "$OUTPUT")"
WORK_DIR=$(mktemp -d "$ISO_ROOT/.capacity-iso-build.XXXXXX")
chmod 0700 "$WORK_DIR"
STAGE="$WORK_DIR/stage"
CANDIDATE="$WORK_DIR/candidate.iso"
LOCK_RECORD="$WORK_DIR/capacity-lock.json"
INSPECTION_RECORD="$WORK_DIR/candidate-inspection.json"
mkdir -m 0700 "$STAGE"

"$PYTHON" "$FOREIGN_LAB" --guest-dir "$GUEST_ROOT" verify \
  --guest linux-alpine-3.24.1-x86_64 \
  --guest guix-system-1.5.0-x86_64 \
  --guest plan9-9front-11983-amd64 \
  --guest redox-0.9.0-server-x86_64 \
  --guest openbsd-7.9-amd64 >/dev/null

OCORE_BOOT_INFO_ENABLED=1 OCORE_PROBE_MODE=0 OCORE_BUILD_DIR="$KERNEL_BUILD_DIR" \
  "$KERNEL_BUILD_SCRIPT" >/dev/null
OCORE_KERNEL="$KERNEL_BUILD_DIR/kernel.elf"
[[ -f "$OCORE_KERNEL" && ! -L "$OCORE_KERNEL" ]] \
  || die "O-core build did not produce a regular kernel ELF"

install_artifact() {
  local source=$1 destination=$2
  [[ -f "$source" && ! -L "$source" ]] || die "capacity artifact is missing or a symlink: $source"
  mkdir -p -- "$(dirname -- "$STAGE/$destination")"
  install -m 0444 "$source" "$STAGE/$destination"
}

install_artifact "$OCORE_KERNEL" boot/entry/000-ostadix/kernel.elf
install_artifact "$GUEST_ROOT/alpine-3.24.1-x86_64/vmlinuz-virt" \
  boot/capacity-host/vmlinuz-virt
install_artifact "$GUEST_ROOT/alpine-3.24.1-x86_64/initramfs-virt" \
  boot/entry/010-alpine/initramfs-virt
install_artifact "$CAPACITY_HOST_INITRAMFS" boot/capacity-host/initramfs.cpio.gz
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
  >"$LOCK_RECORD"
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
  || die "grub-mkrescue did not produce a private capacity ISO candidate"
"$PYTHON" "$ISO_TOOL" inspect "$CANDIDATE" >"$INSPECTION_RECORD"
"$PYTHON" "$ISO_TOOL" publish --source "$CANDIDATE" --output "$OUTPUT" >/dev/null
"$PYTHON" "$ISO_TOOL" inspect "$OUTPUT" >"$WORK_DIR/published-inspection.json"
"$PYTHON" - "$OUTPUT" "$WORK_DIR/published-inspection.json" <<'PY'
import json
from pathlib import Path
import sys

output = sys.argv[1]
metadata = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
print(f"capacity-iso-output: {output}")
print(f"capacity-iso-bytes: {metadata['bytes']}")
print(f"capacity-iso-sha256: {metadata['sha256']}")
print(f"capacity-lock-sha256: {metadata['capacity_lock_sha256']}")
print(f"capacity-entry-count: {len(metadata['entries'])}")
PY
