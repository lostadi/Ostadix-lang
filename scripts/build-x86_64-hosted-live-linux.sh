#!/usr/bin/env bash
# Linux worker for the repository-owned hosted-live release pipeline.
set -euo pipefail
umask 077

# Multipass exec is non-login: rustup installs here but is otherwise absent
# from PATH. Keep every tool lookup explicit and independent of shell startup.
export PATH="/home/ubuntu/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

RUST_TOOLCHAIN=1.97.1
RUST_TARGET=x86_64-unknown-linux-musl
WASM_TARGET=wasm32-wasip1
SOURCE_DATE_EPOCH=315532800
ALPINE_MINIROOTFS_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/alpine-minirootfs-3.24.1-x86_64.tar.gz
ALPINE_MINIROOTFS_BYTES=3698422
ALPINE_MINIROOTFS_SHA256=41f73e3cf5fa919b8aa5ca6b30dc48f0da2720776d7423e2a7748211456fe081
ALPINE_LTS_KERNEL_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot-3.24.1/vmlinuz-lts
ALPINE_LTS_KERNEL_BYTES=14468096
ALPINE_LTS_KERNEL_SHA256=77007123c0591ab4b2a5434ffa1b6a3985b3037d534be78bccfb30f3c9536c54
ALPINE_LTS_INITRAMFS_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot-3.24.1/initramfs-lts
ALPINE_LTS_INITRAMFS_BYTES=27951899
ALPINE_LTS_INITRAMFS_SHA256=e1649e94ef1b276bf22ea4ed2628dd17c7fa7505cd40b2c7aa7fd9ebb71fe5c9
ALPINE_LTS_MODLOOP_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot-3.24.1/modloop-lts
ALPINE_LTS_MODLOOP_BYTES=303034368
ALPINE_LTS_MODLOOP_SHA256=871ef51ed6378283db9462947bb7fb84c1ec31376611eb1a2281b02b9404c0f6
MUSL_DEV_VERSION=1.2.6-r2
HOSTED_STANDARD_BINARIES=(
  O o-cli olangc ocorec o-link o-unlink ogit o-live-host o-node octl
  o-registry o-info ostadix-device
)
HOSTED_ROOT_BINARIES=(
  O o-cli olangc ocorec o-link o-unlink o-notebook ogit o-live-host o-node
  octl o-registry o-info ostadix-device ocore-kernel-world-record
)
HOSTED_BINARIES=("${HOSTED_ROOT_BINARIES[@]}" ostadix-mcp)

trap 'exit 130' INT
trap 'exit 143' TERM

SYSROOT_PACKAGE_SPECS=(
  alpine-baselayout=3.7.2-r1
  alpine-baselayout-data=3.7.2-r1
  alpine-keys=2.6-r0
  alpine-release=3.24.1-r0
  apk-tools=3.0.6-r0
  busybox=1.37.0-r31
  busybox-binsh=1.37.0-r31
  ca-certificates-bundle=20260611-r0
  libapk=3.0.6-r0
  libcrypto3=3.5.7-r0
  libssl3=3.5.7-r0
  musl=1.2.6-r2
  musl-dev=1.2.6-r2
  musl-utils=1.2.6-r2
  scanelf=1.3.9-r1
  ssl_client=1.37.0-r31
  zlib=1.3.2-r0
)

die() {
  printf 'hosted-live-linux-build: ERROR: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'USAGE'
Usage: build-x86_64-hosted-live-linux.sh SOURCE OUTPUT RECEIPT SOURCE_TREE BASE_COMMIT

Internal Linux worker. SOURCE must be an archived staged-index snapshot below
/home/ubuntu/.cache/ostadix/hosted-live-release/runs, never the host-mounted
checkout. OUTPUT and RECEIPT must be new files inside the same private run.
USAGE
}

