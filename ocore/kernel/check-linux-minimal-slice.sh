#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_LINUX_CHECK_DIR:-$ROOT/target/ocore-linux-minimal-check}"
mkdir -p "$BUILD_DIR"

for tool in cargo cmp nm python3 qemu-system-x86_64; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the isolated minimal Linux check" >&2
    exit 127
  fi
done

cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin ocorec
OCOREC="$ROOT/target/debug/ocorec"

SOURCES=(
  "$ROOT/ocore/runtime/x86_64/pages.oc"
  "$ROOT/ocore/runtime/x86_64/capability.oc"
  "$ROOT/ocore/runtime/x86_64/domain_namespace.oc"
  "$ROOT/ocore/runtime/x86_64/image_vfs_storage.oc"
  "$ROOT/ocore/runtime/x86_64/image_vfs.oc"
  "$ROOT/ocore/runtime/x86_64/elf_loader.oc"
  "$ROOT/ocore/runtime/x86_64/memory_object.oc"
  "$ROOT/ocore/runtime/x86_64/mapping.oc"
  "$ROOT/ocore/runtime/x86_64/address_space.oc"
  "$ROOT/ocore/runtime/x86_64/native_abi.oc"
  "$ROOT/ocore/runtime/x86_64/personality.oc"
  "$ROOT/ocore/runtime/x86_64/domain.oc"
  "$ROOT/ocore/runtime/x86_64/process.oc"
  "$ROOT/ocore/runtime/x86_64/thread.oc"
  "$ROOT/ocore/runtime/x86_64/endpoint.oc"
  "$ROOT/ocore/runtime/x86_64/ipc_wait.oc"
  "$ROOT/ocore/runtime/x86_64/user_memory.oc"
  "$ROOT/ocore/runtime/x86_64/personality_rpc.oc"
  "$ROOT/ocore/runtime/x86_64/personality_memory_view.oc"
  "$ROOT/ocore/runtime/x86_64/delegated_resource.oc"
  "$ROOT/ocore/runtime/x86_64/personality_bounded_rpc.oc"
  "$ROOT/ocore/runtime/x86_64/linux_fd_table.oc"
  "$ROOT/ocore/runtime/x86_64/linux_personality.oc"
  "$ROOT/ocore/kernel/linux_personality_semantics.oc"
)

"$OCOREC" "${SOURCES[@]}" \
  --target x86_64-unknown-none --emit obj \
  -o "$BUILD_DIR/linux-semantics-one.o"
"$OCOREC" "${SOURCES[@]}" \
  --target x86_64-unknown-none --emit obj \
  -o "$BUILD_DIR/linux-semantics-two.o"

if ! cmp -s \
    "$BUILD_DIR/linux-semantics-one.o" \
    "$BUILD_DIR/linux-semantics-two.o"; then
  echo "error: minimal Linux O-core semantics object is not reproducible" >&2
  exit 1
fi

SYMBOLS="$(nm "$BUILD_DIR/linux-semantics-one.o")"
for symbol in \
  _O_runtime__linux_fd_table__authorize_write \
  _O_runtime__linux_personality__classify_syscall \
  _O_runtime__linux_personality__complete_write \
  _O_kernel__linux_personality_semantics__self_test; do
  if ! grep -Eq " [Tt] ${symbol}$" <<<"$SYMBOLS"; then
    echo "error: isolated Linux semantics object lacks $symbol" >&2
    exit 1
  fi
done

"$ROOT/ocore/kernel/build-linux-minimal-corpus.sh"
OCORE_BUILD_DIR="$BUILD_DIR/mode19-qemu" \
  "$ROOT/ocore/kernel/smoke-m6b-qemu.sh"
OCORE_BUILD_DIR="$BUILD_DIR/mode25-qemu" \
  "$ROOT/ocore/kernel/smoke-live-linux-personality-qemu.sh"
printf 'minimal Linux isolated compile/oracle + kernel-admin QEMU semantics check: PASS\n'
printf 'live Linux ELF/CPL3 bounded-personality execution: PASS\n'
