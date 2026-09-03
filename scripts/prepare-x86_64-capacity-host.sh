#!/usr/bin/env bash
# Build the x86_64 Linux initramfs used by hosted live and, in the default
# virt-kernel mode, by the absorbed foreign-system laboratory.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
OUTPUT=${1:-"$ROOT/target/ostadix-capacity-host/x86_64/initramfs.cpio.gz"}
ROOTFS_OUTPUT=${OSTADIX_CAPACITY_HOST_ROOTFS_OUTPUT:-}
VENTOY_MODLOOP_OUTPUT=${OSTADIX_CAPACITY_HOST_VENTOY_MODLOOP_OUTPUT:-}
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
    ALPINE_MODLOOP_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot-3.24.1/modloop-lts
    ALPINE_MODLOOP_BYTES=303034368
    ALPINE_MODLOOP_SHA256=871ef51ed6378283db9462947bb7fb84c1ec31376611eb1a2281b02b9404c0f6
    ;;
  *)
    printf 'error: OSTADIX_CAPACITY_HOST_KERNEL_FLAVOR must be virt or lts\n' >&2
    exit 1
    ;;
esac
CACHE_ROOT=${OSTADIX_CAPACITY_HOST_CACHE:-"${XDG_CACHE_HOME:-$HOME/.cache}/ostadix/capacity-host"}
HOSTED_BIN_DIR=${OSTADIX_HOSTED_BIN_DIR:-"$ROOT/target/ostadix-hosted/x86_64/bin"}
HOSTED_SOURCE_ROOT=${OSTADIX_HOSTED_SOURCE_ROOT:-"$ROOT"}
HOSTED_BASE_COMMIT=${OSTADIX_HOSTED_BASE_COMMIT:-}
HOSTED_REVISION=${OSTADIX_HOSTED_REVISION:-}
HOSTED_SOURCE_ARCHIVE_SHA256=${OSTADIX_HOSTED_SOURCE_ARCHIVE_SHA256:-}
HOSTED_BOOT_OBJECTS_ARCHIVE=${OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE:-}
HOSTED_BOOT_OBJECTS_ARCHIVE_SHA256=${OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE_SHA256:-}
HOSTED_BOOT_OBJECTS_RESULT=${OSTADIX_HOSTED_BOOT_OBJECTS_RESULT:-}
HOSTED_CARGO_VENDOR_DIR=${OSTADIX_HOSTED_CARGO_VENDOR_DIR:-}
HOSTED_CARGO_VENDOR_MANIFEST=${OSTADIX_HOSTED_CARGO_VENDOR_MANIFEST:-}
HOSTED_WASM_ARTIFACT=${OSTADIX_HOSTED_WASM_ARTIFACT:-}
HOSTED_WASM_MANIFEST=${OSTADIX_HOSTED_WASM_MANIFEST:-}
HOSTED_WASM_PROJECT=${OSTADIX_HOSTED_WASM_PROJECT:-}
HOSTED_DESKTOP_HELPER="$HOSTED_SOURCE_ROOT/scripts/ostadix-hosted-live-desktop.sh"
PACKAGE_LOCK=${OSTADIX_CAPACITY_HOST_PACKAGE_LOCK:-"$ROOT/evidence/hosted_live_apk_packages.txt"}
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-315532800}
PYTHON=${OSTADIX_PYTHON:-python3}
WORK_DIR=
HOSTED_LEGACY_BINARIES=(
  O o-cli olangc o-link
)
HOSTED_STANDARD_BINARIES=(
  O o-cli olangc ocorec o-link o-unlink ogit o-live-host o-node octl
  o-registry o-info ostadix-device
)
HOSTED_ROOT_BINARIES=(
  O o-cli olangc ocorec o-link o-unlink o-notebook ogit o-live-host o-node
  octl o-registry o-info ostadix-device ocore-kernel-world-record
)
HOSTED_BINARIES=("${HOSTED_ROOT_BINARIES[@]}" ostadix-mcp)
if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
  HOSTED_IMAGE_BINARIES=("${HOSTED_BINARIES[@]}")
else
  HOSTED_IMAGE_BINARIES=("${HOSTED_LEGACY_BINARIES[@]}")
fi

