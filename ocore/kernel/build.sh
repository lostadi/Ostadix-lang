#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KERNEL_DIR="$ROOT/ocore/kernel"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel}"
PROBE_MODE="${OCORE_PROBE_MODE:-0}"
case "$PROBE_MODE" in
  0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 | 25 | 26 | 27 | 28 | 29 | 30 | 31) ;;
  *)
    echo "error: OCORE_PROBE_MODE must be an integer from 0 through 31" >&2
    exit 2
    ;;
esac
mkdir -p "$BUILD_DIR"

cargo build --manifest-path "$ROOT/Cargo.toml" --bin ocorec

M4_IMAGE_DEFINE='-DOCORE_M4_IMAGE_PATH=""'
M5_IMAGE_DEFINE='-DOCORE_M5_IMAGE_PATH=""'
M6_IMAGE_DEFINE='-DOCORE_M6_IMAGE_PATH=""'
M6B_LIVE_IMAGE_DEFINE='-DOCORE_M6B_LIVE_IMAGE_PATH=""'
M6_LINUX_IMAGE_DEFINE='-DOCORE_M6_LINUX_IMAGE_PATH=""'
M7_LINUX_PLAN9_IMAGE_DEFINE='-DOCORE_M7_LINUX_PLAN9_IMAGE_PATH=""'
M7B_LOGICAL_READ_IMAGE_DEFINE='-DOCORE_M7B_LOGICAL_READ_IMAGE_PATH=""'
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
  M5_IMAGE_DIGEST="$(shasum -a 256 "$M5_IMAGE_PATH" | awk '{print $1}')"
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

if (( PROBE_MODE == 24 )); then
  M6B_LIVE_ARTIFACT_OUTPUT="$(
    OCORE_M6B_LIVE_BUILD_DIR="$ROOT/target/ocore-m6b-live-artifacts" \
      "$KERNEL_DIR/build-m6b-live-artifacts.sh"
  )"
  M6B_LIVE_IMAGE_PATH="$(
    printf '%s\n' "$M6B_LIVE_ARTIFACT_OUTPUT" | sed -n 's/^image: //p' | tail -n 1
  )"
  if [[ -z "$M6B_LIVE_IMAGE_PATH" || ! -f "$M6B_LIVE_IMAGE_PATH" ]]; then
    echo "error: M6B live artifact build did not produce an OVFS image" >&2
    exit 1
  fi
  M6B_LIVE_IMAGE_DEFINE="-DOCORE_M6B_LIVE_IMAGE_PATH=\"$M6B_LIVE_IMAGE_PATH\""
fi

if (( PROBE_MODE == 25 )); then
  M6_LINUX_ARTIFACT_OUTPUT="$(
    OCORE_M6_LINUX_BUILD_DIR="$ROOT/target/ocore-m6-linux-live-artifacts" \
      "$KERNEL_DIR/build-m6-linux-live-artifacts.sh"
  )"
  M6_LINUX_IMAGE_PATH="$(
    printf '%s\n' "$M6_LINUX_ARTIFACT_OUTPUT" | sed -n 's/^image: //p' | tail -n 1
  )"
  if [[ -z "$M6_LINUX_IMAGE_PATH" || ! -f "$M6_LINUX_IMAGE_PATH" ]]; then
    echo "error: M6 Linux live artifact build did not produce an OVFS image" >&2
    exit 1
  fi
  M6_LINUX_IMAGE_DEFINE="-DOCORE_M6_LINUX_IMAGE_PATH=\"$M6_LINUX_IMAGE_PATH\""
fi

if (( PROBE_MODE == 26 )); then
  M7_LINUX_PLAN9_ARTIFACT_OUTPUT="$(
    OCORE_M7_BUILD_DIR="$ROOT/target/ocore-m7-linux-plan9-artifacts" \
      "$KERNEL_DIR/build-m7-linux-plan9-artifacts.sh"
  )"
  M7_LINUX_PLAN9_IMAGE_PATH="$(
    printf '%s\n' "$M7_LINUX_PLAN9_ARTIFACT_OUTPUT" | sed -n 's/^image: //p' | tail -n 1
  )"
  if [[ -z "$M7_LINUX_PLAN9_IMAGE_PATH" || ! -f "$M7_LINUX_PLAN9_IMAGE_PATH" ]]; then
    echo "error: M7 Linux/Plan 9 artifact build did not produce an OVFS image" >&2
    exit 1
  fi
  M7_LINUX_PLAN9_IMAGE_DEFINE="-DOCORE_M7_LINUX_PLAN9_IMAGE_PATH=\"$M7_LINUX_PLAN9_IMAGE_PATH\""
