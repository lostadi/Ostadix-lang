#!/usr/bin/env bash
# Inspect and boot the published absorbed-capacity ISO as read-only UEFI media.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
QEMU_BIN=${OCORE_QEMU_BIN:-qemu-system-x86_64}
ISO=${1:-${OSTADIX_CAPACITY_ISO_IMAGE:-"$ROOT/target/ostadix-capacity-iso/x86_64/ostadix-absorbed-capacity-x86_64-uefi.iso"}}
INSPECTOR=${OSTADIX_CAPACITY_ISO_INSPECTOR:-"$ROOT/scripts/ostadix_capacity_iso.py"}
PYTHON=${OSTADIX_PYTHON:-python3}

if [[ $# -gt 1 ]]; then
  printf 'usage: run-x86_64-capacity-iso-qemu.sh [ISO]\n' >&2
  exit 2
fi
for executable in "$QEMU_BIN" "$PYTHON"; do
  command -v "$executable" >/dev/null 2>&1 || {
    printf 'error: required executable is unavailable: %s\n' "$executable" >&2
    exit 127
  }
done
if [[ -L "$INSPECTOR" || ! -f "$INSPECTOR" || ! -x "$INSPECTOR" ]]; then
  printf 'error: capacity ISO inspector is not an executable non-symlink file: %s\n' \
    "$INSPECTOR" >&2
  exit 1
fi
if [[ -L "$ISO" || ! -f "$ISO" ]]; then
  printf 'error: build the capacity ISO first; no regular image exists at %s\n' "$ISO" >&2
  exit 1
fi

# shellcheck source=resolve-x86_64-ovmf-code.sh
source "$ROOT/ocore/kernel/resolve-x86_64-ovmf-code.sh"
OSTADIX_OVMF_CODE=$(resolve_ostadix_x86_64_ovmf_code "$QEMU_BIN")

exec "$PYTHON" -c '
import importlib.util
import os
from pathlib import Path
import sys

inspector_text, qemu, firmware_text, media_text = sys.argv[1:]
spec = importlib.util.spec_from_file_location("ostadix_capacity_iso", inspector_text)
if spec is None or spec.loader is None:
    raise SystemExit("error: cannot load the capacity ISO inspector")
inspector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inspector)

media_descriptor = -1
firmware_descriptor = -1
try:
    media_descriptor = inspector._open_pinned_regular(
        Path(media_text), "capacity ISO", readonly=True
    )
    metadata = inspector.inspect_descriptor(media_descriptor, media_text)
    firmware = Path(firmware_text).resolve(strict=True)
    firmware_descriptor = inspector._open_pinned_regular(firmware, "OVMF code")
    hotkeys = "/".join(entry["hotkey"] for entry in metadata["entries"])
    print("", file=sys.stderr)
    print("OSTADIX x86_64 UEFI ISO", file=sys.stderr)
    print(f"  firmware: {firmware}", file=sys.stderr)
    print(f"  iso:      {media_text}", file=sys.stderr)
    print("  media:    descriptor-pinned, read-only El Torito CD-ROM", file=sys.stderr)
    print("  network:  disabled for the outer QEMU machine", file=sys.stderr)
    print(f"  select:   GRUB hotkeys {hotkeys} or arrow keys", file=sys.stderr)
    print("  exit:     Ctrl-A X", file=sys.stderr)
    print("", file=sys.stderr)
    print(
        "OSTADIX ISO IDENTITY bytes={} sha256={}".format(
            metadata["bytes"], metadata["sha256"]
        ),
        file=sys.stderr,
        flush=True,
    )
    for entry in metadata["entries"]:
        print(
            "  [{}] {} adapter={}".format(
                entry["hotkey"], entry["title"], entry["adapter"]
            ),
            file=sys.stderr,
        )

    media_fd_path = f"/dev/fd/{media_descriptor}"
    firmware_fd_path = f"/dev/fd/{firmware_descriptor}"
    if not os.path.exists(media_fd_path) or not os.path.exists(firmware_fd_path):
        raise SystemExit("error: this host does not expose inherited files via /dev/fd")
    command = [
        qemu,
        "-accel", "tcg",
        "-machine", "q35",
        "-cpu", "max",
        "-smp", "2",
        "-m", "4096M",
        "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={firmware_fd_path}",
        "-drive", f"if=ide,index=2,media=cdrom,format=raw,readonly=on,file={media_fd_path}",
        "-boot", "order=d,strict=on",
        "-nodefaults",
        # Model the hardware entropy that first-boot key provisioning requires;
        # the outer guest still has no network device.
        "-object", "rng-random,filename=/dev/urandom,id=ostadix_rng",
        "-device", "virtio-rng-pci,rng=ostadix_rng",
        "-nic", "none",
        "-display", "none",
        "-serial", "mon:stdio",
        "-no-reboot",
        "-no-shutdown",
    ]
    if (
        "-kernel" in command
        or command[command.index("-nic") + 1] != "none"
        or media_text in command
        or firmware_text in command
    ):
        raise SystemExit("error: capacity boot escaped its firmware/media boundary")
    os.set_inheritable(media_descriptor, True)
    os.set_inheritable(firmware_descriptor, True)
    os.execvp(qemu, command)
finally:
    if firmware_descriptor >= 0:
        os.close(firmware_descriptor)
    if media_descriptor >= 0:
        os.close(media_descriptor)
' "$INSPECTOR" "$QEMU_BIN" "$OSTADIX_OVMF_CODE" "$ISO"