usage() {
  cat <<'USAGE'
Usage: prepare-x86_64-capacity-host.sh [OUTPUT]

Build the Alpine-based x86_64 boot payload that provides the Ostadix Hosted
Workstation. The default virt flavor also launches Guix, OpenBSD, 9front, and Redox
capacity images through local QEMU TCG. The lts flavor emits a small bootstrap
initramfs, the deterministic workstation SquashFS named by
OSTADIX_CAPACITY_HOST_ROOTFS_OUTPUT, and the minimal Ventoy-compatible Alpine
modloop named by OSTADIX_CAPACITY_HOST_VENTOY_MODLOOP_OUTPUT.

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
for tool in curl cpio du gzip sha256sum tar unsquashfs "$PYTHON"; do
  command -v "$tool" >/dev/null 2>&1 || die "required capacity-host tool is unavailable: $tool"
done
if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
  for required_output in "$ROOTFS_OUTPUT" "$VENTOY_MODLOOP_OUTPUT"; do
    [[ "$required_output" = /* ]] \
      || die "lts rootfs and Ventoy modloop outputs must be absolute paths"
  done
  command -v mksquashfs >/dev/null 2>&1 \
    || die "required hosted-live rootfs tool is unavailable: mksquashfs"
fi
OUTPUT_PATHS=("$OUTPUT")
if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
  OUTPUT_PATHS+=("$ROOTFS_OUTPUT" "$VENTOY_MODLOOP_OUTPUT")
fi
for candidate_output in "${OUTPUT_PATHS[@]}"; do
  if [[ -L "$candidate_output" || ( -e "$candidate_output" && ! -f "$candidate_output" ) ]]; then
    die "capacity-host output is a symlink or non-regular path: $candidate_output"
  fi
  [[ ! -e "$candidate_output" ]] \
    || die "refusing to clobber existing capacity-host output: $candidate_output"
done
"$PYTHON" - "${OUTPUT_PATHS[@]}" <<'PY'
from pathlib import Path
import sys

resolved = [str(Path(value).resolve(strict=False)) for value in sys.argv[1:]]
if len(resolved) != len(set(resolved)):
    raise SystemExit("error: capacity-host outputs must resolve to distinct paths")
PY
if [[ -L "$ALPINE_INITRAMFS" || ! -f "$ALPINE_INITRAMFS" ]]; then
  die "pinned Alpine x86_64 initramfs is missing or a symlink: $ALPINE_INITRAMFS"
fi
if [[ -L "$HOSTED_BIN_DIR" || ! -d "$HOSTED_BIN_DIR" ]]; then
  die "hosted Ostadix binary directory is missing or a symlink: $HOSTED_BIN_DIR"
fi
for binary in "${HOSTED_IMAGE_BINARIES[@]}"; do
  if [[ -L "$HOSTED_BIN_DIR/$binary" || ! -f "$HOSTED_BIN_DIR/$binary" \
      || ! -x "$HOSTED_BIN_DIR/$binary" ]]; then
    die "required hosted Ostadix x86_64 binary is unavailable: $HOSTED_BIN_DIR/$binary"
  fi
done
"$PYTHON" - "$HOSTED_BIN_DIR" "${HOSTED_IMAGE_BINARIES[@]}" <<'PY'
from pathlib import Path
import struct
import sys

root = Path(sys.argv[1])
for name in sys.argv[2:]:
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
  [[ -d "$source" && ! -L "$source" ]] \
    || die "hosted Ostadix source directory is missing or a symlink: $source"
done
if [[ ! "$HOSTED_REVISION" =~ ^[0-9a-f]{40}$ ]]; then
  die "OSTADIX_HOSTED_REVISION must be the exact 40-character source commit"
fi
if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
  for source in \
    "$HOSTED_SOURCE_ROOT/Cargo.toml" "$HOSTED_SOURCE_ROOT/Cargo.lock" \
    "$HOSTED_SOURCE_ROOT/crates/ostadix-api" \
    "$HOSTED_SOURCE_ROOT/mcp/ostadix_lang_mcp_server" \
    "$HOSTED_SOURCE_ROOT/scripts/ostadix_wasm_release.py" \
    "$HOSTED_SOURCE_ROOT/src"; do
    if [[ -L "$source" || ! -d "$source" ]]; then
      if [[ -L "$source" || ! -f "$source" ]]; then
        die "hosted Ostadix workstation source path is missing or a symlink: $source"
      fi
    fi
  done
  [[ "$HOSTED_SOURCE_ARCHIVE_SHA256" =~ ^[0-9a-f]{64}$ ]] \
    || die "OSTADIX_HOSTED_SOURCE_ARCHIVE_SHA256 must bind the exact tracked source archive"
  [[ "$HOSTED_BASE_COMMIT" =~ ^[0-9a-f]{40}$ ]] \
    || die "OSTADIX_HOSTED_BASE_COMMIT must be the exact 40-character base commit"
  [[ "$HOSTED_BOOT_OBJECTS_ARCHIVE_SHA256" =~ ^[0-9a-f]{64}$ ]] \
    || die "OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE_SHA256 must bind the exact object archive"
  [[ -f "$HOSTED_BOOT_OBJECTS_ARCHIVE" && ! -L "$HOSTED_BOOT_OBJECTS_ARCHIVE" ]] \
    || die "hosted boot-object archive is missing or a symlink"
  [[ $(sha256sum "$HOSTED_BOOT_OBJECTS_ARCHIVE" | awk '{print $1}') == "$HOSTED_BOOT_OBJECTS_ARCHIVE_SHA256" ]] \
    || die "hosted boot-object archive failed its exact SHA-256 binding"
  [[ -n "$HOSTED_BOOT_OBJECTS_RESULT" ]] \
    || die "OSTADIX_HOSTED_BOOT_OBJECTS_RESULT is required for the workstation receipt"
  [[ ! -e "$HOSTED_BOOT_OBJECTS_RESULT" && ! -L "$HOSTED_BOOT_OBJECTS_RESULT" ]] \
    || die "refusing to clobber boot-object verification result: $HOSTED_BOOT_OBJECTS_RESULT"
  [[ -d "$HOSTED_CARGO_VENDOR_DIR" && ! -L "$HOSTED_CARGO_VENDOR_DIR" ]] \
    || die "hosted Cargo vendor closure is missing or a symlink"
  [[ -f "$HOSTED_CARGO_VENDOR_MANIFEST" && ! -L "$HOSTED_CARGO_VENDOR_MANIFEST" ]] \
    || die "hosted Cargo vendor manifest is missing or a symlink"
  [[ -f "$HOSTED_WASM_ARTIFACT" && ! -L "$HOSTED_WASM_ARTIFACT" ]] \
    || die "hosted Olangc WASM artifact is missing or a symlink"
  [[ -f "$HOSTED_WASM_MANIFEST" && ! -L "$HOSTED_WASM_MANIFEST" ]] \
    || die "hosted Olangc WASM manifest is missing or a symlink"
  [[ -d "$HOSTED_WASM_PROJECT" && ! -L "$HOSTED_WASM_PROJECT" \
      && ! -e "$HOSTED_WASM_PROJECT/target" ]] \
    || die "hosted Olangc materialized project is missing, unsafe, or contains target output"
  [[ -f "$HOSTED_DESKTOP_HELPER" && ! -L "$HOSTED_DESKTOP_HELPER" ]] \
    || die "hosted desktop helper is missing or a symlink"
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
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9+_.-]*=[A-Za-z0-9][A-Za-z0-9+_.-]*", value):
        raise SystemExit(f"error: invalid hosted-live package lock entry: {value}")
print("\n".join(values))
PY
)
if (( ${#PACKAGE_SPECS[@]} == 0 )); then
  die "hosted-live package lock resolved to an empty closure"
fi

mkdir -p -- "$CACHE_ROOT" "$(dirname -- "$OUTPUT")"
if [[ -n "$ROOTFS_OUTPUT" ]]; then
  mkdir -p -- \
    "$(dirname -- "$ROOTFS_OUTPUT")" \
    "$(dirname -- "$VENTOY_MODLOOP_OUTPUT")"
fi
if [[ -L "$CACHE_ROOT" || ! -d "$CACHE_ROOT" ]]; then
  die "capacity-host cache must be a non-symlink directory: $CACHE_ROOT"
fi
MINIROOTFS="$CACHE_ROOT/alpine-minirootfs-3.24.1-x86_64.tar.gz"
MODLOOP="$CACHE_ROOT/modloop-$ALPINE_KERNEL_FLAVOR-3.24.1-x86_64"

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
  "$ALPINE_MODLOOP_BYTES" "$ALPINE_MODLOOP_SHA256" \
  "alpine-modloop-$ALPINE_KERNEL_FLAVOR"

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
  # The virt netboot initramfs omits optical-media and device-mapper drivers.
  # Import its exact matching modloop so the capacity host can remount either a
  # directly attached ISO or Ventoy's mapper-backed presentation of that ISO.
  unsquashfs -f -d "$STAGE/usr/lib" "$MODLOOP" \
    "modules/$ALPINE_KERNEL_RELEASE" >/dev/null
  for module in \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/cdrom/cdrom.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/md/dm-mod.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/scsi/sr_mod.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/fs/isofs/isofs.ko"; do
    [[ -f "$module" && ! -L "$module" ]] \
      || die "pinned Alpine modloop omitted required boot-media module: $module"
  done
else
  # The LTS netboot initramfs has HID/xHCI but omits CONFIG_INPUT_EVDEV=m.
  # Import that exact module plus its matching release metadata from the pinned
  # modloop so libinput can receive /dev/input/event* devices.
  unsquashfs -f -d "$STAGE/usr/lib" "$MODLOOP" \
    "modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/input/evdev.ko" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.alias" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.alias.bin" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.builtin" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.builtin.alias.bin" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.builtin.bin" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.builtin.modinfo" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.dep" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.dep.bin" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.devname" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.order" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.softdep" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.symbols" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.symbols.bin" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.weakdep" >/dev/null
  # Require the complete physical-input path; the LTS kernel supplies its
  # simpledrm/framebuffer console support separately.
  for module in \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/input/evdev.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/char/hw_random/virtio-rng.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/virtio/virtio_pci.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/hid/hid.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/hid/hid-generic.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/hid/usbhid/usbhid.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/usb/host/xhci-hcd.ko" \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/usb/host/xhci-pci.ko"; do
    [[ -f "$module" && ! -L "$module" ]] \
      || die "pinned Alpine LTS initramfs omitted required physical-input module: $module"
  done
  for metadata in modules.alias modules.alias.bin modules.dep modules.dep.bin; do
    path="$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/$metadata"
    [[ -f "$path" && ! -L "$path" && -s "$path" ]] \
      || die "pinned Alpine LTS modloop omitted required module metadata: $metadata"
  done
  grep -Fq 'kernel/drivers/input/evdev.ko:' \
    "$STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/modules.dep" \
    || die "pinned Alpine LTS module metadata omitted evdev"
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
chroot "$STAGE" /bin/sh -c "command -v ip >/dev/null" \
  || die "hosted package closure omitted required command: ip"
if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
  for command in rustc cargo rustfmt cargo-clippy cc git openssl wasm-tools wasmtime; do
    chroot "$STAGE" /bin/sh -c "command -v '$command' >/dev/null" \
      || die "workstation package closure omitted required command: $command"
  done
  for command in clang ld.lld mkfontdir mkfontscale openvt startx Xorg openbox \
      xinit xdg-open xprop xset xsetroot xterm firefox-esr udevd udevadm; do
    chroot "$STAGE" /bin/sh -c "command -v '$command' >/dev/null" \
      || die "workstation package closure omitted required command: $command"
  done
  [[ -f "$STAGE/usr/lib/xorg/modules/input/libinput_drv.so" \
      && ! -L "$STAGE/usr/lib/xorg/modules/input/libinput_drv.so" ]] \
    || die "workstation package closure omitted the Xorg libinput driver"
  # Package scripts and triggers stay disabled for deterministic initramfs
  # assembly. Reproduce mkfontscale's trigger explicitly so Xterm's core
  # `fixed` font is resolvable without package-manager side effects.
  while IFS= read -r font_dir; do
    case "$font_dir" in */encodings) continue ;; esac
    guest_font_dir=${font_dir#"$STAGE"}
    rm -f -- "$font_dir/fonts.dir" "$font_dir/fonts.scale"
    chroot "$STAGE" /usr/bin/mkfontdir "$guest_font_dir"
    chroot "$STAGE" /usr/bin/mkfontscale "$guest_font_dir"
  done < <(find "$STAGE/usr/share/fonts" -mindepth 1 -maxdepth 1 -type d -print \
    | LC_ALL=C sort)
  [[ -s "$STAGE/usr/share/fonts/misc/fonts.dir" \
      && ! -L "$STAGE/usr/share/fonts/misc/fonts.dir" \
      && -s "$STAGE/usr/share/fonts/misc/fonts.alias" \
      && ! -L "$STAGE/usr/share/fonts/misc/fonts.alias" ]] \
    || die "workstation package closure omitted generated X11 core-font indexes"
  grep -Eq '[[:space:]]-misc-fixed-' "$STAGE/usr/share/fonts/misc/fonts.dir" \
    || die "generated X11 font index omitted the misc-fixed font family"
  grep -Eq '^fixed[[:space:]]' "$STAGE/usr/share/fonts/misc/fonts.alias" \
    || die "generated X11 font aliases omitted the fixed font"
fi
# apk.log contains wall-clock progress timestamps and has no runtime value.
# Excluding it makes equal package closures produce byte-identical initramfses.
rm -f -- "$STAGE/var/log/apk.log"

install -d -m 0755 "$STAGE/usr/local/bin" "$STAGE/opt/ostadix/backends" \
  "$STAGE/opt/ostadix/examples" "$STAGE/usr/share/ostadix" "$STAGE/root"
install -m 0444 "$PACKAGE_LOCK" "$STAGE/usr/share/ostadix/hosted-live-apk-packages.txt"
if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
  install -m 0444 "$PACKAGE_LOCK" \
    "$STAGE/usr/share/ostadix/hosted-live-workstation-apk-packages.txt"
fi
# The host resolver is needed only while apk runs. Do not embed VM-specific DNS
# search domains or nameservers into the immutable live image.
printf 'nameserver 1.1.1.1\noptions timeout:2 attempts:2\n' >"$STAGE/etc/resolv.conf"
for binary in "${HOSTED_IMAGE_BINARIES[@]}"; do
  install -m 0555 "$HOSTED_BIN_DIR/$binary" "$STAGE/usr/local/bin/$binary"
done
if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
  install -d -m 0755 "$STAGE/usr/src/ostadix"
  install -d -m 0700 "$STAGE/root/.cargo"
  cp -R --preserve=mode --no-preserve=ownership \
    "$HOSTED_SOURCE_ROOT/." "$STAGE/usr/src/ostadix/"
  "$PYTHON" - "$HOSTED_SOURCE_ROOT" "$STAGE/usr/src/ostadix" <<'PY'
from pathlib import Path
import hashlib
import stat
import sys

source = Path(sys.argv[1])
embedded = Path(sys.argv[2])

def closure(root: Path) -> dict[str, tuple[object, ...]]:
    entries: dict[str, tuple[object, ...]] = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        state = path.lstat()
        mode = stat.S_IMODE(state.st_mode)
        if stat.S_ISLNK(state.st_mode):
            raise SystemExit(f"error: tracked source closure contains a symlink: {path}")
        if stat.S_ISDIR(state.st_mode):
            entries[relative] = ("directory", mode)
            continue
        if not stat.S_ISREG(state.st_mode):
            raise SystemExit(f"error: tracked source closure contains a special file: {path}")
        digest = hashlib.sha256()
        size = 0
        with path.open("rb") as stream:
            while chunk := stream.read(1024 * 1024):
                digest.update(chunk)
                size += len(chunk)
        entries[relative] = ("file", mode, size, digest.hexdigest())
    return entries

if closure(source) != closure(embedded):
    raise SystemExit("error: embedded /usr/src/ostadix differs from the tracked source snapshot")
PY
  install -d -m 0755 \
    "$STAGE/usr/share/ostadix/cargo/vendor" \
    "$STAGE/usr/share/ostadix/boot-objects/v1"
  cp -R --preserve=mode --no-preserve=ownership "$HOSTED_CARGO_VENDOR_DIR/." \
    "$STAGE/usr/share/ostadix/cargo/vendor/"
  install -m 0444 "$HOSTED_CARGO_VENDOR_MANIFEST" \
    "$STAGE/usr/share/ostadix/cargo/cargo-vendor-manifest.json"
  "$PYTHON" - \
    "$STAGE/usr/share/ostadix/cargo/vendor" \
    "$STAGE/usr/share/ostadix/cargo/cargo-vendor-manifest.json" \
    "$STAGE/usr/src/ostadix/Cargo.lock" \
    "$STAGE/usr/src/ostadix/mcp/ostadix_lang_mcp_server/Cargo.lock" <<'PY'
from pathlib import Path, PurePosixPath
import hashlib
import json
import os
import stat
import sys

vendor, manifest_path, root_lock, mcp_lock = map(Path, sys.argv[1:])

def identity(path: Path) -> dict[str, object]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
    return {"bytes": size, "sha256": digest.hexdigest()}

try:
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
except (OSError, UnicodeError, json.JSONDecodeError) as error:
    raise SystemExit(f"error: Cargo vendor manifest is unreadable: {error}")
if not isinstance(manifest, dict) or set(manifest) != {
    "schema", "locks", "package_count", "file_count", "total_bytes", "files"
}:
    raise SystemExit("error: Cargo vendor manifest has the wrong shape")
if manifest["schema"] != "ostadix.cargo-vendor-manifest/v1":
    raise SystemExit("error: Cargo vendor manifest has the wrong schema")
expected_locks = {
    "root": {"path": "Cargo.lock", **identity(root_lock)},
    "mcp": {
        "path": "mcp/ostadix_lang_mcp_server/Cargo.lock",
        **identity(mcp_lock),
    },
}
if manifest["locks"] != expected_locks:
    raise SystemExit("error: Cargo vendor manifest is not bound to both staged lockfiles")

package_count = 0
for path in sorted(vendor.iterdir(), key=lambda item: item.name):
    state = path.lstat()
    if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
        raise SystemExit(f"error: Cargo vendor root contains a non-directory: {path}")
    checksum = path / ".cargo-checksum.json"
    checksum_state = checksum.lstat()
    if stat.S_ISLNK(checksum_state.st_mode) or not stat.S_ISREG(checksum_state.st_mode):
        raise SystemExit(f"error: vendored package has an unsafe checksum file: {path}")
    package_count += 1

actual = []
for root, directory_names, file_names in os.walk(vendor, topdown=True, followlinks=False):
    directory_names.sort()
    file_names.sort()
    root_path = Path(root)
    for name in directory_names:
        path = root_path / name
        state = path.lstat()
        if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
            raise SystemExit(f"error: Cargo vendor closure contains an unsafe directory: {path}")
    for name in file_names:
        path = root_path / name
        state = path.lstat()
        if stat.S_ISLNK(state.st_mode) or not stat.S_ISREG(state.st_mode):
            raise SystemExit(f"error: Cargo vendor closure contains an unsafe file: {path}")
        relative = path.relative_to(vendor).as_posix()
        pure = PurePosixPath(relative)
        if pure.is_absolute() or not pure.parts or any(part in {"", ".", ".."} for part in pure.parts):
            raise SystemExit(f"error: Cargo vendor manifest path is not canonical: {relative!r}")
        actual.append({"path": relative, **identity(path)})
actual.sort(key=lambda record: record["path"])
if manifest["files"] != actual:
    raise SystemExit("error: Cargo vendor files differ from their canonical manifest")
if manifest["package_count"] != package_count:
    raise SystemExit("error: Cargo vendor package count differs from its manifest")
if manifest["file_count"] != len(actual):
    raise SystemExit("error: Cargo vendor file count differs from its manifest")
if manifest["total_bytes"] != sum(record["bytes"] for record in actual):
    raise SystemExit("error: Cargo vendor byte total differs from its manifest")
PY
  tee "$STAGE/root/.cargo/config.toml" >/dev/null <<'CARGO_CONFIG'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "/usr/share/ostadix/cargo/vendor"

[net]
offline = true
CARGO_CONFIG
  chmod 0444 "$STAGE/root/.cargo/config.toml"

  install -d -m 0755 "$STAGE/usr/share/ostadix/wasm"
  install -m 0444 "$HOSTED_WASM_ARTIFACT" \
    "$STAGE/usr/share/ostadix/wasm/hello.wasm"
  install -m 0444 "$HOSTED_WASM_MANIFEST" \
    "$STAGE/usr/share/ostadix/wasm/hello.release.json"
  "$PYTHON" "$HOSTED_SOURCE_ROOT/scripts/ostadix_wasm_release.py" verify \
    --manifest "$STAGE/usr/share/ostadix/wasm/hello.release.json" \
    --project "$HOSTED_WASM_PROJECT" \
    --artifact "$STAGE/usr/share/ostadix/wasm/hello.wasm" \
    --input "$STAGE/usr/src/ostadix/examples/wasm_hello.O" \
    --generator "$STAGE/usr/local/bin/olangc" \
    --source-tree "$HOSTED_REVISION" \
    --base-commit "$HOSTED_BASE_COMMIT" \
    --source-archive-sha256 "$HOSTED_SOURCE_ARCHIVE_SHA256" >/dev/null

  "$PYTHON" - "$HOSTED_BOOT_OBJECTS_ARCHIVE" \
    "$STAGE/usr/share/ostadix/boot-objects/v1" <<'PY'
from pathlib import Path, PurePosixPath
import os
import shutil
import stat
import sys
import tarfile

archive = Path(sys.argv[1])
destination = Path(sys.argv[2])
seen = set()
with tarfile.open(archive, "r:") as source:
    for member in source:
        name = member.name[:-1] if member.name.endswith("/") else member.name
        pure = PurePosixPath(name)
        if (
            pure.is_absolute()
            or not pure.parts
            or any(part in {"", ".", ".."} for part in pure.parts)
            or name != pure.as_posix()
            or name in seen
        ):
            raise SystemExit(f"error: unsafe boot-object archive member: {member.name!r}")
        seen.add(name)
        target = destination.joinpath(*pure.parts)
        if member.isdir():
            target.mkdir(parents=True, exist_ok=True)
            target.chmod(0o755)
            continue
        if not member.isreg():
            raise SystemExit(f"error: boot-object archive member is not regular: {member.name!r}")
        target.parent.mkdir(parents=True, exist_ok=True)
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        descriptor = os.open(target, flags, 0o444)
        try:
            stream = source.extractfile(member)
            if stream is None:
                raise SystemExit(f"error: boot-object archive member has no body: {member.name!r}")
            with os.fdopen(descriptor, "wb", closefd=False) as output:
                shutil.copyfileobj(stream, output, length=1024 * 1024)
                output.flush()
                os.fsync(output.fileno())
            if target.stat().st_size != member.size:
                raise SystemExit(f"error: boot-object archive member is truncated: {member.name!r}")
            os.chmod(target, 0o444)
        finally:
            os.close(descriptor)
if not seen:
    raise SystemExit("error: boot-object archive is empty")
PY
  BOOT_OBJECTS_RESULT_CANDIDATE="$WORK_DIR/boot-objects-verify.json"
  "$PYTHON" "$HOSTED_SOURCE_ROOT/scripts/ostadix_boot_objects.py" verify \
    --store "$STAGE/usr/share/ostadix/boot-objects/v1" \
    --commit "$HOSTED_BASE_COMMIT" \
    --tree "$HOSTED_REVISION" \
    --source-root "$STAGE/usr/src/ostadix" \
    --json >"$BOOT_OBJECTS_RESULT_CANDIDATE"
  "$PYTHON" - "$BOOT_OBJECTS_RESULT_CANDIDATE" \
    "$HOSTED_BASE_COMMIT" "$HOSTED_REVISION" <<'PY'
from pathlib import Path
import json
import sys

path = Path(sys.argv[1])
payload = json.loads(path.read_text(encoding="utf-8"))
required = {
    "schema": "ostadix.boot-object-store-result/v1",
    "ok": True,
    "operation": "verify",
    "commit": sys.argv[2],
    "tree": sys.argv[3],
}
if not isinstance(payload, dict) or any(payload.get(key) != value for key, value in required.items()):
    raise SystemExit("error: boot-object verification result omitted the exact staged source binding")
for field in ("object_count", "binding_count", "logical_bytes", "stored_bytes"):
    if type(payload.get(field)) is not int or payload[field] <= 0:
        raise SystemExit(f"error: boot-object verification result has invalid {field}")
root = payload.get("root_sha256")
if not isinstance(root, str) or len(root) != 64 or any(c not in "0123456789abcdef" for c in root):
    raise SystemExit("error: boot-object verification result has invalid root_sha256")
payload["store"] = "/usr/share/ostadix/boot-objects/v1"
path.write_text(
    json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY
  install -m 0444 "$BOOT_OBJECTS_RESULT_CANDIDATE" "$HOSTED_BOOT_OBJECTS_RESULT"
  install -m 0444 "$BOOT_OBJECTS_RESULT_CANDIDATE" \
    "$STAGE/usr/share/ostadix/boot-objects-v1-verify.json"
  install -m 0555 "$HOSTED_DESKTOP_HELPER" "$STAGE/usr/local/bin/ostadix-desktop"
fi
cp -R --preserve=mode --no-preserve=ownership "$HOSTED_SOURCE_ROOT/backends/." \
  "$STAGE/opt/ostadix/backends/"
for example in hello.O wasm_hello.O webassembly_hello.O \
    shell_hello.O bash_hello.O sql_select.O; do
  install -m 0444 "$HOSTED_SOURCE_ROOT/examples/$example" \
    "$STAGE/opt/ostadix/examples/$example"
done
if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
  tee "$STAGE/usr/local/bin/o" >/dev/null <<'O_WRAPPER'
#!/bin/sh
set -eu
export O_LANG_OCLI_BIN=/usr/local/bin/o-cli
export O_LANG_OLANGC_BIN=/usr/local/bin/olangc
export O_LANG_EVALUATOR_BIN=/usr/local/bin/O
export O_LANG_KERNEL_CLI_BIN=/usr/src/ostadix/scripts/o-kernel.sh
export O_LANG_CAPACITY_BIN=/usr/src/ostadix/scripts/ostadix_capacity.py
export O_LANG_LIVE_BIN=/usr/local/bin/o-live-host
export O_LANG_OGIT_BIN=/usr/local/bin/ogit
export O_LANG_NODE_BIN=/usr/local/bin/o-node
export O_LANG_OCTL_BIN=/usr/local/bin/octl
export O_LANG_REGISTRY_BIN=/usr/local/bin/o-registry
export O_LANG_INFO_BIN=/usr/local/bin/o-info
exec /usr/src/ostadix/scripts/o-cli.sh "$@"
O_WRAPPER
else
  tee "$STAGE/usr/local/bin/o" >/dev/null <<'O_WRAPPER'
#!/bin/sh
set -eu
case "${1:-}" in
  run|routes|optimize|plan|explain|inspect|object|operation|help|--help|-h) exec o-cli "$@" ;;
  *) exec O "$@" ;;
esac
O_WRAPPER
fi
chmod 0555 "$STAGE/usr/local/bin/o"
{
  if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
    printf 'schema=ostadix.hosted-live/v3\n'
  else
    printf 'schema=ostadix.hosted-live/v1\n'
  fi
  printf 'architecture=x86_64\n'
  printf 'hosted_binary_revision=%s\n' "$HOSTED_REVISION"
  if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
    printf 'source.path=/usr/src/ostadix\n'
    printf 'source.base_commit=%s\n' "$HOSTED_BASE_COMMIT"
    printf 'source.archive.sha256=%s\n' "$HOSTED_SOURCE_ARCHIVE_SHA256"
    printf 'source.files=%s\n' \
      "$(find "$STAGE/usr/src/ostadix" -type f | wc -l | tr -d ' ')"
    printf 'wasm.artifact.bytes=%s\n' \
      "$(wc -c <"$STAGE/usr/share/ostadix/wasm/hello.wasm" | tr -d ' ')"
    printf 'wasm.artifact.sha256=%s\n' \
      "$(sha256sum "$STAGE/usr/share/ostadix/wasm/hello.wasm" | awk '{print $1}')"
    printf 'wasm.manifest.sha256=%s\n' \
      "$(sha256sum "$STAGE/usr/share/ostadix/wasm/hello.release.json" | awk '{print $1}')"
  fi
  printf 'package_lock.bytes=%s\n' "$(wc -c <"$PACKAGE_LOCK" | tr -d ' ')"
  printf 'package_lock.sha256=%s\n' "$(sha256sum "$PACKAGE_LOCK" | awk '{print $1}')"
  for binary in "${HOSTED_IMAGE_BINARIES[@]}"; do
    binary_sha=$(sha256sum "$STAGE/usr/local/bin/$binary" | awk '{print $1}')
    binary_bytes=$(wc -c <"$STAGE/usr/local/bin/$binary" | tr -d ' ')
    printf 'binary.%s.bytes=%s\n' "$binary" "$binary_bytes"
    printf 'binary.%s.sha256=%s\n' "$binary" "$binary_sha"
  done
} >"$STAGE/usr/share/ostadix/hosted-live-manifest.txt"
chmod 0444 "$STAGE/usr/share/ostadix/hosted-live-manifest.txt"
printf '%s\n' "$ALPINE_KERNEL_FLAVOR" \
  >"$STAGE/usr/share/ostadix/hosted-live-kernel-flavor"
chmod 0444 "$STAGE/usr/share/ostadix/hosted-live-kernel-flavor"

install -d -m 0755 "$STAGE/media/ostadix" "$STAGE/proc" "$STAGE/sys" \
  "$STAGE/dev" "$STAGE/run" "$STAGE/tmp" "$STAGE/root" "$STAGE/workspace"
chmod 01777 "$STAGE/tmp"
tee "$STAGE/init" >/dev/null <<'INIT'
#!/bin/sh
set -eu

export HOME=/root
export PATH=/usr/local/bin:/sbin:/bin:/usr/sbin:/usr/bin
export O_LANG_ROOT=/usr/src/ostadix
export O_BACKENDS_DIR=/opt/ostadix/backends
export CARGO_HOME=/root/.cargo
export CARGO_TARGET_DIR=/workspace/target
export CARGO_NET_OFFLINE=true
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export OSTADIX_O_INFO_BIN=/usr/local/bin/o-info
export OSTADIX_NOTEBOOK_BROWSER=/usr/bin/firefox-esr
export PYTHONDONTWRITEBYTECODE=1
export TERM=${TERM:-linux}
grep -qs ' /proc proc ' /proc/mounts 2>/dev/null || mount -t proc proc /proc
grep -qs ' /sys sysfs ' /proc/mounts || mount -t sysfs sysfs /sys
if ! grep -qs ' /dev devtmpfs ' /proc/mounts; then
  mount -t devtmpfs devtmpfs /dev 2>/dev/null || {
    mount -t tmpfs -o mode=0755 tmpfs /dev
    mdev -s
  }
fi
mkdir -p /dev/pts
grep -qs ' /dev/pts devpts ' /proc/mounts \
  || mount -t devpts -o gid=5,mode=0620,ptmxmode=0666 devpts /dev/pts
mkdir -p /dev/shm
grep -qs ' /dev/shm tmpfs ' /proc/mounts \
  || mount -t tmpfs -o mode=1777,nosuid,nodev tmpfs /dev/shm
grep -qs ' /run tmpfs ' /proc/mounts \
  || mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /run
grep -qs ' /tmp tmpfs ' /proc/mounts \
  || mount -t tmpfs -o mode=1777,nosuid,nodev tmpfs /tmp

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
cd /workspace
export HOME=/root
export O_LANG_ROOT=/usr/src/ostadix
export O_BACKENDS_DIR=/opt/ostadix/backends
export CARGO_HOME=/root/.cargo
export CARGO_TARGET_DIR=/workspace/target
export CARGO_NET_OFFLINE=true
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export PATH=/usr/local/bin:/sbin:/bin:/usr/sbin:/usr/bin
  export PS1='ostadix-workstation:\w# '
exec /bin/sh -i
SHELL
  chmod 0755 /run/ostadix-live-shell
  if command -v openvt >/dev/null 2>&1 && [ -c /dev/tty1 ]; then
    while :; do
      openvt -c 1 -s -w /run/ostadix-live-shell || true
      emit_error 'OSTADIX HOSTED WORKSTATION: visible shell exited; restarting tty1'
      sleep 1
    done
  fi
  emit_error 'OSTADIX HOSTED WORKSTATION: openvt unavailable; using /dev/console fallback'
  exec /run/ostadix-live-shell
}

mount_read_only_tree() {
  tree=$1
  mount -o bind "$tree" "$tree" \
    && mount -o remount,bind,ro "$tree" "$tree" \
    && awk -v target="$tree" '
      $2 == target {
        count += 1
        split($4, options, ",")
        for (option_index in options) {
          if (options[option_index] == "ro") read_only = 1
        }
      }
      END { exit !(count == 1 && read_only == 1) }
    ' /proc/mounts \
    || return 1
  probe="$tree/.ostadix-live-write-probe"
  if (umask 077; : >"$probe") 2>/dev/null; then
    rm -f -- "$probe"
    return 1
  fi
}

for module in \
  ata_piix ahci nvme virtio_pci virtio_rng \
  xhci_hcd xhci_pci usbhid hid_generic evdev simpledrm \
  cdrom sr_mod isofs dm_mod; do
  modprobe "$module" 2>/dev/null || true
done

selected=
for argument in $(cat /proc/cmdline); do
  case "$argument" in
    ostadix.capacity=*) selected=${argument#ostadix.capacity=} ;;
  esac
done

hosted_flavor=$(cat /usr/share/ostadix/hosted-live-kernel-flavor 2>/dev/null || true)
case "$hosted_flavor" in
  virt|lts) ;;
  *)
    emit_error "OSTADIX HOSTED LIVE: FAIL: invalid kernel flavor: $hosted_flavor"
    exec sh
    ;;
