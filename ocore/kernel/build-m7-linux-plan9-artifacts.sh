#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KERNEL_DIR="$ROOT/ocore/kernel"
USER_DIR="$ROOT/ocore/user"
RUNTIME_DIR="$ROOT/ocore/runtime/x86_64"
BUILD_DIR="${OCORE_M7_LINUX_PLAN9_BUILD_DIR:-${OCORE_M7_BUILD_DIR:-$ROOT/target/ocore-m7-linux-plan9-artifacts}}"
LINUX_BUILD_DIR="$BUILD_DIR/linux-minimal-corpus"
PASS_ONE="$BUILD_DIR/pass-one"
PASS_TWO="$BUILD_DIR/pass-two"
IMAGE_DIR_ONE="$BUILD_DIR/images"
IMAGE_DIR_TWO="$BUILD_DIR/rebuild-images"
mkdir -p \
  "$LINUX_BUILD_DIR" \
  "$PASS_ONE" \
  "$PASS_TWO" \
  "$IMAGE_DIR_ONE" \
  "$IMAGE_DIR_TWO"

EXPECTED_GUEST_BYTES=8520
EXPECTED_GUEST_SHA256="06240b6a840ed4262835aceff64a94f6ebd77838666f05eb7415d9a0d1b5868d"

for tool in cargo clang file nm objdump python3 rustc shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for M7 Linux/Plan 9 artifact construction" >&2
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

# Admit the same independently rebuilt and pinned Linux ELF used by Mode 25.
# The corpus builder double-builds it, checks the source/oracle identity, and
# rejects every negative mutant before it can enter the M7 root image.
OCORE_LINUX_BUILD_DIR="$LINUX_BUILD_DIR" \
  "$KERNEL_DIR/build-linux-minimal-corpus.sh"
GUEST_ELF="$LINUX_BUILD_DIR/pass-one/linux-minimal.elf"
if [[ ! -f "$GUEST_ELF" ]]; then
  echo "error: strict minimal Linux build did not produce its pinned ELF" >&2
  exit 1
fi
GUEST_BYTES="$(wc -c < "$GUEST_ELF" | tr -d ' ')"
GUEST_SHA256="$(shasum -a 256 "$GUEST_ELF" | awk '{print $1}')"
if [[ "$GUEST_BYTES" != "$EXPECTED_GUEST_BYTES" \
    || "$GUEST_SHA256" != "$EXPECTED_GUEST_SHA256" ]]; then
  echo "error: pinned minimal Linux guest identity drift" >&2
  exit 1
fi
python3 "$USER_DIR/verify_linux_minimal_corpus.py" \
  "$GUEST_ELF" \
  "$USER_DIR/linux-minimal-oracle.json"

DAEMON_SOURCE="$RUNTIME_DIR/m7_linux_9pd.oc"
SUPERVISOR_SOURCE="$RUNTIME_DIR/m7_linux_supervisord.oc"
CLIENT_SOURCE="$RUNTIME_DIR/m7_plan9_client.oc"
ORACLE="$USER_DIR/test_9p2000_linux_namespace.py"
for source in \
  "$DAEMON_SOURCE" \
  "$SUPERVISOR_SOURCE" \
  "$CLIENT_SOURCE" \
  "$ORACLE"; do
  if [[ ! -f "$source" ]]; then
    echo "error: required M7 source/oracle is missing: $source" >&2
    exit 1
  fi
done

# The independent wire oracle is a required admission gate, not a smoke-only
# diagnostic. Any malformed frame, fid/generation drift, or corpus mismatch
# must stop artifact construction.
python3 "$ORACLE"

cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --package o-lang --bin ocorec
OCOREC="$ROOT/target/debug/ocorec"
LLD="$(find_lld)"

source_for_principal() {
  case "$1" in
    linux-9pd)
      printf '%s\n' "$DAEMON_SOURCE"
      ;;
    linux-supervisord)
      printf '%s\n' "$SUPERVISOR_SOURCE"
      ;;
    plan9-namespace-client)
      printf '%s\n' "$CLIENT_SOURCE"
      ;;
    *)
      echo "error: unknown M7 principal: $1" >&2
      return 1
      ;;
  esac
}

