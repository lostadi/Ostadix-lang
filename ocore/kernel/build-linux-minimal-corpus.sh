#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_LINUX_BUILD_DIR:-$ROOT/target/ocore-linux-minimal}"
USER_DIR="$ROOT/ocore/user"
PASS_ONE="$BUILD_DIR/pass-one"
PASS_TWO="$BUILD_DIR/pass-two"
mkdir -p "$PASS_ONE" "$PASS_TWO"

for tool in clang objdump python3 rustc shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the minimal Linux corpus" >&2
    exit 127
  fi
done

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

LLD="$(find_lld)"

build_pass() {
  local pass_dir="$1"
  clang -target x86_64-unknown-none-elf -c -x assembler-with-cpp \
    "$USER_DIR/linux_minimal_guest.S" \
    -o "$pass_dir/linux-minimal.o"
  case "$(basename "$LLD")" in
    rust-lld | lld)
      "$LLD" -flavor gnu -m elf_x86_64 -nostdlib --build-id=none \
        --strip-all -z max-page-size=0x1000 \
        -T "$USER_DIR/linux-minimal-user.ld" \
        -o "$pass_dir/linux-minimal.elf" \
        "$pass_dir/linux-minimal.o"
      ;;
    *)
      "$LLD" -m elf_x86_64 -nostdlib --build-id=none --strip-all \
        -z max-page-size=0x1000 \
        -T "$USER_DIR/linux-minimal-user.ld" \
        -o "$pass_dir/linux-minimal.elf" \
        "$pass_dir/linux-minimal.o"
      ;;
  esac
}

build_pass "$PASS_ONE"
build_pass "$PASS_TWO"

if ! cmp -s "$PASS_ONE/linux-minimal.o" "$PASS_TWO/linux-minimal.o" \
    || ! cmp -s "$PASS_ONE/linux-minimal.elf" "$PASS_TWO/linux-minimal.elf"; then
  echo "error: minimal Linux object/ELF rebuild was not deterministic" >&2
  exit 1
fi

for pass_dir in "$PASS_ONE" "$PASS_TWO"; do
  python3 "$USER_DIR/verify_linux_minimal_corpus.py" \
    "$pass_dir/linux-minimal.elf" \
    "$USER_DIR/linux-minimal-oracle.json"
done

PYTHONDONTWRITEBYTECODE=1 python3 "$USER_DIR/test_linux_minimal_corpus.py" \
  "$PASS_ONE/linux-minimal.elf" \
  "$USER_DIR/linux-minimal-oracle.json"

if [[ "$(objdump -d "$PASS_ONE/linux-minimal.elf" | grep -Ec '[[:space:]]syscall([[:space:]]|$)')" != 5 ]]; then
  echo "error: minimal Linux corpus disassembly does not contain five syscall sites" >&2
  exit 1
fi

ELF="$PASS_ONE/linux-minimal.elf"
printf 'minimal Linux deterministic artifact build: PASS\n'
printf 'elf-bytes: %s\n' "$(wc -c < "$ELF" | tr -d ' ')"
printf 'elf: %s\n' "$ELF"
printf 'sha256: %s\n' "$(shasum -a 256 "$ELF" | awk '{print $1}')"
printf 'native-x86_64-linux-replay: PENDING\n'
