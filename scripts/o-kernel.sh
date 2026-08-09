#!/usr/bin/env bash
# Repository-owned O-core kernel operator CLI. This file intentionally delegates
# compilation, linking, QEMU launch, and evidence assertions to the scripts that
# already own those contracts.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
SETUP_SCRIPT=${O_KERNEL_SETUP_SCRIPT:-"$ROOT/setup.sh"}
BUILD_SCRIPT=${O_KERNEL_BUILD_SCRIPT:-"$ROOT/ocore/kernel/build.sh"}
BOOT_SCRIPT=${O_KERNEL_BOOT_SCRIPT:-"$ROOT/ocore/kernel/run-qemu.sh"}
SMOKE_SCRIPT=${O_KERNEL_SMOKE_SCRIPT:-"$ROOT/ocore/kernel/smoke-qemu.sh"}
SMOKE_LIVE_SCRIPT=${O_KERNEL_SMOKE_LIVE_SCRIPT:-"$ROOT/ocore/kernel/smoke-live-qemu.sh"}
GATES_SCRIPT=${O_KERNEL_GATES_SCRIPT:-"$ROOT/boot-and-test.sh"}
MEDIA_BUILD_SCRIPT=${O_KERNEL_MEDIA_BUILD_SCRIPT:-"$ROOT/ocore/kernel/build-x86_64-uefi-media.sh"}
MEDIA_BOOT_SCRIPT=${O_KERNEL_MEDIA_BOOT_SCRIPT:-"$ROOT/ocore/kernel/run-x86_64-uefi-media-qemu.sh"}
MEDIA_SMOKE_SCRIPT=${O_KERNEL_MEDIA_SMOKE_SCRIPT:-"$ROOT/ocore/kernel/smoke-x86_64-uefi-media-qemu.sh"}
BOOT_INFO_SMOKE_SCRIPT=${O_KERNEL_BOOT_INFO_SMOKE_SCRIPT:-"$ROOT/ocore/kernel/smoke-x86_64-boot-info-qemu.sh"}
SMP_SMOKE_SCRIPT=${O_KERNEL_SMP_SMOKE_SCRIPT:-"$ROOT/ocore/kernel/smoke-x86_64-smp-qemu.sh"}
MEDIA_INSPECT_SCRIPT=${O_KERNEL_MEDIA_INSPECT_SCRIPT:-"$ROOT/scripts/ostadix_boot_media.py"}
MEDIA_WRITER_SCRIPT=${O_KERNEL_MEDIA_WRITER_SCRIPT:-"$ROOT/scripts/ostadix_media_writer.py"}
PHYSICAL_EVIDENCE_SCRIPT=${O_KERNEL_PHYSICAL_EVIDENCE_SCRIPT:-"$ROOT/scripts/ostadix_physical_evidence.py"}

usage() {
    cat <<'USAGE'
Usage: o kernel <command>

Build and boot the freestanding O-core kernel under local QEMU.

Commands:
  doctor       Check the O-core compiler, linker, ELF, and QEMU prerequisites
  doctor-media Check the optional GRUB, FAT, firmware, and O-core media tools
  build        Build the baseline kernel image
  image        Rebuild and describe the baseline kernel ELF
  media        Build a deterministic x86_64 GPT/UEFI disk image
  inspect-media  Strictly inspect a generated OSTADIX disk image
  prepare-write  Inspect external media and derive a bound confirmation token
  write-media   Write and verify external media using that exact token
  boot-challenge  Generate a fresh challenge for one physical boot attempt
  prepare-physical  Bind challenged media to one declared machine profile
  record-physical   Validate and record one authority-free serial observation
  boot         Boot the baseline kernel with an interactive serial terminal
  boot-media   Boot the generated disk through edk2/OVMF (no QEMU -kernel)
  console      Boot the bounded native M5 `o> ` control console
  smoke        Run the bounded baseline boot assertion
  smoke-media  Prove deterministic media rebuild and UEFI disk boot
  smoke-boot-info  Prove bounded firmware handoff and challenged mode-0 lifecycle
  smoke-smp    Prove bounded challenged four-vCPU INIT/SIPI and barrier progress
  smoke-live   Drive and verify the native M5 console lifecycle
  gates        Run every manifest-defined portable O-core QEMU evidence gate
  help         Show this help

Interactive QEMU escape: Ctrl-A X

The media path builds a physical-writeable GPT image but validates it under
QEMU/TCG. Mode 34 proves only bounded four-vCPU startup and one barrier; neither
the media path nor that probe is general or physical SMP, Linux/Plan 9, or
device-isolation evidence.
USAGE
}

die_usage() {
    printf 'error: %s\n\n' "$*" >&2
    usage >&2
    exit 2
}

