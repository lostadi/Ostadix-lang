#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
QEMU_BIN="${OCORE_QEMU_BIN:-qemu-system-x86_64}"
MEDIA="${OSTADIX_MEDIA_IMAGE:-$ROOT/target/ostadix-media/x86_64/ostadix-x86_64-uefi.img}"

if [[ $# -ne 0 ]]; then
  echo "usage: run-x86_64-uefi-media-qemu.sh" >&2
  exit 2
fi
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  printf 'error: QEMU executable is unavailable: %s\n' "$QEMU_BIN" >&2
  exit 127
fi

if [[ -z "${OSTADIX_OVMF_CODE:-}" ]]; then
  for candidate in \
    /opt/homebrew/opt/qemu/share/qemu/edk2-x86_64-code.fd \
    /usr/local/opt/qemu/share/qemu/edk2-x86_64-code.fd \
    /usr/share/OVMF/OVMF_CODE.fd \
    /usr/share/edk2/x64/OVMF_CODE.fd; do
    if [[ -f "$candidate" ]]; then
      OSTADIX_OVMF_CODE="$candidate"
      break
    fi
  done
fi
if [[ -z "${OSTADIX_OVMF_CODE:-}" || ! -f "$OSTADIX_OVMF_CODE" ]]; then
  echo "error: UEFI firmware not found; set OSTADIX_OVMF_CODE to an OVMF/edk2 x86_64 code image" >&2
  exit 127
fi

"$ROOT/ocore/kernel/build-x86_64-uefi-media.sh" "$MEDIA"
cat >&2 <<EOF

OSTADIX x86_64 UEFI media boot
  firmware: $OSTADIX_OVMF_CODE
  media:    $MEDIA
  exit:     Ctrl-A X

EOF

exec "$QEMU_BIN" \
  -accel tcg \
  -machine q35 \
  -m 128M \
  -drive "if=pflash,unit=0,format=raw,readonly=on,file=$OSTADIX_OVMF_CODE" \
  -drive "if=none,id=ostadix,format=raw,readonly=on,file=$MEDIA" \
  -device virtio-blk-pci,drive=ostadix \
  -nodefaults \
  -nic none \
  -display none \
  -serial mon:stdio \
  -no-reboot \
  -no-shutdown