esac

if [ "$selected" = hosted ]; then
  if [ "$hosted_flavor" = lts ]; then
    if awk '$2 == "/" && $3 == "overlay" { found = 1 } END { exit !found }' \
        /proc/mounts \
        && (umask 077; : >/etc/.ostadix-overlay-write-probe) \
        && rm -f /etc/.ostadix-overlay-write-probe; then
      emit_line 'OSTADIX HOSTED ROOTFS OVERLAY: PASS'
    else
      emit_error 'OSTADIX HOSTED ROOTFS OVERLAY: FAIL'
      hosted_shell
    fi
    if mount_read_only_tree /usr/src/ostadix \
        && mount_read_only_tree /usr/share/ostadix/boot-objects/v1 \
        && mount_read_only_tree /usr/share/ostadix/wasm; then
      emit_line 'OSTADIX HOSTED READ-ONLY TREES: PASS'
    else
      emit_error 'OSTADIX HOSTED READ-ONLY TREES: FAIL'
      hosted_shell
    fi
  fi
  loopback_link=missing
  if ip link set dev lo up 2>/tmp/ostadix-loopback.err; then
    loopback_link=$(ip link show dev lo 2>/dev/null | head -n 1 || printf missing)
  fi
  case "$loopback_link" in
    *'<LOOPBACK,UP,'*) emit_line 'OSTADIX HOSTED LOOPBACK: PASS' ;;
    *)
      emit_error "OSTADIX HOSTED LOOPBACK: FAIL: link=$loopback_link"
      cat /tmp/ostadix-loopback.err >&2 2>/dev/null || true
      hosted_shell
      ;;
  esac
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
  if apk --version 2>&1 | grep -q '^apk-tools 3\.0\.6' \
      && apk info -e cargo >/dev/null 2>&1 \
      && apk info -e rust >/dev/null 2>&1 \
      && apk info -e firefox-esr >/dev/null 2>&1; then
    emit_line 'OSTADIX HOSTED APK: PASS'
  else
    emit_error 'OSTADIX HOSTED APK: FAIL'
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
  if [ "$hosted_flavor" = lts ]; then
    rustc_version=$(rustc --version 2>&1 || true)
  case "$rustc_version" in
    'rustc 1.96.1 '*) emit_line 'OSTADIX HOSTED RUSTC: PASS' ;;
    *)
      emit_error "OSTADIX HOSTED RUSTC: FAIL: $rustc_version"
      hosted_shell
      ;;
  esac
  cargo_version=$(cargo --version 2>&1 || true)
  case "$cargo_version" in
    'cargo 1.96.1 '*) emit_line 'OSTADIX HOSTED CARGO: PASS' ;;
    *)
      emit_error "OSTADIX HOSTED CARGO: FAIL: $cargo_version"
      hosted_shell
      ;;
  esac
  mkdir -p /tmp/ostadix-cargo-hello/src /tmp/ostadix-cargo-target
  cat >/tmp/ostadix-cargo-hello/Cargo.toml <<'CARGO_TOML'
