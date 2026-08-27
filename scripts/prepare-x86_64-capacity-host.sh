#!/usr/bin/env bash
# Build the x86_64 Linux initramfs used to launch absorbed foreign systems.
# This is an explicit networked preparation step. The capacity ISO build itself
# consumes the resulting immutable file and performs no package downloads.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
OUTPUT=${1:-"$ROOT/target/ostadix-capacity-host/x86_64/initramfs.cpio.gz"}
GUEST_ROOT=${OSTADIX_GUEST_ROOT:-"${XDG_DATA_HOME:-$HOME/.local/share}/ostadix/guests"}
ALPINE_INITRAMFS=${OSTADIX_CAPACITY_HOST_BASE_INITRAMFS:-"$GUEST_ROOT/alpine-3.24.1-x86_64/initramfs-virt"}
ALPINE_MINIROOTFS_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/alpine-minirootfs-3.24.1-x86_64.tar.gz
ALPINE_MINIROOTFS_BYTES=3698422
ALPINE_MINIROOTFS_SHA256=41f73e3cf5fa919b8aa5ca6b30dc48f0da2720776d7423e2a7748211456fe081
ALPINE_KERNEL_RELEASE=6.18.35-0-virt
ALPINE_MODLOOP_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot-3.24.1/modloop-virt
ALPINE_MODLOOP_BYTES=22867968
ALPINE_MODLOOP_SHA256=78907e7cc812d555f08d4e1133d090cf11fa197370882adfe67b0a5986ccb3f9
CACHE_ROOT=${OSTADIX_CAPACITY_HOST_CACHE:-"${XDG_CACHE_HOME:-$HOME/.cache}/ostadix/capacity-host"}
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-315532800}
PYTHON=${OSTADIX_PYTHON:-python3}
WORK_DIR=

usage() {
  cat <<'USAGE'
Usage: prepare-x86_64-capacity-host.sh [OUTPUT]

Build the Alpine-based x86_64 initramfs that launches Guix, OpenBSD, 9front,
and Redox capacity images through local QEMU TCG. Run this script on Linux as
root (or through sudo) after fetching the x86_64 Alpine foreign-lab guest.

This command accesses Alpine's HTTPS repositories. The later capacity ISO
build forces Cargo offline, downloads no guest media, and binds the resulting
initramfs by size and SHA-256.
USAGE
}

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