link_principal() {
  local pass_dir="$1"
  local principal="$2"
  local source
  source="$(source_for_principal "$principal")"
  "$OCOREC" \
    "$source" \
    --target x86_64-unknown-none \
    --emit obj \
    -o "$pass_dir/${principal}.o"
  if [[ "$(nm "$pass_dir/${principal}.o" | awk '$2 == "T" && $3 == "_start" { count += 1 } END { print count + 0 }')" != 1 ]]; then
    echo "error: ${principal}.o does not export exactly one _start" >&2
    exit 1
  fi
  case "$(basename "$LLD")" in
    rust-lld | lld)
      "$LLD" -flavor gnu -m elf_x86_64 -nostdlib --build-id=none \
        --strip-all -z max-page-size=0x1000 \
        -T "$USER_DIR/static-user.ld" \
        -o "$pass_dir/${principal}.elf" "$pass_dir/${principal}.o"
      ;;
    *)
      "$LLD" -m elf_x86_64 -nostdlib --build-id=none --strip-all \
        -z max-page-size=0x1000 -T "$USER_DIR/static-user.ld" \
        -o "$pass_dir/${principal}.elf" "$pass_dir/${principal}.o"
      ;;
  esac
}

build_pass() {
  local pass_dir="$1"
  local principal
  for principal in \
    linux-9pd \
    linux-supervisord \
    plan9-namespace-client; do
    link_principal "$pass_dir" "$principal"
  done
}

check_principal_elf() {
  local elf="$1"
  local principal="$2"
  local file_headers
  local program_headers
  local section_headers
  local entry_address
  local flags
  local load_count
  local text_vma
  local rodata_vma
  local marker
  local expected_load_count
  local expected_flags
  file_headers="$(objdump -f "$elf")"
  program_headers="$(objdump -p "$elf")"
  section_headers="$(objdump -h "$elf")"
  entry_address="$(printf '%s\n' "$file_headers" | awk '$1 == "start" && ($2 == "address" || $2 == "address:") { print $NF }')"
  flags="$(printf '%s\n' "$program_headers" | awk '/ flags / { print $NF }')"
  load_count="$(grep -Ec '^    LOAD ' <<<"$program_headers")"
  text_vma="$(printf '%s\n' "$section_headers" | awk '$2 == ".text" { print $4 }')"
  rodata_vma="$(printf '%s\n' "$section_headers" | awk '$2 == ".rodata" { print $4 }')"
  case "$principal" in
    linux-9pd)
      marker='M7 Linux 9P daemon g1: online'
      expected_load_count=3
      expected_flags=$'r-x\nr--\nrw-\nrw-'
      ;;
    linux-supervisord)
      marker='M7 Linux supervisor policy loop: PASS'
      expected_load_count=1
      expected_flags=$'r-x\nrw-'
      ;;
    plan9-namespace-client)
      marker='M7 Plan 9 client ELF: online'
      expected_load_count=2
      expected_flags=$'r-x\nr--\nrw-'
      ;;
    *)
      echo "error: unknown M7 principal profile: $principal" >&2
      exit 1
      ;;
  esac
  if ! grep -Fq 'file format elf64-x86-64' <<<"$file_headers" \
      || ! grep -Eq 'architecture: (x86_64|i386:x86-64)' <<<"$file_headers" \
      || [[ "$entry_address" != 0x0000000002000000 ]] \
      || [[ "$load_count" != "$expected_load_count" ]] \
      || [[ "$(grep -Ec '^   STACK ' <<<"$program_headers")" != 1 ]] \
      || [[ "$flags" != "$expected_flags" ]] \
      || grep -Eq 'flags [^[:space:]]*w[^[:space:]]*x|flags [^[:space:]]*x[^[:space:]]*w' <<<"$program_headers" \
      || grep -Eq 'NEEDED|RPATH|RUNPATH|INTERP' <<<"$program_headers" \
      || [[ "$text_vma" != 0000000002000000 ]] \
      || [[ "$(objdump -d "$elf" | grep -Ec '[[:space:]]syscall([[:space:]]|$)')" == 0 ]] \
      || ! grep -aFq "$marker" "$elf"; then
    echo "error: noncanonical M7 Linux/Plan 9 static ELF: $elf" >&2
    printf '%s\n%s\n%s\n' \
      "$file_headers" "$program_headers" "$section_headers" >&2
    exit 1
  fi
  if [[ "$principal" == linux-supervisord ]]; then
    if [[ -n "$rodata_vma" ]]; then
      echo "error: M7 supervisor must retain its RX-only payload profile" >&2
      exit 1
    fi
  elif [[ ! "$rodata_vma" =~ ^00000000020[0-9a-f]{5}$ ]]; then
    echo "error: M7 daemon/client must carry a bounded read-only marker segment" >&2
    exit 1
  fi
}

