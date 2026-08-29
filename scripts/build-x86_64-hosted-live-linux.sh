#!/usr/bin/env bash
# Linux worker for the repository-owned hosted-live release pipeline.
set -euo pipefail
umask 077

# Multipass exec is non-login: rustup installs here but is otherwise absent
# from PATH. Keep every tool lookup explicit and independent of shell startup.
export PATH="/home/ubuntu/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

RUST_TOOLCHAIN=1.97.1
RUST_TARGET=x86_64-unknown-linux-musl
SOURCE_DATE_EPOCH=315532800
ALPINE_MINIROOTFS_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/alpine-minirootfs-3.24.1-x86_64.tar.gz
ALPINE_MINIROOTFS_BYTES=3698422
ALPINE_MINIROOTFS_SHA256=41f73e3cf5fa919b8aa5ca6b30dc48f0da2720776d7423e2a7748211456fe081
MUSL_DEV_VERSION=1.2.6-r2

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
SHARED_CACHE=/home/ubuntu/.cache/ostadix/hosted-live-release/shared
GUEST_ROOT=/home/ubuntu/.local/share/ostadix/guests
CAPACITY_HOST_CACHE=/home/ubuntu/.cache/ostadix/capacity-host

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
for cache in "$SHARED_CACHE" "$GUEST_ROOT" "$CAPACITY_HOST_CACHE"; do
  [[ ! -L "$cache" ]] || die "release cache must not be a symlink: $cache"
  case "$(realpath -m -- "$cache")" in
    /home/ubuntu/*) ;;
    *) die "release cache escaped the native guest home: $cache" ;;
  esac
done
mkdir -p -- "$SHARED_CACHE" "$GUEST_ROOT" "$CAPACITY_HOST_CACHE" "$RUN_ROOT/output"

for required in \
  Cargo.lock Cargo.toml rust-toolchain.toml backends examples \
  evidence/absorbed_capacity_iso.toml evidence/hosted_live_apk_packages.txt \
  scripts/foreign_kernel_lab.py scripts/prepare-x86_64-capacity-host.sh \
  ocore/kernel/build-x86_64-capacity-iso.sh \
  ocore/kernel/smoke-x86_64-hosted-live-qemu.py; do
  [[ -e "$SOURCE_ROOT/$required" && ! -L "$SOURCE_ROOT/$required" ]] \
    || die "staged source snapshot is missing required path: $required"
done

for tool in \
  cargo chroot clang cmp cpio curl file find flock grep gzip grub-mkrescue install ld.lld \
  llvm-ar python3 qemu-system-x86_64 realpath rustup sha256sum sudo \
  tar tee unsquashfs xorriso zstd; do
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

verify_file() {
  local path=$1 expected_bytes=$2 expected_sha=$3 actual_bytes actual_sha
  [[ -f "$path" && ! -L "$path" ]] || return 1
  actual_bytes=$(wc -c <"$path" | tr -d ' ')
  actual_sha=$(sha256sum "$path" | awk '{print $1}')
  [[ "$actual_bytes" == "$expected_bytes" && "$actual_sha" == "$expected_sha" ]]
}

if ! verify_file "$MINIROOTFS" "$ALPINE_MINIROOTFS_BYTES" "$ALPINE_MINIROOTFS_SHA256"; then
  PARTIAL="$DOWNLOAD_ROOT/.alpine-minirootfs.$$.partial"
  rm -f -- "$PARTIAL"
  curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
    --output "$PARTIAL" "$ALPINE_MINIROOTFS_URL"
  verify_file "$PARTIAL" "$ALPINE_MINIROOTFS_BYTES" "$ALPINE_MINIROOTFS_SHA256" \
    || die "pinned Alpine minirootfs failed exact size/SHA-256 verification"
  chmod 0444 "$PARTIAL"
  mv -f -- "$PARTIAL" "$MINIROOTFS"
fi

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

TOOL_ROOT="$RUN_ROOT/tooling"
TARGET_ROOT="$RUN_ROOT/cargo-x86_64-musl"
HOSTED_BIN_DIR="$RUN_ROOT/hosted-bin"
mkdir -p -- "$TOOL_ROOT" "$TARGET_ROOT" "$HOSTED_BIN_DIR"
LINKER="$TOOL_ROOT/x86_64-alpine-linux-musl-clang"
cat >"$LINKER" <<EOF
#!/bin/sh
exec $(command -v clang) --target=x86_64-alpine-linux-musl --sysroot="$SYSROOT" -fuse-ld=lld "\$@"
EOF
chmod 0555 "$LINKER"
LLVM_AR=$(command -v llvm-ar)

env \
  AR_x86_64_unknown_linux_musl="$LLVM_AR" \
  CC_x86_64_unknown_linux_musl="$LINKER" \
  CARGO_BUILD_JOBS=1 \
  CARGO_INCREMENTAL=0 \
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  CARGO_PROFILE_RELEASE_LTO=false \
  CARGO_TARGET_DIR="$TARGET_ROOT" \
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$LINKER" \
  RUSTFLAGS="--remap-path-prefix=$SOURCE_ROOT=/usr/src/ostadix -C target-feature=+crt-static" \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  rustup run "$RUST_TOOLCHAIN" cargo build \
    --manifest-path "$SOURCE_ROOT/Cargo.toml" \
    --locked --release --target "$RUST_TARGET" --package o-lang \
    --bin O --bin o-cli --bin olangc --bin o-link

SYSROOT_MANIFEST_AFTER="$RUN_ROOT/output/sysroot-manifest-after-build.json"
write_sysroot_manifest "$SYSROOT" "$SYSROOT_MANIFEST_AFTER"
cmp -s "$SYSROOT_MANIFEST" "$SYSROOT_MANIFEST_AFTER" \
  || die "x86_64 musl sysroot changed after its admitted content manifest"
rm -f -- "$SYSROOT_MANIFEST_AFTER"

for binary in O o-cli olangc o-link; do
  source_binary="$TARGET_ROOT/$RUST_TARGET/release/$binary"
  [[ -f "$source_binary" && -x "$source_binary" ]] \
    || die "static build omitted hosted binary: $binary"
  install -m 0555 "$source_binary" "$HOSTED_BIN_DIR/$binary"
done

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

sudo install -d -m 0755 "$CAPACITY_HOST_CACHE"
if [[ ! -e "$CAPACITY_HOST_CACHE/alpine-minirootfs-3.24.1-x86_64.tar.gz" ]]; then
  sudo install -m 0444 "$MINIROOTFS" \
    "$CAPACITY_HOST_CACHE/alpine-minirootfs-3.24.1-x86_64.tar.gz"
fi
INITRAMFS="$RUN_ROOT/output/initramfs.cpio.gz"
sudo env \
  OSTADIX_CAPACITY_HOST_CACHE="$CAPACITY_HOST_CACHE" \
  OSTADIX_CAPACITY_HOST_PACKAGE_LOCK="$SOURCE_ROOT/evidence/hosted_live_apk_packages.txt" \
  OSTADIX_GUEST_ROOT="$GUEST_ROOT" \
  OSTADIX_HOSTED_BIN_DIR="$HOSTED_BIN_DIR" \
  OSTADIX_HOSTED_REVISION="$SOURCE_TREE" \
  OSTADIX_HOSTED_SOURCE_ROOT="$SOURCE_ROOT" \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  "$SOURCE_ROOT/scripts/prepare-x86_64-capacity-host.sh" "$INITRAMFS"

ISO_ROOT="$RUN_ROOT/iso-work"
NATIVE_TARGET="$RUN_ROOT/cargo-native"
mkdir -p -- "$ISO_ROOT" "$NATIVE_TARGET" "$(dirname -- "$OUTPUT")"
env \
  CARGO_TARGET_DIR="$NATIVE_TARGET" \
  OCORE_CAPACITY_ISO_KERNEL_BUILD_DIR="$RUN_ROOT/ocore-kernel" \
  OCORE_LLD="$(command -v ld.lld)" \
  OSTADIX_CAPACITY_HOST_INITRAMFS="$INITRAMFS" \
  OSTADIX_CAPACITY_ISO_ROOT="$ISO_ROOT" \
  OSTADIX_GRUB_MKRESCUE="$(command -v grub-mkrescue)" \
  OSTADIX_GUEST_ROOT="$GUEST_ROOT" \
  RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN" \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  "$SOURCE_ROOT/ocore/kernel/build-x86_64-capacity-iso.sh" "$OUTPUT"

INSPECTION="$RUN_ROOT/output/iso-inspection.json"
SMOKE="$RUN_ROOT/output/qemu-smoke.json"
python3 "$SOURCE_ROOT/scripts/ostadix_capacity_iso.py" inspect "$OUTPUT" >"$INSPECTION"
python3 "$SOURCE_ROOT/ocore/kernel/smoke-x86_64-hosted-live-qemu.py" \
  --timeout "${OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT:-180}" "$OUTPUT" >"$SMOKE"

SYSROOT_LOCK_FILE="$RUN_ROOT/output/sysroot-packages.txt"
printf '%s\n' "${SYSROOT_PACKAGE_SPECS[@]}" | LC_ALL=C sort >"$SYSROOT_LOCK_FILE"
python3 - \
  "$OUTPUT" "$RECEIPT" "$INSPECTION" "$SMOKE" "$INITRAMFS" \
  "$HOSTED_BIN_DIR" "$SOURCE_ROOT/evidence/hosted_live_apk_packages.txt" \
  "$SYSROOT_LOCK_FILE" "$SOURCE_TREE" "$BASE_COMMIT" "$ARCHIVE_SHA256" \
  "$RUST_VERSION" "$HOST_ARCH" "$SYSROOT_MANIFEST" \
  "$GUEST_ROOT" "$GUEST_CACHE_VERIFICATION" \
  "$CAPACITY_HOST_CACHE/alpine-minirootfs-3.24.1-x86_64.tar.gz" \
  "$CAPACITY_HOST_CACHE/modloop-virt-3.24.1-x86_64" <<'PY'
from pathlib import Path
import hashlib
import json
import os
import sys

(
    iso_text,
    receipt_text,
    inspection_text,
    smoke_text,
    initramfs_text,
    binaries_text,
    package_lock_text,
    sysroot_lock_text,
    source_tree,
    base_commit,
    archive_sha256,
    rust_version,
    host_arch,
    sysroot_manifest_text,
    guest_root,
    guest_verification_text,
    capacity_minirootfs_text,
    capacity_modloop_text,
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
payload = {
    "schema": "ostadix.hosted-live-release/v1",
    "source": {
        "staged_tree": source_tree,
        "base_commit": base_commit,
        "archive_sha256": archive_sha256,
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
        "sysroot_package_lock": Path(sysroot_lock_text).read_text(encoding="utf-8").splitlines(),
        "sysroot_manifest": identity(Path(sysroot_manifest_text)),
        "capacity_host_package_lock": identity(Path(package_lock_text)),
        "apk_repository_boundary": {
            "exact_versions": True,
            "signed_index_and_packages": True,
            "independent_apk_blob_hash_lock": False,
            "repository_availability_required": True,
        },
        "cache_inputs": {
            "guest_root": guest_root,
            "guest_verification": {
                "identity": identity(Path(guest_verification_text)),
                "records": Path(guest_verification_text).read_text(encoding="utf-8").splitlines(),
            },
            "capacity_host_minirootfs": identity(Path(capacity_minirootfs_text)),
            "capacity_host_modloop": identity(Path(capacity_modloop_text)),
        },
    },
    "binaries": {
        name: identity(binaries / name)
        for name in ("O", "o-cli", "olangc", "o-link")
    },
    "initramfs": identity(Path(initramfs_text)),
    "iso": json.loads(Path(inspection_text).read_text(encoding="utf-8")),
    "smoke": json.loads(Path(smoke_text).read_text(encoding="utf-8")),
    "claim_boundary": {
        "substrate": "aarch64-or-x86_64-linux-multipass-plus-x86_64-qemu-tcg-ovmf",
        "physical_hardware_proof": False,
        "secure_boot_proof": False,
        "hermetic": False,
        "host_mounts_may_be_visible": True,
    },
}
if payload["iso"]["sha256"] != identity(iso)["sha256"]:
    raise SystemExit("error: ISO changed after strict inspection")
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