if [[ $# -ne 5 ]]; then
  usage >&2
  exit 2
fi
[[ $(uname -s) == Linux ]] || die "this worker requires Linux"
[[ $(id -u) -ne 0 ]] || die "run the worker as the Multipass user, not root"

SOURCE_ROOT=$(realpath -e -- "$1")
OUTPUT=$(realpath -m -- "$2")
RECEIPT=$(realpath -m -- "$3")
SOURCE_TREE=$4
BASE_COMMIT=$5
RUN_ROOT=$(dirname -- "$SOURCE_ROOT")
ARCHIVE_SHA256=${OSTADIX_HOSTED_LIVE_ARCHIVE_SHA256:-}
BOOT_OBJECTS_ARCHIVE=${OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE:-}
BOOT_OBJECTS_ARCHIVE_SHA256=${OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE_SHA256:-}
SHARED_CACHE=/home/ubuntu/.cache/ostadix/hosted-live-release/shared
CAPACITY_HOST_CACHE=/home/ubuntu/.cache/ostadix/capacity-host
GUEST_ROOT=/home/ubuntu/.local/share/ostadix/guests

case "$SOURCE_ROOT" in
  /home/ubuntu/.cache/ostadix/hosted-live-release/runs/*/source) ;;
  *) die "source snapshot escaped the guest-owned release root: $SOURCE_ROOT" ;;
esac
case "$OUTPUT" in "$RUN_ROOT"/*) ;; *) die "output escaped the private run root" ;; esac
case "$RECEIPT" in "$RUN_ROOT"/*) ;; *) die "receipt escaped the private run root" ;; esac
[[ ! -e "$OUTPUT" && ! -L "$OUTPUT" ]] || die "refusing to clobber output: $OUTPUT"
[[ ! -e "$RECEIPT" && ! -L "$RECEIPT" ]] || die "refusing to clobber receipt: $RECEIPT"
[[ "$SOURCE_TREE" =~ ^[0-9a-f]{40}$ ]] || die "SOURCE_TREE must be a 40-character Git tree OID"
[[ "$BASE_COMMIT" =~ ^[0-9a-f]{40}$ ]] || die "BASE_COMMIT must be a 40-character Git commit OID"
[[ "$ARCHIVE_SHA256" =~ ^[0-9a-f]{64}$ ]] || die "OSTADIX_HOSTED_LIVE_ARCHIVE_SHA256 is required"
[[ "$BOOT_OBJECTS_ARCHIVE_SHA256" =~ ^[0-9a-f]{64}$ ]] \
  || die "OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE_SHA256 is required"
[[ -n "$BOOT_OBJECTS_ARCHIVE" ]] \
  || die "OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE is required"
BOOT_OBJECTS_ARCHIVE=$(realpath -e -- "$BOOT_OBJECTS_ARCHIVE")
case "$BOOT_OBJECTS_ARCHIVE" in
  "$RUN_ROOT"/*) ;;
  *) die "boot-object archive escaped the private run root" ;;
esac
[[ -f "$BOOT_OBJECTS_ARCHIVE" && ! -L "$BOOT_OBJECTS_ARCHIVE" ]] \
  || die "boot-object archive is not a regular non-symlink file"
[[ $(sha256sum "$BOOT_OBJECTS_ARCHIVE" | awk '{print $1}') == "$BOOT_OBJECTS_ARCHIVE_SHA256" ]] \
  || die "boot-object archive failed its transferred SHA-256 binding"
for cache in "$SHARED_CACHE" "$CAPACITY_HOST_CACHE" "$GUEST_ROOT"; do
  [[ ! -L "$cache" ]] || die "release cache must not be a symlink: $cache"
  case "$(realpath -m -- "$cache")" in
    /home/ubuntu/*) ;;
    *) die "release cache escaped the native guest home: $cache" ;;
  esac
done
mkdir -p -- "$SHARED_CACHE" "$CAPACITY_HOST_CACHE" "$GUEST_ROOT" "$RUN_ROOT/output"

for required in \
  Cargo.lock Cargo.toml rust-toolchain.toml backends crates/ostadix-api examples src \
  evidence/foreign_kernel_lab.toml evidence/hosted_live_apk_packages.txt \
  evidence/hosted_live_physical_iso.toml evidence/hosted_live_workstation_apk_packages.txt \
  mcp/ostadix_lang_mcp_server/Cargo.lock mcp/ostadix_lang_mcp_server/Cargo.toml \
  mcp/ostadix_lang_mcp_server/src \
  scripts/ostadix-hosted-live-desktop.sh scripts/ostadix_boot_objects.py \
  scripts/foreign_kernel_lab.py scripts/ostadix_wasm_release.py \
  scripts/prepare-x86_64-capacity-host.sh \
  ocore/kernel/build.sh \
  ocore/kernel/build-x86_64-hosted-live-iso.sh \
  ocore/kernel/resolve-x86_64-ovmf-code.sh \
  ocore/kernel/smoke-x86_64-hosted-live-qemu.py \
  ocore/kernel/smoke-x86_64-hosted-live-ocore-qemu.py \
  ocore/kernel/smoke-x86_64-hosted-live-vga-qemu.py; do
  [[ -e "$SOURCE_ROOT/$required" && ! -L "$SOURCE_ROOT/$required" ]] \
    || die "staged source snapshot is missing required path: $required"
done

for tool in \
  cargo chroot clang cmp cpio curl file find flock grep gzip grub-mkrescue install ld.lld \
  llvm-ar python3 qemu-system-x86_64 realpath rustup sha256sum sudo \
  mksquashfs tar tee unsquashfs xorriso zstd; do
  command -v "$tool" >/dev/null 2>&1 || die "required Linux build tool is unavailable: $tool"
done
sudo -n true >/dev/null 2>&1 || die "passwordless sudo is required inside the Multipass guest"
SOURCE_SYMLINK=$(find "$SOURCE_ROOT" -type l -print -quit)
[[ -z "$SOURCE_SYMLINK" ]] \
  || die "staged source snapshot contains a forbidden symlink: $SOURCE_SYMLINK"

HOST_ARCH=$(uname -m)
case "$HOST_ARCH" in
  x86_64) ;;
  aarch64|arm64)
    [[ -r /proc/sys/fs/binfmt_misc/qemu-x86_64 ]] \
      || die "AArch64 host requires registered qemu-x86_64 binfmt support"
    grep -qx enabled /proc/sys/fs/binfmt_misc/qemu-x86_64 \
      || die "qemu-x86_64 binfmt registration is present but not enabled"
    ;;
  *) die "unsupported Linux build architecture: $HOST_ARCH" ;;
esac

exec 9<"$SHARED_CACHE"
flock 9
DOWNLOAD_ROOT="$SHARED_CACHE/downloads"
SYSROOT="$RUN_ROOT/sysroot-alpine-3.24.1-x86_64-musl-$MUSL_DEV_VERSION"
mkdir -p -- "$DOWNLOAD_ROOT"
MINIROOTFS="$DOWNLOAD_ROOT/alpine-minirootfs-3.24.1-x86_64.tar.gz"
LTS_KERNEL="$DOWNLOAD_ROOT/vmlinuz-lts-3.24.1-x86_64"
LTS_INITRAMFS="$DOWNLOAD_ROOT/initramfs-lts-3.24.1-x86_64"
LTS_MODLOOP="$DOWNLOAD_ROOT/modloop-lts-3.24.1-x86_64"

verify_file() {
  local path=$1 expected_bytes=$2 expected_sha=$3 actual_bytes actual_sha
  [[ -f "$path" && ! -L "$path" ]] || return 1
  actual_bytes=$(wc -c <"$path" | tr -d ' ')
  actual_sha=$(sha256sum "$path" | awk '{print $1}')
  [[ "$actual_bytes" == "$expected_bytes" && "$actual_sha" == "$expected_sha" ]]
}

fetch_pinned() {
  local path=$1 url=$2 expected_bytes=$3 expected_sha=$4 label=$5 partial
  if verify_file "$path" "$expected_bytes" "$expected_sha"; then
    return
  fi
  partial="$DOWNLOAD_ROOT/.$label.$$.partial"
  rm -f -- "$partial"
  curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
    --output "$partial" "$url"
  verify_file "$partial" "$expected_bytes" "$expected_sha" \
    || die "pinned $label failed exact size/SHA-256 verification"
  chmod 0444 "$partial"
  mv -f -- "$partial" "$path"
}

fetch_pinned "$MINIROOTFS" "$ALPINE_MINIROOTFS_URL" \
  "$ALPINE_MINIROOTFS_BYTES" "$ALPINE_MINIROOTFS_SHA256" alpine-minirootfs
fetch_pinned "$LTS_KERNEL" "$ALPINE_LTS_KERNEL_URL" \
  "$ALPINE_LTS_KERNEL_BYTES" "$ALPINE_LTS_KERNEL_SHA256" alpine-vmlinuz-lts
fetch_pinned "$LTS_INITRAMFS" "$ALPINE_LTS_INITRAMFS_URL" \
  "$ALPINE_LTS_INITRAMFS_BYTES" "$ALPINE_LTS_INITRAMFS_SHA256" alpine-initramfs-lts
fetch_pinned "$LTS_MODLOOP" "$ALPINE_LTS_MODLOOP_URL" \
  "$ALPINE_LTS_MODLOOP_BYTES" "$ALPINE_LTS_MODLOOP_SHA256" alpine-modloop-lts

EXPECTED_SYSROOT_LOCK=$(printf '%s\n' "${SYSROOT_PACKAGE_SPECS[@]}" | LC_ALL=C sort)
[[ ! -e "$SYSROOT" && ! -L "$SYSROOT" ]] \
  || die "private per-run sysroot already exists: $SYSROOT"
mkdir -m 0700 "$SYSROOT"
sudo tar -xzf "$MINIROOTFS" -C "$SYSROOT"
printf '%s\n%s\n' \
  'https://dl-cdn.alpinelinux.org/alpine/v3.24/main' \
  'https://dl-cdn.alpinelinux.org/alpine/v3.24/community' \
  | sudo tee "$SYSROOT/etc/apk/repositories" >/dev/null
sudo cp --remove-destination /etc/resolv.conf "$SYSROOT/etc/resolv.conf"
sudo chroot "$SYSROOT" /sbin/apk --no-cache add "${SYSROOT_PACKAGE_SPECS[@]}"
sudo chroot "$SYSROOT" /sbin/apk info -e "musl-dev=$MUSL_DEV_VERSION" >/dev/null \
  || die "sysroot did not resolve the exact musl-dev version"
INSTALLED_SYSROOT_LOCK=$(python3 - "$SYSROOT/lib/apk/db/installed" <<'PY'
from pathlib import Path
import sys

records = Path(sys.argv[1]).read_text(encoding="utf-8").strip().split("\n\n")
values = []
for record in records:
    fields = {}
    for line in record.splitlines():
        if len(line) >= 3 and line[1] == ":":
            fields[line[0]] = line[2:]
    if "P" in fields and "V" in fields:
        values.append(f"{fields['P']}={fields['V']}")
print("\n".join(sorted(values)))
PY
)
[[ "$INSTALLED_SYSROOT_LOCK" == "$EXPECTED_SYSROOT_LOCK" ]] \
  || die "resolved x86_64 musl sysroot closure differs from its exact lock"
printf '%s\n' "$EXPECTED_SYSROOT_LOCK" \
  | sudo tee "$SYSROOT/.ostadix-package-lock" >/dev/null
printf '# resolver removed after exact signed APK resolution\n' \
  | sudo tee "$SYSROOT/etc/resolv.conf" >/dev/null
[[ -f "$SYSROOT/usr/lib/libc.a" ]] || die "x86_64 musl sysroot omitted libc.a"

write_sysroot_manifest() {
  local root=$1 output=$2
  sudo python3 - "$root" >"$output" <<'PY'
from pathlib import Path
import hashlib
import json
import os
import stat
import sys

root = Path(sys.argv[1])
entries = []

def visit(path: Path, relative: str) -> None:
    state = path.lstat()
    record = {
        "gid": state.st_gid,
        "mode": stat.S_IMODE(state.st_mode),
        "path": relative,
        "uid": state.st_uid,
    }
    if stat.S_ISDIR(state.st_mode):
        record["type"] = "directory"
        entries.append(record)
        for name in sorted(os.listdir(path)):
            visit(path / name, name if relative == "." else f"{relative}/{name}")
    elif stat.S_ISREG(state.st_mode):
        digest = hashlib.sha256()
        size = 0
        with path.open("rb") as stream:
            while chunk := stream.read(1024 * 1024):
                digest.update(chunk)
                size += len(chunk)
        record.update(type="file", bytes=size, sha256=digest.hexdigest())
        entries.append(record)
    elif stat.S_ISLNK(state.st_mode):
        record.update(type="symlink", target=os.readlink(path))
        entries.append(record)
    else:
        record.update(type="special", rdev=state.st_rdev)
        entries.append(record)

visit(root, ".")
payload = {"schema": "ostadix.sysroot-manifest/v1", "entries": entries}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

SYSROOT_MANIFEST="$RUN_ROOT/output/sysroot-manifest.json"
write_sysroot_manifest "$SYSROOT" "$SYSROOT_MANIFEST"

if ! rustup run "$RUST_TOOLCHAIN" rustc --version >/dev/null 2>&1; then
  rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal
fi
RUST_VERSION=$(rustup run "$RUST_TOOLCHAIN" rustc --version)
[[ "$RUST_VERSION" == "rustc $RUST_TOOLCHAIN "* ]] \
  || die "release toolchain mismatch: $RUST_VERSION"
rustup target add --toolchain "$RUST_TOOLCHAIN" "$RUST_TARGET"
rustup target add --toolchain "$RUST_TOOLCHAIN" "$WASM_TARGET"

TOOL_ROOT="$RUN_ROOT/tooling"
TARGET_ROOT="$RUN_ROOT/cargo-x86_64-musl"
MCP_TARGET_ROOT="$RUN_ROOT/cargo-ostadix-mcp-x86_64-musl"
HOSTED_BIN_DIR="$RUN_ROOT/hosted-bin"
CARGO_VENDOR_DIR="$RUN_ROOT/cargo-vendor"
CARGO_VENDOR_MANIFEST="$RUN_ROOT/output/cargo-vendor-manifest.json"
CARGO_BUILD_HOME="$RUN_ROOT/cargo-home"
[[ ! -e "$CARGO_VENDOR_DIR" && ! -L "$CARGO_VENDOR_DIR" ]] \
  || die "private Cargo vendor destination already exists"
mkdir -p -- \
  "$TOOL_ROOT" "$TARGET_ROOT" "$MCP_TARGET_ROOT" "$HOSTED_BIN_DIR" \
  "$CARGO_BUILD_HOME"
rustup run "$RUST_TOOLCHAIN" cargo vendor \
  --locked --versioned-dirs \
  --manifest-path "$SOURCE_ROOT/Cargo.toml" \
  --sync "$SOURCE_ROOT/mcp/ostadix_lang_mcp_server/Cargo.toml" \
  "$CARGO_VENDOR_DIR" >/dev/null
python3 - \
  "$CARGO_VENDOR_DIR" "$CARGO_VENDOR_MANIFEST" \
  "$SOURCE_ROOT/Cargo.lock" \
  "$SOURCE_ROOT/mcp/ostadix_lang_mcp_server/Cargo.lock" <<'PY'
from pathlib import Path
import hashlib
import json
import os
import stat
import sys

vendor, output, root_lock, mcp_lock = map(Path, sys.argv[1:])

def identity(path: Path) -> dict[str, object]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
    return {"bytes": size, "sha256": digest.hexdigest()}

package_directories = []
for path in sorted(vendor.iterdir(), key=lambda item: item.name):
    state = path.lstat()
    if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
        raise SystemExit(f"error: Cargo vendor root contains a non-directory: {path}")
    if not (path / ".cargo-checksum.json").is_file():
        raise SystemExit(f"error: vendored package omitted .cargo-checksum.json: {path}")
    package_directories.append(path)
if not package_directories:
    raise SystemExit("error: Cargo vendor closure is empty")

files = []
for root, directory_names, file_names in os.walk(vendor, topdown=True, followlinks=False):
    directory_names.sort()
    file_names.sort()
    root_path = Path(root)
    for name in directory_names:
        path = root_path / name
        state = path.lstat()
        if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
            raise SystemExit(f"error: Cargo vendor closure contains an unsafe directory: {path}")
    for name in file_names:
        path = root_path / name
        state = path.lstat()
        if stat.S_ISLNK(state.st_mode) or not stat.S_ISREG(state.st_mode):
            raise SystemExit(f"error: Cargo vendor closure contains an unsafe file: {path}")
        record = identity(path)
        record["path"] = path.relative_to(vendor).as_posix()
        files.append(record)
files.sort(key=lambda record: record["path"])
payload = {
    "schema": "ostadix.cargo-vendor-manifest/v1",
    "locks": {
        "root": {"path": "Cargo.lock", **identity(root_lock)},
        "mcp": {
            "path": "mcp/ostadix_lang_mcp_server/Cargo.lock",
            **identity(mcp_lock),
        },
    },
    "package_count": len(package_directories),
    "file_count": len(files),
    "total_bytes": sum(record["bytes"] for record in files),
    "files": files,
}
encoded = (json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n").encode()
descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o444)
try:
    with os.fdopen(descriptor, "wb", closefd=False) as stream:
        stream.write(encoded)
        stream.flush()
        os.fsync(stream.fileno())
finally:
    os.close(descriptor)
PY
cat >"$CARGO_BUILD_HOME/config.toml" <<EOF
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "$CARGO_VENDOR_DIR"

[net]
offline = true
EOF
chmod 0444 "$CARGO_BUILD_HOME/config.toml"
LINKER="$TOOL_ROOT/x86_64-alpine-linux-musl-clang"
cat >"$LINKER" <<EOF
#!/bin/sh
exec $(command -v clang) --target=x86_64-alpine-linux-musl --sysroot="$SYSROOT" -fuse-ld=lld "\$@"
EOF
chmod 0555 "$LINKER"
LLVM_AR=$(command -v llvm-ar)
CARGO_BIN_ARGS=()
for binary in "${HOSTED_ROOT_BINARIES[@]}"; do
  CARGO_BIN_ARGS+=(--bin "$binary")
done

env \
  AR_x86_64_unknown_linux_musl="$LLVM_AR" \
  CC_x86_64_unknown_linux_musl="$LINKER" \
  CARGO_HOME="$CARGO_BUILD_HOME" \
  CARGO_BUILD_JOBS=1 \
  CARGO_INCREMENTAL=0 \
  CARGO_NET_OFFLINE=true \
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  CARGO_PROFILE_RELEASE_LTO=false \
  CARGO_TARGET_DIR="$TARGET_ROOT" \
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$LINKER" \
  RUSTFLAGS="--remap-path-prefix=$SOURCE_ROOT=/usr/src/ostadix -C target-feature=+crt-static" \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  rustup run "$RUST_TOOLCHAIN" cargo build \
    --manifest-path "$SOURCE_ROOT/Cargo.toml" \
    --locked --release --target "$RUST_TARGET" --package o-lang \
    --features notebook \
    "${CARGO_BIN_ARGS[@]}"

env \
  AR_x86_64_unknown_linux_musl="$LLVM_AR" \
  CC_x86_64_unknown_linux_musl="$LINKER" \
  CARGO_HOME="$CARGO_BUILD_HOME" \
  CARGO_BUILD_JOBS=1 \
  CARGO_INCREMENTAL=0 \
  CARGO_NET_OFFLINE=true \
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  CARGO_PROFILE_RELEASE_LTO=false \
  CARGO_TARGET_DIR="$MCP_TARGET_ROOT" \
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$LINKER" \
  RUSTFLAGS="--remap-path-prefix=$SOURCE_ROOT=/usr/src/ostadix -C target-feature=+crt-static" \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  rustup run "$RUST_TOOLCHAIN" cargo build \
    --manifest-path "$SOURCE_ROOT/mcp/ostadix_lang_mcp_server/Cargo.toml" \
    --locked --release --target "$RUST_TARGET" --package ostadix-mcp-server \
    --bin ostadix-mcp

SYSROOT_MANIFEST_AFTER="$RUN_ROOT/output/sysroot-manifest-after-build.json"
write_sysroot_manifest "$SYSROOT" "$SYSROOT_MANIFEST_AFTER"
cmp -s "$SYSROOT_MANIFEST" "$SYSROOT_MANIFEST_AFTER" \
  || die "x86_64 musl sysroot changed after its admitted content manifest"
rm -f -- "$SYSROOT_MANIFEST_AFTER"

for binary in "${HOSTED_BINARIES[@]}"; do
  if [[ "$binary" == ostadix-mcp ]]; then
    source_binary="$MCP_TARGET_ROOT/$RUST_TARGET/release/$binary"
  else
    source_binary="$TARGET_ROOT/$RUST_TARGET/release/$binary"
  fi
  [[ -f "$source_binary" && -x "$source_binary" ]] \
    || die "static build omitted hosted binary: $binary"
  install -m 0555 "$source_binary" "$HOSTED_BIN_DIR/$binary"
done

# Materialize the exact Olangc-generated Cargo project with the admitted x86_64
# compiler, then compile that project once with the native Multipass CPU. The
# boot gate regenerates and hashes the same project, verifies this receipt-bound
# module, and separately compiles a tiny WASI probe. It never substitutes a
# 25-minute nested-TCG cold build for ordinary workstation startup.
WASM_PROJECT="$RUN_ROOT/olangc-wasm-project"
WASM_TARGET_ROOT="$RUN_ROOT/cargo-olangc-wasm"
WASM_RELEASE_ROOT="$RUN_ROOT/olangc-wasm-release"
WASM_ARTIFACT="$WASM_RELEASE_ROOT/hello.wasm"
WASM_MANIFEST="$RUN_ROOT/output/olangc-wasm-release.json"
for path in \
  "$WASM_PROJECT" "$WASM_TARGET_ROOT" "$WASM_RELEASE_ROOT" "$WASM_MANIFEST"; do
  [[ ! -e "$path" && ! -L "$path" ]] \
    || die "refusing stale Olangc WASM release path: $path"
done
mkdir -m 0700 "$WASM_RELEASE_ROOT"
"$HOSTED_BIN_DIR/olangc" \
  "$SOURCE_ROOT/examples/wasm_hello.O" \
  --target wasm \
  --output "$WASM_ARTIFACT" \
  --shim-dir "$SOURCE_ROOT/backends" \
  --materialize-only "$WASM_PROJECT"
[[ -d "$WASM_PROJECT" && ! -L "$WASM_PROJECT" \
    && ! -e "$WASM_PROJECT/target" && ! -e "$WASM_ARTIFACT" ]] \
  || die "Olangc materialization produced an unsafe project or an unexpected artifact"
env \
  CARGO_HOME="$CARGO_BUILD_HOME" \
  CARGO_BUILD_JOBS=1 \
  CARGO_INCREMENTAL=0 \
  CARGO_NET_OFFLINE=true \
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  CARGO_PROFILE_RELEASE_LTO=false \
  CARGO_PROFILE_RELEASE_OPT_LEVEL=1 \
  CARGO_TARGET_DIR="$WASM_TARGET_ROOT" \
  RUSTFLAGS="--remap-path-prefix=$WASM_PROJECT=/usr/share/ostadix/wasm/project" \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  rustup run "$RUST_TOOLCHAIN" cargo build \
    --manifest-path "$WASM_PROJECT/Cargo.toml" \
    --locked --release --target "$WASM_TARGET"
WASM_BUILT_ARTIFACT="$WASM_TARGET_ROOT/$WASM_TARGET/release/hello.wasm"
[[ -f "$WASM_BUILT_ARTIFACT" && ! -L "$WASM_BUILT_ARTIFACT" \
    && -s "$WASM_BUILT_ARTIFACT" ]] \
  || die "native release build omitted the materialized Olangc WASM artifact"
install -m 0444 "$WASM_BUILT_ARTIFACT" "$WASM_ARTIFACT"
python3 "$SOURCE_ROOT/scripts/ostadix_wasm_release.py" create \
  --project "$WASM_PROJECT" \
  --artifact "$WASM_ARTIFACT" \
  --input "$SOURCE_ROOT/examples/wasm_hello.O" \
  --generator "$HOSTED_BIN_DIR/olangc" \
  --source-tree "$SOURCE_TREE" \
  --base-commit "$BASE_COMMIT" \
  --source-archive-sha256 "$ARCHIVE_SHA256" \
  --rust-toolchain "$RUST_VERSION" \
  --output "$WASM_MANIFEST" >/dev/null
python3 "$SOURCE_ROOT/scripts/ostadix_wasm_release.py" verify \
  --manifest "$WASM_MANIFEST" \
  --project "$WASM_PROJECT" \
  --artifact "$WASM_ARTIFACT" \
  --input "$SOURCE_ROOT/examples/wasm_hello.O" \
  --generator "$HOSTED_BIN_DIR/olangc" \
  --source-tree "$SOURCE_TREE" \
  --base-commit "$BASE_COMMIT" \
  --source-archive-sha256 "$ARCHIVE_SHA256" >/dev/null

GUEST_ARGUMENTS=(
  --guest linux-alpine-3.24.1-x86_64
  --guest guix-system-1.5.0-x86_64
  --guest plan9-9front-11983-amd64
  --guest redox-0.9.0-server-x86_64
  --guest openbsd-7.9-amd64
)
python3 "$SOURCE_ROOT/scripts/foreign_kernel_lab.py" --guest-dir "$GUEST_ROOT" \
  fetch "${GUEST_ARGUMENTS[@]}"
GUEST_CACHE_VERIFICATION="$RUN_ROOT/output/guest-cache-verification.txt"
python3 "$SOURCE_ROOT/scripts/foreign_kernel_lab.py" --guest-dir "$GUEST_ROOT" \
  verify "${GUEST_ARGUMENTS[@]}" | tee "$GUEST_CACHE_VERIFICATION"

OCORE_TARGET_ROOT="$RUN_ROOT/cargo-ocore-host"
OCORE_BUILD_ROOT="$RUN_ROOT/ocore-kernel"
OCORE_BUILD_LOG="$RUN_ROOT/output/ocore-build.log"
OCORE_LLD_PATH=$(command -v ld.lld)
for path in "$OCORE_TARGET_ROOT" "$OCORE_BUILD_ROOT" "$OCORE_BUILD_LOG"; do
  [[ ! -e "$path" && ! -L "$path" ]] \
    || die "refusing stale O-core release path: $path"
done
[[ "$OCORE_LLD_PATH" = /* && -x "$OCORE_LLD_PATH" ]] \
  || die "ld.lld did not resolve to an executable absolute path"
mkdir -m 0700 "$OCORE_TARGET_ROOT" "$OCORE_BUILD_ROOT"
env \
  CARGO_HOME="$CARGO_BUILD_HOME" \
  CARGO_BUILD_JOBS=1 \
  CARGO_INCREMENTAL=0 \
  CARGO_NET_OFFLINE=true \
  CARGO_TARGET_DIR="$OCORE_TARGET_ROOT" \
  OCORE_BOOT_INFO_ENABLED=1 \
  OCORE_BUILD_DIR="$OCORE_BUILD_ROOT" \
  OCORE_LLD="$OCORE_LLD_PATH" \
  OCORE_PROBE_MODE=0 \
  RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN" \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  "$SOURCE_ROOT/ocore/kernel/build.sh" >"$OCORE_BUILD_LOG" 2>&1
OCORE_KERNEL="$OCORE_BUILD_ROOT/kernel.elf"
[[ -f "$OCORE_KERNEL" && ! -L "$OCORE_KERNEL" && -s "$OCORE_KERNEL" ]] \
  || die "O-core build omitted its regular nonempty kernel ELF"

sudo install -d -m 0755 "$CAPACITY_HOST_CACHE"
if [[ ! -e "$CAPACITY_HOST_CACHE/alpine-minirootfs-3.24.1-x86_64.tar.gz" ]]; then
  sudo install -m 0444 "$MINIROOTFS" \
    "$CAPACITY_HOST_CACHE/alpine-minirootfs-3.24.1-x86_64.tar.gz"
fi
if [[ ! -e "$CAPACITY_HOST_CACHE/modloop-lts-3.24.1-x86_64" ]]; then
  sudo install -m 0444 "$LTS_MODLOOP" \
    "$CAPACITY_HOST_CACHE/modloop-lts-3.24.1-x86_64"
fi
INITRAMFS="$RUN_ROOT/output/initramfs.cpio.gz"
ROOTFS_IMAGE="$RUN_ROOT/output/rootfs.squashfs"
VENTOY_MODLOOP="$RUN_ROOT/output/modloop-lts"
BOOT_OBJECTS_RESULT="$RUN_ROOT/output/boot-objects-verify.json"
sudo env \
  OSTADIX_CAPACITY_HOST_BASE_INITRAMFS="$LTS_INITRAMFS" \
  OSTADIX_CAPACITY_HOST_CACHE="$CAPACITY_HOST_CACHE" \
  OSTADIX_CAPACITY_HOST_KERNEL_FLAVOR=lts \
  OSTADIX_CAPACITY_HOST_PACKAGE_LOCK="$SOURCE_ROOT/evidence/hosted_live_workstation_apk_packages.txt" \
  OSTADIX_CAPACITY_HOST_ROOTFS_OUTPUT="$ROOTFS_IMAGE" \
  OSTADIX_CAPACITY_HOST_VENTOY_MODLOOP_OUTPUT="$VENTOY_MODLOOP" \
  OSTADIX_HOSTED_BIN_DIR="$HOSTED_BIN_DIR" \
  OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE="$BOOT_OBJECTS_ARCHIVE" \
  OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE_SHA256="$BOOT_OBJECTS_ARCHIVE_SHA256" \
  OSTADIX_HOSTED_BOOT_OBJECTS_RESULT="$BOOT_OBJECTS_RESULT" \
  OSTADIX_HOSTED_CARGO_VENDOR_DIR="$CARGO_VENDOR_DIR" \
  OSTADIX_HOSTED_CARGO_VENDOR_MANIFEST="$CARGO_VENDOR_MANIFEST" \
  OSTADIX_HOSTED_WASM_ARTIFACT="$WASM_ARTIFACT" \
  OSTADIX_HOSTED_WASM_MANIFEST="$WASM_MANIFEST" \
  OSTADIX_HOSTED_WASM_PROJECT="$WASM_PROJECT" \
  OSTADIX_HOSTED_BASE_COMMIT="$BASE_COMMIT" \
  OSTADIX_HOSTED_REVISION="$SOURCE_TREE" \
  OSTADIX_HOSTED_SOURCE_ARCHIVE_SHA256="$ARCHIVE_SHA256" \
  OSTADIX_HOSTED_SOURCE_ROOT="$SOURCE_ROOT" \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  "$SOURCE_ROOT/scripts/prepare-x86_64-capacity-host.sh" "$INITRAMFS"

CAPACITY_HOST_INITRAMFS="$RUN_ROOT/output/capacity-host-initramfs.cpio.gz"
sudo env \
  OSTADIX_CAPACITY_HOST_BASE_INITRAMFS="$GUEST_ROOT/alpine-3.24.1-x86_64/initramfs-virt" \
  OSTADIX_CAPACITY_HOST_CACHE="$CAPACITY_HOST_CACHE" \
  OSTADIX_CAPACITY_HOST_KERNEL_FLAVOR=virt \
  OSTADIX_CAPACITY_HOST_PACKAGE_LOCK="$SOURCE_ROOT/evidence/hosted_live_apk_packages.txt" \
  OSTADIX_GUEST_ROOT="$GUEST_ROOT" \
  OSTADIX_HOSTED_BIN_DIR="$HOSTED_BIN_DIR" \
  OSTADIX_HOSTED_REVISION="$SOURCE_TREE" \
  OSTADIX_HOSTED_SOURCE_ROOT="$SOURCE_ROOT" \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  "$SOURCE_ROOT/scripts/prepare-x86_64-capacity-host.sh" "$CAPACITY_HOST_INITRAMFS"

ROOTFS_WASM_EXTRACT="$RUN_ROOT/output/rootfs-wasm-extract"
[[ ! -e "$ROOTFS_WASM_EXTRACT" && ! -L "$ROOTFS_WASM_EXTRACT" ]] \
  || die "refusing stale SquashFS WASM verification path: $ROOTFS_WASM_EXTRACT"
mkdir -m 0700 "$ROOTFS_WASM_EXTRACT"
unsquashfs -f -d "$ROOTFS_WASM_EXTRACT" "$ROOTFS_IMAGE" \
  usr/share/ostadix/wasm/hello.wasm \
  usr/share/ostadix/wasm/hello.release.json \
  usr/local/bin/olangc \
  usr/src/ostadix/examples/wasm_hello.O >/dev/null
ROOTFS_WASM_ARTIFACT="$ROOTFS_WASM_EXTRACT/usr/share/ostadix/wasm/hello.wasm"
ROOTFS_WASM_MANIFEST="$ROOTFS_WASM_EXTRACT/usr/share/ostadix/wasm/hello.release.json"
ROOTFS_WASM_GENERATOR="$ROOTFS_WASM_EXTRACT/usr/local/bin/olangc"
ROOTFS_WASM_INPUT="$ROOTFS_WASM_EXTRACT/usr/src/ostadix/examples/wasm_hello.O"
for extracted in \
  "$ROOTFS_WASM_ARTIFACT" "$ROOTFS_WASM_MANIFEST" \
  "$ROOTFS_WASM_GENERATOR" "$ROOTFS_WASM_INPUT"; do
  [[ -f "$extracted" && ! -L "$extracted" ]] \
    || die "SquashFS omitted a regular Olangc WASM release input: $extracted"
done
cmp -s "$WASM_ARTIFACT" "$ROOTFS_WASM_ARTIFACT" \
  || die "SquashFS Olangc WASM artifact differs from the built module"
cmp -s "$WASM_MANIFEST" "$ROOTFS_WASM_MANIFEST" \
  || die "SquashFS Olangc WASM manifest differs from the built manifest"
python3 "$SOURCE_ROOT/scripts/ostadix_wasm_release.py" verify \
  --manifest "$ROOTFS_WASM_MANIFEST" \
  --project "$WASM_PROJECT" \
  --artifact "$ROOTFS_WASM_ARTIFACT" \
  --input "$ROOTFS_WASM_INPUT" \
  --generator "$ROOTFS_WASM_GENERATOR" \
  --source-tree "$SOURCE_TREE" \
  --base-commit "$BASE_COMMIT" \
  --source-archive-sha256 "$ARCHIVE_SHA256" >/dev/null

ISO_ROOT="$RUN_ROOT/iso-work"
mkdir -p -- "$ISO_ROOT" "$(dirname -- "$OUTPUT")"
env \
  OSTADIX_FOREIGN_KERNEL_LAB="$SOURCE_ROOT/scripts/foreign_kernel_lab.py" \
  OSTADIX_GUEST_ROOT="$GUEST_ROOT" \
  OSTADIX_HOSTED_LIVE_ALPINE_INITRAMFS="$GUEST_ROOT/alpine-3.24.1-x86_64/initramfs-virt" \
  OSTADIX_HOSTED_LIVE_CAPACITY_HOST_INITRAMFS="$CAPACITY_HOST_INITRAMFS" \
  OSTADIX_HOSTED_LIVE_CAPACITY_HOST_KERNEL="$GUEST_ROOT/alpine-3.24.1-x86_64/vmlinuz-virt" \
  OSTADIX_HOSTED_LIVE_INITRAMFS="$INITRAMFS" \
  OSTADIX_HOSTED_LIVE_ISO_ROOT="$ISO_ROOT" \
  OSTADIX_HOSTED_LIVE_KERNEL="$LTS_KERNEL" \
  OSTADIX_HOSTED_LIVE_OCORE_KERNEL="$OCORE_KERNEL" \
  OSTADIX_HOSTED_LIVE_ROOTFS="$ROOTFS_IMAGE" \
  OSTADIX_HOSTED_LIVE_VENTOY_MODLOOP="$VENTOY_MODLOOP" \
  OSTADIX_GRUB_MKRESCUE="$(command -v grub-mkrescue)" \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  "$SOURCE_ROOT/ocore/kernel/build-x86_64-hosted-live-iso.sh" "$OUTPUT"

INSPECTION="$RUN_ROOT/output/iso-inspection.json"
SERIAL_SMOKE="$RUN_ROOT/output/qemu-serial-smoke.json"
VISUAL_SMOKE="$RUN_ROOT/output/qemu-visual-smoke.json"
OCORE_SMOKE="$RUN_ROOT/output/qemu-ocore-smoke.json"
HOSTED_SMOKE_TIMEOUT=${OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT:-1800}
OCORE_SMOKE_TIMEOUT=${OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT:-900}
python3 "$SOURCE_ROOT/scripts/ostadix_capacity_iso.py" inspect "$OUTPUT" >"$INSPECTION"
python3 "$SOURCE_ROOT/ocore/kernel/smoke-x86_64-hosted-live-qemu.py" \
  --timeout "$HOSTED_SMOKE_TIMEOUT" "$OUTPUT" >"$SERIAL_SMOKE"
# shellcheck source=../ocore/kernel/resolve-x86_64-ovmf-code.sh
source "$SOURCE_ROOT/ocore/kernel/resolve-x86_64-ovmf-code.sh"
OVMF_CODE=$(resolve_ostadix_x86_64_ovmf_code "$(command -v qemu-system-x86_64)")
python3 "$SOURCE_ROOT/ocore/kernel/smoke-x86_64-hosted-live-vga-qemu.py" \
  --firmware "$OVMF_CODE" \
  --qemu "$(command -v qemu-system-x86_64)" \
  --evidence-dir "$RUN_ROOT/output/qemu-vga-evidence" \
  --timeout "$HOSTED_SMOKE_TIMEOUT" \
  "$OUTPUT" >"$VISUAL_SMOKE"
python3 "$SOURCE_ROOT/ocore/kernel/smoke-x86_64-hosted-live-ocore-qemu.py" \
  --firmware "$OVMF_CODE" \
  --qemu "$(command -v qemu-system-x86_64)" \
  --timeout "$OCORE_SMOKE_TIMEOUT" \
  "$OUTPUT" >"$OCORE_SMOKE"

SYSROOT_LOCK_FILE="$RUN_ROOT/output/sysroot-packages.txt"
printf '%s\n' "${SYSROOT_PACKAGE_SPECS[@]}" | LC_ALL=C sort >"$SYSROOT_LOCK_FILE"
python3 - \
  "$OUTPUT" "$RECEIPT" "$INSPECTION" "$SERIAL_SMOKE" "$VISUAL_SMOKE" "$OCORE_SMOKE" \
  "$INITRAMFS" "$ROOTFS_IMAGE" "$VENTOY_MODLOOP" "$OCORE_KERNEL" \
  "$HOSTED_BIN_DIR" "$SOURCE_ROOT/evidence/hosted_live_workstation_apk_packages.txt" \
  "$SYSROOT_LOCK_FILE" "$SOURCE_TREE" "$BASE_COMMIT" "$ARCHIVE_SHA256" \
  "$BOOT_OBJECTS_ARCHIVE_SHA256" "$BOOT_OBJECTS_RESULT" \
  "$RUST_VERSION" "$HOST_ARCH" "$SYSROOT_MANIFEST" "$CARGO_VENDOR_MANIFEST" \
  "$ROOTFS_WASM_MANIFEST" \
  "$CAPACITY_HOST_INITRAMFS" "$GUEST_CACHE_VERIFICATION" \
  "$SOURCE_ROOT/evidence/foreign_kernel_lab.toml" \
  "$SOURCE_ROOT/evidence/hosted_live_apk_packages.txt" \
  "$CAPACITY_HOST_CACHE/alpine-minirootfs-3.24.1-x86_64.tar.gz" \
  "$CAPACITY_HOST_CACHE/modloop-virt-3.24.1-x86_64" \
  "$LTS_KERNEL" "$LTS_INITRAMFS" "$LTS_MODLOOP" <<'PY'
from pathlib import Path
import hashlib
import json
import os
import sys

(
    iso_text,
    receipt_text,
    inspection_text,
    serial_smoke_text,
    visual_smoke_text,
    ocore_smoke_text,
    initramfs_text,
    rootfs_text,
    ventoy_modloop_text,
    ocore_kernel_text,
    binaries_text,
    package_lock_text,
    sysroot_lock_text,
    source_tree,
    base_commit,
    archive_sha256,
    boot_objects_archive_sha256,
    boot_objects_result_text,
    rust_version,
    host_arch,
    sysroot_manifest_text,
    cargo_vendor_manifest_text,
    wasm_manifest_text,
    capacity_host_initramfs_text,
    guest_verification_text,
    foreign_manifest_text,
    capacity_package_lock_text,
    alpine_minirootfs_text,
    alpine_virt_modloop_text,
    alpine_lts_kernel_text,
    alpine_lts_initramfs_text,
    alpine_lts_modloop_text,
) = sys.argv[1:]

def identity(path: Path) -> dict[str, object]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
    return {"bytes": size, "sha256": digest.hexdigest()}

iso = Path(iso_text)
receipt = Path(receipt_text)
binaries = Path(binaries_text)
inspection = json.loads(Path(inspection_text).read_text(encoding="utf-8"))
iso_identity = identity(iso)
serial_smoke = json.loads(Path(serial_smoke_text).read_text(encoding="utf-8"))
visual_smoke = json.loads(Path(visual_smoke_text).read_text(encoding="utf-8"))
ocore_kernel = identity(Path(ocore_kernel_text))
initramfs_identity = identity(Path(initramfs_text))
rootfs_identity = identity(Path(rootfs_text))
ventoy_modloop_identity = identity(Path(ventoy_modloop_text))
ocore_smoke = json.loads(Path(ocore_smoke_text).read_text(encoding="utf-8"))
boot_objects = json.loads(Path(boot_objects_result_text).read_text(encoding="utf-8"))
olangc_wasm = json.loads(Path(wasm_manifest_text).read_text(encoding="utf-8"))
required_boot_object_fields = {
    "schema": "ostadix.boot-object-store-result/v1",
    "ok": True,
    "operation": "verify",
    "commit": base_commit,
    "tree": source_tree,
}
if not isinstance(boot_objects, dict) or any(
    boot_objects.get(key) != value for key, value in required_boot_object_fields.items()
):
    raise SystemExit("error: boot-object result is not bound to the exact staged source")
for field in ("object_count", "binding_count", "logical_bytes", "stored_bytes"):
    if type(boot_objects.get(field)) is not int or boot_objects[field] <= 0:
        raise SystemExit(f"error: boot-object result has invalid {field}")
root_sha256 = boot_objects.get("root_sha256")
if (
    not isinstance(root_sha256, str)
    or len(root_sha256) != 64
    or any(character not in "0123456789abcdef" for character in root_sha256)
):
    raise SystemExit("error: boot-object result has invalid root_sha256")
boot_objects["store"] = "/usr/share/ostadix/boot-objects/v1"
inspection_identity = {
    "bytes": inspection.get("bytes"),
    "sha256": inspection.get("sha256"),
}
if inspection_identity != iso_identity:
    raise SystemExit("error: ISO changed after strict inspection")
for label, smoke in (
    ("serial", serial_smoke),
    ("graphical", visual_smoke),
    ("O-core", ocore_smoke),
):
    if not isinstance(smoke, dict) or smoke.get("iso") != iso_identity:
        raise SystemExit(f"error: {label} smoke booted bytes other than the inspected ISO")

payload = {
    "schema": "ostadix.hosted-live-release/v6",
    "source": {
        "staged_tree": source_tree,
        "base_commit": base_commit,
        "archive_sha256": archive_sha256,
        "boot_objects_archive_sha256": boot_objects_archive_sha256,
        "boot_objects": boot_objects,
    },
    "build": {
        "host_architecture": host_arch,
        "target": "x86_64-unknown-linux-musl",
        "rust_toolchain": rust_version,
        "cargo_build_jobs": 1,
        "cargo_codegen_units": 16,
        "cargo_lto": False,
        "source_date_epoch": 315532800,
        "musl_dev_version": "1.2.6-r2",
        "workstation_package_roots": [
            "build-base=0.5-r4",
            "cargo=1.96.1-r0",
            "clang22=22.1.3-r2",
            "eudev=3.2.14-r6",
            "firefox-esr=140.12.0-r0",
            "git=2.54.0-r0",
            "lld22=22.1.3-r0",
            "openbox=3.6.1-r8",
            "openssl=3.5.8-r0",
            "rust=1.96.1-r0",
            "rust-clippy=1.96.1-r0",
            "rust-wasm=1.96.1-r0",
            "rustfmt=1.96.1-r0",
            "wasm-tools=1.236.0-r0",
            "wasmtime=44.0.1-r0",
            "xdg-utils=1.2.1-r1",
            "xf86-input-libinput=1.5.0-r0",
            "xinit=1.4.4-r0",
            "xorg-server=21.1.24-r0",
            "xset=1.2.5-r1",
            "xsetroot=1.1.3-r1",
            "xterm=410-r0",
        ],
        "workstation_source_path": "/usr/src/ostadix",
        "sysroot_package_lock": Path(sysroot_lock_text).read_text(encoding="utf-8").splitlines(),
        "sysroot_manifest": identity(Path(sysroot_manifest_text)),
        "hosted_live_package_lock": identity(Path(package_lock_text)),
        "cargo_vendor_manifest": identity(Path(cargo_vendor_manifest_text)),
        "ocore": {
            "compiler_target": "x86_64-unknown-none",
            "assembler_target": "x86_64-unknown-none-elf",
            "probe_mode": 0,
            "boot_info_enabled": True,
            "linker": "ld.lld",
            "cargo_build_jobs": 1,
            "cargo_offline": True,
        },
        "apk_repository_boundary": {
            "exact_versions": True,
            "signed_index_and_packages": True,
            "independent_apk_blob_hash_lock": False,
            "repository_availability_required": True,
        },
        "cache_inputs": {
            "alpine_minirootfs": identity(Path(alpine_minirootfs_text)),
            "alpine_lts_kernel": identity(Path(alpine_lts_kernel_text)),
            "alpine_lts_initramfs": identity(Path(alpine_lts_initramfs_text)),
            "alpine_lts_modloop": identity(Path(alpine_lts_modloop_text)),
        },
    },
    "binaries": {
        name: identity(binaries / name)
        for name in (
            "O", "o-cli", "olangc", "ocorec", "o-link", "o-unlink", "o-notebook",
            "ogit", "o-live-host", "o-node", "octl", "o-registry", "o-info",
            "ostadix-device",
            "ocore-kernel-world-record", "ostadix-mcp",
        )
    },
    "rootfs_objects": {
        "olangc_wasm_hello": {
            "manifest_path": "/usr/share/ostadix/wasm/hello.release.json",
            "artifact_path": "/usr/share/ostadix/wasm/hello.wasm",
            "manifest": identity(Path(wasm_manifest_text)),
            "descriptor": olangc_wasm,
        }
    },
    "capacity": {
        "host_initramfs": identity(Path(capacity_host_initramfs_text)),
        "foreign_manifest": identity(Path(foreign_manifest_text)),
        "package_lock": identity(Path(capacity_package_lock_text)),
        "guest_verification": {
            "identity": identity(Path(guest_verification_text)),
            "records": Path(guest_verification_text).read_text(encoding="utf-8").splitlines(),
        },
        "virt_modloop": identity(Path(alpine_virt_modloop_text)),
        "boot_routes": {
            "direct": ["hosted", "ocore", "alpine"],
            "nested_qemu_tcg": ["guix", "openbsd", "plan9", "redox"],
        },
    },
    "initramfs": initramfs_identity,
    "rootfs": rootfs_identity,
    "ventoy_modloop": ventoy_modloop_identity,
    "ocore_kernel": ocore_kernel,
    "iso": inspection,
    "smoke": {
        "schema": "ostadix.hosted-live-boot-gates/v6",
        "serial": serial_smoke,
        "graphical": visual_smoke,
        "ocore": ocore_smoke,
    },
    "boot_profile": {
        "kind": "physical-hosted-workstation-plus-capacity",
        "default_entry": "hosted",
        "kernel_flavor": "alpine-lts",
        "rootfs_layout": "verified-squashfs-plus-tmpfs-overlay",
        "ventoy_compatibility": "alpine-hook-plus-minimal-modloop",
        "desktop_session": "openbox-xterm",
        "preferred_console": "tty0",
        "panic_timeout_seconds": 0,
        "ocore_entry": "direct-multiboot2-serial",
        "direct_entries": ["hosted", "ocore", "alpine"],
        "nested_qemu_tcg_entries": ["guix", "openbsd", "plan9", "redox"],
        "ventoy_mode": "grub2-filename-suffix",
    },
    "claim_boundary": {
        "substrate": "aarch64-or-x86_64-linux-multipass-plus-x86_64-qemu-tcg-ovmf",
        "physical_hardware_proof": False,
        "secure_boot_proof": False,
        "hermetic": False,
        "host_mounts_may_be_visible": True,
        "foreign_entries_nested_qemu_tcg": True,
        "foreign_entries_direct_grub": False,
        "combined_capacity_menu_execution_proof": False,
        "foreign_guest_gui_proof": False,
        "foreign_guest_package_manager_execution_proof": False,
        "ventoy_foreign_route_proof": False,
    },
}
ocore_artifacts = [
    artifact
    for artifact in inspection.get("artifacts", [])
    if isinstance(artifact, dict)
    and artifact.get("iso_path") == "/boot/ocore/kernel.elf"
    and artifact.get("role") == "ocore-kernel"
]
if len(ocore_artifacts) != 1 or {
    "bytes": ocore_artifacts[0].get("bytes"),
    "sha256": ocore_artifacts[0].get("sha256"),
} != ocore_kernel:
    raise SystemExit("error: strict ISO inspection disagrees with the built O-core kernel")
rootfs_artifacts = [
    artifact
    for artifact in inspection.get("artifacts", [])
    if isinstance(artifact, dict)
    and artifact.get("iso_path") == "/boot/hosted/rootfs.squashfs"
    and artifact.get("role") == "linux-rootfs"
]
if len(rootfs_artifacts) != 1 or {
    "bytes": rootfs_artifacts[0].get("bytes"),
    "sha256": rootfs_artifacts[0].get("sha256"),
} != rootfs_identity:
    raise SystemExit("error: strict ISO inspection disagrees with the built SquashFS root")
ventoy_modloop_artifacts = [
    artifact
    for artifact in inspection.get("artifacts", [])
    if isinstance(artifact, dict)
    and artifact.get("iso_path") == "/boot/modloop-lts"
    and artifact.get("role") == "linux-modloop"
]
if len(ventoy_modloop_artifacts) != 1 or {
    "bytes": ventoy_modloop_artifacts[0].get("bytes"),
    "sha256": ventoy_modloop_artifacts[0].get("sha256"),
} != ventoy_modloop_identity:
    raise SystemExit("error: strict ISO inspection disagrees with the Ventoy modloop")
capacity_host_artifacts = [
    artifact
    for artifact in inspection.get("artifacts", [])
    if isinstance(artifact, dict)
    and artifact.get("iso_path") == "/boot/capacity-host/initramfs.cpio.gz"
    and artifact.get("role") == "linux-initrd"
]
if len(capacity_host_artifacts) != 1 or {
    "bytes": capacity_host_artifacts[0].get("bytes"),
    "sha256": capacity_host_artifacts[0].get("sha256"),
} != payload["capacity"]["host_initramfs"]:
    raise SystemExit("error: strict ISO inspection disagrees with the capacity-host initramfs")
encoded = (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode("utf-8")
descriptor = os.open(receipt, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o444)
try:
    remaining = memoryview(encoded)
    while remaining:
        written = os.write(descriptor, remaining)
        if written <= 0:
            raise OSError("short write while creating hosted-live receipt")
        remaining = remaining[written:]
    os.fsync(descriptor)
finally:
    os.close(descriptor)
os.chmod(receipt, 0o444)
PY

printf 'hosted-live-output: %s\n' "$OUTPUT"
printf 'hosted-live-sha256: %s\n' "$(sha256sum "$OUTPUT" | awk '{print $1}')"
printf 'hosted-live-receipt: %s\n' "$RECEIPT"
