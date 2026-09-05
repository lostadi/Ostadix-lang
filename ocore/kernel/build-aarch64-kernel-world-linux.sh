#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD="${OCORE_LINUX_BUILD_DIR:-$ROOT/target/ocore-real-linux/monitor}"
PAYLOAD="${OCORE_LINUX_PAYLOAD_DIR:-$ROOT/target/ocore-real-linux/payload}"
OCOREC="${OCORE_LINUX_OCOREC_BIN:-$ROOT/target/debug/ocorec}"
LLD="${OCORE_LINUX_LLD:-ld.lld}"
for tool in clang dtc python3 "$LLD"; do
    command -v "$tool" >/dev/null || { echo "missing monitor build tool: $tool" >&2; exit 2; }
done
[[ -x "$OCOREC" ]] || { echo "missing already-built compiler: $OCOREC" >&2; exit 2; }
mkdir -p "$BUILD"
python3 - "$PAYLOAD" "$ROOT/ocore/kernel/aarch64/kernel_world/guest.dts" "$BUILD/guest.dts" <<'PY'
import hashlib
import json
from pathlib import Path
import struct
import sys

payload, template, output = map(Path, sys.argv[1:])
manifest = json.loads((payload / "linux-payload.json").read_text())
if manifest["linux_version"] != "6.12.43" or manifest["source_modifications"] is not False:
    raise SystemExit("unexpected pinned Linux payload identity")
if manifest["source_archive_sha256"] != "0fcbbbbcd456e87bbbfc8bf37af541fda62ccfcce76903503424fd101ef7bdee":
    raise SystemExit("unexpected upstream source digest")
image = (payload / "Image").read_bytes()
if len(image) != manifest["artifacts"]["Image"]["bytes"] or hashlib.sha256(image).hexdigest() != manifest["artifacts"]["Image"]["sha256"]:
    raise SystemExit("Linux Image differs from payload manifest")
offset, size = struct.unpack_from("<QQ", image, 8)
if image[56:60] != b"ARM\x64" or offset != 0 or not 0 < size <= 0x07000000:
    raise SystemExit("Linux Image exceeds the bounded direct-boot layout")
initrd_size = (payload / "initramfs.cpio").stat().st_size
if not 0 < initrd_size <= 0x08000000:
    raise SystemExit("initramfs exceeds its 128MiB payload window")
output.write_text(template.read_text().replace("INITRD_END", hex(0x48000000 + initrd_size)))
PY
dtc -I dts -O dtb -o "$BUILD/guest.dtb" "$BUILD/guest.dts"
"$OCOREC" \
    "$ROOT/ocore/runtime/aarch64/kernel_world_monitor.oc" \
    "$ROOT/ocore/runtime/aarch64/kernel_world_virtio.oc" \
    "$ROOT/ocore/runtime/aarch64/kernel_world_virtio_selftest.oc" \
    --target aarch64-unknown-none --emit obj --keep-asm -o "$BUILD/kernel.o"
clang -target aarch64-unknown-none-elf -ffreestanding -fno-stack-protector \
    -c "$ROOT/ocore/kernel/aarch64/kernel_world/monitor.S" -o "$BUILD/monitor.o"
"$LLD" -m aarch64elf -nostdlib --build-id=none -z max-page-size=0x1000 \
    -T "$ROOT/ocore/kernel/aarch64/kernel_world/linker.ld" \
    -o "$BUILD/monitor.elf" "$BUILD/monitor.o" "$BUILD/kernel.o"
printf 'KernelWorld AArch64 monitor: %s\n' "$BUILD/monitor.elf"