fi

if (( PROBE_MODE == 31 )); then
  M7B_LOGICAL_READ_ARTIFACT_OUTPUT="$(
    OCORE_M7B_BUILD_DIR="$ROOT/target/ocore-m7b-logical-read-artifacts" \
      "$KERNEL_DIR/build-m7b-logical-read-artifacts.sh"
  )"
  M7B_LOGICAL_READ_IMAGE_PATH="$(
    printf '%s\n' "$M7B_LOGICAL_READ_ARTIFACT_OUTPUT" \
      | sed -n 's/^image: //p' | tail -n 1
  )"
  if [[ -z "$M7B_LOGICAL_READ_IMAGE_PATH" \
      || ! -f "$M7B_LOGICAL_READ_IMAGE_PATH" ]]; then
    echo "error: M7B-1 LogicalRead artifact build did not produce an OVFS image" >&2
    exit 1
  fi
  M7B_LOGICAL_READ_IMAGE_DEFINE="-DOCORE_M7B_LOGICAL_READ_IMAGE_PATH=\"$M7B_LOGICAL_READ_IMAGE_PATH\""
fi

if (( PROBE_MODE == 20 || PROBE_MODE == 21 || PROBE_MODE == 22 || PROBE_MODE == 23 )); then
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
KERNEL_WORLD_DEVICE_SOURCE="$ROOT/ocore/runtime/x86_64/kernel_world_device_stub.oc"
KERNEL_WORLD_EXECUTION_SOURCE="$ROOT/ocore/runtime/x86_64/kernel_world_execution_stub.oc"
KERNEL_WORLD_SEMANTICS_SOURCE="$KERNEL_DIR/kernel_world_semantics_stub.oc"
WORLD_IDENTITY_SEMANTICS_SOURCE="$KERNEL_DIR/world_identity_semantics_stub.oc"
WORLD_IDENTITY_SOURCES=()
WORLD_PROTOCOL_SEMANTICS_SOURCE="$KERNEL_DIR/world_protocol_semantics_stub.oc"
WORLD_PROTOCOL_SOURCES=()
WORLD_VALUE_SEMANTICS_SOURCE="$KERNEL_DIR/world_value_semantics_stub.oc"
WORLD_VALUE_SOURCES=()
WORLD_RECEIPT_SEMANTICS_SOURCE="$KERNEL_DIR/world_receipt_semantics_stub.oc"
WORLD_RECEIPT_SOURCES=()
ENDPOINT_SOURCE="$ROOT/ocore/runtime/x86_64/endpoint.oc"
if (( PROBE_MODE == 20 || PROBE_MODE == 21 || PROBE_MODE == 22 || PROBE_MODE == 23 )); then
  KERNEL_WORLD_BOOT_SOURCE="$ROOT/ocore/runtime/x86_64/kernel_world_boot.oc"
  KERNEL_WORLD_SEMANTICS_SOURCE="$KERNEL_DIR/kernel_world_semantics.oc"
fi
if (( PROBE_MODE == 19 || PROBE_MODE == 20 || PROBE_MODE == 21 || PROBE_MODE == 22 || PROBE_MODE == 23 || PROBE_MODE == 28 || PROBE_MODE == 29 || PROBE_MODE == 30 )); then
  # Mode 19's direct memory-view oracle and the KernelWorld probes use neither
  # IPC queues nor endpoint lifecycle. Link a fail-closed API substitute that
  # preserves common one-shot initialization. Mode 28 is likewise a direct
  # freestanding codec oracle with no endpoint traffic. Other probes retain
  # the full four-message endpoint implementation where required.
  ENDPOINT_SOURCE="$ROOT/ocore/runtime/x86_64/endpoint_probe_stub.oc"
