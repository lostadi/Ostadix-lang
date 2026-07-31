#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KERNEL_DIR="$ROOT/ocore/kernel"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel}"
PROBE_MODE="${OCORE_PROBE_MODE:-0}"
case "$PROBE_MODE" in
  0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22) ;;
  *)
    echo "error: OCORE_PROBE_MODE must be an integer from 0 through 22" >&2
    exit 2
    ;;
esac
mkdir -p "$BUILD_DIR"

cargo build --manifest-path "$ROOT/Cargo.toml" --bin ocorec

M4_IMAGE_DEFINE='-DOCORE_M4_IMAGE_PATH=""'
M5_IMAGE_DEFINE='-DOCORE_M5_IMAGE_PATH=""'
M6_IMAGE_DEFINE='-DOCORE_M6_IMAGE_PATH=""'
KERNEL_WORLD_RECORD_DEFINE='-DOCORE_KERNEL_WORLD_RECORD_PATH=""'
if (( PROBE_MODE == 15 )); then
  M4_ARTIFACT_OUTPUT="$(
    OCORE_M4_BUILD_DIR="$ROOT/target/ocore-m4" \
      "$KERNEL_DIR/build-m4-artifacts.sh"
  )"
  M4_IMAGE_PATH="$(
    printf '%s\n' "$M4_ARTIFACT_OUTPUT" | sed -n 's/^image: //p' | tail -n 1
  )"
  if [[ -z "$M4_IMAGE_PATH" || ! -f "$M4_IMAGE_PATH" ]]; then
    echo "error: M4 artifact build did not produce an OVFS image" >&2
    exit 1
  fi
  M4_IMAGE_DEFINE="-DOCORE_M4_IMAGE_PATH=\"$M4_IMAGE_PATH\""
fi

if (( PROBE_MODE == 16 )); then
  M5_ARTIFACT_OUTPUT="$(
    OCORE_M5_BUILD_DIR="$ROOT/target/ocore-m5-artifacts" \
      "$KERNEL_DIR/build-m5-artifacts.sh"
  )"
  M5_IMAGE_PATH="$(
    printf '%s\n' "$M5_ARTIFACT_OUTPUT" | sed -n 's/^image: //p' | tail -n 1
  )"
  if [[ -z "$M5_IMAGE_PATH" || ! -f "$M5_IMAGE_PATH" ]]; then
    echo "error: M5 artifact build did not produce an OVFS image" >&2
    exit 1
  fi
  M5_IMAGE_DEFINE="-DOCORE_M5_IMAGE_PATH=\"$M5_IMAGE_PATH\""
fi

if (( PROBE_MODE == 18 )); then
  M6_ARTIFACT_OUTPUT="$(
    OCORE_M6_BUILD_DIR="$ROOT/target/ocore-m6-artifacts" \
      "$KERNEL_DIR/build-m6-artifacts.sh"
  )"
  M6_IMAGE_PATH="$(
    printf '%s\n' "$M6_ARTIFACT_OUTPUT" | sed -n 's/^image: //p' | tail -n 1
  )"
  if [[ -z "$M6_IMAGE_PATH" || ! -f "$M6_IMAGE_PATH" ]]; then
    echo "error: M6A artifact build did not produce an OVFS image" >&2
    exit 1
  fi
  M6_IMAGE_DEFINE="-DOCORE_M6_IMAGE_PATH=\"$M6_IMAGE_PATH\""
fi

if (( PROBE_MODE == 20 || PROBE_MODE == 21 || PROBE_MODE == 22 )); then
  cargo build --quiet --manifest-path "$ROOT/Cargo.toml" \
    --bin ocore-kernel-world-record
  RECORD_BUILD_DIR="$BUILD_DIR/kernel-world-record"
  mkdir -p "$RECORD_BUILD_DIR"
  RECORD_ONE="$RECORD_BUILD_DIR/kernel-world-one.record"
  RECORD_TWO="$RECORD_BUILD_DIR/kernel-world-two.record"
  RECORD_TOOL="$ROOT/target/debug/ocore-kernel-world-record"
  FIXTURE="$KERNEL_DIR/kernel-world-fixture"
  "$RECORD_TOOL" \
    --manifest "$FIXTURE/package.toml" \
    --payload "$FIXTURE/payload" \
    --output "$RECORD_ONE" >/dev/null
  "$RECORD_TOOL" \
    --manifest "$FIXTURE/package.toml" \
    --payload "$FIXTURE/payload" \
    --output "$RECORD_TWO" >/dev/null
  if ! cmp -s "$RECORD_ONE" "$RECORD_TWO"; then
    echo "error: native KernelWorld record rebuild was not deterministic" >&2
    exit 1
  fi
  KERNEL_WORLD_RECORD_DEFINE="-DOCORE_KERNEL_WORLD_RECORD_PATH=\"$RECORD_ONE\""
fi

