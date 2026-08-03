#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KERNEL_DIR="$ROOT/ocore/kernel/aarch64"
RUNTIME_DIR="$ROOT/ocore/runtime/aarch64"
BUILD_DIR="${OCORE_G2_BUILD_DIR:-$ROOT/target/ocore-g2-aarch64/build}"

mkdir -p "$BUILD_DIR"

if [[ -n "${OCORE_G2_OCOREC_BIN:-}" ]]; then
  OCOREC_BIN="$OCORE_G2_OCOREC_BIN"
  if [[ ! -x "$OCOREC_BIN" ]]; then
    echo "error: OCORE_G2_OCOREC_BIN is not executable: $OCOREC_BIN" >&2
    exit 2
  fi
else
  cargo build --quiet --locked --manifest-path "$ROOT/Cargo.toml" --bin ocorec
  OCOREC_BIN="$ROOT/target/debug/ocorec"
fi

if ! command -v clang >/dev/null 2>&1; then
  echo "error: clang is required for the AArch64 G2 boot/vector objects" >&2
  exit 127
fi

find_lld() {
  if [[ -n "${OCORE_G2_LLD:-}" ]]; then
    if [[ -x "$OCORE_G2_LLD" ]]; then
      printf '%s\n' "$OCORE_G2_LLD"
      return 0
    fi
    echo "error: OCORE_G2_LLD is not executable: $OCORE_G2_LLD" >&2
    return 1
  fi

  local candidate
  for candidate in ld.lld rust-lld lld; do
    if command -v "$candidate" >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
  done
  for candidate in \
    /opt/homebrew/opt/lld/bin/ld.lld \
    /opt/homebrew/opt/lld@21/bin/ld.lld \
    /opt/homebrew/opt/llvm/bin/ld.lld \
    /usr/local/opt/lld/bin/ld.lld \
    /usr/local/opt/llvm/bin/ld.lld
  do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  echo "error: no LLD-compatible linker found for AArch64 G2" >&2
  return 1
}

LLD_BIN="$(find_lld)"

"$OCOREC_BIN" \
  "$RUNTIME_DIR/g2_kernel.oc" \
  "$RUNTIME_DIR/g2_user_a.oc" \
  "$RUNTIME_DIR/g2_user_b.oc" \
  --target aarch64-unknown-none \
  --emit obj \
  --keep-asm \
  -o "$BUILD_DIR/kernel.o"

clang -target aarch64-unknown-none-elf \
  -ffreestanding -fno-stack-protector \
  -c -x assembler-with-cpp \
  "$KERNEL_DIR/boot.S" -o "$BUILD_DIR/boot.o"

clang -target aarch64-unknown-none-elf \
  -ffreestanding -fno-stack-protector \
  -c -x assembler-with-cpp \
  "$KERNEL_DIR/vectors.S" -o "$BUILD_DIR/vectors.o"

case "$(basename "$LLD_BIN")" in
  rust-lld | lld)
    "$LLD_BIN" -flavor gnu -m aarch64elf -nostdlib --build-id=none \
      -z max-page-size=0x1000 \
      -T "$KERNEL_DIR/linker.ld" \
      -o "$BUILD_DIR/kernel.elf" \
      "$BUILD_DIR/boot.o" "$BUILD_DIR/vectors.o" "$BUILD_DIR/kernel.o"
    ;;
  *)
    "$LLD_BIN" -m aarch64elf -nostdlib --build-id=none \
      -z max-page-size=0x1000 \
      -T "$KERNEL_DIR/linker.ld" \
      -o "$BUILD_DIR/kernel.elf" \
      "$BUILD_DIR/boot.o" "$BUILD_DIR/vectors.o" "$BUILD_DIR/kernel.o"
    ;;
esac

printf 'g2-aarch64-object: %s\n' "$BUILD_DIR/kernel.o"
printf 'g2-aarch64-assembly: %s\n' "$BUILD_DIR/kernel.s"
printf 'g2-aarch64-kernel: %s\n' "$BUILD_DIR/kernel.elf"