fi
if (( PROBE_MODE == 30 )); then
  WORLD_RECEIPT_SEMANTICS_SOURCE="$KERNEL_DIR/world_receipt_semantics.oc"
  WORLD_RECEIPT_SOURCES=(
    "$ROOT/ocore/world/identity.oc"
    "$ROOT/ocore/world/protocol.oc"
    "$ROOT/ocore/world/value.oc"
    "$ROOT/ocore/world/sha256.oc"
    "$ROOT/ocore/world/value_codec.oc"
    "$ROOT/ocore/world/receipt.oc"
    "$ROOT/ocore/world/receipt_codec.oc"
  )
fi
if (( PROBE_MODE == 29 )); then
  WORLD_VALUE_SEMANTICS_SOURCE="$KERNEL_DIR/world_value_semantics.oc"
  WORLD_VALUE_SOURCES=(
    "$ROOT/ocore/world/protocol.oc"
    "$ROOT/ocore/world/value.oc"
    "$ROOT/ocore/world/sha256.oc"
    "$ROOT/ocore/world/value_codec.oc"
  )
fi
if (( PROBE_MODE == 23 )); then
  KERNEL_WORLD_DEVICE_SOURCE="$ROOT/ocore/runtime/x86_64/kernel_world_device.oc"
  KERNEL_WORLD_EXECUTION_SOURCE="$ROOT/ocore/runtime/x86_64/kernel_world_execution.oc"
  # Keep the portable execution/device oracle within the bootstrap's fixed
  # 512 KiB reserve without weakening the linker assertion or inflating the
  # historical Modes 20-22 contract harness.
  KERNEL_WORLD_SEMANTICS_SOURCE="$KERNEL_DIR/kernel_world_execution_device_semantics.oc"
fi
if (( PROBE_MODE == 27 || PROBE_MODE == 28 )); then
  WORLD_IDENTITY_SEMANTICS_SOURCE="$KERNEL_DIR/world_identity_semantics.oc"
fi
if (( PROBE_MODE == 27 )); then
  WORLD_IDENTITY_SOURCES=("$ROOT/ocore/world/identity.oc")
fi
if (( PROBE_MODE == 28 )); then
  WORLD_PROTOCOL_SEMANTICS_SOURCE="$KERNEL_DIR/world_protocol_semantics.oc"
  WORLD_PROTOCOL_SOURCES=(
    "$ROOT/ocore/world/identity.oc"
    "$ROOT/ocore/world/protocol.oc"
    "$ROOT/ocore/world/codec.oc"
  )
fi

LINUX_PERSONALITY_SOURCES=(
  "$KERNEL_DIR/linux_personality_semantics_stub.oc"
)
if (( PROBE_MODE == 19 )); then
  LINUX_PERSONALITY_SOURCES=(
    "$ROOT/ocore/runtime/x86_64/linux_fd_table.oc"
    "$ROOT/ocore/runtime/x86_64/linux_personality.oc"
    "$KERNEL_DIR/linux_personality_semantics.oc"
  )
fi
if (( PROBE_MODE == 25 || PROBE_MODE == 26 )); then
  LINUX_PERSONALITY_SOURCES=(
    "$ROOT/ocore/runtime/x86_64/linux_fd_table.oc"
    "$ROOT/ocore/runtime/x86_64/linux_personality.oc"
    "$KERNEL_DIR/linux_personality_semantics_stub.oc"
  )
fi

M6B_SEMANTICS_SOURCE="$KERNEL_DIR/m6b_semantics_stub.oc"
if (( PROBE_MODE == 19 )); then
  M6B_SEMANTICS_SOURCE="$KERNEL_DIR/m6b_semantics.oc"
fi

IMAGE_VFS_STORAGE_SOURCE="$ROOT/ocore/runtime/x86_64/image_vfs_storage.oc"
if (( PROBE_MODE == 26 || PROBE_MODE == 31 )); then
  IMAGE_VFS_STORAGE_SOURCE="$ROOT/ocore/runtime/x86_64/image_vfs_storage_m7.oc"
fi