KERNEL_WORLD_BOOT_SOURCE="$ROOT/ocore/runtime/x86_64/kernel_world_boot_stub.oc"
KERNEL_WORLD_SEMANTICS_SOURCE="$KERNEL_DIR/kernel_world_semantics_stub.oc"
if (( PROBE_MODE == 20 || PROBE_MODE == 21 || PROBE_MODE == 22 )); then
  KERNEL_WORLD_BOOT_SOURCE="$ROOT/ocore/runtime/x86_64/kernel_world_boot.oc"
  KERNEL_WORLD_SEMANTICS_SOURCE="$KERNEL_DIR/kernel_world_semantics.oc"
fi

"$ROOT/target/debug/ocorec" \
  "$ROOT/ocore/runtime/x86_64/serial.oc" \
  "$ROOT/ocore/runtime/x86_64/pages.oc" \
  "$ROOT/ocore/runtime/x86_64/user_memory.oc" \
  "$ROOT/ocore/runtime/x86_64/domain_namespace.oc" \
  "$ROOT/ocore/runtime/x86_64/image_vfs.oc" \
  "$ROOT/ocore/runtime/x86_64/elf_loader.oc" \
  "$ROOT/ocore/runtime/x86_64/service_registry.oc" \
  "$ROOT/ocore/runtime/x86_64/package_root.oc" \
  "$ROOT/ocore/runtime/x86_64/live_supervisor.oc" \
  "$ROOT/ocore/runtime/x86_64/serial_control.oc" \
  "$ROOT/ocore/runtime/x86_64/native_control.oc" \
  "$ROOT/ocore/runtime/x86_64/address_space.oc" \
  "$ROOT/ocore/runtime/x86_64/native_abi.oc" \
  "$ROOT/ocore/runtime/x86_64/personality.oc" \
  "$ROOT/ocore/runtime/x86_64/domain.oc" \
  "$ROOT/ocore/runtime/x86_64/process.oc" \
  "$ROOT/ocore/runtime/x86_64/thread.oc" \
  "$ROOT/ocore/runtime/x86_64/scheduler.oc" \
  "$ROOT/ocore/runtime/x86_64/capability.oc" \
  "$ROOT/ocore/runtime/x86_64/memory_object.oc" \
  "$ROOT/ocore/runtime/x86_64/endpoint.oc" \
  "$ROOT/ocore/runtime/x86_64/cap_transfer.oc" \
  "$ROOT/ocore/runtime/x86_64/ipc_wait.oc" \
  "$ROOT/ocore/runtime/x86_64/ipc_lifecycle.oc" \
  "$ROOT/ocore/runtime/x86_64/personality_supervision.oc" \
  "$ROOT/ocore/runtime/x86_64/personality_rpc.oc" \
  "$ROOT/ocore/runtime/x86_64/personality_memory_view.oc" \
  "$ROOT/ocore/runtime/x86_64/delegated_resource.oc" \
  "$ROOT/ocore/runtime/x86_64/kernel_world_record.oc" \
  "$ROOT/ocore/runtime/x86_64/kernel_world_admission.oc" \
  "$KERNEL_WORLD_BOOT_SOURCE" \
  "$ROOT/ocore/runtime/x86_64/vm_object.oc" \
  "$ROOT/ocore/runtime/x86_64/svm_execution.oc" \
  "$ROOT/ocore/runtime/x86_64/mapping.oc" \
  "$ROOT/ocore/runtime/x86_64/interrupts.oc" \
  "$ROOT/ocore/runtime/x86_64/trap.oc" \
  "$ROOT/ocore/runtime/x86_64/syscall.oc" \
  "$ROOT/ocore/runtime/x86_64/user.oc" \
  "$ROOT/ocore/runtime/x86_64/m1_user.oc" \
  "$ROOT/ocore/runtime/x86_64/m2_user.oc" \
  "$ROOT/ocore/runtime/x86_64/m3_user.oc" \
  "$KERNEL_DIR/m1.oc" \
  "$KERNEL_DIR/m2.oc" \
  "$KERNEL_DIR/m3.oc" \
  "$KERNEL_DIR/m3_live.oc" \
  "$KERNEL_DIR/m4.oc" \
  "$KERNEL_DIR/m5.oc" \
  "$KERNEL_DIR/m5_selftest.oc" \
  "$KERNEL_DIR/m5_semantics.oc" \
  "$KERNEL_DIR/m6.oc" \
  "$KERNEL_DIR/m6b_semantics.oc" \
  "$KERNEL_WORLD_SEMANTICS_SOURCE" \
  "$KERNEL_DIR/scheduler_bridge.oc" \
  "$KERNEL_DIR/main.oc" \
  --target x86_64-unknown-none \
  --emit obj \
  --keep-asm \
  -o "$BUILD_DIR/kernel.o"

clang -target x86_64-unknown-none-elf -c -x assembler-with-cpp \
  -DOCORE_PROBE_MODE="$PROBE_MODE" \
  "$M4_IMAGE_DEFINE" \
  "$M5_IMAGE_DEFINE" \
  "$M6_IMAGE_DEFINE" \
  "$KERNEL_WORLD_RECORD_DEFINE" \
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
