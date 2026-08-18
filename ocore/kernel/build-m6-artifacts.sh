#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_M6_BUILD_DIR:-$ROOT/target/ocore-m6-artifacts}"
USER_DIR="$ROOT/ocore/user"
PASS_ONE="$BUILD_DIR/pass-one"
PASS_TWO="$BUILD_DIR/pass-two"
IMAGE_DIR_ONE="$BUILD_DIR/images"
IMAGE_DIR_TWO="$BUILD_DIR/rebuild-images"
mkdir -p "$PASS_ONE" "$PASS_TWO" "$IMAGE_DIR_ONE" "$IMAGE_DIR_TWO"

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
  for candidate in \
    /opt/homebrew/opt/lld/bin/ld.lld \
    /opt/homebrew/opt/lld@21/bin/ld.lld \
    /opt/homebrew/opt/llvm/bin/ld.lld; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  echo "error: no LLD-compatible linker found" >&2
  return 1
}

if ! command -v objdump >/dev/null 2>&1; then
  echo "error: objdump is required for strict M6A ELF inspection" >&2
  exit 127
fi
if ! command -v nm >/dev/null 2>&1; then
  echo "error: nm is required for M6A entry-symbol inspection" >&2
  exit 127
fi

cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --package o-lang --bin ocorec
OCOREC="$ROOT/target/debug/ocorec"
LLD="$(find_lld)"

link_service() {
  local pass_dir="$1"
  local service="$2"
  "$OCOREC" \
    "$ROOT/ocore/runtime/x86_64/m6_${service}.oc" \
    --target x86_64-unknown-none \
    --emit obj \
    -o "$pass_dir/${service}.o"
  if [[ "$(nm "$pass_dir/${service}.o" | awk '$2 == "T" && $3 == "_start" { count += 1 } END { print count + 0 }')" != 1 ]]; then
    echo "error: ${service}.o does not export exactly one _start" >&2
    exit 1
  fi
  case "$(basename "$LLD")" in
    rust-lld | lld)
      "$LLD" -flavor gnu -m elf_x86_64 -nostdlib --build-id=none \
        --strip-all -z max-page-size=0x1000 \
        -T "$USER_DIR/static-user.ld" \
        -o "$pass_dir/${service}.elf" "$pass_dir/${service}.o"
      ;;
    *)
      "$LLD" -m elf_x86_64 -nostdlib --build-id=none --strip-all \
        -z max-page-size=0x1000 -T "$USER_DIR/static-user.ld" \
        -o "$pass_dir/${service}.elf" "$pass_dir/${service}.o"
      ;;
  esac
}

build_pass() {
  local pass_dir="$1"
  local service
  for service in client personalityd supervisord observer; do
    link_service "$pass_dir" "$service"
  done
}

check_elf() {
  local elf="$1"
  local file_headers
  local program_headers
  local section_headers
  local entry_address
  local flags
  local text_vma
  local rodata_vma
  file_headers="$(objdump -f "$elf")"
  program_headers="$(objdump -p "$elf")"
  section_headers="$(objdump -h "$elf")"
  entry_address="$(printf '%s\n' "$file_headers" | awk '$1 == "start" && ($2 == "address" || $2 == "address:") { print $NF }')"
  flags="$(printf '%s\n' "$program_headers" | awk '/ flags / { print $NF }')"
  text_vma="$(printf '%s\n' "$section_headers" | awk '$2 == ".text" { print $4 }')"
  rodata_vma="$(printf '%s\n' "$section_headers" | awk '$2 == ".rodata" { print $4 }')"
  if ! grep -Fq 'file format elf64-x86-64' <<<"$file_headers" \
      || ! grep -Eq 'architecture: (x86_64|i386:x86-64)' <<<"$file_headers" \
      || [[ "$entry_address" != 0x0000000002000000 ]] \
      || [[ "$(grep -Ec '^    LOAD ' <<<"$program_headers")" != 2 ]] \
      || [[ "$(grep -Ec '^   STACK ' <<<"$program_headers")" != 1 ]] \
      || [[ "$flags" != $'r-x\nr--\nrw-' ]] \
      || grep -Eq 'flags [^[:space:]]*w[^[:space:]]*x|flags [^[:space:]]*x[^[:space:]]*w' <<<"$program_headers" \
      || grep -Eq 'NEEDED|RPATH|RUNPATH|INTERP' <<<"$program_headers" \
      || [[ "$text_vma" != 0000000002000000 ]] \
      || ! grep -Eq '^00000000020[0-9a-f]{5}$' <<<"$rodata_vma" \
      || [[ "$(objdump -d "$elf" | grep -Ec '[[:space:]]syscall([[:space:]]|$)')" == 0 ]]; then
    echo "error: noncanonical M6A static ELF: $elf" >&2
    printf '%s\n%s\n%s\n' "$file_headers" "$program_headers" "$section_headers" >&2
    exit 1
  fi
}

