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

find_lld() {
  if [[ -n "${OCORE_LLD:-}" ]]; then
    if [[ -x "$OCORE_LLD" ]]; then
      echo "$OCORE_LLD"
      return 0
    fi
    echo "error: OCORE_LLD is set but not executable: $OCORE_LLD" >&2
    return 1
  fi

  local rust_sysroot
  local rust_host
  rust_sysroot="$(rustc --print sysroot)"
  rust_host="$(rustc -vV | sed -n 's/^host: //p')"

  local candidates=(
    "$rust_sysroot/lib/rustlib/$rust_host/bin/rust-lld"
  )

  local command_candidate
  for command_candidate in rust-lld ld.lld lld; do
    if command -v "$command_candidate" >/dev/null 2>&1; then
      candidates+=("$(command -v "$command_candidate")")
    fi
  done

  local brew_prefix
  for brew_prefix in lld lld@21 llvm; do
    if command -v brew >/dev/null 2>&1; then
      local prefix
      if prefix="$(brew --prefix "$brew_prefix" 2>/dev/null)"; then
        candidates+=(
          "$prefix/bin/rust-lld"
          "$prefix/bin/ld.lld"
          "$prefix/bin/lld"
        )
      fi
    fi
  done

  candidates+=(
    "/opt/homebrew/opt/lld/bin/ld.lld"
    "/opt/homebrew/opt/lld/bin/lld"
    "/opt/homebrew/opt/lld@21/bin/ld.lld"
    "/opt/homebrew/opt/lld@21/bin/lld"
    "/opt/homebrew/opt/llvm/bin/ld.lld"
    "/usr/local/opt/lld/bin/ld.lld"
    "/usr/local/opt/lld/bin/lld"
    "/usr/local/opt/lld@21/bin/ld.lld"
    "/usr/local/opt/lld@21/bin/lld"
    "/usr/local/opt/llvm/bin/ld.lld"
  )

  local candidate
  for candidate in "${candidates[@]}"; do
    if [[ -x "$candidate" ]]; then
      echo "$candidate"
      return 0
    fi
  done

  echo "error: no LLD-compatible linker found" >&2
  echo "hint: install one with: brew install lld@21" >&2
  echo "hint: or set OCORE_LLD=/absolute/path/to/rust-lld-or-ld.lld" >&2
  return 1
}

LLD="$(find_lld)"
case "$(basename "$LLD")" in
  rust-lld | lld)
    "$LLD" -flavor gnu -m elf_x86_64 -nostdlib \
      -z max-page-size=0x1000 \
      -T "$KERNEL_DIR/linker.ld" \
      -o "$BUILD_DIR/kernel.elf" \
      "$BUILD_DIR/boot.o" "$BUILD_DIR/kernel.o"
    ;;
  *)
    "$LLD" -m elf_x86_64 -nostdlib \
      -z max-page-size=0x1000 \
      -T "$KERNEL_DIR/linker.ld" \
      -o "$BUILD_DIR/kernel.elf" \
      "$BUILD_DIR/boot.o" "$BUILD_DIR/kernel.o"
    ;;
esac

file "$BUILD_DIR/kernel.o"
file "$BUILD_DIR/kernel.elf"
echo "kernel: $BUILD_DIR/kernel.elf"
