#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KERNEL_DIR="$ROOT/ocore/kernel"
USER_DIR="$ROOT/ocore/user"
RUNTIME_DIR="$ROOT/ocore/runtime/x86_64"
WORLD_DIR="$ROOT/ocore/world"
BUILD_DIR="${OCORE_M7B_BUILD_DIR:-$ROOT/target/ocore-m7b-logical-read-artifacts}"
PASS_ONE="$BUILD_DIR/pass-one"
PASS_TWO="$BUILD_DIR/pass-two"
IMAGE_ONE_DIR="$BUILD_DIR/images"
IMAGE_TWO_DIR="$BUILD_DIR/rebuild-images"
OBJECT_FILE="$BUILD_DIR/m7b-logical-object.bin"
mkdir -p "$PASS_ONE" "$PASS_TWO" "$IMAGE_ONE_DIR" "$IMAGE_TWO_DIR"

for tool in cargo cmp file nm objdump python3 rustc shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for M7B-1 artifact construction" >&2
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

CLIENT_SOURCE="$RUNTIME_DIR/m7b_logical_read_client.oc"
PROVIDER_SOURCE="$RUNTIME_DIR/m7b_9p_provider.oc"
ORACLE="$USER_DIR/test_m7b_logical_read.py"
for source in "$CLIENT_SOURCE" "$PROVIDER_SOURCE" "$ORACLE"; do
  if [[ ! -f "$source" ]]; then
    echo "error: required M7B-1 source is missing: $source" >&2
    exit 1
  fi
done

python3 "$ORACLE"
printf 'm7b-logical-object!\n' > "$OBJECT_FILE"
if [[ "$(wc -c < "$OBJECT_FILE" | tr -d ' ')" != 20 \
    || "$(shasum -a 256 "$OBJECT_FILE" | awk '{print $1}')" \
      != 59a08e13c63eb8acdae93f4caf05130733a0f5ab24e564fb1206f0f1d055809b ]]; then
  echo "error: immutable M7B-1 object identity drift" >&2
  exit 1
fi

cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin ocorec
OCOREC="$ROOT/target/debug/ocorec"
LLD="$(find_lld)"

compile_principal() {
  local pass_dir="$1"
  local principal="$2"
  local object="$pass_dir/${principal}.o"
  if [[ "$principal" == m7b-logical-read ]]; then
    "$OCOREC" \
      "$WORLD_DIR/sha256.oc" \
      "$CLIENT_SOURCE" \
      --target x86_64-unknown-none --emit obj -o "$object"
  else
    "$OCOREC" \
      "$RUNTIME_DIR/native_abi.oc" \
      "$PROVIDER_SOURCE" \
      --target x86_64-unknown-none --emit obj -o "$object"
  fi
  if [[ "$(nm "$object" | awk '$2 == "T" && $3 == "_start" { count += 1 } END { print count + 0 }')" != 1 ]]; then
    echo "error: $principal does not export exactly one _start" >&2
    exit 1
  fi
  case "$(basename "$LLD")" in
    rust-lld | lld)
      "$LLD" -flavor gnu -m elf_x86_64 -nostdlib --build-id=none \
        --strip-all -z max-page-size=0x1000 -T "$USER_DIR/static-user.ld" \
        -o "$pass_dir/${principal}.elf" "$object"
      ;;
    *)
      "$LLD" -m elf_x86_64 -nostdlib --build-id=none --strip-all \
        -z max-page-size=0x1000 -T "$USER_DIR/static-user.ld" \
        -o "$pass_dir/${principal}.elf" "$object"
      ;;
  esac
}

build_pass() {
  local pass_dir="$1"
  compile_principal "$pass_dir" m7b-logical-read
  compile_principal "$pass_dir" m7b-9pd
}

