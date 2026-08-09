#!/usr/bin/env bash
# Build one deterministic GPT/ESP image that boots the current x86_64 kernel
# through UEFI GRUB and the kernel's Multiboot2 entry.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MEDIA_ROOT="${OSTADIX_MEDIA_ROOT:-$ROOT/target/ostadix-media/x86_64}"
KERNEL_BUILD_DIR="${OCORE_MEDIA_KERNEL_BUILD_DIR:-$ROOT/target/ocore-media-kernel}"
OUTPUT="${1:-$MEDIA_ROOT/ostadix-x86_64-uefi.img}"
GRUB_MKSTANDALONE="${OSTADIX_GRUB_MKSTANDALONE:-}"
MFORMAT="${OSTADIX_MFORMAT:-mformat}"
MCOPY="${OSTADIX_MCOPY:-mcopy}"
PYTHON="${OSTADIX_PYTHON:-python3}"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-315532800}"
ESP_BYTES="${OSTADIX_ESP_BYTES:-67108864}"
PROBE_MODE="${OCORE_MEDIA_PROBE_MODE:-0}"
BOOT_CHALLENGE="${OSTADIX_BOOT_CHALLENGE:-}"
SOURCE_COMMIT=""
SOURCE_ROOT="$ROOT"
SOURCE_SNAPSHOT=""
WORK_DIR=""
export SOURCE_DATE_EPOCH

cleanup() {
  if [[ -n "$WORK_DIR" ]]; then
    rm -rf -- "$WORK_DIR"
  fi
  if [[ -n "$SOURCE_SNAPSHOT" ]]; then
    rm -rf -- "$SOURCE_SNAPSHOT"
  fi
}
trap cleanup EXIT INT TERM

require_challenged_git_state() {
  local observed_commit
  if [[ -z "$BOOT_CHALLENGE" ]]; then
    return 0
  fi
  observed_commit="$(git -C "$ROOT" rev-parse --verify 'HEAD^{commit}')"
  if [[ "$observed_commit" != "$SOURCE_COMMIT" ]]; then
    echo "error: challenged OSTADIX source commit changed during the media build" >&2
    return 1
  fi
  if [[ -n "$(git -C "$ROOT" status --porcelain --untracked-files=all)" ]]; then
    echo "error: challenged OSTADIX worktree changed during the media build" >&2
    return 1
  fi
}

if [[ -z "$GRUB_MKSTANDALONE" ]]; then
  if command -v x86_64-elf-grub-mkstandalone >/dev/null 2>&1; then
    GRUB_MKSTANDALONE=x86_64-elf-grub-mkstandalone
  elif command -v grub-mkstandalone >/dev/null 2>&1; then
    GRUB_MKSTANDALONE=grub-mkstandalone
  else
    echo "error: required OSTADIX media tool is unavailable: x86_64-elf-grub-mkstandalone or grub-mkstandalone" >&2
    exit 127
  fi
fi

