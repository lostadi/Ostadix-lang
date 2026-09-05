#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
SOURCE="$ROOT/ocore/kernel/native-cluster"
BUILD="${OCORE_NATIVE_CLUSTER_BUILD_DIR:-$ROOT/target/ocore-native-cluster}"
COMPILER="${OCOREC_BIN:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/ocorec}"
LINKER="${OCORE_LLD:-$(command -v ld.lld || true)}"
[[ -x "$COMPILER" && -x "$LINKER" ]] || {
  echo "native cluster requires prebuilt OCOREC_BIN and OCORE_LLD/ld.lld" >&2
  exit 1
}
mkdir -p "$BUILD"
"$COMPILER" \
  "$ROOT/ocore/runtime/x86_64/serial.oc" \
  "$ROOT/ocore/runtime/x86_64/rtl8139.oc" \
  "$ROOT/ocore/world/sha256.oc" \
  "$ROOT/ocore/world/native_session.oc" \
  "$ROOT/ocore/world/native_distributed.oc" \
  "$SOURCE/main.oc" \
  --target x86_64-unknown-none --emit obj --keep-asm -o "$BUILD/kernel.o"
clang -target x86_64-unknown-none-elf -c -x assembler-with-cpp \
  "$SOURCE/boot.S" -o "$BUILD/boot.o"
LINKER_FLAVOR=()
case "$(basename "$LINKER")" in rust-lld | lld) LINKER_FLAVOR=(-flavor gnu);; esac
"$LINKER" ${LINKER_FLAVOR[@]+"${LINKER_FLAVOR[@]}"} -m elf_x86_64 -nostdlib -z max-page-size=0x1000 \
  -T "$SOURCE/linker.ld" -o "$BUILD/kernel.elf" "$BUILD/boot.o" "$BUILD/kernel.o"
echo "native-cluster-kernel: $BUILD/kernel.elf"