check_elf() {
  local elf="$1"
  local marker="$2"
  local file_headers
  local entry_address
  file_headers="$(objdump -f "$elf")"
  entry_address="$(printf '%s\n' "$file_headers" | awk '$1 == "start" && ($2 == "address" || $2 == "address:") { print $NF }')"
  if ! grep -Fq 'file format elf64-x86-64' <<<"$file_headers" \
      || [[ "$entry_address" != 0x0000000002000000 ]] \
      || [[ "$(objdump -p "$elf" | grep -Ec '^   STACK ')" != 1 ]] \
      || objdump -p "$elf" | grep -Eq 'NEEDED|RPATH|RUNPATH|INTERP' \
      || objdump -p "$elf" | grep -Eq 'flags [^[:space:]]*w[^[:space:]]*x|flags [^[:space:]]*x[^[:space:]]*w' \
      || [[ "$(objdump -d "$elf" | grep -Ec '[[:space:]]syscall([[:space:]]|$)')" == 0 ]] \
      || ! grep -aFq "$marker" "$elf"; then
    echo "error: noncanonical M7B-1 static ELF: $elf" >&2
    exit 1
  fi
}

pack_image() {
  local pass_dir="$1"
  local image_dir="$2"
  python3 "$USER_DIR/pack_ovfs.py" "$image_dir" \
    --max-image-bytes 98304 \
    --entry /bin/m7b-logical-read.elf "$pass_dir/m7b-logical-read.elf" 3 \
    --entry /sbin/m7b-9pd.elf "$pass_dir/m7b-9pd.elf" 3 \
    --entry /objects/logical-read-v1 "$OBJECT_FILE" 1
}

verify_image() {
  local image="$1"
  local verification
  verification="$(python3 "$USER_DIR/verify_ovfs.py" \
    --max-image-bytes 98304 "$image")"
  if ! grep -Eq '^OVFSIMG1 verified: 3 files, [0-9]+ bytes, sha256=[0-9a-f]{64}$' <<<"$verification" \
      || ! grep -Fq '/bin/m7b-logical-read.elf: valid' <<<"$verification" \
      || ! grep -Fq '/sbin/m7b-9pd.elf: valid' <<<"$verification"; then
    echo "error: M7B-1 OVFS does not contain the exact admitted corpus" >&2
    printf '%s\n' "$verification" >&2
    exit 1
  fi
}

build_pass "$PASS_ONE"
build_pass "$PASS_TWO"
for principal in m7b-logical-read m7b-9pd; do
  check_elf "$PASS_ONE/${principal}.elf" 'M7B-1'
  check_elf "$PASS_TWO/${principal}.elf" 'M7B-1'
  if ! cmp -s "$PASS_ONE/${principal}.o" "$PASS_TWO/${principal}.o" \
      || ! cmp -s "$PASS_ONE/${principal}.elf" "$PASS_TWO/${principal}.elf"; then
    echo "error: $principal object/ELF rebuild was not deterministic" >&2
    exit 1
  fi
done
if [[ "$(objdump -d "$PASS_ONE/m7b-9pd.elf" | grep -Eci '[[:space:]]ud2([[:space:]]|$)')" == 0 ]]; then
  echo "error: M7B-1 provider lacks its deliberate #UD fault boundary" >&2
  exit 1
fi

IMAGE_ONE="$(pack_image "$PASS_ONE" "$IMAGE_ONE_DIR")"
IMAGE_TWO="$(pack_image "$PASS_TWO" "$IMAGE_TWO_DIR")"
verify_image "$IMAGE_ONE"
verify_image "$IMAGE_TWO"
if ! cmp -s "$IMAGE_ONE" "$IMAGE_TWO"; then
  echo "error: M7B-1 OVFS rebuild was not deterministic" >&2
  exit 1
fi

printf 'M7B-1 LogicalRead artifact build: PASS\n'
for artifact in \
  "$OBJECT_FILE" \
  "$PASS_ONE/m7b-logical-read.elf" \
  "$PASS_ONE/m7b-9pd.elf" \
  "$IMAGE_ONE"; do
  printf 'artifact: %s bytes=%s sha256=%s\n' \
    "$artifact" \
    "$(wc -c < "$artifact" | tr -d ' ')" \
    "$(shasum -a 256 "$artifact" | awk '{print $1}')"
done
printf 'image: %s\n' "$IMAGE_ONE"
