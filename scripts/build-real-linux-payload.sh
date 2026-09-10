#!/usr/bin/env bash
# Build the pinned, unmodified foreign kernel on a Linux build machine.
# This produces payloads only; the O-core monitor is built separately.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="${OCORE_LINUX_PAYLOAD_BUILD:-$ROOT/target/ocore-real-linux/payload-build}"
OUT="${OCORE_LINUX_PAYLOAD_OUT:-$ROOT/target/ocore-real-linux/payload}"
VERSION=6.12.43
SHA256=0fcbbbbcd456e87bbbfc8bf37af541fda62ccfcce76903503424fd101ef7bdee
URL="https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$VERSION.tar.xz"
JOBS="${OCORE_LINUX_JOBS:-4}"
export ARCH=arm64
export KBUILD_BUILD_TIMESTAMP='1970-01-01 00:00:00 +0000'
export KBUILD_BUILD_USER=ostadi
export KBUILD_BUILD_HOST=ostadix
export KBUILD_BUILD_VERSION=1
export SOURCE_DATE_EPOCH=0

if [[ "$(uname -s)" != Linux ]]; then
  echo 'Build this payload in a Linux environment (for example Multipass moral-gaur).' >&2
  exit 2
fi
if [[ "$(uname -m)" != aarch64 && -z "${CROSS_COMPILE:-}" ]]; then
  export CROSS_COMPILE=aarch64-linux-gnu-
fi
for tool in make gcc flex bison bc curl tar sha256sum python3; do
  command -v "$tool" >/dev/null || { echo "missing dependency: $tool" >&2; exit 2; }
done
mkdir -p "$BUILD" "$OUT"
ARCHIVE="$BUILD/linux-$VERSION.tar.xz"
if [[ ! -f "$ARCHIVE" ]]; then
  curl --fail --location --retry 2 "$URL" -o "$ARCHIVE.partial"
  mv "$ARCHIVE.partial" "$ARCHIVE"
fi
printf '%s  %s\n' "$SHA256" "$ARCHIVE" | sha256sum -c -
if [[ ! -d "$BUILD/linux-$VERSION" ]]; then
  tar -xf "$ARCHIVE" -C "$BUILD"
fi
# Reused source directories must still match the verified upstream archive.
# Out-of-tree Kbuild leaves this tree untouched.
python3 - "$ARCHIVE" "$BUILD" <<'PY'
import hashlib
import os
from pathlib import Path
import sys
import tarfile

archive, root = Path(sys.argv[1]), Path(sys.argv[2])
with tarfile.open(archive) as source:
    expected_paths = set()
    for member in source:
        expected_paths.add(str(Path(member.name)))
        path = root / member.name
        if member.isfile():
            if not path.is_file() or path.is_symlink():
                raise SystemExit(f"source file missing or replaced: {member.name}")
            with source.extractfile(member) as expected, path.open("rb") as actual:
                if hashlib.file_digest(expected, "sha256").digest() != hashlib.file_digest(actual, "sha256").digest():
                    raise SystemExit(f"source file differs from pinned archive: {member.name}")
        elif member.issym():
            if not path.is_symlink() or os.readlink(path) != member.linkname:
                raise SystemExit(f"source symlink differs from pinned archive: {member.name}")
        elif not member.isdir():
            raise SystemExit(f"unsupported source archive entry: {member.name}")
    source_root = root / "linux-6.12.43"
    actual_paths = {str(p.relative_to(root)) for p in source_root.rglob("*")}
    actual_paths.add(source_root.name)
    if actual_paths != expected_paths:
        raise SystemExit("source tree contains entries absent from the pinned archive")
PY
SRC="$BUILD/linux-$VERSION"
OBJ="$BUILD/build"
mkdir -p "$OBJ"
make -C "$SRC" O="$OBJ" tinyconfig
CONFIG="$SRC/scripts/config"
for symbol in EXPERT MULTIUSER PRINTK BINFMT_ELF BLK_DEV_INITRD RD_GZIP \
  BLOCK BLK_DEV ARM64_PLATFORM_DEVICES ARCH_VIRT ARM_AMBA ARM_GIC ARM_ARCH_TIMER OF TTY \
  SERIAL_AMBA_PL011 SERIAL_AMBA_PL011_CONSOLE VIRTIO_MENU VIRTIO VIRTIO_MMIO \
  VIRTIO_BLK DEVTMPFS DEVTMPFS_MOUNT TMPFS PROC_FS SYSFS FUTEX \
  POSIX_TIMERS; do
  "$CONFIG" --file "$OBJ/.config" --enable "$symbol"
done
"$CONFIG" --file "$OBJ/.config" --disable RANDOMIZE_BASE --disable MODULES \
  --disable DEBUG_INFO --disable ARM64_VA_BITS_52 --disable ARM64_VA_BITS_48 \
  --enable ARM64_VA_BITS_39 --disable ARM64_PA_BITS_52 --enable ARM64_PA_BITS_48 \
  --set-str LOCALVERSION '-ostadix-kernelworld'
make -C "$SRC" O="$OBJ" olddefconfig
for symbol in BINFMT_ELF BLK_DEV_INITRD ARM_GIC ARM_ARCH_TIMER \
  SERIAL_AMBA_PL011_CONSOLE VIRTIO_MMIO VIRTIO_BLK DEVTMPFS; do
  if ! grep -qx "CONFIG_${symbol}=y" "$OBJ/.config"; then
    echo "required builtin kernel option did not resolve: $symbol" >&2
    exit 2
  fi
done
make -C "$SRC" O="$OBJ" -j"$JOBS" Image
cp "$OBJ/arch/arm64/boot/Image" "$OUT/Image"
cp "$OBJ/.config" "$OUT/linux.config"
python3 - "$OUT" "$VERSION" "$SHA256" "$URL" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

out, version, source_sha, source_url = sys.argv[1:]
out = Path(out)
image = (out / "Image").read_bytes()
if image[56:60] != b"ARM\x64":
    raise SystemExit("Linux build did not produce an uncompressed AArch64 Image")
manifest = {
    "schema": "ostadix.real-linux-payload/v1",
    "linux_version": version,
    "source_url": source_url,
    "source_archive_sha256": source_sha,
    "source_modifications": False,
    "compiler": subprocess.check_output([os.environ.get("CROSS_COMPILE", "") + "gcc", "--version"], text=True).splitlines()[0],
    "artifacts": {
        name: {"sha256": hashlib.sha256((out / name).read_bytes()).hexdigest(), "bytes": (out / name).stat().st_size}
        for name in ("Image", "linux.config")
    },
    "scope": "foreign Linux payload; boot and isolation require the O-core monitor qualification",
}
(out / "linux-payload.json").write_text(json.dumps(manifest, indent=2) + "\n")
PY
printf 'Pinned Linux payload: %s\n' "$OUT/Image"