pack_image() {
  local pass_dir="$1"
  local image_dir="$2"
  python3 "$USER_DIR/pack_ovfs.py" "$image_dir" \
    --max-image-bytes 98304 \
    --entry /bin/linux-minimal.elf "$GUEST_ELF" 3 \
    --entry /sbin/linux-9pd.elf "$pass_dir/linux-9pd.elf" 3 \
    --entry /sbin/linux-supervisord.elf "$pass_dir/linux-supervisord.elf" 3 \
    --entry /bin/plan9-namespace-client.elf "$pass_dir/plan9-namespace-client.elf" 3
}

verify_image() {
  local image="$1"
  local verification
  local actual
  local expected
  verification="$(
    python3 "$USER_DIR/verify_ovfs.py" \
      --max-image-bytes 98304 "$image"
  )"
  actual="$(printf '%s\n' "$verification" | sed -n '/^\/bin\//p; /^\/sbin\//p')"
  expected=$'/bin/linux-minimal.elf: valid\n/bin/plan9-namespace-client.elf: valid\n/sbin/linux-9pd.elf: valid\n/sbin/linux-supervisord.elf: valid'
  if ! grep -Eq '^OVFSIMG1 verified: 4 files, [0-9]+ bytes, sha256=[0-9a-f]{64}$' <<<"$verification" \
      || [[ "$actual" != "$expected" ]]; then
    echo "error: M7 Linux/Plan 9 OVFS does not contain the exact four-principal corpus" >&2
    printf '%s\n' "$verification" >&2
    exit 1
  fi
}

build_pass "$PASS_ONE"
build_pass "$PASS_TWO"

for principal in \
  linux-9pd \
  linux-supervisord \
  plan9-namespace-client; do
  check_principal_elf "$PASS_ONE/${principal}.elf" "$principal"
  check_principal_elf "$PASS_TWO/${principal}.elf" "$principal"
  if ! cmp -s "$PASS_ONE/${principal}.o" "$PASS_TWO/${principal}.o" \
      || ! cmp -s "$PASS_ONE/${principal}.elf" "$PASS_TWO/${principal}.elf"; then
    echo "error: ${principal} object/ELF rebuild was not deterministic" >&2
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
  echo "error: M7 Linux/Plan 9 OVFSIMG1 rebuild was not deterministic" >&2
  exit 1
fi

file \
  "$GUEST_ELF" \
  "$PASS_ONE/linux-9pd.elf" \
  "$PASS_ONE/linux-supervisord.elf" \
  "$PASS_ONE/plan9-namespace-client.elf" \
  "$IMAGE_ONE"
printf 'M7 Linux/Plan 9 artifact build: PASS\n'
printf 'artifact: %s bytes=%s sha256=%s\n' \
  "$GUEST_ELF" \
  "$GUEST_BYTES" \
  "$GUEST_SHA256"
for principal in \
  linux-9pd \
  linux-supervisord \
  plan9-namespace-client; do
  artifact="$PASS_ONE/${principal}.elf"
  printf 'artifact: %s bytes=%s sha256=%s\n' \
    "$artifact" \
    "$(wc -c < "$artifact" | tr -d ' ')" \
    "$(shasum -a 256 "$artifact" | awk '{print $1}')"
done
printf 'image-bytes: %s\nimage: %s\nsha256: %s\n' \
  "$(wc -c < "$IMAGE_ONE" | tr -d ' ')" \
  "$IMAGE_ONE" \
  "$DIGEST_ONE"
