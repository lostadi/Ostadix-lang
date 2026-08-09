#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SMOKE_ROOT="${OSTADIX_SMP_SMOKE_ROOT:-$ROOT/target/ostadix-smp-smoke}"
QEMU_BIN="${OCORE_QEMU_BIN:-qemu-system-x86_64}"
TIMEOUT="${OSTADIX_SMP_TIMEOUT_SECONDS:-10}"
IMAGE="$SMOKE_ROOT/ostadix-smp4.img"
RUN_ROOT=""
POSITIVE_TRANSCRIPT=""
NEGATIVE_TRANSCRIPT=""
TRANSCRIPT_CHECK=""
TRANSCRIPT_CHECK_STDOUT=""

cleanup_run() {
  local status=$?
  if [[ -z "$RUN_ROOT" ]]; then
    return
  fi
  if (( status == 0 )); then
    # Remove only the exact files created beneath this invocation's mktemp
    # directory. If an unexpected file exists, rmdir fails closed and leaves
    # the unique directory intact rather than recursively deleting it.
    rm -f -- \
      "$POSITIVE_TRANSCRIPT" \
      "$NEGATIVE_TRANSCRIPT" \
      "$TRANSCRIPT_CHECK" \
      "$TRANSCRIPT_CHECK_STDOUT"
    rmdir -- "$RUN_ROOT" 2>/dev/null || true
  else
    printf 'OSTADIX SMP run artifacts retained: %s\n' "$RUN_ROOT" >&2
  fi
}
trap cleanup_run EXIT

