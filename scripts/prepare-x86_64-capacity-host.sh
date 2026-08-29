#!/usr/bin/env bash
# Build the x86_64 Linux initramfs used by hosted live and, in the default
# virt-kernel mode, by the absorbed foreign-system laboratory.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
OUTPUT=${1:-"$ROOT/target/ostadix-capacity-host/x86_64/initramfs.cpio.gz"}
GUEST_ROOT=${OSTADIX_GUEST_ROOT:-"${XDG_DATA_HOME:-$HOME/.local/share}/ostadix/guests"}
ALPINE_KERNEL_FLAVOR=${OSTADIX_CAPACITY_HOST_KERNEL_FLAVOR:-virt}
ALPINE_MINIROOTFS_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/alpine-minirootfs-3.24.1-x86_64.tar.gz
ALPINE_MINIROOTFS_BYTES=3698422
ALPINE_MINIROOTFS_SHA256=41f73e3cf5fa919b8aa5ca6b30dc48f0da2720776d7423e2a7748211456fe081
case "$ALPINE_KERNEL_FLAVOR" in
  virt)
    ALPINE_INITRAMFS=${OSTADIX_CAPACITY_HOST_BASE_INITRAMFS:-"$GUEST_ROOT/alpine-3.24.1-x86_64/initramfs-virt"}
    ALPINE_KERNEL_RELEASE=6.18.35-0-virt
    ALPINE_MODLOOP_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot-3.24.1/modloop-virt
    ALPINE_MODLOOP_BYTES=22867968
    ALPINE_MODLOOP_SHA256=78907e7cc812d555f08d4e1133d090cf11fa197370882adfe67b0a5986ccb3f9
    ;;
  lts)
    ALPINE_INITRAMFS=${OSTADIX_CAPACITY_HOST_BASE_INITRAMFS:-"$GUEST_ROOT/alpine-3.24.1-x86_64/initramfs-lts"}
    ALPINE_KERNEL_RELEASE=6.18.35-0-lts
    ALPINE_MODLOOP_URL=
    ALPINE_MODLOOP_BYTES=
    ALPINE_MODLOOP_SHA256=
    ;;
  *)
    printf 'error: OSTADIX_CAPACITY_HOST_KERNEL_FLAVOR must be virt or lts\n' >&2
    exit 1
    ;;
esac
CACHE_ROOT=${OSTADIX_CAPACITY_HOST_CACHE:-"${XDG_CACHE_HOME:-$HOME/.cache}/ostadix/capacity-host"}
HOSTED_BIN_DIR=${OSTADIX_HOSTED_BIN_DIR:-"$ROOT/target/ostadix-hosted/x86_64/bin"}
HOSTED_SOURCE_ROOT=${OSTADIX_HOSTED_SOURCE_ROOT:-"$ROOT"}
HOSTED_REVISION=${OSTADIX_HOSTED_REVISION:-}
PACKAGE_LOCK=${OSTADIX_CAPACITY_HOST_PACKAGE_LOCK:-"$ROOT/evidence/hosted_live_apk_packages.txt"}
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-315532800}
PYTHON=${OSTADIX_PYTHON:-python3}
WORK_DIR=