pack_image() {
  local pass_dir="$1"
  local image_dir="$2"
  python3 "$USER_DIR/pack_ovfs.py" "$image_dir" \
    --entry /sbin/m6-client.elf "$pass_dir/client.elf" 3 \
    --entry /sbin/m6-personalityd.elf "$pass_dir/personalityd.elf" 3 \
    --entry /sbin/m6-supervisord.elf "$pass_dir/supervisord.elf" 3 \
    --entry /sbin/m6-observer.elf "$pass_dir/observer.elf" 3
}

verify_image() {
  local image="$1"
  local verification
  local actual
  local expected
  verification="$(python3 "$USER_DIR/verify_ovfs.py" "$image")"
  actual="$(printf '%s\n' "$verification" | sed -n '/^\/sbin\//p')"
  expected=$'/sbin/m6-client.elf: valid\n/sbin/m6-observer.elf: valid\n/sbin/m6-personalityd.elf: valid\n/sbin/m6-supervisord.elf: valid'
  if ! grep -Eq '^OVFSIMG1 verified: 4 files, [0-9]+ bytes, sha256=[0-9a-f]{64}$' <<<"$verification" \
      || [[ "$actual" != "$expected" ]]; then
    echo "error: M6A OVFS does not contain the exact four-service corpus" >&2
    printf '%s\n' "$verification" >&2
    exit 1
  fi
}

build_pass "$PASS_ONE"
build_pass "$PASS_TWO"

for service in client personalityd supervisord observer; do
  check_elf "$PASS_ONE/${service}.elf"
  check_elf "$PASS_TWO/${service}.elf"
  if ! cmp -s "$PASS_ONE/${service}.o" "$PASS_TWO/${service}.o" \
      || ! cmp -s "$PASS_ONE/${service}.elf" "$PASS_TWO/${service}.elf"; then
    echo "error: ${service} object/ELF rebuild was not deterministic" >&2
    exit 1
  fi
done

IMAGE_ONE="$(pack_image "$PASS_ONE" "$IMAGE_DIR_ONE")"
IMAGE_TWO="$(pack_image "$PASS_TWO" "$IMAGE_DIR_TWO")"
verify_image "$IMAGE_ONE"
verify_image "$IMAGE_TWO"
DIGEST_ONE="$(shasum -a 256 "$IMAGE_ONE" | awk '{print $1}')"
DIGEST_TWO="$(shasum -a 256 "$IMAGE_TWO" | awk '{print $1}')"
if [[ "$DIGEST_ONE" != "$DIGEST_TWO" ]] || ! cmp -s "$IMAGE_ONE" "$IMAGE_TWO"; then
  echo "error: M6A OVFSIMG1 rebuild was not deterministic" >&2
  exit 1
fi

file \
  "$PASS_ONE/client.elf" \
  "$PASS_ONE/personalityd.elf" \
  "$PASS_ONE/supervisord.elf" \
  "$PASS_ONE/observer.elf" \
  "$IMAGE_ONE"
printf 'M6A artifact build: PASS\n'
for service in client personalityd supervisord observer; do
  printf 'elf-%s-bytes: %s\n' \
    "$service" "$(wc -c < "$PASS_ONE/${service}.elf" | tr -d ' ')"
done
printf 'image-bytes: %s\nimage: %s\nsha256: %s\n' \
  "$(wc -c < "$IMAGE_ONE" | tr -d ' ')" "$IMAGE_ONE" "$DIGEST_ONE"
