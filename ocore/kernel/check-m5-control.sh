#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_M5_BUILD_DIR:-$ROOT/target/ocore-m5}"
mkdir -p "$BUILD_DIR"

find_lld() {
  if [[ -n "${OCORE_LLD:-}" && -x "${OCORE_LLD:-}" ]]; then
    printf '%s\n' "$OCORE_LLD"
    return 0
  fi
  local rust_lld
  rust_lld="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/rust-lld"
  if [[ -x "$rust_lld" ]]; then
    printf '%s\n' "$rust_lld"
    return 0
  fi
  local candidate
  for candidate in /opt/homebrew/opt/lld/bin/ld.lld /opt/homebrew/opt/lld@21/bin/ld.lld /opt/homebrew/opt/llvm/bin/ld.lld; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  echo "error: no LLD-compatible linker found" >&2
  return 1
}

cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin ocorec
OCOREC="$ROOT/target/debug/ocorec"
LLD="$(find_lld)"

"$OCOREC" \
  "$ROOT/ocore/runtime/x86_64/capability.oc" \
  "$ROOT/ocore/runtime/x86_64/package_root.oc" \
  "$ROOT/ocore/runtime/x86_64/live_supervisor.oc" \
  "$ROOT/ocore/runtime/x86_64/serial_control.oc" \
  "$ROOT/ocore/kernel/m5_selftest.oc" \
  --target x86_64-unknown-none \
  --emit obj \
  -o "$BUILD_DIR/control-selftest.o"

"$OCOREC" \
  "$ROOT/ocore/runtime/x86_64/capability.oc" \
  "$ROOT/ocore/runtime/x86_64/package_root.oc" \
  "$ROOT/ocore/runtime/x86_64/live_supervisor.oc" \
  "$ROOT/ocore/runtime/x86_64/serial_control.oc" \
  "$ROOT/ocore/runtime/x86_64/native_control.oc" \
  "$ROOT/ocore/kernel/m5_native_control_selftest.oc" \
  --target x86_64-unknown-none \
  --emit obj \
  -o "$BUILD_DIR/native-control-selftest.o"

link_service() {
  local service="$1"
  "$OCOREC" \
    "$ROOT/ocore/runtime/x86_64/native_abi.oc" \
    "$ROOT/ocore/runtime/x86_64/m5_${service}.oc" \
    --target x86_64-unknown-none \
    --emit obj \
    -o "$BUILD_DIR/m5-${service}.o"
  case "$(basename "$LLD")" in
    rust-lld | lld)
      "$LLD" -flavor gnu -m elf_x86_64 -nostdlib --build-id=none \
        -z max-page-size=0x1000 \
        -T "$ROOT/ocore/user/static-user.ld" \
        -o "$BUILD_DIR/m5-${service}.elf" "$BUILD_DIR/m5-${service}.o"
      ;;
    *)
      "$LLD" -m elf_x86_64 -nostdlib --build-id=none \
        -z max-page-size=0x1000 \
        -T "$ROOT/ocore/user/static-user.ld" \
        -o "$BUILD_DIR/m5-${service}.elf" "$BUILD_DIR/m5-${service}.o"
      ;;
  esac
}

for service in init supervisor pkgd repl; do
  link_service "$service"
done

file \
  "$BUILD_DIR/m5-init.elf" \
  "$BUILD_DIR/m5-supervisor.elf" \
  "$BUILD_DIR/m5-pkgd.elf" \
  "$BUILD_DIR/m5-repl.elf"

printf '%s\n' \
  "M5 isolated control-plane compile: PASS" \
  "M5 native command transaction compile: PASS" \
  "M5 four-service ELF link: PASS"