if [[ $# -ne 0 ]]; then
  echo "usage: smoke-x86_64-smp-qemu.sh" >&2
  exit 2
fi
if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  printf 'error: QEMU executable is unavailable: %s\n' "$QEMU_BIN" >&2
  exit 127
fi
if [[ -z "${OSTADIX_BOOT_CHALLENGE:-}" ]]; then
  OSTADIX_BOOT_CHALLENGE="$(
    "$ROOT/scripts/ostadix_physical_evidence.py" challenge --raw
  )"
fi
if [[ ! "$OSTADIX_BOOT_CHALLENGE" =~ ^[0-9a-f]{64}$ ]] \
    || [[ "$OSTADIX_BOOT_CHALLENGE" == 0000000000000000000000000000000000000000000000000000000000000000 ]]; then
  echo "error: OSTADIX_BOOT_CHALLENGE must be a nonzero 64-hex value" >&2
  exit 2
fi
SOURCE_COMMIT="$(git -C "$ROOT" rev-parse --verify 'HEAD^{commit}')"
if [[ ! "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: SMP smoke requires a 40-hex Git source commit" >&2
  exit 1
fi

if [[ -z "${OSTADIX_OVMF_CODE:-}" ]]; then
  for candidate in \
    /opt/homebrew/opt/qemu/share/qemu/edk2-x86_64-code.fd \
    /usr/local/opt/qemu/share/qemu/edk2-x86_64-code.fd \
    /usr/share/OVMF/OVMF_CODE.fd \
    /usr/share/edk2/x64/OVMF_CODE.fd; do
    if [[ -f "$candidate" ]]; then
      OSTADIX_OVMF_CODE="$candidate"
      break
    fi
  done
fi
if [[ -z "${OSTADIX_OVMF_CODE:-}" || ! -f "$OSTADIX_OVMF_CODE" ]]; then
  echo "error: UEFI firmware not found; set OSTADIX_OVMF_CODE" >&2
  exit 127
fi

mkdir -p "$SMOKE_ROOT"
RUN_ROOT="$(mktemp -d "$SMOKE_ROOT/run.XXXXXX")"
POSITIVE_TRANSCRIPT="$RUN_ROOT/smp4.serial"
NEGATIVE_TRANSCRIPT="$RUN_ROOT/smp1-negative.serial"
TRANSCRIPT_CHECK="$RUN_ROOT/transcript-check.json"
TRANSCRIPT_CHECK_STDOUT="$RUN_ROOT/transcript-check.stdout"
OCORE_MEDIA_PROBE_MODE=34 \
OSTADIX_BOOT_CHALLENGE="$OSTADIX_BOOT_CHALLENGE" \
OSTADIX_MEDIA_ROOT="$SMOKE_ROOT/media-build" \
OCORE_MEDIA_KERNEL_BUILD_DIR="$SMOKE_ROOT/kernel" \
  "$ROOT/ocore/kernel/build-x86_64-uefi-media.sh" "$IMAGE" \
  >"$SMOKE_ROOT/build.txt"

python3 - \
  "$QEMU_BIN" \
  "$OSTADIX_OVMF_CODE" \
  "$IMAGE" \
  "$TIMEOUT" \
  "$OSTADIX_BOOT_CHALLENGE" \
  "$SOURCE_COMMIT" \
  "$POSITIVE_TRANSCRIPT" \
  "$NEGATIVE_TRANSCRIPT" <<'PY'
from pathlib import Path
import re
import subprocess
import sys

(
    qemu,
    firmware,
    media,
    timeout_text,
    challenge,
    source_commit,
    positive_path,
    negative_path,
) = sys.argv[1:]


def run(cpu_count: int) -> tuple[bytes, bytes, bool]:
    command = [
        qemu,
        "-accel", "tcg",
        "-machine", "q35",
        "-cpu", "max",
        "-smp", f"{cpu_count},sockets=1,cores={cpu_count},threads=1",
        "-m", "128M",
        "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={firmware}",
        "-drive", f"if=none,id=ostadix,format=raw,readonly=on,file={media}",
        "-device", "virtio-blk-pci,drive=ostadix",
        "-nodefaults",
        "-nic", "none",
        "-display", "none",
        "-serial", "stdio",
        "-monitor", "none",
        "-no-reboot",
        "-no-shutdown",
    ]
    try:
        result = subprocess.run(
            command,
            capture_output=True,
            check=False,
            timeout=float(timeout_text),
        )
        return result.stdout, result.stderr, False
    except subprocess.TimeoutExpired as error:
        return error.stdout or b"", error.stderr or b"", True


positive, positive_stderr, positive_timed_out = run(4)
Path(positive_path).write_bytes(positive)
positive_text = positive.decode("ascii", "replace").replace("\r\n", "\n").replace("\r", "\n")
required = [
    "O-core kernel: serial online",
    "BootInfoV1: malformed fixture rejected",
    "BootInfoV1: Multiboot2 normalized",
    "BootInfoV1: ACPI status valid",
    "BootInfoV1: EFI64 boot services exited",
    "BootInfoV1: firmware allocator window admitted",
    f"OSTADIX boot challenge: {challenge}",
    f"OSTADIX source commit: {source_commit}",
    "SMP boot inspection window closed: PASS",
    "SMP page protections: W^X online",
    "SMP firmware Multiboot2/ACPI handoff: PASS",
    "SMP MADT enabled type-9 rejection: PASS",
    "SMP low-memory trampoline admission: PASS",
    "SMP firmware MADT 4-CPU topology: PASS",
    "SMP timing source PIT: validated",
    "SMP x2APIC preparation: PASS",
    "SMP x2APIC INIT/SIPI: PASS",
    "SMP AP hardware identities unique: PASS",
    "SMP AP stacks isolated: PASS",
    "SMP BSP/AP barrier: 4 CPUs PASS",
    "SMP post-barrier timer: online",
    "SMP post-barrier heartbeat: online",
]
missing = [marker for marker in required if positive_text.splitlines().count(marker) != 1]
positions = [positive_text.find(marker + "\n") for marker in required]
if positions != sorted(positions):
    missing.append("causal SMP marker order")

cpu_pattern = re.compile(
    r"^OSTADIX SMP CPU logical=([0-9]+) apic=([0-9]+) "
    r"stack=(0x[0-9a-f]{16}) online$",
    re.MULTILINE,
)
records = [
    (int(logical), int(apic), int(stack, 16))
    for logical, apic, stack in cpu_pattern.findall(positive_text)
]
if [record[0] for record in records] != [0, 1, 2, 3]:
    missing.append("exact logical CPU order 0,1,2,3")
if len({record[1] for record in records}) != 4:
    missing.append("four unique APIC identities")
if len({record[2] for record in records}) != 4:
    missing.append("four unique stack assignments")
if any(stack == 0 or stack % 16 for _, _, stack in records):
    missing.append("nonzero 16-byte-aligned stacks")
if "SMP probe: REJECT" in positive_text:
    missing.append("no positive rejection")
if missing or not positive_timed_out:
    print(
        f"SMP positive control failed; missing={missing!r} "
        f"timed_out={positive_timed_out}",
        file=sys.stderr,
    )
    print("stdout:", positive_text, file=sys.stderr)
    print("stderr:", positive_stderr.decode("utf-8", "replace"), file=sys.stderr)
    raise SystemExit(1)

negative, negative_stderr, negative_timed_out = run(1)
Path(negative_path).write_bytes(negative)
negative_text = negative.decode("ascii", "replace").replace("\r\n", "\n").replace("\r", "\n")
for marker in (
    "SMP firmware MADT 4-CPU topology: PASS",
    "SMP MADT enabled type-9 rejection: PASS",
    "SMP x2APIC preparation: PASS",
    "SMP x2APIC INIT/SIPI: PASS",
    "OSTADIX SMP CPU logical=",
    "SMP BSP/AP barrier: 4 CPUs PASS",
    "SMP post-barrier heartbeat: online",
):
    if marker in negative_text:
        raise SystemExit(f"SMP negative control exposed forbidden marker: {marker!r}")
if negative_text.splitlines().count("SMP probe: REJECT") != 1 or not negative_timed_out:
    print(
        f"SMP negative control failed; timed_out={negative_timed_out}",
        file=sys.stderr,
    )
    print("stdout:", negative_text, file=sys.stderr)
    print("stderr:", negative_stderr.decode("utf-8", "replace"), file=sys.stderr)
    raise SystemExit(1)

print(positive_text, end="")
print("OSTADIX x86_64 UEFI SMP positive control: PASS")
print("OSTADIX x86_64 UEFI SMP one-CPU negative control: PASS")
PY

# Reuse the exact grammar used by authority-free physical observations while
# retaining an explicit QEMU/TCG substrate label.  This creates no physical
# evidence and grants no release or admission authority.
"$ROOT/scripts/ostadix_physical_evidence.py" check-transcript \
  --profile smp4 \
  --context qemu-tcg \
  --transcript "$POSITIVE_TRANSCRIPT" \
  --challenge "$OSTADIX_BOOT_CHALLENGE" \
  --source-commit "$SOURCE_COMMIT" \
  --expected-cpus 4 \
  --output "$TRANSCRIPT_CHECK" \
  >"$TRANSCRIPT_CHECK_STDOUT"

printf '%s\n' \
  "OSTADIX SMP transcript grammar: PASS" \
  "OSTADIX SMP boundary: QEMU/TCG q35 + OVMF, exactly four vCPUs; not physical-machine evidence"
