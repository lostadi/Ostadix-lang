#!/usr/bin/env bash
# Build, inspect, and boot the exact OSTADIX UEFI ISO as read-only CD media.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
QEMU_BIN="${OCORE_QEMU_BIN:-qemu-system-x86_64}"
ISO="${OSTADIX_ISO_IMAGE:-$ROOT/target/ostadix-iso/x86_64/ostadix-x86_64-uefi.iso}"
BUILD_SCRIPT="${OSTADIX_ISO_BUILD_SCRIPT:-$ROOT/ocore/kernel/build-x86_64-uefi-iso.sh}"
INSPECTOR="${OSTADIX_ISO_INSPECTOR:-$ROOT/scripts/ostadix_boot_iso.py}"
PYTHON="${OSTADIX_PYTHON:-python3}"

if [[ $# -ne 0 ]]; then
  echo "usage: run-x86_64-uefi-iso-qemu.sh" >&2
  exit 2
fi
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  printf 'error: QEMU executable is unavailable: %s\n' "$QEMU_BIN" >&2
  exit 127
fi
if ! command -v "$PYTHON" >/dev/null 2>&1; then
  printf 'error: Python executable is unavailable: %s\n' "$PYTHON" >&2
  exit 127
fi
for script in "$BUILD_SCRIPT" "$INSPECTOR"; do
  if [[ -L "$script" || ! -f "$script" || ! -x "$script" ]]; then
    printf 'error: required OSTADIX ISO script is not an executable non-symlink file: %s\n' \
      "$script" >&2
    exit 1
  fi
done

"$BUILD_SCRIPT" "$ISO"
if [[ -L "$ISO" || ! -f "$ISO" ]]; then
  printf 'error: OSTADIX ISO is not a regular non-symlink file: %s\n' "$ISO" >&2
  exit 1
fi
"$INSPECTOR" inspect "$ISO" >/dev/null

# shellcheck source=resolve-x86_64-ovmf-code.sh
source "$ROOT/ocore/kernel/resolve-x86_64-ovmf-code.sh"
OSTADIX_OVMF_CODE="$(resolve_ostadix_x86_64_ovmf_code "$QEMU_BIN")"

cat >&2 <<EOF

OSTADIX x86_64 UEFI ISO boot
  firmware: $OSTADIX_OVMF_CODE
  iso:      $ISO
  media:    read-only El Torito CD-ROM
  exit:     Ctrl-A X

EOF

exec "$PYTHON" -c '
import importlib.util
import os
from pathlib import Path
import sys

inspector_text, qemu, firmware_text, media_text = sys.argv[1:]
spec = importlib.util.spec_from_file_location("ostadix_boot_iso", inspector_text)
if spec is None or spec.loader is None:
    raise SystemExit("error: cannot load the OSTADIX ISO inspector")
inspector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inspector)
media_descriptor = -1
firmware_descriptor = -1
try:
    media_descriptor = inspector._open_pinned_regular(
        Path(media_text), nofollow=True
    )
    inspector.inspect_descriptor(media_descriptor, media_text)
    firmware_descriptor = inspector._open_pinned_regular(
        Path(firmware_text), nofollow=False
    )
    media_fd_path = f"/dev/fd/{media_descriptor}"
    firmware_fd_path = f"/dev/fd/{firmware_descriptor}"
    if not os.path.exists(media_fd_path) or not os.path.exists(firmware_fd_path):
        raise SystemExit(
            "error: this host does not expose inherited descriptors via /dev/fd"
        )
    command = [
        qemu,
        "-accel", "tcg",
        "-machine", "q35",
        "-m", "128M",
        "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={firmware_fd_path}",
        "-drive", f"if=ide,index=2,media=cdrom,format=raw,readonly=on,file={media_fd_path}",
        "-boot", "order=d,strict=on",
        "-nodefaults",
        "-nic", "none",
        "-display", "none",
        "-serial", "mon:stdio",
        "-no-reboot",
        "-no-shutdown",
    ]
    if "-kernel" in command or command[command.index("-nic") + 1] != "none":
        raise SystemExit(
            "error: ISO boot command escaped its firmware-only/no-network boundary"
        )
    os.set_inheritable(media_descriptor, True)
    os.set_inheritable(firmware_descriptor, True)
    os.execvp(qemu, command)
finally:
    if firmware_descriptor >= 0:
        os.close(firmware_descriptor)
    if media_descriptor >= 0:
        os.close(media_descriptor)
' "$INSPECTOR" "$QEMU_BIN" "$OSTADIX_OVMF_CODE" "$ISO"