require_no_args() {
    if [[ $# -ne 0 ]]; then
        die_usage "command does not accept arguments: $*"
    fi
}

require_at_most_one_arg() {
    if [[ $# -gt 1 ]]; then
        die_usage "command accepts at most one path argument: $*"
    fi
}

require_executable() {
    if [[ ! -x "$1" ]]; then
        printf 'error: required O-core script is missing or not executable: %s\n' "$1" >&2
        exit 1
    fi
}

baseline_build_dir() {
    printf '%s\n' "${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel}"
}

run_baseline_build() {
    local build_dir
    build_dir=$(baseline_build_dir)
    require_executable "$BUILD_SCRIPT"
    OCORE_PROBE_MODE=0 OCORE_BUILD_DIR="$build_dir" "$BUILD_SCRIPT"
}

describe_image() {
    local build_dir image bytes digest
    build_dir=$(baseline_build_dir)
    run_baseline_build
    image="$build_dir/kernel.elf"
    if [[ ! -f "$image" ]]; then
        printf 'error: kernel build completed without producing %s\n' "$image" >&2
        exit 1
    fi
    bytes=$(wc -c <"$image" | tr -d ' ')
    if command -v shasum >/dev/null 2>&1; then
        digest=$(shasum -a 256 "$image" | awk '{print $1}')
    elif command -v sha256sum >/dev/null 2>&1; then
        digest=$(sha256sum "$image" | awk '{print $1}')
    else
        printf 'error: shasum or sha256sum is required to describe the kernel image\n' >&2
        exit 127
    fi
    printf 'profile=baseline\nprobe_mode=0\nimage=%s\nbytes=%s\nsha256=%s\n' \
        "$image" "$bytes" "$digest"
}

command_name=${1:-help}
if [[ $# -gt 0 ]]; then
    shift
fi

case "$command_name" in
    help|-h|--help)
        require_no_args "$@"
        usage
        ;;
    doctor|check)
        require_no_args "$@"
        require_executable "$SETUP_SCRIPT"
        exec "$SETUP_SCRIPT" --with-ocore --check --no-env
        ;;
    doctor-media)
        require_no_args "$@"
        require_executable "$SETUP_SCRIPT"
        exec "$SETUP_SCRIPT" --with-ocore-media --check --no-env
        ;;
    build)
        require_no_args "$@"
        run_baseline_build
        ;;
    image)
        require_no_args "$@"
        describe_image
        ;;
    media)
        require_at_most_one_arg "$@"
        require_executable "$MEDIA_BUILD_SCRIPT"
        exec "$MEDIA_BUILD_SCRIPT" "$@"
        ;;
    inspect-media)
        require_at_most_one_arg "$@"
        require_executable "$MEDIA_INSPECT_SCRIPT"
        media_path=${1:-"$ROOT/target/ostadix-media/x86_64/ostadix-x86_64-uefi.img"}
        exec "$MEDIA_INSPECT_SCRIPT" inspect "$media_path"
        ;;
    prepare-write)
        require_executable "$MEDIA_WRITER_SCRIPT"
        exec "$MEDIA_WRITER_SCRIPT" prepare "$@"
        ;;
    write-media)
        require_executable "$MEDIA_WRITER_SCRIPT"
        exec "$MEDIA_WRITER_SCRIPT" write "$@"
        ;;
    boot-challenge)
        require_no_args "$@"
        require_executable "$PHYSICAL_EVIDENCE_SCRIPT"
        exec "$PHYSICAL_EVIDENCE_SCRIPT" challenge --raw
        ;;
    prepare-physical)
        require_executable "$PHYSICAL_EVIDENCE_SCRIPT"
        exec "$PHYSICAL_EVIDENCE_SCRIPT" prepare "$@"
        ;;
    record-physical)
        require_executable "$PHYSICAL_EVIDENCE_SCRIPT"
        exec "$PHYSICAL_EVIDENCE_SCRIPT" verify "$@"
        ;;
    boot)
        require_no_args "$@"
        require_executable "$BOOT_SCRIPT"
        exec env \
            OCORE_PROBE_MODE=0 \
            OCORE_BUILD_DIR="$(baseline_build_dir)" \
            "$BOOT_SCRIPT"
        ;;
    boot-media)
        require_no_args "$@"
        require_executable "$MEDIA_BOOT_SCRIPT"
        exec "$MEDIA_BOOT_SCRIPT"
        ;;
    console)
        require_no_args "$@"
        require_executable "$BOOT_SCRIPT"
        exec env \
            OCORE_PROBE_MODE=16 \
            OCORE_BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-m5-native}" \
            "$BOOT_SCRIPT"
        ;;
    smoke)
        require_no_args "$@"
        require_executable "$SMOKE_SCRIPT"
        exec "$SMOKE_SCRIPT"
        ;;
    smoke-media)
        require_no_args "$@"
        require_executable "$MEDIA_SMOKE_SCRIPT"
        exec "$MEDIA_SMOKE_SCRIPT"
        ;;
    smoke-boot-info)
        require_no_args "$@"
        require_executable "$BOOT_INFO_SMOKE_SCRIPT"
        exec "$BOOT_INFO_SMOKE_SCRIPT"
        ;;
    smoke-smp)
        require_no_args "$@"
        require_executable "$SMP_SMOKE_SCRIPT"
        exec "$SMP_SMOKE_SCRIPT"
        ;;
    smoke-live)
        require_no_args "$@"
        require_executable "$SMOKE_LIVE_SCRIPT"
        exec "$SMOKE_LIVE_SCRIPT"
        ;;
    gates)
        require_no_args "$@"
        require_executable "$GATES_SCRIPT"
        exec "$GATES_SCRIPT" smoke
        ;;
    *)
        die_usage "unknown kernel command '$command_name'"
        ;;
esac
