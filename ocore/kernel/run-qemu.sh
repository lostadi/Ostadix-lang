#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel}"
PROBE_MODE="${OCORE_PROBE_MODE:-0}"
QEMU_BIN="${OCORE_QEMU_BIN:-qemu-system-x86_64}"

BUILD_OUTPUT="$("$ROOT/ocore/kernel/build.sh")"
printf '%s\n' "$BUILD_OUTPUT"

if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  printf 'error: QEMU executable is not installed: %s\n' "$QEMU_BIN" >&2
  if [[ "$QEMU_BIN" == "qemu-system-x86_64" ]]; then
    echo "install locally with: brew install qemu" >&2
  fi
  exit 127
fi

if [[ "$PROBE_MODE" == "16" ]]; then
  M5_DIGEST="$(
    printf '%s\n' "$BUILD_OUTPUT" | sed -n 's/^m5-sha256: //p' | tail -n 1
  )"
  if [[ ! "$M5_DIGEST" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: mode-16 build did not report the embedded M5 image digest" >&2
    exit 1
  fi
  cat >&2 <<EOF

O-core native console commands after the 'o> ' prompt:
  status
  install $M5_DIGEST 5 1
  activate $M5_DIGEST

The install/activate sequence runs the bounded M5 lifecycle and then parks the
console while the kernel proves fault containment, restart, and reclamation.
Exit QEMU at any time with Ctrl-A X.

EOF
else
  printf '\nO-core baseline boot. Exit QEMU with Ctrl-A X.\n\n' >&2
fi

exec "$QEMU_BIN" \
  -machine q35 \
  -m 128M \
  -kernel "$BUILD_DIR/kernel.elf" \
  -display none \
  -serial mon:stdio \
  -no-reboot \
  -no-shutdown
