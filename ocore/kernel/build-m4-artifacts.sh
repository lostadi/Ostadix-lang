#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_M4_BUILD_DIR:-$ROOT/target/ocore-m4}"
USER_DIR="$ROOT/ocore/user"
mkdir -p "$BUILD_DIR/corpus" "$BUILD_DIR/images"

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

find_objdump() {
  local candidate
  for candidate in llvm-objdump /opt/homebrew/opt/llvm/bin/llvm-objdump; do
    if command -v "$candidate" >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  echo "error: llvm-objdump is required for ELF verification" >&2
  return 1
}

cargo build --manifest-path "$ROOT/Cargo.toml" --package o-lang --bin ocorec
OCOREC="$ROOT/target/debug/ocorec"
LLD="$(find_lld)"
OBJDUMP="$(find_objdump)"

build_program() {
  local name="$1"
  local source="$2"
  "$OCOREC" \
    "$ROOT/ocore/runtime/x86_64/native_abi.oc" \
    "$source" \
    --target x86_64-unknown-none \
    --emit obj \
    -o "$BUILD_DIR/$name.o"
  case "$(basename "$LLD")" in
    rust-lld | lld)
      "$LLD" -flavor gnu -m elf_x86_64 -nostdlib --build-id=none \
        -z max-page-size=0x1000 -T "$USER_DIR/static-user.ld" \
        -o "$BUILD_DIR/$name.elf" "$BUILD_DIR/$name.o"
      ;;
    *)
      "$LLD" -m elf_x86_64 -nostdlib --build-id=none \
        -z max-page-size=0x1000 -T "$USER_DIR/static-user.ld" \
        -o "$BUILD_DIR/$name.elf" "$BUILD_DIR/$name.o"
      ;;
  esac
}

build_program personality-alpha \
  "$ROOT/ocore/runtime/x86_64/m4_personality_alpha.oc"
build_program personality-beta \
  "$ROOT/ocore/runtime/x86_64/m4_personality_beta.oc"

file "$BUILD_DIR/personality-alpha.elf" "$BUILD_DIR/personality-beta.elf"
"$OBJDUMP" -p "$BUILD_DIR/personality-alpha.elf"
"$OBJDUMP" -p "$BUILD_DIR/personality-beta.elf"

python3 "$USER_DIR/make_m4_elf_corpus.py" \
  "$BUILD_DIR/personality-alpha.elf" "$BUILD_DIR/corpus"

pack_image() {
  python3 "$USER_DIR/pack_ovfs.py" "$BUILD_DIR/images" \
    --entry /bin/personality-alpha.elf "$BUILD_DIR/personality-alpha.elf" 3 \
    --entry /bin/personality-beta.elf "$BUILD_DIR/personality-beta.elf" 3 \
    --entry /corpus/malformed.elf "$BUILD_DIR/corpus/malformed.elf" 5 \
    --entry /corpus/overlap.elf "$BUILD_DIR/corpus/overlap.elf" 5 \
    --entry /corpus/wx.elf "$BUILD_DIR/corpus/wx.elf" 5
}

IMAGE_ONE="$(pack_image)"
DIGEST_ONE="$(shasum -a 256 "$IMAGE_ONE" | awk '{print $1}')"
IMAGE_TWO="$(pack_image)"
DIGEST_TWO="$(shasum -a 256 "$IMAGE_TWO" | awk '{print $1}')"
if [[ "$IMAGE_ONE" != "$IMAGE_TWO" || "$DIGEST_ONE" != "$DIGEST_TWO" ]]; then
  echo "error: OVFSIMG1 rebuild was not deterministic" >&2
  exit 1
fi

python3 "$USER_DIR/verify_ovfs.py" --expect-m4 "$IMAGE_ONE"
file "$BUILD_DIR/corpus/malformed.elf" \
  "$BUILD_DIR/corpus/overlap.elf" \
  "$BUILD_DIR/corpus/wx.elf" \
  "$IMAGE_ONE"
printf 'M4 artifact build: PASS\nimage: %s\nsha256: %s\n' \
  "$IMAGE_ONE" "$DIGEST_ONE"