if [[ $# -gt 1 ]]; then
  usage >&2
  exit 2
fi
if [[ $(id -u) -ne 0 ]]; then
  die "capacity-host preparation must run as root so the initramfs preserves device metadata"
fi
if [[ ! "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]] \
    || (( SOURCE_DATE_EPOCH < 315532800 || SOURCE_DATE_EPOCH > 2147483647 )); then
  die "SOURCE_DATE_EPOCH must be an integer from 315532800 through 2147483647"
fi
for tool in curl cpio gzip sha256sum tar unsquashfs "$PYTHON"; do
  command -v "$tool" >/dev/null 2>&1 || die "required capacity-host tool is unavailable: $tool"
done
if [[ -L "$ALPINE_INITRAMFS" || ! -f "$ALPINE_INITRAMFS" ]]; then
  die "pinned Alpine x86_64 initramfs is missing or a symlink: $ALPINE_INITRAMFS"
fi

mkdir -p -- "$CACHE_ROOT" "$(dirname -- "$OUTPUT")"
if [[ -L "$CACHE_ROOT" || ! -d "$CACHE_ROOT" ]]; then
  die "capacity-host cache must be a non-symlink directory: $CACHE_ROOT"
fi
MINIROOTFS="$CACHE_ROOT/alpine-minirootfs-3.24.1-x86_64.tar.gz"
MODLOOP="$CACHE_ROOT/modloop-virt-3.24.1-x86_64"

verify_file() {
  local path=$1 expected_bytes=$2 expected_sha=$3 actual_bytes actual_sha
  [[ ! -L "$path" && -f "$path" ]] || return 1
  actual_bytes=$(wc -c <"$path" | tr -d ' ')
  actual_sha=$(sha256sum "$path" | awk '{print $1}')
  [[ "$actual_bytes" == "$expected_bytes" && "$actual_sha" == "$expected_sha" ]]
}

fetch_pinned() {
  local path=$1 url=$2 expected_bytes=$3 expected_sha=$4 label=$5 candidate
  if verify_file "$path" "$expected_bytes" "$expected_sha"; then
    return
  fi
  candidate="$CACHE_ROOT/.$label.$$.partial"
  rm -f -- "$candidate"
  curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
    --output "$candidate" "$url"
  verify_file "$candidate" "$expected_bytes" "$expected_sha" \
    || die "downloaded $label failed its exact size/SHA-256 pin"
  chmod 0444 "$candidate"
  mv -f -- "$candidate" "$path"
}

fetch_pinned "$MINIROOTFS" "$ALPINE_MINIROOTFS_URL" \
  "$ALPINE_MINIROOTFS_BYTES" "$ALPINE_MINIROOTFS_SHA256" alpine-minirootfs
fetch_pinned "$MODLOOP" "$ALPINE_MODLOOP_URL" \
  "$ALPINE_MODLOOP_BYTES" "$ALPINE_MODLOOP_SHA256" alpine-modloop-virt

WORK_DIR=$(mktemp -d "$(dirname -- "$OUTPUT")/.capacity-host.XXXXXX")
STAGE="$WORK_DIR/root"
CANDIDATE="$WORK_DIR/initramfs.cpio.gz"
mkdir -p -- "$STAGE"

# The upstream initramfs supplies the exact kernel's storage and ISO9660
# modules. The minirootfs supplies apk, trust roots, musl, and a normal Alpine
# userspace. Package scripts are disabled because this is an initramfs closure,
# not a persistent Alpine installation.
(
  cd "$STAGE"
  gzip -dc "$ALPINE_INITRAMFS" | cpio --quiet -idmu --no-absolute-filenames
  tar -xzf "$MINIROOTFS"
)
# The netboot initramfs intentionally omits optical-media drivers. Import the
# exact matching virt modloop so the standalone capacity host can mount the
# same read-only ISO after GRUB hands control to Linux.
unsquashfs -f -d "$STAGE/usr/lib" "$MODLOOP" \
  "modules/$ALPINE_KERNEL_RELEASE" >/dev/null
for module in \
  "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/cdrom/cdrom.ko" \
  "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/scsi/sr_mod.ko" \
  "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/fs/isofs/isofs.ko"; do
  [[ -f "$module" && ! -L "$module" ]] \
    || die "pinned Alpine modloop omitted required optical-media module: $module"
done
if [[ -e "$STAGE/lib/modules" || -L "$STAGE/lib/modules" ]]; then
  die "capacity-host stage unexpectedly already defines /lib/modules"
fi
ln -s ../usr/lib/modules "$STAGE/lib/modules"
printf '%s\n%s\n' \
  'https://dl-cdn.alpinelinux.org/alpine/v3.24/main' \
  'https://dl-cdn.alpinelinux.org/alpine/v3.24/community' \
  >"$STAGE/etc/apk/repositories"
cp --remove-destination /etc/resolv.conf "$STAGE/etc/resolv.conf"

if ! chroot "$STAGE" /sbin/apk --no-cache --no-scripts add \
  qemu-system-x86_64 qemu-ui-curses; then
  die "Alpine failed to resolve the configured v3.24 capacity-host closure"
fi
chroot "$STAGE" /sbin/apk info -vv | LC_ALL=C sort \
  >"$STAGE/usr/share/ostadix-capacity-host-packages.txt"
# apk.log contains wall-clock progress timestamps and has no runtime value.
# Excluding it makes equal package closures produce byte-identical initramfses.
rm -f -- "$STAGE/var/log/apk.log"

install -d -m 0755 "$STAGE/media/ostadix" "$STAGE/proc" "$STAGE/sys" \
  "$STAGE/dev" "$STAGE/run" "$STAGE/tmp" "$STAGE/root"
chmod 01777 "$STAGE/tmp"
tee "$STAGE/init" >/dev/null <<'INIT'
#!/bin/sh
set -eu

export HOME=/root PATH=/sbin:/bin:/usr/sbin:/usr/bin TERM=${TERM:-linux}
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null || {
  mount -t tmpfs -o mode=0755 tmpfs /dev
  mdev -s
}
mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /run
mount -t tmpfs -o mode=1777,nosuid,nodev tmpfs /tmp

for module in ata_piix ahci cdrom sr_mod isofs; do
  modprobe "$module" 2>/dev/null || true
done

media=
for device in /dev/sr0 /dev/cdrom /dev/sda /dev/vda; do
  if [ -b "$device" ] && mount -t iso9660 -o ro "$device" /media/ostadix 2>/dev/null; then
    if [ -f /media/ostadix/ostadix/capacity.lock.json ]; then
      media=/media/ostadix
      break
    fi
    umount /media/ostadix || true
  fi
done
if [ -z "$media" ]; then
  echo 'OSTADIX CAPACITY HOST ERROR: capacity ISO could not be mounted' >&2
  exec sh
fi

selected=
for argument in $(cat /proc/cmdline); do
  case "$argument" in
    ostadix.capacity=*) selected=${argument#ostadix.capacity=} ;;
  esac
done

common_args='-accel tcg -machine q35 -cpu qemu64 -smp 1 -nic none -monitor none -no-reboot'
echo "OSTADIX CAPACITY HOST READY: $selected"

case "$selected" in
  guix-system-1.5.0-x86_64)
    echo "OSTADIX CAPACITY LAUNCH: $selected adapter=qemu-tcg-linux-direct"
    # shellcheck disable=SC2086
    exec qemu-system-x86_64 $common_args -m 1024M -display none -serial stdio \
      -kernel "$media/ostadix/guix/linux-libre-6.17.12-bzimage" \
      -initrd "$media/ostadix/guix/guix-1.5.0-initrd.cpio.gz" \
      -append 'root=31393730-3031-3031-3139-313133333833 gnu.system=/gnu/store/63qbjpi3vph650zyr1mayjpgh8h39vj3-system gnu.load=/gnu/store/63qbjpi3vph650zyr1mayjpgh8h39vj3-system/boot modprobe.blacklist=radeon,amdgpu console=tty0 console=ttyS0,115200n8 earlycon=uart8250,io,0x3f8,115200n8 loglevel=7' \
      -drive "if=none,id=cd0,media=cdrom,format=raw,readonly=on,file=$media/ostadix/guix/guix-system-install-1.5.0.x86_64-linux.iso" \
      -device ide-cd,drive=cd0,bus=ide.0
    ;;
  plan9-9front-11983-amd64)
    echo "OSTADIX CAPACITY LAUNCH: $selected adapter=qemu-tcg-qcow2"
    echo 'At bootargs choose local!/dev/sdF0/fs, then accept user glenda.'
    # shellcheck disable=SC2086
    exec qemu-system-x86_64 $common_args -m 512M -display none -serial stdio \
      -boot order=c,strict=on \
      -drive "file=$media/ostadix/9front/9front-11983.amd64.qcow2,if=none,id=disk0,media=disk,readonly=on,snapshot=on,format=qcow2" \
      -device virtio-blk-pci,drive=disk0
    ;;
  redox-0.9.0-server-x86_64)
    echo "OSTADIX CAPACITY LAUNCH: $selected adapter=qemu-tcg-raw-cd"
    # shellcheck disable=SC2086
    exec qemu-system-x86_64 $common_args -m 1024M -display none -serial stdio \
      -boot order=d,strict=on -nodefaults \
      -drive "if=none,id=cd0,media=cdrom,format=raw,readonly=on,file=$media/ostadix/redox/redox-server-0.9.0-livedisk.iso" \
      -device ide-cd,drive=cd0,bus=ide.0
    ;;
  openbsd-7.9-amd64)
    echo "OSTADIX CAPACITY LAUNCH: $selected adapter=qemu-tcg-raw-cd-curses"
    # shellcheck disable=SC2086
    exec qemu-system-x86_64 $common_args -m 1024M -display curses -serial none \
      -boot order=d,strict=on \
      -drive "if=none,id=cd0,media=cdrom,format=raw,readonly=on,file=$media/ostadix/openbsd/install79.iso" \
      -device ide-cd,drive=cd0,bus=ide.0
    ;;
  shell|'')
    echo 'OSTADIX CAPACITY HOST SHELL: no foreign system selected'
    exec sh
    ;;
  *)
    echo "OSTADIX CAPACITY HOST ERROR: unknown selection '$selected'" >&2
    exec sh
    ;;