[package]
name = "ostadix-live-hello"
version = "0.1.0"
edition = "2021"
CARGO_TOML
  cat >/tmp/ostadix-cargo-hello/src/main.rs <<'RUST_SOURCE'
fn main() {
    println!("ostadix-cargo-hello");
}
RUST_SOURCE
  if CARGO_HOME=/root/.cargo \
      cargo fmt --manifest-path /tmp/ostadix-cargo-hello/Cargo.toml \
      -- --check >/tmp/ostadix-cargo-fmt.out \
      2>/tmp/ostadix-cargo-fmt.err; then
    emit_line 'OSTADIX HOSTED RUSTFMT: PASS'
  else
    emit_error 'OSTADIX HOSTED RUSTFMT: FAIL'
    cat /tmp/ostadix-cargo-fmt.err >&2 2>/dev/null || true
    hosted_shell
  fi
  if CARGO_HOME=/root/.cargo \
      CARGO_TARGET_DIR=/tmp/ostadix-cargo-target \
      cargo clippy --offline --quiet \
      --manifest-path /tmp/ostadix-cargo-hello/Cargo.toml \
      -- -D warnings >/tmp/ostadix-cargo-clippy.out \
      2>/tmp/ostadix-cargo-clippy.err; then
    emit_line 'OSTADIX HOSTED CLIPPY: PASS'
  else
    emit_error 'OSTADIX HOSTED CLIPPY: FAIL'
    cat /tmp/ostadix-cargo-clippy.err >&2 2>/dev/null || true
    hosted_shell
  fi
  cargo_hello_output=
  if cargo_hello_output=$(CARGO_HOME=/root/.cargo \
      CARGO_TARGET_DIR=/tmp/ostadix-cargo-target \
      cargo run --offline --quiet \
      --manifest-path /tmp/ostadix-cargo-hello/Cargo.toml \
      2>/tmp/ostadix-cargo-hello.err) \
      && [ "$cargo_hello_output" = ostadix-cargo-hello ]; then
    emit_line 'OSTADIX HOSTED CARGO HELLO: PASS'
  else
    emit_error "OSTADIX HOSTED CARGO HELLO: FAIL: $cargo_hello_output"
    cat /tmp/ostadix-cargo-hello.err >&2 2>/dev/null || true
    hosted_shell
  fi
  entropy_device=platform
  entropy_vendor=$(cat /sys/class/dmi/id/sys_vendor 2>/dev/null || true)
  case "$entropy_vendor" in
    QEMU*)
      entropy_device=virtio-rng-pci
      entropy_bound=
      for device in /sys/bus/virtio/drivers/virtio_rng/virtio*; do
        if [ -e "$device" ]; then
          entropy_bound=1
          break
        fi
      done
      if [ -z "$entropy_bound" ]; then
        emit_error 'OSTADIX HOSTED ENTROPY: FAIL: QEMU virtio RNG is not bound'
        hosted_shell
      fi
      ;;
  esac
  entropy_probe=$(timeout -s TERM -k 1 5 python3 -c \
    'import os; probe = os.getrandom(32); print(len(probe))' \
    2>/dev/null || true)
  entropy_available=$(cat /proc/sys/kernel/random/entropy_avail 2>/dev/null || true)
  entropy_ready=
  if [ "$entropy_probe" = 32 ]; then
    case "$entropy_available" in
      ''|*[!0-9]*) ;;
      *)
        if [ "$entropy_available" -ge 128 ]; then
          emit_line "OSTADIX HOSTED ENTROPY: PASS device=$entropy_device crng_bytes=32 available=$entropy_available"
          entropy_ready=1
        fi
        ;;
    esac
  fi
  if [ "$entropy_ready" != 1 ]; then
    emit_error "OSTADIX HOSTED ENTROPY: FAIL: device=$entropy_device crng_bytes=$entropy_probe available=$entropy_available"
    hosted_shell
  fi
  node_smoke_config=/tmp/ostadix-node-smoke/config
  node_smoke_state=/tmp/ostadix-node-smoke/state
  emit_node_diagnostic_tail() {
    node_diagnostic_path=$1
    if [ -f "$node_diagnostic_path" ]; then
      emit_line "O-NODE DIAGNOSTIC: $node_diagnostic_path"
      tail -c 16384 "$node_diagnostic_path" 2>/dev/null \
        | while IFS= read -r node_diagnostic_line \
            || [ -n "$node_diagnostic_line" ]; do
          emit_line "O-NODE DIAGNOSTIC: $node_diagnostic_line"
        done
    fi
  }
  # The CLI readiness timer begins after identity provisioning. Keep that
  # pre-listener work bounded separately. The explicit P-256 profile retains
  # a fresh PKI proof without making cross-architecture TCG generate four
  # RSA-3072 keys. The 900-second outer budget is for nested AArch64-to-x86_64
  # TCG and does not relax the 30-second listener-readiness deadline below;
  # ordinary `o node start` remains RSA-3072 by default.
  node_smoke_stage=start-command
  node_smoke_status=0
  node_smoke_failed=
  if XDG_CONFIG_HOME="$node_smoke_config" XDG_STATE_HOME="$node_smoke_state" \
      timeout -s TERM -k 5 900 o node start --startup-timeout-seconds 30 \
      --fresh-pki-key-algorithm ec-p256 \
      >/tmp/ostadix-node-start.out 2>/tmp/ostadix-node-start.err; then
    :
  else
    node_smoke_status=$?
    node_smoke_failed=1
  fi
  if [ -z "$node_smoke_failed" ]; then
    node_smoke_stage=start-marker
    if grep -Fxq 'development PKI key algorithm: ec-p256' \
        /tmp/ostadix-node-start.out \
        && grep -Fxq 'pairing CA key algorithm: ec-p256' \
        /tmp/ostadix-node-start.out \
        && grep -Fxq 'fresh PKI key algorithm selection: ec-p256' \
        /tmp/ostadix-node-start.out \
        && grep -Eq '^o-node started: .+' /tmp/ostadix-node-start.out; then
      :
    else
      node_smoke_status=$?
      node_smoke_failed=1
    fi
  fi
  if [ -z "$node_smoke_failed" ]; then
    node_smoke_stage=status-command
    if XDG_CONFIG_HOME="$node_smoke_config" XDG_STATE_HOME="$node_smoke_state" \
        timeout -s TERM -k 5 30 o node status >/tmp/ostadix-node-status.out \
        2>/tmp/ostadix-node-status.err; then
      :
    else
      node_smoke_status=$?
      node_smoke_failed=1
    fi
  fi
  if [ -z "$node_smoke_failed" ]; then
    node_smoke_stage=status-marker
    if grep -Eq '^running pid=[0-9]+ ' /tmp/ostadix-node-status.out; then
      :
    else
      node_smoke_status=$?
      node_smoke_failed=1
    fi
  fi
  if [ -z "$node_smoke_failed" ]; then
    node_smoke_stage=stop-command
    if XDG_CONFIG_HOME="$node_smoke_config" XDG_STATE_HOME="$node_smoke_state" \
        timeout -s TERM -k 5 30 o node stop \
        >/tmp/ostadix-node-stop.out 2>/tmp/ostadix-node-stop.err; then
      :
    else
      node_smoke_status=$?
      node_smoke_failed=1
    fi
  fi
  if [ -z "$node_smoke_failed" ]; then
    node_smoke_stage=stop-marker
    if grep -Fxq 'o-node stopped' /tmp/ostadix-node-stop.out; then
      :
    else
      node_smoke_status=$?
      node_smoke_failed=1
    fi
  fi
  if [ -z "$node_smoke_failed" ]; then
    emit_line 'OSTADIX HOSTED O-NODE: PASS'
  else
    XDG_CONFIG_HOME="$node_smoke_config" XDG_STATE_HOME="$node_smoke_state" \
      timeout -s TERM -k 5 30 o node stop \
      >/tmp/ostadix-node-cleanup.out 2>&1 || true
    emit_line "O-NODE DIAGNOSTIC: stage=$node_smoke_stage status=$node_smoke_status pki=ec-p256"
    for node_diagnostic_path in \
      /tmp/ostadix-node-start.out /tmp/ostadix-node-start.err \
      /tmp/ostadix-node-status.out /tmp/ostadix-node-status.err \
      /tmp/ostadix-node-stop.out /tmp/ostadix-node-stop.err \
      /tmp/ostadix-node-cleanup.out \
      "$node_smoke_state/ostadix/node/o-node.log"; do
      emit_node_diagnostic_tail "$node_diagnostic_path"
    done
    emit_error "OSTADIX HOSTED O-NODE: FAIL: stage=$node_smoke_stage status=$node_smoke_status pki=ec-p256"
    hosted_shell
  fi
  OSTADIX_NOTEBOOK_NO_OPEN=1 O_BACKENDS_DIR=/usr/src/ostadix/backends \
    o-notebook >/tmp/ostadix-notebook.out 2>/tmp/ostadix-notebook.err &
  notebook_pid=$!
  notebook_ready=
  notebook_probe_status=0
  if timeout -s TERM -k 5 180 python3 - \
      >/tmp/ostadix-notebook-probe.out \
      2>/tmp/ostadix-notebook-probe.err <<'PY'
