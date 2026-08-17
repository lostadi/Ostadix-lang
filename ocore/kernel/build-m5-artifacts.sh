#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_M5_BUILD_DIR:-$ROOT/target/ocore-m5-artifacts}"
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
  for candidate in /opt/homebrew/opt/lld/bin/ld.lld /opt/homebrew/opt/lld@21/bin/ld.lld /opt/homebrew/opt/llvm/bin/ld.lld; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  echo "error: no LLD-compatible linker found" >&2
  return 1
}

cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --package o-lang --bin ocorec
OCOREC="$ROOT/target/debug/ocorec"
LLD="$(find_lld)"

link_service() {
  local pass_dir="$1"
  local service="$2"
  "$OCOREC" \
    "$ROOT/ocore/runtime/x86_64/native_abi.oc" \
    "$ROOT/ocore/runtime/x86_64/m5_${service}.oc" \
    --target x86_64-unknown-none \
    --emit obj \
    -o "$pass_dir/${service}.o"
  case "$(basename "$LLD")" in
    rust-lld | lld)
      "$LLD" -flavor gnu -m elf_x86_64 -nostdlib --build-id=none \
        -z max-page-size=0x1000 -T "$USER_DIR/static-user.ld" \
        -o "$pass_dir/${service}.elf" "$pass_dir/${service}.o"
      ;;
    *)
      "$LLD" -m elf_x86_64 -nostdlib --build-id=none \
        -z max-page-size=0x1000 -T "$USER_DIR/static-user.ld" \
        -o "$pass_dir/${service}.elf" "$pass_dir/${service}.o"
      ;;
  esac
}

build_pass() {
  local pass_dir="$1"
  local service
  for service in init supervisor pkgd repl; do
    link_service "$pass_dir" "$service"
  done
}

pack_image() {
  local pass_dir="$1"
  local image_dir="$2"
  python3 "$USER_DIR/pack_ovfs.py" "$image_dir" \
    --entry /sbin/init.elf "$pass_dir/init.elf" 3 \
    --entry /sbin/supervisor.elf "$pass_dir/supervisor.elf" 3 \
    --entry /sbin/pkgd.elf "$pass_dir/pkgd.elf" 3 \
    --entry /sbin/repl.elf "$pass_dir/repl.elf" 3
}

build_pass "$PASS_ONE"
build_pass "$PASS_TWO"

for service in init supervisor pkgd repl; do
  if ! cmp -s "$PASS_ONE/${service}.elf" "$PASS_TWO/${service}.elf"; then
    echo "error: ${service}.elf rebuild was not deterministic" >&2
    exit 1
  fi
done

IMAGE_ONE="$(pack_image "$PASS_ONE" "$IMAGE_DIR_ONE")"
IMAGE_TWO="$(pack_image "$PASS_TWO" "$IMAGE_DIR_TWO")"
DIGEST_ONE="$(shasum -a 256 "$IMAGE_ONE" | awk '{print $1}')"
DIGEST_TWO="$(shasum -a 256 "$IMAGE_TWO" | awk '{print $1}')"
if [[ "$DIGEST_ONE" != "$DIGEST_TWO" ]] || ! cmp -s "$IMAGE_ONE" "$IMAGE_TWO"; then
  echo "error: M5 OVFSIMG1 rebuild was not deterministic" >&2
  exit 1
fi

python3 "$USER_DIR/verify_ovfs.py" --expect-m5 "$IMAGE_ONE"
python3 "$USER_DIR/verify_ovfs.py" --expect-m5 "$IMAGE_TWO"

file \
  "$PASS_ONE/init.elf" \
  "$PASS_ONE/supervisor.elf" \
  "$PASS_ONE/pkgd.elf" \
  "$PASS_ONE/repl.elf" \
  "$IMAGE_ONE"

printf 'M5 artifact build: PASS\n'
for service in init supervisor pkgd repl; do
  printf 'elf-%s-bytes: %s\n' \
    "$service" "$(wc -c < "$PASS_ONE/${service}.elf" | tr -d ' ')"
done
printf 'image-bytes: %s\nimage: %s\nsha256: %s\n' \
  "$(wc -c < "$IMAGE_ONE" | tr -d ' ')" "$IMAGE_ONE" "$DIGEST_ONE"
