#!/usr/bin/env bash
set -euo pipefail

SOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(cd "$SOURCE_DIR/../../.." && pwd -P)"
OUTPUT_DIR="${OCORE_LINUX_GUEST_BUILD_DIR:-$ROOT/target/ocore-linux-guest}"
GUEST_CC="${OCORE_LINUX_GUEST_CC:-aarch64-linux-gnu-gcc}"
mkdir -p "$OUTPUT_DIR"

# Compile on native AArch64 with OCORE_LINUX_GUEST_CC=gcc or use an explicit
# AArch64 cross compiler. Never silently fall back to a host-architecture init.
"$GUEST_CC" -std=c11 -O2 -static -Wall -Wextra -Werror \
  -fno-ident -Wl,--build-id=none "$SOURCE_DIR/init.c" -o "$OUTPUT_DIR/init"

# Construct deterministic newc directly, including /dev/console. No privileged
# mknod, host cpio dialect, timestamps, or compression library is required.
python3 - "$OUTPUT_DIR/init" "$OUTPUT_DIR/initramfs.cpio" <<'PY'
import pathlib
import stat
import struct
import sys

binary_path, archive_path = map(pathlib.Path, sys.argv[1:])
binary = binary_path.read_bytes()
if len(binary) < 64 or binary[:6] != b"\x7fELF\x02\x01":
    raise SystemExit("init must be a little-endian ELF64 executable")
if struct.unpack_from("<H", binary, 18)[0] != 183:
    raise SystemExit("init must target AArch64 (EM_AARCH64=183)")
phoff = struct.unpack_from("<Q", binary, 32)[0]
phentsize, phnum = struct.unpack_from("<HH", binary, 54)
for i in range(phnum):
    offset = phoff + i * phentsize
    if phentsize < 56 or offset + phentsize > len(binary):
        raise SystemExit("malformed ELF program headers")
    if struct.unpack_from("<I", binary, offset)[0] == 3:
        raise SystemExit("init requires an ELF interpreter; compile it statically")

archive = bytearray()

def entry(name, mode, content=b"", major=0, minor=0):
    encoded = name.encode("ascii") + b"\0"
    fields = [1, mode, 0, 0, 1, 0, len(content), 0, 0,
              major, minor, len(encoded), 0]
    archive.extend(b"070701" + b"".join(f"{n:08x}".encode() for n in fields))
    archive.extend(encoded)
    archive.extend(b"\0" * (-len(archive) % 4))
    archive.extend(content)
    archive.extend(b"\0" * (-len(archive) % 4))

entry("dev", stat.S_IFDIR | 0o755)
entry("dev/console", stat.S_IFCHR | 0o600, major=5, minor=1)
entry("init", stat.S_IFREG | 0o755, binary)
entry("TRAILER!!!", 0)
archive.extend(b"\0" * (-len(archive) % 512))
archive_path.write_bytes(archive)
PY

printf 'linux-guest-init: %s\nlinux-guest-initramfs: %s\n' \
  "$OUTPUT_DIR/init" "$OUTPUT_DIR/initramfs.cpio"