import json
import time
import urllib.request

root_url = "http://127.0.0.1:8888/"
deadline = time.monotonic() + 30
last_error = None
while time.monotonic() < deadline:
    try:
        root = urllib.request.urlopen(root_url, timeout=0.5).read()
        if b"<title>O \xc2\xb7 Notebook</title>" not in root:
            raise RuntimeError("notebook title marker is absent")
        break
    except Exception as error:
        last_error = error
        time.sleep(0.1)
else:
    raise SystemExit(f"notebook root did not become ready: {last_error!r}")

request = urllib.request.Request(
    root_url + "eval",
    data=json.dumps(
        {"code": "python^(\n__oval_result__ = 6 * 7\n)_python"}
    ).encode("utf-8"),
    headers={"Content-Type": "application/json"},
)
response = json.load(urllib.request.urlopen(request, timeout=120))
if response.get("ok") is not True or "42" not in json.dumps(response.get("result")):
    raise SystemExit(f"notebook evaluation returned an invalid response: {response!r}")
print("notebook HTTP and Python-backend evaluation are ready")
PY
  then
    notebook_ready=1
  else
    notebook_probe_status=$?
  fi
  kill "$notebook_pid" 2>/dev/null || true
  wait "$notebook_pid" 2>/dev/null || true
  if [ "$notebook_ready" = 1 ]; then
    emit_line 'OSTADIX HOSTED NOTEBOOK: PASS'
  else
    emit_line "NOTEBOOK DIAGNOSTIC: probe_status=$notebook_probe_status"
    for notebook_diagnostic_path in \
      /tmp/ostadix-notebook.out /tmp/ostadix-notebook.err \
      /tmp/ostadix-notebook-probe.out /tmp/ostadix-notebook-probe.err; do
      if [ -s "$notebook_diagnostic_path" ]; then
        emit_line "NOTEBOOK DIAGNOSTIC: $notebook_diagnostic_path"
        tail -c 16384 "$notebook_diagnostic_path" 2>/dev/null \
          | while IFS= read -r notebook_diagnostic_line \
              || [ -n "$notebook_diagnostic_line" ]; do
              emit_line "NOTEBOOK DIAGNOSTIC: $notebook_diagnostic_line"
            done
      fi
    done
    emit_error 'OSTADIX HOSTED NOTEBOOK: FAIL'
    hosted_shell
  fi
  missing_standard=
  for binary in \
    O o-cli olangc ocorec o-link o-unlink ogit o-live-host o-node octl \
    o-registry o-info ostadix-device; do
    command -v "$binary" >/dev/null 2>&1 \
      || missing_standard="$missing_standard $binary"
  done
  if [ -z "$missing_standard" ]; then
    emit_line 'OSTADIX HOSTED STANDARD BINARIES: PASS'
  else
    emit_error "OSTADIX HOSTED STANDARD BINARIES: FAIL:$missing_standard"
    hosted_shell
  fi
  missing_declared=
  for binary in \
    O o-cli olangc ocorec o-link o-unlink o-notebook ogit o-live-host o-node \
    octl o-registry o-info ostadix-device ocore-kernel-world-record; do
    command -v "$binary" >/dev/null 2>&1 \
      || missing_declared="$missing_declared $binary"
  done
  if [ -z "$missing_declared" ]; then
    emit_line 'OSTADIX HOSTED DECLARED ROOT BINARIES: PASS'
  else
    emit_error "OSTADIX HOSTED DECLARED ROOT BINARIES: FAIL:$missing_declared"
    hosted_shell
  fi
  if o kernel help >/tmp/ostadix-route-kernel.out 2>&1 \
      && o capacity --help >/tmp/ostadix-route-capacity.out 2>&1 \
      && o node --help >/tmp/ostadix-route-node.out 2>&1 \
      && o node-host --help >/tmp/ostadix-route-node-host.out 2>&1 \
      && o registry --help >/tmp/ostadix-route-registry.out 2>&1 \
      && o info --help >/tmp/ostadix-route-info.out 2>&1 \
      && o live --help >/tmp/ostadix-route-live.out 2>&1 \
      && o receipt --help >/tmp/ostadix-route-receipt.out 2>&1; then
    emit_line 'OSTADIX HOSTED UNIFIED ROUTES: PASS'
  else
    emit_error 'OSTADIX HOSTED UNIFIED ROUTES: FAIL'
    hosted_shell
  fi
  if [ -f /usr/src/ostadix/Cargo.toml ] \
      && [ -f /usr/src/ostadix/Cargo.lock ] \
      && [ -d /usr/src/ostadix/crates/ostadix-api ] \
      && [ -d /usr/src/ostadix/mcp/ostadix_lang_mcp_server ] \
      && [ ! -e /usr/src/ostadix/.git ] \
      && grep -Eq '^source\.archive\.sha256=[0-9a-f]{64}$' \
        /usr/share/ostadix/hosted-live-manifest.txt; then
    emit_line 'OSTADIX HOSTED SOURCE SNAPSHOT: PASS'
  else
    emit_error 'OSTADIX HOSTED SOURCE SNAPSHOT: FAIL'
    hosted_shell
  fi
  hosted_source_tree=$(sed -n 's/^hosted_binary_revision=//p' \
    /usr/share/ostadix/hosted-live-manifest.txt)
  hosted_base_commit=$(sed -n 's/^source.base_commit=//p' \
    /usr/share/ostadix/hosted-live-manifest.txt)
  hosted_archive_sha256=$(sed -n 's/^source.archive.sha256=//p' \
    /usr/share/ostadix/hosted-live-manifest.txt)
  if python3 /usr/src/ostadix/scripts/smoke_ostadix_mcp.py \
      --root /usr/src/ostadix \
      --binary /usr/local/bin/ostadix-mcp \
      --o-info /usr/local/bin/o-info \
      --runtime-bin-dir /usr/local/bin \
      --server-cwd /workspace \
      --require-wasm-materialization \
      --wasm-release-manifest /usr/share/ostadix/wasm/hello.release.json \
      --wasm-release-artifact /usr/share/ostadix/wasm/hello.wasm \
      --wasm-source-tree "$hosted_source_tree" \
      --wasm-base-commit "$hosted_base_commit" \
      --wasm-source-archive-sha256 "$hosted_archive_sha256" \
      --timeout 60 >/tmp/ostadix-mcp-smoke.out \
      2>/tmp/ostadix-mcp-smoke.err \
      && grep -Fxq 'ostadix-mcp stdio release smoke: PASS' \
        /tmp/ostadix-mcp-smoke.out \
      && [ "$(grep -Ec '^ostadix-mcp o_olangc wasm materialization: PASS root_sha256=[0-9a-f]{64}$' \
        /tmp/ostadix-mcp-smoke.out)" -eq 1 ] \
      && [ "$(grep -Ec '^ostadix-mcp o_olangc wasm artifact: PASS tree=[0-9a-f]{40} bytes=[1-9][0-9]* sha256=[0-9a-f]{64}$' \
        /tmp/ostadix-mcp-smoke.out)" -eq 1 ]; then
    wasm_project_sha256=$(sed -n \
      's/^ostadix-mcp o_olangc wasm materialization: PASS root_sha256=//p' \
      /tmp/ostadix-mcp-smoke.out)
    wasm_artifact_record=$(sed -n \
      's/^ostadix-mcp o_olangc wasm artifact: PASS //p' \
      /tmp/ostadix-mcp-smoke.out)
    emit_line "OSTADIX HOSTED OLANGC MATERIALIZATION: PASS root_sha256=$wasm_project_sha256"
    emit_line "OSTADIX HOSTED OLANGC WASM ARTIFACT: PASS $wasm_artifact_record"
  else
    for mcp_diagnostic_path in \
      /tmp/ostadix-mcp-smoke.out /tmp/ostadix-mcp-smoke.err; do
      if [ -f "$mcp_diagnostic_path" ]; then
        emit_line "MCP DIAGNOSTIC: $mcp_diagnostic_path"
        tail -c 16384 "$mcp_diagnostic_path" 2>/dev/null \
          | while IFS= read -r mcp_diagnostic_line \
              || [ -n "$mcp_diagnostic_line" ]; do
              emit_line "MCP DIAGNOSTIC: $mcp_diagnostic_line"
            done
      fi
    done
    emit_error 'OSTADIX HOSTED OLANGC MATERIALIZATION: FAIL'
    emit_error 'OSTADIX HOSTED OLANGC WASM ARTIFACT: FAIL'
    emit_error 'OSTADIX HOSTED MCP: FAIL'
    hosted_shell
  fi
  cat >/tmp/ostadix-rust-wasm-probe.rs <<'RUST_WASM_SOURCE'
