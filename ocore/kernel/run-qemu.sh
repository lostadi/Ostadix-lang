#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel}"

"$ROOT/ocore/kernel/build.sh"

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "error: qemu-system-x86_64 is not installed" >&2
  echo "install locally with: brew install qemu" >&2
  exit 127
fi

exec qemu-system-x86_64 \
  -machine q35 \
  -m 128M \
  -kernel "$BUILD_DIR/kernel.elf" \
  -display none \
  -serial stdio \
  -no-reboot \
  -no-shutdown