usage() {
  cat <<'USAGE'
Usage: prepare-x86_64-capacity-host.sh [OUTPUT]

Build the Alpine-based x86_64 initramfs that provides the hosted Ostadix live
CLI. The default virt flavor also launches Guix, OpenBSD, 9front, and Redox
capacity images through local QEMU TCG. The lts flavor uses Alpine's upstream
hardware-oriented initramfs and is the physical Hosted Live release substrate.

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
if [[ -L "$HOSTED_BIN_DIR" || ! -d "$HOSTED_BIN_DIR" ]]; then
  die "hosted Ostadix binary directory is missing or a symlink: $HOSTED_BIN_DIR"
fi
for binary in O o-cli olangc o-link; do
  if [[ -L "$HOSTED_BIN_DIR/$binary" || ! -f "$HOSTED_BIN_DIR/$binary" \
      || ! -x "$HOSTED_BIN_DIR/$binary" ]]; then
    die "required hosted Ostadix x86_64 binary is unavailable: $HOSTED_BIN_DIR/$binary"
  fi
done
"$PYTHON" - "$HOSTED_BIN_DIR" <<'PY'
from pathlib import Path
import struct
import sys

root = Path(sys.argv[1])
for name in ("O", "o-cli", "olangc", "o-link"):
    path = root / name
    with path.open("rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:6] != b"\x7fELF\x02\x01":
            raise SystemExit(f"error: hosted Ostadix binary is not ELF64 little-endian: {path}")
        if struct.unpack_from("<H", header, 18)[0] != 62:
            raise SystemExit(f"error: hosted Ostadix binary is not x86_64: {path}")
        program_offset = struct.unpack_from("<Q", header, 32)[0]
        program_size = struct.unpack_from("<H", header, 54)[0]
        program_count = struct.unpack_from("<H", header, 56)[0]
        if program_size < 56 or program_count > 256:
            raise SystemExit(f"error: hosted Ostadix ELF program table is invalid: {path}")
        stream.seek(program_offset)
        for _ in range(program_count):
            program = stream.read(program_size)
            if len(program) != program_size:
                raise SystemExit(f"error: hosted Ostadix ELF program table is truncated: {path}")
            if struct.unpack_from("<I", program)[0] == 3:
                raise SystemExit(f"error: hosted Ostadix binary is dynamically linked: {path}")
PY
for source in "$HOSTED_SOURCE_ROOT/backends" "$HOSTED_SOURCE_ROOT/examples"; do
  if [[ -L "$source" || ! -d "$source" ]]; then
    die "hosted Ostadix source directory is missing or a symlink: $source"
  fi
done
if [[ ! "$HOSTED_REVISION" =~ ^[0-9a-f]{40}$ ]]; then
  die "OSTADIX_HOSTED_REVISION must be the exact 40-character source commit"
fi
if [[ -L "$PACKAGE_LOCK" || ! -f "$PACKAGE_LOCK" ]]; then
  die "hosted-live package lock is missing or a symlink: $PACKAGE_LOCK"
fi
mapfile -t PACKAGE_SPECS < <(
  "$PYTHON" - "$PACKAGE_LOCK" <<'PY'
from pathlib import Path
import re
import sys

path = Path(sys.argv[1])
values = [
    line.strip()
    for line in path.read_text(encoding="utf-8").splitlines()
    if line.strip() and not line.lstrip().startswith("#")
]
if not values or values != sorted(set(values)):
    raise SystemExit("error: hosted-live package lock must be nonempty, sorted, and unique")
for value in values:
    if not re.fullmatch(r"[a-z0-9][a-z0-9+_.-]*=[A-Za-z0-9][A-Za-z0-9+_.-]*", value):
        raise SystemExit(f"error: invalid hosted-live package lock entry: {value}")
print("\n".join(values))
PY
)
if (( ${#PACKAGE_SPECS[@]} == 0 )); then
  die "hosted-live package lock resolved to an empty closure"
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
if [[ "$ALPINE_KERNEL_FLAVOR" == virt ]]; then
  fetch_pinned "$MODLOOP" "$ALPINE_MODLOOP_URL" \
    "$ALPINE_MODLOOP_BYTES" "$ALPINE_MODLOOP_SHA256" alpine-modloop-virt
fi

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
if [[ "$ALPINE_KERNEL_FLAVOR" == virt ]]; then
  # The virt netboot initramfs omits optical-media drivers. Import its exact
  # matching modloop so the laboratory host can remount a directly attached ISO.
  unsquashfs -f -d "$STAGE/usr/lib" "$MODLOOP" \
    "modules/$ALPINE_KERNEL_RELEASE" >/dev/null
  for module in \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/cdrom/cdrom.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/scsi/sr_mod.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/fs/isofs/isofs.ko"; do
    [[ -f "$module" && ! -L "$module" ]] \
      || die "pinned Alpine modloop omitted required optical-media module: $module"
  done
else
  # Alpine's LTS netboot initramfs is already a bounded matching module set.
  # Require the pieces needed for a USB keyboard and common xHCI controllers;
  # the LTS kernel supplies simpledrm/framebuffer console support built in.
  for module in \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/hid/hid.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/hid/hid-generic.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/hid/usbhid/usbhid.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/usb/host/xhci-hcd.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/usb/host/xhci-pci.ko"; do
    [[ -f "$module" && ! -L "$module" ]] \
      || die "pinned Alpine LTS initramfs omitted required physical-input module: $module"
  done
  [[ -f "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/modules.dep" ]] \
    || die "pinned Alpine LTS initramfs omitted modules.dep"
fi
if [[ ! -e "$STAGE/lib/modules" && ! -L "$STAGE/lib/modules" ]]; then
  ln -s ../usr/lib/modules "$STAGE/lib/modules"
fi
printf '%s\n%s\n' \
  'https://dl-cdn.alpinelinux.org/alpine/v3.24/main' \
  'https://dl-cdn.alpinelinux.org/alpine/v3.24/community' \
  >"$STAGE/etc/apk/repositories"
cp --remove-destination /etc/resolv.conf "$STAGE/etc/resolv.conf"

if ! chroot "$STAGE" /sbin/apk --no-cache --no-scripts add "${PACKAGE_SPECS[@]}"; then
  die "Alpine failed to resolve the exact locked v3.24 capacity-host closure"
fi
"$PYTHON" - "$STAGE/lib/apk/db/installed" \
  >"$STAGE/usr/share/ostadix-capacity-host-packages.txt" <<'PY'
from pathlib import Path
import sys

records = Path(sys.argv[1]).read_text(encoding="utf-8").strip().split("\n\n")
packages = []
for record in records:
    fields = {}
    for line in record.splitlines():
        if len(line) >= 3 and line[1] == ":":
            fields[line[0]] = line[2:]
    if "P" in fields and "V" in fields:
        packages.append(f"{fields['P']}={fields['V']}")
print("\n".join(sorted(packages)))
PY
printf '%s\n' "${PACKAGE_SPECS[@]}" >"$WORK_DIR/expected-packages.txt"
cmp -s "$WORK_DIR/expected-packages.txt" \
  "$STAGE/usr/share/ostadix-capacity-host-packages.txt" \
  || die "resolved Alpine package closure differs from hosted-live package lock"
# apk.log contains wall-clock progress timestamps and has no runtime value.
# Excluding it makes equal package closures produce byte-identical initramfses.
rm -f -- "$STAGE/var/log/apk.log"

install -d -m 0755 "$STAGE/usr/local/bin" "$STAGE/opt/ostadix/backends" \
  "$STAGE/opt/ostadix/examples" "$STAGE/usr/share/ostadix"
install -m 0444 "$PACKAGE_LOCK" "$STAGE/usr/share/ostadix/hosted-live-apk-packages.txt"
# The host resolver is needed only while apk runs. Do not embed VM-specific DNS
# search domains or nameservers into the immutable live image.
printf 'nameserver 1.1.1.1\noptions timeout:2 attempts:2\n' >"$STAGE/etc/resolv.conf"
for binary in O o-cli olangc o-link; do
  install -m 0555 "$HOSTED_BIN_DIR/$binary" "$STAGE/usr/local/bin/$binary"
done
cp -R --no-preserve=ownership "$HOSTED_SOURCE_ROOT/backends/." \
  "$STAGE/opt/ostadix/backends/"
for example in hello.O shell_hello.O bash_hello.O sql_select.O; do
  install -m 0444 "$HOSTED_SOURCE_ROOT/examples/$example" \
    "$STAGE/opt/ostadix/examples/$example"
done
tee "$STAGE/usr/local/bin/o" >/dev/null <<'O_WRAPPER'
#!/bin/sh
set -eu
case "${1:-}" in
  run|plan|explain|inspect|help|--help|-h) exec o-cli "$@" ;;
  *) exec O "$@" ;;
esac
O_WRAPPER
chmod 0555 "$STAGE/usr/local/bin/o"
{
  printf 'schema=ostadix.hosted-live/v1\n'
  printf 'architecture=x86_64\n'
  printf 'hosted_binary_revision=%s\n' "$HOSTED_REVISION"
  printf 'package_lock.bytes=%s\n' "$(wc -c <"$PACKAGE_LOCK" | tr -d ' ')"
  printf 'package_lock.sha256=%s\n' "$(sha256sum "$PACKAGE_LOCK" | awk '{print $1}')"
  for binary in O o-cli olangc o-link; do
    binary_sha=$(sha256sum "$STAGE/usr/local/bin/$binary" | awk '{print $1}')
    binary_bytes=$(wc -c <"$STAGE/usr/local/bin/$binary" | tr -d ' ')
    printf 'binary.%s.bytes=%s\n' "$binary" "$binary_bytes"
    printf 'binary.%s.sha256=%s\n' "$binary" "$binary_sha"
  done
} >"$STAGE/usr/share/ostadix/hosted-live-manifest.txt"
chmod 0444 "$STAGE/usr/share/ostadix/hosted-live-manifest.txt"

install -d -m 0755 "$STAGE/media/ostadix" "$STAGE/proc" "$STAGE/sys" \
  "$STAGE/dev" "$STAGE/run" "$STAGE/tmp" "$STAGE/root"
chmod 01777 "$STAGE/tmp"
tee "$STAGE/init" >/dev/null <<'INIT'
#!/bin/sh
set -eu

export HOME=/root
export PATH=/usr/local/bin:/sbin:/bin:/usr/sbin:/usr/bin
export O_BACKENDS_DIR=/opt/ostadix/backends
export TERM=${TERM:-linux}
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null || {
  mount -t tmpfs -o mode=0755 tmpfs /dev
  mdev -s
}
mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /run
mount -t tmpfs -o mode=1777,nosuid,nodev tmpfs /tmp

emit_line() {
  for output in /dev/tty0 /dev/ttyS0; do
    if [ -c "$output" ]; then
      printf '%s\n' "$*" >"$output" 2>/dev/null || true
    fi
  done
}

emit_error() {
  emit_line "$*"
}

hosted_shell() {
  cat >/run/ostadix-live-shell <<'SHELL'
#!/bin/sh
cd /opt/ostadix
export HOME=/root
export O_BACKENDS_DIR=/opt/ostadix/backends
export PATH=/usr/local/bin:/sbin:/bin:/usr/sbin:/usr/bin
export PS1='ostadix-live:\w# '
exec /bin/sh -i
SHELL
  chmod 0755 /run/ostadix-live-shell
  if command -v openvt >/dev/null 2>&1 && [ -c /dev/tty1 ]; then
    while :; do
      openvt -c 1 -s -w /run/ostadix-live-shell || true
      emit_error 'OSTADIX HOSTED LIVE: visible shell exited; restarting tty1'
      sleep 1
    done
  fi
  emit_error 'OSTADIX HOSTED LIVE: openvt unavailable; using /dev/console fallback'
  exec /run/ostadix-live-shell
}

for module in \
  ata_piix ahci nvme xhci_hcd xhci_pci usbhid hid_generic simpledrm \
  cdrom sr_mod isofs; do
  modprobe "$module" 2>/dev/null || true
done

selected=
for argument in $(cat /proc/cmdline); do
  case "$argument" in
    ostadix.capacity=*) selected=${argument#ostadix.capacity=} ;;
  esac
done

if [ "$selected" = hosted ]; then
  cd /opt/ostadix
  smoke_output=
  if smoke_output=$(O /opt/ostadix/examples/hello.O "$O_BACKENDS_DIR" 2>&1) \
      && [ "$smoke_output" = '[number] 2' ]; then
    emit_line 'OSTADIX HOSTED O SMOKE: PASS'
  else
    emit_error "OSTADIX HOSTED O SMOKE: FAIL: $smoke_output"
    hosted_shell
  fi
  if [ "$(bash -lc 'printf ostadix-bash')" = ostadix-bash ]; then
    emit_line 'OSTADIX HOSTED BASH: PASS'
  else
    emit_error 'OSTADIX HOSTED BASH: FAIL'
    hosted_shell
  fi
  if [ "$(sqlite3 ':memory:' 'select 1 + 1;')" = 2 ]; then
    emit_line 'OSTADIX HOSTED SQLITE: PASS'
  else
    emit_error 'OSTADIX HOSTED SQLITE: FAIL'
    hosted_shell
  fi
  if olangc /opt/ostadix/examples/hello.O --target ir \
      --shim-dir "$O_BACKENDS_DIR" >/tmp/ostadix-hello.ir 2>/tmp/ostadix-olangc.err \
      && [ -s /tmp/ostadix-hello.ir ]; then
    emit_line 'OSTADIX HOSTED OLANGC IR: PASS'
  else
    emit_error 'OSTADIX HOSTED OLANGC IR: FAIL'
    cat /tmp/ostadix-olangc.err >&2 2>/dev/null || true
    hosted_shell
  fi
  if o-cli --help 2>&1 | grep -q '^Usage: o-cli'; then
    emit_line 'OSTADIX HOSTED O-CLI: PASS'
  else
    emit_error 'OSTADIX HOSTED O-CLI: FAIL'
    hosted_shell
  fi
  if o-link --literal /opt/ostadix/examples/hello.O \
      -o /tmp/ostadix-linked.O >/tmp/ostadix-olink.out 2>/tmp/ostadix-olink.err \
      && [ -s /tmp/ostadix-linked.O ]; then
    emit_line 'OSTADIX HOSTED O-LINK: PASS'
  else
    emit_error 'OSTADIX HOSTED O-LINK: FAIL'
    cat /tmp/ostadix-olink.err >&2 2>/dev/null || true
    hosted_shell
  fi
  emit_line 'OSTADIX HOSTED LIVE READY'
  emit_line 'Try: O /opt/ostadix/examples/hello.O "$O_BACKENDS_DIR"'
  emit_line '     O --repl "$O_BACKENDS_DIR"'
  emit_line '     olangc /opt/ostadix/examples/hello.O --target ir --shim-dir "$O_BACKENDS_DIR"'
  hosted_shell
fi

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