fn main() {
    println!("ostadix-rust-wasm-probe");
}
RUST_WASM_SOURCE
  if rustc --edition 2021 --target wasm32-wasip1 \
      --crate-name ostadix_rust_wasm_probe \
      /tmp/ostadix-rust-wasm-probe.rs \
      -o /tmp/ostadix-rust-wasm-probe.wasm \
      >/tmp/ostadix-rust-wasm-probe.out \
      2>/tmp/ostadix-rust-wasm-probe.err \
      && python3 /usr/src/ostadix/scripts/ostadix_wasm_release.py \
        verify-module /tmp/ostadix-rust-wasm-probe.wasm \
        >/tmp/ostadix-rust-wasm-module.json \
        2>>/tmp/ostadix-rust-wasm-probe.err; then
    emit_line 'OSTADIX HOSTED RUST WASM: PASS'
  else
    emit_error 'OSTADIX HOSTED RUST WASM: FAIL'
    tail -c 16384 /tmp/ostadix-rust-wasm-probe.err >&2 2>/dev/null || true
    hosted_shell
  fi
  wasm_runtime_version=$(wasmtime --version 2>/dev/null || true)
  wasm_tools_version=$(wasm-tools --version 2>/dev/null || true)
  wasm_runtime_version_ok=false
  wasm_tools_version_ok=false
  case "$wasm_runtime_version" in
    'wasmtime 44.0.1'*) wasm_runtime_version_ok=true ;;
  esac
  case "$wasm_tools_version" in
    'wasm-tools 1.236.0'*) wasm_tools_version_ok=true ;;
  esac
  if [ "$wasm_runtime_version_ok" = true ] \
      && [ "$wasm_tools_version_ok" = true ]; then
    emit_line 'OSTADIX HOSTED WASM RUNTIME: PASS'
  else
    emit_error "OSTADIX HOSTED WASM RUNTIME: FAIL: wasmtime=$wasm_runtime_version wasm-tools=$wasm_tools_version"
    hosted_shell
  fi
  olangc_wasm_run_status=0
  timeout -s TERM -k 5 300 \
    wasmtime /usr/share/ostadix/wasm/hello.wasm \
    >/tmp/ostadix-olangc-wasm-run.out \
    2>/tmp/ostadix-olangc-wasm-run.err \
    || olangc_wasm_run_status=$?
  olangc_wasm_output_marker=false
  if grep -Fq 'OSTADIX OLANGC WASM EXECUTION PASS' \
      /tmp/ostadix-olangc-wasm-run.out; then
    olangc_wasm_output_marker=true
  fi
  if [ "$olangc_wasm_run_status" -eq 0 ] \
      && [ "$olangc_wasm_output_marker" = true ]; then
    emit_line 'OSTADIX HOSTED OLANGC WASM EXECUTION: PASS'
  else
    emit_line "OLANGC WASM DIAGNOSTIC: status=$olangc_wasm_run_status output_marker=$olangc_wasm_output_marker"
    for olangc_wasm_diagnostic_path in \
      /tmp/ostadix-olangc-wasm-run.out /tmp/ostadix-olangc-wasm-run.err; do
      if [ -f "$olangc_wasm_diagnostic_path" ]; then
        emit_line "OLANGC WASM DIAGNOSTIC: $olangc_wasm_diagnostic_path"
        tail -c 16384 "$olangc_wasm_diagnostic_path" 2>/dev/null \
          | while IFS= read -r olangc_wasm_diagnostic_line \
              || [ -n "$olangc_wasm_diagnostic_line" ]; do
              emit_line "OLANGC WASM DIAGNOSTIC: $olangc_wasm_diagnostic_line"
            done
      fi
    done
    emit_error 'OSTADIX HOSTED OLANGC WASM EXECUTION: FAIL'
    hosted_shell
  fi
  webassembly_backend_status=0
  timeout -s TERM -k 5 120 \
    O /opt/ostadix/examples/webassembly_hello.O "$O_BACKENDS_DIR" \
      >/tmp/ostadix-webassembly-backend.out \
      2>/tmp/ostadix-webassembly-backend.err \
      || webassembly_backend_status=$?
  webassembly_backend_output_marker=false
  if grep -Fqx 'OSTADIX WEBASSEMBLY BACKEND PASS' \
      /tmp/ostadix-webassembly-backend.out; then
    webassembly_backend_output_marker=true
  fi
  if [ "$webassembly_backend_status" -eq 0 ] \
      && [ "$webassembly_backend_output_marker" = true ]; then
    emit_line 'OSTADIX HOSTED WEBASSEMBLY BACKEND: PASS'
  else
    emit_line "WEBASSEMBLY BACKEND DIAGNOSTIC: status=$webassembly_backend_status output_marker=$webassembly_backend_output_marker"
    for webassembly_backend_diagnostic_path in \
      /tmp/ostadix-webassembly-backend.out \
      /tmp/ostadix-webassembly-backend.err; do
      if [ -f "$webassembly_backend_diagnostic_path" ]; then
        emit_line "WEBASSEMBLY BACKEND DIAGNOSTIC: $webassembly_backend_diagnostic_path"
        tail -c 16384 "$webassembly_backend_diagnostic_path" 2>/dev/null \
          | while IFS= read -r webassembly_backend_diagnostic_line \
              || [ -n "$webassembly_backend_diagnostic_line" ]; do
              emit_line "WEBASSEMBLY BACKEND DIAGNOSTIC: $webassembly_backend_diagnostic_line"
            done
      fi
    done
    emit_error 'OSTADIX HOSTED WEBASSEMBLY BACKEND: FAIL'
    hosted_shell
  fi
  emit_line 'OSTADIX HOSTED MCP: PASS'
  if o object verify >/tmp/ostadix-object-verify.out \
      2>/tmp/ostadix-object-verify.err; then
    emit_line 'OSTADIX BOOT OBJECTS: PASS'
  else
    emit_error 'OSTADIX BOOT OBJECTS: FAIL'
    cat /tmp/ostadix-object-verify.err >&2 2>/dev/null || true
    hosted_shell
  fi
  if python3 /usr/src/ostadix/scripts/ostadix_boot_objects.py verify \
      --store /usr/share/ostadix/boot-objects/v1 \
      --source-root /usr/src/ostadix --json \
      >/tmp/ostadix-source-object-verify.json \
      2>/tmp/ostadix-source-object-verify.err; then
    emit_line 'OSTADIX HOSTED SOURCE OBJECT CLOSURE: PASS'
  else
    emit_error 'OSTADIX HOSTED SOURCE OBJECT CLOSURE: FAIL'
    cat /tmp/ostadix-source-object-verify.err >&2 2>/dev/null || true
    hosted_shell
  fi
    hosted_ready() {
      emit_line 'OSTADIX HOSTED LIVE READY'
      emit_line 'Try: O /opt/ostadix/examples/hello.O "$O_BACKENDS_DIR"'
      emit_line '     O --repl "$O_BACKENDS_DIR"'
      emit_line '     olangc /opt/ostadix/examples/hello.O --target ir --shim-dir "$O_BACKENDS_DIR"'
    }
    hosted_ready
    emit_line 'Source: /usr/src/ostadix    Scratch workspace: /workspace'
    if ! ostadix-desktop launch; then
      emit_error 'OSTADIX HOSTED DESKTOP: FAIL: launcher returned nonzero'
      hosted_shell
    fi
    emit_error 'OSTADIX HOSTED DESKTOP: FAIL: launcher returned unexpectedly'
    hosted_shell
  fi
  hosted_ready() {
    emit_line 'OSTADIX HOSTED LIVE READY'
    emit_line 'Try: O /opt/ostadix/examples/hello.O "$O_BACKENDS_DIR"'
    emit_line '     O --repl "$O_BACKENDS_DIR"'
    emit_line '     olangc /opt/ostadix/examples/hello.O --target ir --shim-dir "$O_BACKENDS_DIR"'
  }
  hosted_ready
  hosted_shell