if [[ $# -gt 1 ]]; then
  echo "usage: build-x86_64-uefi-media.sh [OUTPUT]" >&2
  exit 2
fi
if [[ ! "$PROBE_MODE" =~ ^[0-9]+$ ]] || (( PROBE_MODE < 0 || PROBE_MODE > 34 )); then
  echo "error: OCORE_MEDIA_PROBE_MODE must be an integer from 0 through 34" >&2
  exit 2
fi
if [[ -n "$BOOT_CHALLENGE" && ! "$BOOT_CHALLENGE" =~ ^[0-9a-f]{64}$ ]]; then
  echo "error: OSTADIX_BOOT_CHALLENGE must be exactly 64 lowercase hexadecimal digits" >&2
  exit 2
fi
if [[ "$BOOT_CHALLENGE" == 0000000000000000000000000000000000000000000000000000000000000000 ]]; then
  echo "error: OSTADIX_BOOT_CHALLENGE must not use the all-zero sentinel" >&2
  exit 2
fi
if (( PROBE_MODE == 33 || PROBE_MODE == 34 )) && [[ -z "$BOOT_CHALLENGE" ]]; then
  echo "error: OCORE_MEDIA_PROBE_MODE=$PROBE_MODE requires OSTADIX_BOOT_CHALLENGE" >&2
  exit 2
fi
if [[ -n "$BOOT_CHALLENGE" ]]; then
  if ! command -v git >/dev/null 2>&1 \
      || [[ "$(git -C "$ROOT" rev-parse --show-toplevel 2>/dev/null || true)" != "$ROOT" ]]; then
    echo "error: challenged OSTADIX media requires the canonical Git worktree" >&2
    exit 1
  fi
  if [[ -n "$(git -C "$ROOT" status --porcelain --untracked-files=all)" ]]; then
    echo "error: challenged OSTADIX media requires a clean Git worktree" >&2
    exit 1
  fi
  SOURCE_COMMIT="$(git -C "$ROOT" rev-parse --verify 'HEAD^{commit}')"
  if [[ ! "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
    echo "error: challenged OSTADIX media requires a 40-hex Git source commit" >&2
    exit 1
  fi
  if ! command -v tar >/dev/null 2>&1; then
    echo "error: challenged OSTADIX media requires tar for a committed source snapshot" >&2
    exit 127
  fi

  # The challenge and source-commit labels are only useful when the compiler
  # consumes bytes from that exact commit.  Export a private Git-tree snapshot
  # instead of building from the mutable canonical checkout.  The target link
  # shares build artifacts only; every source and build script is read from
  # the private export of the named commit rather than the mutable checkout.
  SOURCE_SNAPSHOT="$(mktemp -d "${TMPDIR:-/tmp}/ostadix-source.XXXXXX")"
  git -C "$ROOT" archive --format=tar "$SOURCE_COMMIT" \
    | tar -xf - -C "$SOURCE_SNAPSHOT"
  if [[ -e "$SOURCE_SNAPSHOT/target" || -L "$SOURCE_SNAPSHOT/target" ]]; then
    echo "error: committed OSTADIX source unexpectedly contains target" >&2
    exit 1
  fi
  ln -s "$ROOT/target" "$SOURCE_SNAPSHOT/target"
  SOURCE_ROOT="$SOURCE_SNAPSHOT"
  require_challenged_git_state
fi
GRUB_CFG="$SOURCE_ROOT/ocore/kernel/x86_64/grub.cfg"
GPT_TOOL="$SOURCE_ROOT/scripts/ostadix_boot_media.py"
KERNEL_BUILD_SCRIPT="$SOURCE_ROOT/ocore/kernel/build.sh"
if [[ ! "$ESP_BYTES" =~ ^[0-9]+$ ]] || (( ESP_BYTES < 33554432 || ESP_BYTES > 536870912 || ESP_BYTES % 512 != 0 )); then
  echo "error: OSTADIX_ESP_BYTES must be a 512-byte-aligned integer from 33554432 through 536870912" >&2
  exit 2
fi
for tool in "$GRUB_MKSTANDALONE" "$MFORMAT" "$MCOPY" "$PYTHON"; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'error: required OSTADIX media tool is unavailable: %s\n' "$tool" >&2
    exit 127
  fi
done
for source in "$GRUB_CFG" "$GPT_TOOL" "$KERNEL_BUILD_SCRIPT"; do
  if [[ ! -f "$source" ]]; then
    printf 'error: required OSTADIX media source is missing: %s\n' "$source" >&2
    exit 1
  fi
done

mkdir -p "$MEDIA_ROOT" "$(dirname "$OUTPUT")"
WORK_DIR="$(mktemp -d "$MEDIA_ROOT/.media-build.XXXXXX")"

STAGE="$WORK_DIR/stage"
ESP="$WORK_DIR/esp.fat"
RENDERED_GRUB_CFG="$WORK_DIR/grub.cfg"
FAT_IDENTITY="$WORK_DIR/fat-identity.txt"
mkdir -p "$STAGE/EFI/BOOT" "$STAGE/boot"

OCORE_PROBE_MODE="$PROBE_MODE" OCORE_BUILD_DIR="$KERNEL_BUILD_DIR" \
  "$KERNEL_BUILD_SCRIPT" >/dev/null
KERNEL="$KERNEL_BUILD_DIR/kernel.elf"
if [[ ! -f "$KERNEL" ]]; then
  printf 'error: kernel build completed without producing %s\n' "$KERNEL" >&2
  exit 1
fi

cp "$KERNEL" "$STAGE/boot/kernel.elf"
"$PYTHON" - "$KERNEL" "$GRUB_CFG" "$RENDERED_GRUB_CFG" "$FAT_IDENTITY" "$BOOT_CHALLENGE" "$SOURCE_COMMIT" <<'PY'
from hashlib import sha256
from pathlib import Path
import sys

kernel_path, template_path, rendered_path, identity_path = map(Path, sys.argv[1:5])
challenge = sys.argv[5]
source_commit = sys.argv[6]
kernel = kernel_path.read_bytes()
template = template_path.read_bytes()
marker = b"@OSTADIX_FAT_UUID@"
arguments_marker = b"@OSTADIX_KERNEL_ARGS@"
if template.count(marker) != 1:
    raise SystemExit(
        "error: GRUB configuration must contain exactly one "
        "@OSTADIX_FAT_UUID@ marker"
    )
if template.count(arguments_marker) != 1:
    raise SystemExit(
        "error: GRUB configuration must contain exactly one "
        "@OSTADIX_KERNEL_ARGS@ marker"
    )
arguments = b""
if challenge:
    arguments = (
        b"ostadix.challenge="
        + challenge.encode("ascii")
        + b" ostadix.source_commit="
        + source_commit.encode("ascii")
    )
template = template.replace(arguments_marker, arguments)

# This is the versioned identity preimage, not a digest of the completed ESP:
# the completed ESP contains BOOTX64.EFI, which itself embeds this identity.
preimage = (
    b"OSTADIX/FAT-IDENTITY/V1\0"
    + len(kernel).to_bytes(8, "big")
    + kernel
    + len(template).to_bytes(8, "big")
    + template
)
identity_digest = sha256(preimage).digest()
serial = int.from_bytes(identity_digest[:4], "big")
if serial == 0:
    serial = 1

# GRUB reads the little-endian FAT volume ID into a host integer, then renders
# that integer's high and low 16-bit words (0x12345678 becomes 1234-5678).
fat_uuid = f"{serial >> 16:04X}-{serial & 0xffff:04X}"
rendered_path.write_bytes(template.replace(marker, fat_uuid.encode("ascii")))
identity_path.write_text(
    f"{serial:08X}\n{fat_uuid}\n{identity_digest.hex()}\n",
    encoding="ascii",
)
PY
FAT_SERIAL="$(sed -n '1p' "$FAT_IDENTITY")"
FAT_UUID="$(sed -n '2p' "$FAT_IDENTITY")"
FAT_IDENTITY_SHA256="$(sed -n '3p' "$FAT_IDENTITY")"
if [[ ! "$FAT_SERIAL" =~ ^[0-9A-F]{8}$ ]] \
  || [[ ! "$FAT_UUID" =~ ^[0-9A-F]{4}-[0-9A-F]{4}$ ]] \
  || [[ ! "$FAT_IDENTITY_SHA256" =~ ^[0-9a-f]{64}$ ]]; then
  echo "error: derived OSTADIX FAT identity has an invalid representation" >&2
  exit 1
fi

SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" "$GRUB_MKSTANDALONE" \
  --format=x86_64-efi \
  --output="$STAGE/EFI/BOOT/BOOTX64.EFI" \
  --locales="" \
  --fonts="" \
  --modules="part_gpt fat normal search search_fs_uuid multiboot2 serial configfile" \
  "boot/grub/grub.cfg=$RENDERED_GRUB_CFG"

# FAT timestamps cannot represent dates before 1980.  Preserve this exact
# epoch during recursive mcopy so directory entries remain reproducible.
find "$STAGE" -exec touch -t 198001010000 {} +
"$PYTHON" - "$ESP" "$ESP_BYTES" <<'PY'
from pathlib import Path
import sys

with Path(sys.argv[1]).open("wb") as stream:
    stream.truncate(int(sys.argv[2]))
PY
"$MFORMAT" -i "$ESP" -F -v OSTADIX -N "0x$FAT_SERIAL" ::
"$PYTHON" - "$ESP" "$FAT_SERIAL" <<'PY'
from pathlib import Path
import sys

esp_path = Path(sys.argv[1])
expected = int(sys.argv[2], 16)
with esp_path.open("rb") as stream:
    boot_sector = stream.read(512)
if len(boot_sector) != 512 or boot_sector[66] != 0x29:
    raise SystemExit("error: mformat did not produce the expected FAT32 volume-ID field")
observed = int.from_bytes(boot_sector[67:71], "little")
if observed != expected:
    raise SystemExit(
        f"error: FAT volume ID mismatch: expected {expected:08X}, observed {observed:08X}"
    )
PY
"$MCOPY" -smp -i "$ESP" "$STAGE/EFI" "$STAGE/boot" ::/

# Catch a caller checkout change before publishing the image.  The actual
# compilation already used SOURCE_ROOT's committed snapshot, so even an
# edit-and-revert race cannot change the bytes attributed to SOURCE_COMMIT.
require_challenged_git_state

PACK_METADATA="$WORK_DIR/pack.json"
"$PYTHON" "$GPT_TOOL" pack --esp "$ESP" --output "$OUTPUT" >"$PACK_METADATA"
"$PYTHON" "$GPT_TOOL" inspect "$OUTPUT" >"$WORK_DIR/inspect.json"
if ! cmp -s "$PACK_METADATA" "$WORK_DIR/inspect.json"; then
  echo "error: packed and independently inspected OSTADIX media metadata differ" >&2
  exit 1
fi

"$PYTHON" - "$OUTPUT" "$PACK_METADATA" <<'PY'
import json
from pathlib import Path
import sys

image = Path(sys.argv[1]).resolve()
metadata = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
print(f"media: {image}")
print(f"media-bytes: {metadata['bytes']}")
print(f"media-sha256: {metadata['sha256']}")
print(f"esp-sha256: {metadata['esp_sha256']}")
print(f"disk-guid: {metadata['disk_guid']}")
PY
printf 'fat-uuid: %s\n' "$FAT_UUID"
printf 'fat-identity-sha256: %s\n' "$FAT_IDENTITY_SHA256"
printf 'probe-mode: %s\n' "$PROBE_MODE"
if [[ -n "$BOOT_CHALLENGE" ]]; then
  printf 'boot-challenge: %s\n' "$BOOT_CHALLENGE"
  printf 'source-commit: %s\n' "$SOURCE_COMMIT"
fi