M6_SOURCE="$KERNEL_DIR/m6_stub.oc"
BOUNDED_PERSONALITY_SOURCES=()
if (( PROBE_MODE == 18 || PROBE_MODE == 24 )); then
  M6_SOURCE="$KERNEL_DIR/m6.oc"
fi
if (( PROBE_MODE == 25 || PROBE_MODE == 26 )); then
  M6_SOURCE="$KERNEL_DIR/m6_linux.oc"
fi
if (( PROBE_MODE == 31 )); then
  M6_SOURCE="$KERNEL_DIR/m7b_logical_read.oc"
fi
if (( PROBE_MODE == 18 || PROBE_MODE == 19 || PROBE_MODE == 24 || PROBE_MODE == 25 || PROBE_MODE == 26 )); then
  # Mode 19 directly exercises views/resources and its pinned Linux oracle
  # composes the bounded router. Modes 18 and 24 share m6.oc; Modes 25 and 26
  # select the separate m6_linux.oc implementation so their pinned foreign-ABI
  # slices cannot consume Mode 24's fixed kernel-image headroom. Other probes
  # retain only the fail-closed M6 adapter surface.
  BOUNDED_PERSONALITY_SOURCES=(
    "$ROOT/ocore/runtime/x86_64/personality_memory_view.oc"
    "$ROOT/ocore/runtime/x86_64/personality_bounded_rpc.oc"
    "$ROOT/ocore/runtime/x86_64/delegated_resource.oc"
  )
fi

M5_SOURCE="$KERNEL_DIR/m5_stub.oc"
if (( PROBE_MODE == 16 )); then
  M5_SOURCE="$KERNEL_DIR/m5.oc"
fi
M5_SELFTEST_SOURCE="$KERNEL_DIR/m5_selftest_stub.oc"
M5_SEMANTICS_SOURCE="$KERNEL_DIR/m5_semantics_stub.oc"
if (( PROBE_MODE == 17 )); then
  M5_SELFTEST_SOURCE="$KERNEL_DIR/m5_selftest.oc"
  M5_SEMANTICS_SOURCE="$KERNEL_DIR/m5_semantics.oc"
fi

M1_SOURCE="$KERNEL_DIR/m1.oc"
M2_SOURCE="$KERNEL_DIR/m2.oc"
M3_SOURCE="$KERNEL_DIR/m3.oc"
M3_LIVE_SOURCE="$KERNEL_DIR/m3_live.oc"
M4_SOURCE="$KERNEL_DIR/m4.oc"
if (( PROBE_MODE == 25 || PROBE_MODE == 26 || PROBE_MODE == 28 || PROBE_MODE == 29 || PROBE_MODE == 30 || PROBE_MODE == 31 )); then
  # Modes 25, 26, and 31 enter only kernel::m6, while Mode 28 enters only the
  # World protocol oracle. Keep historical probes fail-closed without linking
  # their unreachable harness bodies, preserving the hard bootstrap headroom
  # assertion.
  M1_SOURCE="$KERNEL_DIR/m1_mode25_stub.oc"
  M2_SOURCE="$KERNEL_DIR/m2_mode25_stub.oc"
  M3_SOURCE="$KERNEL_DIR/m3_mode25_stub.oc"
  M3_LIVE_SOURCE="$KERNEL_DIR/m3_live_mode25_stub.oc"
  M4_SOURCE="$KERNEL_DIR/m4_mode25_stub.oc"
fi