fi

# Ventoy 1.1.x's Alpine hook inserts its mapper setup between this ebegin/eend
# pair. Direct optical and raw-USB boots pass through the same no-op helpers.
ebegin() { :; }
eend() { :; }
ebegin 'Mounting boot media'
:
eend 0
if ! modprobe dm_mod 2>/dev/null; then
  echo 'OSTADIX CAPACITY HOST ERROR: cannot load required module dm_mod' >&2
  exec sh
fi

media=
attempt=0
while [ "$attempt" -lt 30 ] && [ -z "$media" ]; do
  mdev -s 2>/dev/null || true
  for device in \
    /dev/disk/by-label/OSTADIX_CAPACITY /dev/mapper/ventoy /dev/dm-* \
    /dev/sr* /dev/sd* /dev/vd* /dev/nvme*n* /dev/mmcblk*p*; do
    [ -b "$device" ] || continue
    block_identity=" $(blkid "$device" 2>/dev/null || true) "
    case "$block_identity" in
      *' LABEL="OSTADIX_CAPACITY" '*) ;;
      *) continue ;;
    esac
    if mount -t iso9660 -o ro "$device" /media/ostadix 2>/dev/null; then
      if [ -f /media/ostadix/ostadix/capacity.lock.json ]; then
        media=/media/ostadix
        break
      fi
      umount /media/ostadix || true
    fi
  done
  attempt=$((attempt + 1))
  [ -n "$media" ] || sleep 1
done
if [ -z "$media" ]; then
  echo 'OSTADIX CAPACITY HOST ERROR: labeled capacity ISO could not be mounted' >&2
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
STAGE_DU_BYTES=$(du -sb -- "$STAGE" | awk '{print $1}')
UNCOMPRESSED_SIZE_FILE="$WORK_DIR/uncompressed-cpio-bytes.txt"

pack_cpio() {
  local source_stage=$1 candidate=$2 size_file=$3
  (
    cd "$source_stage"
    find . -xdev -print0 | LC_ALL=C sort -z \
      | cpio --quiet --null --reproducible -o -H newc \
      | "$PYTHON" -c 'import os, sys
total = 0
while True:
    chunk = sys.stdin.buffer.read(1024 * 1024)
    if not chunk:
        break
    total += len(chunk)
    sys.stdout.buffer.write(chunk)
sys.stdout.buffer.flush()
descriptor = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o444)
try:
    os.write(descriptor, f"{total}\n".encode("ascii"))
    os.fsync(descriptor)
finally:
    os.close(descriptor)' "$size_file" \
      | gzip -n -9 >"$candidate"
  )
}

ROOTFS_CANDIDATE=
ROOTFS_BYTES=0
ROOTFS_SHA256=
if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then
  ROOTFS_CANDIDATE="$WORK_DIR/rootfs.squashfs"
  env -u SOURCE_DATE_EPOCH mksquashfs "$STAGE" "$ROOTFS_CANDIDATE" \
    -comp gzip -noappend -no-recovery -no-exports -no-xattrs \
    -repro-time "$SOURCE_DATE_EPOCH" -processors 1 -no-progress -exit-on-error \
    >/dev/null
  chmod 0444 "$ROOTFS_CANDIDATE"
  ROOTFS_BYTES=$(wc -c <"$ROOTFS_CANDIDATE" | tr -d ' ')
  ROOTFS_SHA256=$(sha256sum "$ROOTFS_CANDIDATE" | awk '{print $1}')

  BOOT_STAGE="$WORK_DIR/bootstrap"
  MODULE_META="$WORK_DIR/module-meta"
  mkdir -p -- "$BOOT_STAGE" "$MODULE_META"
  (
    cd "$BOOT_STAGE"
    gzip -dc "$ALPINE_INITRAMFS" | cpio --quiet -idmu --no-absolute-filenames
  )
  unsquashfs -f -d "$MODULE_META" "$MODLOOP" \
    "modules/$ALPINE_KERNEL_RELEASE/modules.dep" >/dev/null
  mapfile -t BOOT_MODULE_PATHS < <(
    "$PYTHON" - "$MODULE_META/modules/$ALPINE_KERNEL_RELEASE/modules.dep" <<'PY'
from pathlib import Path
import sys

dependencies: dict[str, list[str]] = {}
for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    name, separator, values = line.partition(":")
    if separator:
        dependencies[name] = values.split()

roots = [
    "kernel/drivers/ata/ahci.ko",
    "kernel/drivers/ata/ata_piix.ko",
    "kernel/drivers/block/loop.ko",
    "kernel/drivers/char/hw_random/virtio-rng.ko",
    "kernel/drivers/md/dm-mod.ko",
    "kernel/drivers/nvme/host/nvme.ko",
    "kernel/drivers/scsi/sd_mod.ko",
    "kernel/drivers/scsi/sr_mod.ko",
    "kernel/drivers/usb/host/ehci-pci.ko",
    "kernel/drivers/usb/host/ohci-pci.ko",
    "kernel/drivers/usb/host/uhci-hcd.ko",
    "kernel/drivers/usb/host/xhci-pci.ko",
    "kernel/drivers/usb/storage/uas.ko",
    "kernel/drivers/usb/storage/usb-storage.ko",
    "kernel/drivers/virtio/virtio_pci.ko",
    "kernel/fs/isofs/isofs.ko",
    "kernel/fs/overlayfs/overlay.ko",
    "kernel/fs/squashfs/squashfs.ko",
]
missing = sorted(set(roots) - dependencies.keys())
if missing:
    raise SystemExit(f"error: pinned LTS modloop omitted bootstrap modules: {missing}")
closure: set[str] = set()
pending = list(roots)
while pending:
    path = pending.pop()
    if path in closure:
        continue
    if path not in dependencies:
        raise SystemExit(f"error: pinned LTS module dependency is undeclared: {path}")
    closure.add(path)
    pending.extend(dependencies[path])
for path in sorted(closure):
    print(f"modules/{Path(sys.argv[1]).parent.name}/{path}")
PY
  )
  MODULE_METADATA=(
    modules.alias modules.alias.bin modules.builtin modules.builtin.alias.bin
    modules.builtin.bin modules.builtin.modinfo modules.dep modules.dep.bin
    modules.devname modules.order modules.softdep modules.symbols
    modules.symbols.bin modules.weakdep
  )
  BOOT_MODULE_INPUTS=("${BOOT_MODULE_PATHS[@]}")
  for metadata_name in "${MODULE_METADATA[@]}"; do
    BOOT_MODULE_INPUTS+=("modules/$ALPINE_KERNEL_RELEASE/$metadata_name")
  done
  unsquashfs -f -d "$BOOT_STAGE/usr/lib" "$MODLOOP" \
    "${BOOT_MODULE_INPUTS[@]}" >/dev/null
  if [[ ! -e "$BOOT_STAGE/lib/modules" && ! -L "$BOOT_STAGE/lib/modules" ]]; then
    ln -s ../usr/lib/modules "$BOOT_STAGE/lib/modules"
  fi
  install -d -m 0755 "$BOOT_STAGE/etc"
  {
    printf 'OSTADIX_ROOTFS_BYTES=%s\n' "$ROOTFS_BYTES"
    printf 'OSTADIX_ROOTFS_SHA256=%s\n' "$ROOTFS_SHA256"
    printf 'OSTADIX_ROOTFS_PATH=%s\n' /boot/hosted/rootfs.squashfs
    printf 'OSTADIX_VENTOY_MODLOOP_PATH=%s\n' /boot/modloop-lts
  } >"$BOOT_STAGE/etc/ostadix-rootfs.identity"
  chmod 0444 "$BOOT_STAGE/etc/ostadix-rootfs.identity"

  VENTOY_MODLOOP_ROOT="$WORK_DIR/ventoy-modloop-root"
  VENTOY_MODLOOP_CANDIDATE="$WORK_DIR/modloop-lts"
  install -d -m 0755 \
    "$VENTOY_MODLOOP_ROOT/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/md"
  install -m 0444 \
    "$BOOT_STAGE/usr/lib/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/md/dm-mod.ko" \
    "$VENTOY_MODLOOP_ROOT/modules/$ALPINE_KERNEL_RELEASE/kernel/drivers/md/dm-mod.ko"
  env -u SOURCE_DATE_EPOCH \
    mksquashfs "$VENTOY_MODLOOP_ROOT" "$VENTOY_MODLOOP_CANDIDATE" \
    -comp gzip -noappend -no-recovery -no-exports -no-xattrs \
    -repro-time "$SOURCE_DATE_EPOCH" -processors 1 -no-progress -exit-on-error \
    >/dev/null
  chmod 0444 "$VENTOY_MODLOOP_CANDIDATE"
  VENTOY_MODLOOP_BYTES=$(wc -c <"$VENTOY_MODLOOP_CANDIDATE" | tr -d ' ')
  VENTOY_MODLOOP_SHA256=$(sha256sum "$VENTOY_MODLOOP_CANDIDATE" | awk '{print $1}')

  tee "$BOOT_STAGE/init" >/dev/null <<'BOOTSTRAP_INIT'