esac

echo "OSTADIX CAPACITY EXITED: $selected status=$?" >&2
exec sh
INIT
chmod 0755 "$STAGE/init"

find "$STAGE" -xdev -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
(
  cd "$STAGE"
  find . -xdev -print0 | LC_ALL=C sort -z \
    | cpio --quiet --null --reproducible -o -H newc \
    | gzip -n -9 >"$CANDIDATE"
)
chmod 0444 "$CANDIDATE"

metadata=$($PYTHON - "$CANDIDATE" "$STAGE/usr/share/ostadix-capacity-host-packages.txt" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

image = Path(sys.argv[1])
packages = Path(sys.argv[2]).read_text(encoding="utf-8").splitlines()
digest = hashlib.sha256()
size = 0
with image.open("rb") as stream:
    while chunk := stream.read(1024 * 1024):
        digest.update(chunk)
        size += len(chunk)
print(json.dumps({
    "schema": "ostadix.capacity-host-initramfs/v1",
    "architecture": "x86_64",
    "bytes": size,
    "sha256": digest.hexdigest(),
    "packages": packages,
}, sort_keys=True, separators=(",", ":")))
PY
)
mv -f -- "$CANDIDATE" "$OUTPUT"
chmod 0444 "$OUTPUT"
printf '%s\n' "$metadata"
printf 'capacity-host-initramfs: %s\n' "$OUTPUT"