"$ROOT/target/debug/ocorec" \
  ${WORLD_IDENTITY_SOURCES[@]+"${WORLD_IDENTITY_SOURCES[@]}"} \
  ${WORLD_PROTOCOL_SOURCES[@]+"${WORLD_PROTOCOL_SOURCES[@]}"} \
  ${WORLD_VALUE_SOURCES[@]+"${WORLD_VALUE_SOURCES[@]}"} \
  ${WORLD_RECEIPT_SOURCES[@]+"${WORLD_RECEIPT_SOURCES[@]}"} \
  "$ROOT/ocore/runtime/x86_64/serial.oc" \
  "$ROOT/ocore/runtime/x86_64/pages.oc" \
  "$ROOT/ocore/runtime/x86_64/user_memory.oc" \
  "$ROOT/ocore/runtime/x86_64/domain_namespace.oc" \
  "$IMAGE_VFS_STORAGE_SOURCE" \
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
  "$ENDPOINT_SOURCE" \
  "$ROOT/ocore/runtime/x86_64/cap_transfer.oc" \
  "$ROOT/ocore/runtime/x86_64/ipc_wait.oc" \
  "$ROOT/ocore/runtime/x86_64/ipc_lifecycle.oc" \
  "$ROOT/ocore/runtime/x86_64/personality_supervision.oc" \
  "$ROOT/ocore/runtime/x86_64/personality_rpc.oc" \
  ${BOUNDED_PERSONALITY_SOURCES[@]+"${BOUNDED_PERSONALITY_SOURCES[@]}"} \
  "${LINUX_PERSONALITY_SOURCES[@]}" \
  "$ROOT/ocore/runtime/x86_64/kernel_world_record.oc" \
  "$ROOT/ocore/runtime/x86_64/kernel_world_admission.oc" \
  "$KERNEL_WORLD_DEVICE_SOURCE" \
  "$KERNEL_WORLD_BOOT_SOURCE" \
  "$ROOT/ocore/runtime/x86_64/vm_object.oc" \
  "$ROOT/ocore/runtime/x86_64/svm_execution.oc" \
  "$KERNEL_WORLD_EXECUTION_SOURCE" \
  "$ROOT/ocore/runtime/x86_64/mapping.oc" \
  "$ROOT/ocore/runtime/x86_64/interrupts.oc" \
  "$ROOT/ocore/runtime/x86_64/trap.oc" \
  "$ROOT/ocore/runtime/x86_64/syscall.oc" \
  "$ROOT/ocore/runtime/x86_64/user.oc" \
  "$ROOT/ocore/runtime/x86_64/m1_user.oc" \
  "$ROOT/ocore/runtime/x86_64/m2_user.oc" \
  "$ROOT/ocore/runtime/x86_64/m3_user.oc" \
  "$M1_SOURCE" \
  "$M2_SOURCE" \
  "$M3_SOURCE" \
  "$M3_LIVE_SOURCE" \
  "$M4_SOURCE" \
  "$M5_SOURCE" \
  "$M5_SELFTEST_SOURCE" \
  "$M5_SEMANTICS_SOURCE" \
  "$M6_SOURCE" \
  "$M6B_SEMANTICS_SOURCE" \
  "$KERNEL_WORLD_SEMANTICS_SOURCE" \
  "$WORLD_IDENTITY_SEMANTICS_SOURCE" \
  "$WORLD_PROTOCOL_SEMANTICS_SOURCE" \
  "$WORLD_VALUE_SEMANTICS_SOURCE" \
  "$WORLD_RECEIPT_SEMANTICS_SOURCE" \
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
  "$M6B_LIVE_IMAGE_DEFINE" \
  "$M6_LINUX_IMAGE_DEFINE" \
  "$M7_LINUX_PLAN9_IMAGE_DEFINE" \
  "$M7B_LOGICAL_READ_IMAGE_DEFINE" \
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

if (( PROBE_MODE == 25 )); then
  # Record the exact content-addressed image path passed to boot.S's .incbin.
  # The live gate hashes this resolved build output rather than a possibly
  # stale artifact that merely has the previously expected filename.
  printf '%s\n' "$M6_LINUX_IMAGE_PATH" > "$BUILD_DIR/m6-linux-image.path"
fi
if (( PROBE_MODE == 26 )); then
  printf '%s\n' "$M7_LINUX_PLAN9_IMAGE_PATH" > "$BUILD_DIR/m7-linux-plan9-image.path"
fi
if (( PROBE_MODE == 31 )); then
  printf '%s\n' "$M7B_LOGICAL_READ_IMAGE_PATH" > "$BUILD_DIR/m7b-logical-read-image.path"
fi

file "$BUILD_DIR/kernel.o"
file "$BUILD_DIR/kernel.elf"
if (( PROBE_MODE == 16 )); then
  printf 'm5-image: %s\nm5-sha256: %s\n' \
    "$M5_IMAGE_PATH" "$M5_IMAGE_DIGEST"
fi
echo "kernel: $BUILD_DIR/kernel.elf"