#!/bin/sh
set -eu

BB=/bin/busybox
PATH=/usr/local/bin:/sbin:/bin:/usr/sbin:/usr/bin
export PATH
. /etc/ostadix-rootfs.identity

fail() {
  message="OSTADIX HOSTED BOOTSTRAP: FAIL: $*"
  "$BB" echo "$message" >/dev/console 2>/dev/null || true
  "$BB" echo "$message" >/dev/ttyS0 2>/dev/null || true
  exec "$BB" sh
}

"$BB" mount -t proc proc /proc || fail 'cannot mount proc'
"$BB" mount -t sysfs sysfs /sys || fail 'cannot mount sysfs'
"$BB" mount -t devtmpfs devtmpfs /dev || fail 'cannot mount devtmpfs'
"$BB" mkdir -p /media/ostadix /lower /upper /newroot

for module in \
  ahci ata_piix nvme virtio_pci virtio_rng \
  ehci_pci ohci_pci uhci_hcd xhci_pci \
  usb_storage uas sd_mod sr_mod isofs loop squashfs overlay; do
  modprobe "$module" 2>/dev/null || fail "cannot load required module $module"
done
"$BB" mdev -s || fail 'cannot populate device nodes'

# Ventoy 1.1.x's Alpine hook inserts its mapper setup between this ebegin/eend
# pair. On direct optical or raw-USB boot these no-op helpers keep the same path.
ebegin() { :; }
eend() { :; }
ebegin 'Mounting boot media'
:
eend 0
modprobe dm_mod 2>/dev/null || fail 'cannot load required module dm_mod'

rootfs_argument=
modloop_argument=
for argument in $("$BB" cat /proc/cmdline); do
  case "$argument" in
    ostadix.rootfs=*) rootfs_argument=${argument#ostadix.rootfs=} ;;
    modloop=*) modloop_argument=${argument#modloop=} ;;
  esac
done
[ "$rootfs_argument" = "$OSTADIX_ROOTFS_PATH" ] \
  || fail "unexpected rootfs path $rootfs_argument"
[ "$modloop_argument" = "$OSTADIX_VENTOY_MODLOOP_PATH" ] \
  || fail "unexpected Ventoy modloop path $modloop_argument"

media=
rootfs_file=
attempt=0
while [ "$attempt" -lt 30 ] && [ -z "$media" ]; do
  "$BB" mdev -s 2>/dev/null || true
  for device in \
    /dev/disk/by-label/OSTADIX_CAPACITY /dev/mapper/ventoy /dev/dm-* \
    /dev/sr* /dev/sd* /dev/vd* /dev/nvme*n* /dev/mmcblk*p*; do
    [ -b "$device" ] || continue
    block_identity=" $("$BB" blkid "$device" 2>/dev/null || true) "
    case "$block_identity" in
      *' LABEL="OSTADIX_CAPACITY" '*) ;;
      *) continue ;;
    esac
    if "$BB" mount -t iso9660 -o ro "$device" /media/ostadix 2>/dev/null; then
      candidate_rootfs="/media/ostadix$OSTADIX_ROOTFS_PATH"
      if [ -f /media/ostadix/ostadix/capacity.lock.json ] \
          && [ -f "$candidate_rootfs" ]; then
        set -- $("$BB" wc -c "$candidate_rootfs")
        if [ "$1" = "$OSTADIX_ROOTFS_BYTES" ]; then
          set -- $("$BB" sha256sum "$candidate_rootfs")
          if [ "$1" = "$OSTADIX_ROOTFS_SHA256" ]; then
            media=$device
            rootfs_file=$candidate_rootfs
            break
          fi
        fi
      fi
      "$BB" umount /media/ostadix || true
    fi
  done
  attempt=$((attempt + 1))
  [ -n "$media" ] || "$BB" sleep 1
done
[ -n "$media" ] || fail 'cannot locate OSTADIX_CAPACITY media by label'
[ -n "$rootfs_file" ] || fail 'verified rootfs path was not retained'

loop_device=$("$BB" losetup -f) || fail 'cannot allocate loop device'
"$BB" losetup -r "$loop_device" "$rootfs_file" || fail 'cannot bind rootfs loop device'
"$BB" mount -t squashfs -o ro "$loop_device" /lower \
  || fail 'cannot mount verified SquashFS root'
"$BB" mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /upper \
  || fail 'cannot mount writable overlay storage'
"$BB" mkdir -p /upper/root /upper/work
"$BB" mount -t overlay \
  -o lowerdir=/lower,upperdir=/upper/root,workdir=/upper/work overlay /newroot \
  || fail 'cannot mount writable root overlay'

"$BB" mkdir -p /newroot/run
"$BB" mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /newroot/run \
  || fail 'cannot mount runtime tmpfs'
"$BB" mkdir -p \
  /newroot/run/ostadix-live/media \
  /newroot/run/ostadix-live/lower \
  /newroot/run/ostadix-live/upper \
  /newroot/proc /newroot/sys /newroot/dev
"$BB" mount --move /media/ostadix /newroot/run/ostadix-live/media \
  || fail 'cannot retain ISO mount'
"$BB" mount --move /lower /newroot/run/ostadix-live/lower \
  || fail 'cannot retain SquashFS mount'
"$BB" mount --move /upper /newroot/run/ostadix-live/upper \
  || fail 'cannot retain overlay storage'
"$BB" mount --move /proc /newroot/proc || fail 'cannot move proc'
"$BB" mount --move /sys /newroot/sys || fail 'cannot move sysfs'
"$BB" mount --move /dev /newroot/dev || fail 'cannot move devtmpfs'

"$BB" echo \
  "OSTADIX HOSTED ROOTFS: PASS bytes=$OSTADIX_ROOTFS_BYTES sha256=$OSTADIX_ROOTFS_SHA256" \
  >/newroot/dev/ttyS0 2>/dev/null || true
exec "$BB" switch_root /newroot /init
BOOTSTRAP_INIT
  chmod 0755 "$BOOT_STAGE/init"
  find "$BOOT_STAGE" -xdev -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
  pack_cpio "$BOOT_STAGE" "$CANDIDATE" "$UNCOMPRESSED_SIZE_FILE"
else
  pack_cpio "$STAGE" "$CANDIDATE" "$UNCOMPRESSED_SIZE_FILE"
fi
chmod 0444 "$CANDIDATE"

metadata=$($PYTHON - \
  "$CANDIDATE" "$STAGE/usr/share/ostadix-capacity-host-packages.txt" \
  "$UNCOMPRESSED_SIZE_FILE" "$STAGE_DU_BYTES" \
  "$ROOTFS_CANDIDATE" "$ROOTFS_BYTES" "$ROOTFS_SHA256" \
  "${VENTOY_MODLOOP_CANDIDATE:-}" "${VENTOY_MODLOOP_BYTES:-0}" \
  "${VENTOY_MODLOOP_SHA256:-}" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

image = Path(sys.argv[1])
packages = Path(sys.argv[2]).read_text(encoding="utf-8").splitlines()
uncompressed_cpio_bytes = int(Path(sys.argv[3]).read_text(encoding="ascii").strip())
stage_du_bytes = int(sys.argv[4])
rootfs_path = Path(sys.argv[5]) if sys.argv[5] else None
rootfs_bytes = int(sys.argv[6])
rootfs_sha256 = sys.argv[7]
ventoy_modloop_path = Path(sys.argv[8]) if sys.argv[8] else None
ventoy_modloop_bytes = int(sys.argv[9])
ventoy_modloop_sha256 = sys.argv[10]
digest = hashlib.sha256()
size = 0
with image.open("rb") as stream:
    while chunk := stream.read(1024 * 1024):
        digest.update(chunk)
        size += len(chunk)
payload = {
    "schema": "ostadix.capacity-host-initramfs/v2" if rootfs_path else "ostadix.capacity-host-initramfs/v1",
    "architecture": "x86_64",
    "bytes": size,
    "uncompressed_cpio_bytes": uncompressed_cpio_bytes,
    "stage_du_bytes": stage_du_bytes,
    "sha256": digest.hexdigest(),
    "packages": packages,
}
if rootfs_path:
    if rootfs_path.stat().st_size != rootfs_bytes:
        raise SystemExit("error: SquashFS size changed before publication")
    rootfs_hasher = hashlib.sha256()
    with rootfs_path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            rootfs_hasher.update(chunk)
    rootfs_digest = rootfs_hasher.hexdigest()
    if rootfs_digest != rootfs_sha256:
        raise SystemExit("error: SquashFS digest changed before publication")
    payload["rootfs"] = {
        "bytes": rootfs_bytes,
        "compression": "gzip",
        "format": "squashfs",
        "sha256": rootfs_sha256,
    }
    if not ventoy_modloop_path or ventoy_modloop_path.stat().st_size != ventoy_modloop_bytes:
        raise SystemExit("error: Ventoy modloop size changed before publication")
    ventoy_modloop_hasher = hashlib.sha256()
    with ventoy_modloop_path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            ventoy_modloop_hasher.update(chunk)
    if ventoy_modloop_hasher.hexdigest() != ventoy_modloop_sha256:
        raise SystemExit("error: Ventoy modloop digest changed before publication")
    payload["ventoy_modloop"] = {
        "bytes": ventoy_modloop_bytes,
        "compression": "gzip",
        "format": "squashfs",
        "sha256": ventoy_modloop_sha256,
    }
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
)
if [[ -n "$ROOTFS_CANDIDATE" ]]; then
  mv -- "$ROOTFS_CANDIDATE" "$ROOTFS_OUTPUT"
  chmod 0444 "$ROOTFS_OUTPUT"
  mv -- "$VENTOY_MODLOOP_CANDIDATE" "$VENTOY_MODLOOP_OUTPUT"
  chmod 0444 "$VENTOY_MODLOOP_OUTPUT"
fi
mv -- "$CANDIDATE" "$OUTPUT"
chmod 0444 "$OUTPUT"
printf '%s\n' "$metadata"
printf 'capacity-host-initramfs: %s\n' "$OUTPUT"
if [[ -n "$ROOTFS_OUTPUT" ]]; then
  printf 'capacity-host-rootfs: %s\n' "$ROOTFS_OUTPUT"
  printf 'capacity-host-ventoy-modloop: %s\n' "$VENTOY_MODLOOP_OUTPUT"
fi
