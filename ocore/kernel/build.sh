#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KERNEL_DIR="$ROOT/ocore/kernel"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel}"
mkdir -p "$BUILD_DIR"

cargo build --manifest-path "$ROOT/Cargo.toml" --bin ocorec

"$ROOT/target/debug/ocorec" \
  "$ROOT/ocore/runtime/x86_64/serial.oc" \
  "$ROOT/ocore/runtime/x86_64/pages.oc" \
  "$ROOT/ocore/runtime/x86_64/capability.oc" \
  "$ROOT/ocore/runtime/x86_64/interrupts.oc" \
  "$ROOT/ocore/runtime/x86_64/syscall.oc" \
  "$KERNEL_DIR/main.oc" \
  --target x86_64-unknown-none \
  --emit obj \
  --keep-asm \
  -o "$BUILD_DIR/kernel.o"

clang -target x86_64-unknown-none-elf -c -x assembler \
  "$KERNEL_DIR/boot.S" -o "$BUILD_DIR/boot.o"

RUST_SYSROOT="$(rustc --print sysroot)"
LLD="$RUST_SYSROOT/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/rust-lld"
if [[ ! -x "$LLD" ]]; then
  echo "error: rust-lld not found at $LLD" >&2
  exit 1
fi

"$LLD" -flavor gnu -m elf_x86_64 -nostdlib \
  -z max-page-size=0x1000 \
  -T "$KERNEL_DIR/linker.ld" \
  -o "$BUILD_DIR/kernel.elf" \
  "$BUILD_DIR/boot.o" "$BUILD_DIR/kernel.o"

file "$BUILD_DIR/kernel.o"
file "$BUILD_DIR/kernel.elf"
echo "kernel: $BUILD_DIR/kernel.elf"
