#!/usr/bin/env bash
# O-lang cross-platform setup & bootstrap script
# Sets up dependencies, builds Rust + C17 + Python editions for the *current machine*,
# and leaves everything in a convenient runnable form. Native/kernel and guest-lab
# dependencies are explicit profiles; setup never downloads or boots a foreign OS.
#
# Supports: macOS, Windows (Git Bash/WSL), Debian/Ubuntu, Arch/CachyOS, Fedora, Gentoo,
#           NixOS, TinyCore, Alpine, openSUSE, Void, FreeBSD, and many others via fallbacks.
#
# Usage:
#   ./setup.sh                  # normal setup
#   ./setup.sh --minimal        # core only, no prompts for nix/extras
#   ./setup.sh --full --verify  # hosted + Nix + O-core tools, then hosted verification
#   ./setup.sh --with-guest-tools --with-ubuntu-vm
#   ./setup.sh --help
#
# After run, see the "Runnable forms" section printed at the end.
# Recommended for docker: docker run -it -v "$PWD:/ws" -w /ws debian bash -c 'apt update && apt install -y sudo curl && ./setup.sh --minimal --verify'

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$PROJECT_ROOT"

# --- Defaults ---
MINIMAL=false
FULL=false
YES=false
VERIFY=false
INSTALL_WRAPPERS=true
INSTALL_MCP=true
DRY_RUN=false
WITH_NIX=false
NIX_EXPLICIT=false
SKIP_NIX=false
WITH_OCORE=false
WITH_OCORE_MEDIA=false
WITH_HOSTED_RUNTIMES=false
WITH_LINUX_KERNEL_TOOLS=false
WITH_GUEST_TOOLS=false
WITH_UBUNTU_VM=false
PERSIST_ENV=false
WRITE_ENV=true
CHECK_ONLY=false
VERIFY_OCORE=false
DEPS_ONLY=false
WINDOWS_RERUN_REQUIRED=false
ENV_FILE="${OSTADIX_ENV_FILE:-$HOME/.config/ostadix/env.sh}"
GUESTS_DIR="${OSTADIX_GUESTS_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/ostadix/guests}"

EVALUATOR_ALIAS=ostadix-evaluator
RUST_BIN_TARGETS=(O olangc ocorec o-link o-unlink ogit o-live-host o-node octl o-registry)
RUST_STALE_BINARIES=(O o olangc ocorec o-link olink o-unlink ogit o-live-host o-node octl o-registry o-notebook "$EVALUATOR_ALIAS")
WRAPPER_TARGETS=(O o olangc o-c olangc-c o-notebook)
CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"

# --- Arg parsing ---
usage() {
  local status="${1:-0}"
  cat <<EOF
O-lang setup script

Options:
  -h, --help                  Show this help
  -m, --minimal               Minimal hosted build; explicit --with-* flags still compose
  -f, --full                  Notebook, Racket, Nix, WASI, and O-core build/QEMU tools
  -y, --yes                   Non-interactive for the profiles explicitly selected
  -v, --verify                Verify the hosted Rust, C17, AOT, and Python forms
  --with-nix                  Install/verify Nix using the official installer when absent
  --no-nix                    Exclude Nix even when --full is selected
  --with-ocore                Install Clang, LLD, ELF tools, and x86/AArch64 QEMU
  --with-ocore-media          Also install deterministic x86_64 UEFI-media tools
  --with-hosted-runtimes      Open-source backend runtimes (macOS/Debian; excludes Java/licensed tools)
  --with-linux-kernel-tools   Linux-only kernel development dependencies (tools, not sources)
  --with-guest-tools          QEMU image/compression tools for user-supplied guest media
  --with-ubuntu-vm            Also install Multipass for the ubuntu_vm^ backend
  --verify-ocore              Build and run the bounded x86 O-core QEMU smoke
  --check                     Non-installing capability check for selected profiles
  --deps-only                 Install dependencies/env only; skip Ostadix builds
  --env-file PATH             Managed environment file (default: ~/.config/ostadix/env.sh)
  --no-env                    Do not create the managed environment file
  --persist-env               Idempotently source the env file from the current shell rc
  --no-wrappers               Do not create convenience wrappers in ~/.local/bin
  --no-mcp                    Do not build the ostadix-mcp server
  --dry-run                   Print exact planned commands; make no changes

Examples:
  ./setup.sh
  ./setup.sh --minimal --verify
  ./setup.sh --full -y
  ./setup.sh --with-ocore --verify-ocore
  ./setup.sh --with-ocore-media --deps-only
  ./setup.sh --full --with-hosted-runtimes
  ./setup.sh --with-linux-kernel-tools --deps-only
  ./setup.sh --with-guest-tools --with-ubuntu-vm --deps-only
  ./setup.sh --full --dry-run

Scope:
  Guest tooling is a reference lab for user-supplied Linux/9front/OpenBSD media.
  It is not evidence that O-core boots or supports those foreign kernels.
EOF
  exit "$status"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage 0 ;;
    -m|--minimal) MINIMAL=true; shift ;;
    -f|--full) FULL=true; shift ;;
    -y|--yes) YES=true; shift ;;
    -v|--verify) VERIFY=true; shift ;;
    --with-nix) WITH_NIX=true; NIX_EXPLICIT=true; SKIP_NIX=false; shift ;;
    --no-nix) SKIP_NIX=true; WITH_NIX=false; shift ;;
    --with-ocore) WITH_OCORE=true; shift ;;
    --with-ocore-media) WITH_OCORE_MEDIA=true; WITH_OCORE=true; shift ;;
    --with-hosted-runtimes) WITH_HOSTED_RUNTIMES=true; shift ;;
    --with-linux-kernel-tools) WITH_LINUX_KERNEL_TOOLS=true; shift ;;
    --with-guest-tools) WITH_GUEST_TOOLS=true; shift ;;
    --with-ubuntu-vm) WITH_UBUNTU_VM=true; WITH_GUEST_TOOLS=true; shift ;;
    --verify-ocore) VERIFY_OCORE=true; WITH_OCORE=true; shift ;;
    --check) CHECK_ONLY=true; shift ;;
    --deps-only) DEPS_ONLY=true; shift ;;
    --env-file)
      [[ $# -ge 2 && -n "$2" ]] || { echo "Error: --env-file requires a path" >&2; usage 2; }
      ENV_FILE="$2"
      shift 2
      ;;
    --env-file=*) ENV_FILE="${1#*=}"; [[ -n "$ENV_FILE" ]] || usage 2; shift ;;
    --no-env) WRITE_ENV=false; shift ;;
    --persist-env) PERSIST_ENV=true; shift ;;
    --no-wrappers) INSTALL_WRAPPERS=false; shift ;;
    --no-mcp) INSTALL_MCP=false; shift ;;
    --dry-run) DRY_RUN=true; shift ;;
    *) echo "Unknown option: $1" >&2; usage 2 ;;
  esac
done

if $MINIMAL && $FULL; then
  echo "Error: --minimal and --full are mutually exclusive"
  exit 2
fi

if $CHECK_ONLY && $DRY_RUN; then
  echo "Error: --check and --dry-run are mutually exclusive" >&2
  exit 2
fi

if $CHECK_ONLY && { $DEPS_ONLY || $VERIFY || $VERIFY_OCORE || $PERSIST_ENV; }; then
  echo "Error: --check cannot be combined with build, verification, or persistence actions" >&2
  exit 2
fi

if $DEPS_ONLY && { $VERIFY || $VERIFY_OCORE; }; then
  echo "Error: --deps-only cannot be combined with --verify or --verify-ocore" >&2
  exit 2
fi

if $PERSIST_ENV && ! $WRITE_ENV; then
  echo "Error: --persist-env cannot be combined with --no-env" >&2
  exit 2
fi

if $FULL; then
  WITH_OCORE=true
  if ! $SKIP_NIX; then
    WITH_NIX=true
  fi
fi

if $FULL; then
  RUST_BIN_TARGETS+=(o-notebook)
fi

echo "=== O-lang cross-platform setup ==="
echo "Project root: $PROJECT_ROOT"
echo "Host: $(uname -a)"
echo "Options: minimal=$MINIMAL full=$FULL yes=$YES verify=$VERIFY nix=$WITH_NIX ocore=$WITH_OCORE ocore_media=$WITH_OCORE_MEDIA hosted_runtimes=$WITH_HOSTED_RUNTIMES linux_kernel_tools=$WITH_LINUX_KERNEL_TOOLS guest_tools=$WITH_GUEST_TOOLS ubuntu_vm=$WITH_UBUNTU_VM verify_ocore=$VERIFY_OCORE check=$CHECK_ONLY deps_only=$DEPS_ONLY env=$WRITE_ENV persist_env=$PERSIST_ENV wrappers=$INSTALL_WRAPPERS mcp=$INSTALL_MCP dry_run=$DRY_RUN"
echo

# --- OS / Distro Detection ---
OS_TYPE="$(uname -s | tr '[:upper:]' '[:lower:]')"
DISTRO_ID=""
DISTRO_LIKE=""
DISTRO="unknown"
PLATFORM="unknown"

if [[ -f /etc/os-release ]]; then
  . /etc/os-release
  DISTRO_ID="${ID:-}"
  DISTRO_LIKE="${ID_LIKE:-$DISTRO_ID}"
fi

if [[ "$OS_TYPE" == "darwin" ]]; then
  PLATFORM="macos"
elif [[ "$OS_TYPE" == "linux" ]]; then
  PLATFORM="linux"
  if [[ "$DISTRO_ID" =~ (arch|manjaro|endeavouros|cachyos|garuda|artix) || "$DISTRO_LIKE" =~ arch ]]; then
    DISTRO="arch"
  elif [[ "$DISTRO_ID" =~ (ubuntu|debian|mint|pop|kali|parrot|raspbian|linuxmint) || "$DISTRO_LIKE" =~ debian ]]; then
    DISTRO="debian"
  elif [[ "$DISTRO_ID" =~ (fedora|centos|rhel|rocky|almalinux|nobara|ol|amzn) || "$DISTRO_LIKE" =~ (fedora|rhel) ]]; then
    DISTRO="fedora"
  elif [[ "$DISTRO_ID" == "gentoo" || "$DISTRO_LIKE" =~ gentoo ]]; then
    DISTRO="gentoo"
  elif [[ "$DISTRO_ID" == "nixos" ]]; then
    DISTRO="nixos"
  elif [[ "$DISTRO_ID" =~ (tinycore|core) ]]; then
    DISTRO="tinycore"
  elif [[ "$DISTRO_ID" == "alpine" || "$DISTRO_LIKE" =~ alpine ]]; then
    DISTRO="alpine"
  elif [[ "$DISTRO_ID" =~ (opensuse|suse) || "$DISTRO_LIKE" =~ suse ]]; then
    DISTRO="opensuse"
  elif [[ "$DISTRO_ID" == "void" ]]; then
    DISTRO="void"
  else
    DISTRO="unknown"
  fi
elif [[ "$OS_TYPE" =~ (freebsd|dragonfly|netbsd|openbsd) ]]; then
  PLATFORM="bsd"
  DISTRO="$OS_TYPE"
elif [[ "$OS_TYPE" =~ (mingw|msys|cygwin) ]]; then
  PLATFORM="windows"
  DISTRO="windows-bash"
else
  PLATFORM="unknown"
  DISTRO="unknown"
fi

# Test harnesses may inspect another host's dependency plan, but never let a
# stale environment variable redirect a real package-manager invocation.
if [[ -n "${OSTADIX_SETUP_PLATFORM:-}" || -n "${OSTADIX_SETUP_DISTRO:-}" ]]; then
  if ! $DRY_RUN || [[ "${OSTADIX_SETUP_TEST_OVERRIDES:-}" != "1" ]]; then
    echo "Error: OSTADIX_SETUP_PLATFORM/DISTRO are test-only and require --dry-run plus OSTADIX_SETUP_TEST_OVERRIDES=1." >&2
    exit 2
  fi
  PLATFORM="${OSTADIX_SETUP_PLATFORM:-$PLATFORM}"
  DISTRO="${OSTADIX_SETUP_DISTRO:-$DISTRO}"
fi

if $WITH_NIX && [[ "$PLATFORM" != "macos" && "$PLATFORM" != "linux" ]]; then
  if $FULL && ! $NIX_EXPLICIT; then
    echo "Note: Nix is not auto-installed on $PLATFORM; continuing with the remaining --full profile."
    WITH_NIX=false
  else
    echo "Error: --with-nix is supported by this installer only on macOS and Linux." >&2
    exit 2
  fi
fi

if $WITH_HOSTED_RUNTIMES && ! $CHECK_ONLY && \
    [[ "$PLATFORM" != "macos" && !( "$PLATFORM" == "linux" && "$DISTRO" == "debian" ) ]]; then
  echo "Error: automatic --with-hosted-runtimes package installation is currently validated only for macOS/Homebrew and Debian-family hosts." >&2
  echo "Use --check on this host to inventory the required executables." >&2
  exit 2
fi

if $WITH_OCORE_MEDIA && ! $CHECK_ONLY && \
    [[ "$PLATFORM" != "macos" && !( "$PLATFORM" == "linux" && "$DISTRO" == "debian" ) ]]; then
  echo "Error: automatic --with-ocore-media installation is currently validated only for macOS/Homebrew and Debian-family hosts." >&2
  echo "Install GRUB x86_64 EFI, mtools, and OVMF manually, then use --check." >&2
  exit 2
fi

echo "Detected: Platform=$PLATFORM Distro=$DISTRO (ID=$DISTRO_ID)"
echo

has_cmd() { command -v "$1" &>/dev/null; }

print_command() {
  printf '[DRY]'
  printf ' %q' "$@"
  printf '\n'
}

run_cmd() {
  if $DRY_RUN; then
    print_command "$@"
  else
    "$@"
  fi
}

run_privileged() {
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    run_cmd "$@"
  elif has_cmd sudo; then
    run_cmd sudo "$@"
  elif $DRY_RUN; then
    print_command sudo "$@"
  else
    echo "Error: administrator access is required for: $*" >&2
    return 1
  fi
}

append_unique() {
  local array_name="$1"
  shift
  local candidate existing present
  for candidate in "$@"; do
    present=false
    eval "local current=(\"\${${array_name}[@]}\")"
    for existing in "${current[@]}"; do
      if [[ "$existing" == "$candidate" ]]; then
        present=true
        break
      fi
    done
    if ! $present; then
      eval "$array_name+=(\"\$candidate\")"
    fi
  done
}

remove_managed_file() {
  local path="$1"
  if [[ -d "$path" && ! -L "$path" ]]; then
    echo "  Skipping directory at managed file path: $path"
    return
  fi
  if [[ -e "$path" || -L "$path" ]]; then
    run_cmd rm -f "$path"
  elif $DRY_RUN; then
    echo "[DRY] rm -f $path"
  fi
}

directory_is_case_insensitive() {
  local dir="$1"
  mkdir -p "$dir"
  local lower="$dir/.olang_case_probe_$$"
  local upper="$dir/.OLANG_CASE_PROBE_$$"
  rm -f "$lower" "$upper"
  : > "$lower"
  local insensitive=false
  if [[ -e "$upper" ]]; then
    insensitive=true
  fi
  rm -f "$lower" "$upper"
  $insensitive
}

clean_rust_release_binaries() {
  echo ">>> Removing stale Rust release binaries..."
  for bin in "${RUST_STALE_BINARIES[@]}"; do
    remove_managed_file "$PROJECT_ROOT/target/release/$bin"
    remove_managed_file "$PROJECT_ROOT/target/release/$bin.d"
  done
}

refresh_cargo_bin_binaries() {
  echo ">>> Refreshing installed Rust binaries in $CARGO_BIN_DIR..."
  if $DRY_RUN; then
    run_cmd mkdir -p "$CARGO_BIN_DIR"
    for bin in "${RUST_STALE_BINARIES[@]}"; do
      echo "[DRY] remove stale $CARGO_BIN_DIR/$bin"
    done
    for bin in "${RUST_BIN_TARGETS[@]}"; do
      echo "[DRY] replace $CARGO_BIN_DIR/$bin from $PROJECT_ROOT/target/release/$bin"
    done
    echo "[DRY] replace $CARGO_BIN_DIR/$EVALUATOR_ALIAS from $PROJECT_ROOT/target/release/O"
    echo "[DRY] install $CARGO_BIN_DIR/o through scripts/install-o-cli-wrapper.sh"
    return
  fi

  mkdir -p "$CARGO_BIN_DIR"
  for bin in "${RUST_STALE_BINARIES[@]}"; do
    remove_managed_file "$CARGO_BIN_DIR/$bin"
  done
  for bin in "${RUST_BIN_TARGETS[@]}"; do
    local src="$PROJECT_ROOT/target/release/$bin"
    local dst="$CARGO_BIN_DIR/$bin"
    if [[ ! -x "$src" ]]; then
      echo "Expected freshly built binary missing: $src" >&2
      exit 1
    fi
    cp "$src" "$dst"
    chmod +x "$dst"
  done
  cp "$PROJECT_ROOT/target/release/O" "$CARGO_BIN_DIR/$EVALUATOR_ALIAS"
  chmod +x "$CARGO_BIN_DIR/$EVALUATOR_ALIAS"
  "$PROJECT_ROOT/scripts/install-o-cli-wrapper.sh" "$CARGO_BIN_DIR/o"
  if directory_is_case_insensitive "$CARGO_BIN_DIR"; then
    echo "  $CARGO_BIN_DIR shares O/o; the wrapper dispatches by invocation spelling."
  fi
}

create_rust_alias_binaries() {
  echo ">>> Recreating Rust alias binaries..."
  if $DRY_RUN; then
    echo "[DRY] replace $PROJECT_ROOT/target/release/o from $PROJECT_ROOT/target/release/O if filesystem is case-sensitive"
    return
  fi
  if directory_is_case_insensitive "$PROJECT_ROOT/target/release"; then
    echo "  target/release is case-insensitive; O also satisfies lowercase o."
    return
  fi
  cp "$PROJECT_ROOT/target/release/O" "$PROJECT_ROOT/target/release/o"
  chmod +x "$PROJECT_ROOT/target/release/o"
}

# --- Install system dependencies (extended) ---
install_system_deps() {
  echo ">>> Installing system dependencies..."
  if $WITH_LINUX_KERNEL_TOOLS && [[ "$PLATFORM" != "linux" ]]; then
    echo "Error: --with-linux-kernel-tools is supported only on a Linux host." >&2
    echo "On macOS, use --with-guest-tools --with-ubuntu-vm for an isolated Linux development VM." >&2
    return 2
  fi

  case "$PLATFORM" in
    macos)
      if ! has_cmd brew && ! $DRY_RUN; then
        echo "Homebrew not found. Please install it:"
        echo '  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"'
        return 1
      fi
      local mac_packages=(gcc make python@3.12 curl git pkg-config openssl sqlite)
      if $FULL; then
        append_unique mac_packages racket
      fi
      if $WITH_OCORE; then
        append_unique mac_packages llvm lld binutils qemu cmake
      fi
      if $WITH_OCORE_MEDIA; then
        append_unique mac_packages x86_64-elf-grub mtools
      fi
      if $WITH_HOSTED_RUNTIMES; then
        append_unique mac_packages node ruby racket ghc ocaml sbcl mono wabt wasmtime
      fi
      if $WITH_GUEST_TOOLS; then
        append_unique mac_packages qemu coreutils xz zstd
      fi
      # Setup installs only declared prerequisites. Avoid an implicit global
      # Homebrew update/cleanup of unrelated packages and caches.
      run_cmd env HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 \
        brew install --quiet "${mac_packages[@]}"
      if $WITH_HOSTED_RUNTIMES; then
        # Never permit Octave's source build to pull a Java toolchain onto this
        # machine; use an available bottle or fail with an explicit boundary.
        run_cmd env HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 \
          brew install --force-bottle octave
      fi
      if ! xcode-select -p >/dev/null 2>&1; then
        run_cmd xcode-select --install
      fi
      if $WITH_UBUNTU_VM && ! has_cmd multipass; then
        run_cmd env HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 \
          brew install --cask multipass
      fi
      ;;

    linux)
      case "$DISTRO" in
        debian)
          local debian_packages=(build-essential gcc g++ make python3 python3-pip python3-venv curl git pkg-config libssl-dev sqlite3 ca-certificates perl file)
          if $FULL; then
            append_unique debian_packages racket
          fi
          if $WITH_OCORE; then
            append_unique debian_packages clang lld llvm binutils qemu-system-x86 qemu-system-arm cmake
          fi
          if $WITH_OCORE_MEDIA; then
            append_unique debian_packages grub-efi-amd64-bin mtools ovmf
          fi
          if $WITH_HOSTED_RUNTIMES; then
            append_unique debian_packages nodejs ruby racket ghc ocaml sbcl mono-devel octave wabt
          fi
          if $WITH_GUEST_TOOLS; then
            append_unique debian_packages qemu-system-x86 qemu-system-arm qemu-utils gzip xz-utils zstd
          fi
          if $WITH_LINUX_KERNEL_TOOLS; then
            append_unique debian_packages openssl bc bison flex libelf-dev dwarves cpio rsync kmod libncurses-dev xz-utils zstd
          fi
          if $WITH_UBUNTU_VM; then
            append_unique debian_packages snapd
          fi
          run_privileged apt-get update -qq
          run_privileged apt-get install -y -qq --no-install-recommends "${debian_packages[@]}"
          ;;

        arch)
          local arch_packages=(base-devel gcc make python python-pip curl git pkgconf openssl sqlite perl file)
          if $FULL; then
            append_unique arch_packages racket
          fi
          if $WITH_OCORE; then
            append_unique arch_packages clang lld llvm binutils qemu-full cmake
          fi
          if $WITH_GUEST_TOOLS; then
            append_unique arch_packages qemu-full gzip xz zstd
          fi
          if $WITH_LINUX_KERNEL_TOOLS; then
            append_unique arch_packages bc bison flex libelf pahole cpio rsync kmod ncurses xz zstd
          fi
          run_privileged pacman -S --noconfirm --needed "${arch_packages[@]}"
          ;;

        fedora)
          local fedora_packages=(gcc gcc-c++ make python3 python3-pip curl git openssl-devel pkgconf-pkg-config sqlite perl file)
          if $FULL; then
            append_unique fedora_packages racket
          fi
          if $WITH_OCORE; then
            append_unique fedora_packages clang lld llvm binutils qemu-system-x86 qemu-system-aarch64 qemu-img cmake
          fi
          if $WITH_GUEST_TOOLS; then
            append_unique fedora_packages qemu-system-x86 qemu-system-aarch64 qemu-img gzip xz zstd
          fi
          if $WITH_LINUX_KERNEL_TOOLS; then
            append_unique fedora_packages openssl bc bison flex elfutils-libelf-devel dwarves cpio rsync kmod ncurses-devel xz zstd
          fi
          run_privileged dnf install -y "${fedora_packages[@]}"
          ;;

        gentoo)
          local gentoo_packages=(sys-devel/gcc sys-devel/make dev-lang/python net-misc/curl dev-vcs/git dev-libs/openssl dev-db/sqlite dev-lang/perl sys-apps/file)
          if $FULL; then
            append_unique gentoo_packages dev-scheme/racket
          fi
          if $WITH_OCORE; then
            append_unique gentoo_packages sys-devel/clang sys-devel/lld sys-devel/llvm sys-devel/binutils app-emulation/qemu dev-build/cmake
          fi
          if $WITH_GUEST_TOOLS; then
            append_unique gentoo_packages app-emulation/qemu app-arch/gzip app-arch/xz-utils app-arch/zstd
          fi
          if $WITH_LINUX_KERNEL_TOOLS; then
            append_unique gentoo_packages sys-devel/bc sys-devel/bison sys-devel/flex dev-libs/elfutils dev-util/pahole app-arch/cpio net-misc/rsync sys-apps/kmod sys-libs/ncurses app-arch/xz-utils app-arch/zstd
          fi
          run_privileged emerge --quiet --ask=n "${gentoo_packages[@]}"
          ;;

        nixos)
          echo "NixOS: recommend managing via nixos-rebuild or home-manager."
          local nix_packages=(nixpkgs#rustup nixpkgs#gcc nixpkgs#gnumake nixpkgs#python3 nixpkgs#sqlite nixpkgs#curl nixpkgs#git nixpkgs#pkg-config nixpkgs#openssl nixpkgs#perl nixpkgs#file)
          if $FULL; then
            append_unique nix_packages nixpkgs#racket
          fi
          if $WITH_OCORE; then
            append_unique nix_packages nixpkgs#clang nixpkgs#lld nixpkgs#llvm nixpkgs#binutils nixpkgs#qemu nixpkgs#cmake
          fi
          if $WITH_GUEST_TOOLS; then
            append_unique nix_packages nixpkgs#qemu nixpkgs#gzip nixpkgs#xz nixpkgs#zstd
          fi
          if $WITH_LINUX_KERNEL_TOOLS; then
            append_unique nix_packages nixpkgs#bc nixpkgs#bison nixpkgs#flex nixpkgs#elfutils nixpkgs#pahole nixpkgs#cpio nixpkgs#rsync nixpkgs#kmod nixpkgs#ncurses nixpkgs#xz nixpkgs#zstd
          fi
          if has_cmd nix || $DRY_RUN; then
            run_cmd nix --extra-experimental-features "nix-command flakes" profile install "${nix_packages[@]}"
          else
            echo "Error: NixOS was detected but nix is not available on PATH." >&2
            return 1
          fi
          ;;

        tinycore)
          echo "TinyCore: minimal - run manually if needed:"
          echo "  tce-load -wi gcc make python3.12 sqlite3 curl git"
          if $FULL || $WITH_OCORE || $WITH_GUEST_TOOLS || $WITH_LINUX_KERNEL_TOOLS; then
            echo "Error: optional full/native/kernel profiles have no verified TinyCore package map." >&2
            return 2
          fi
          run_cmd tce-load -wi gcc make python3.12 sqlite3 curl git
          ;;

        alpine)
          local alpine_packages=(build-base gcc g++ make python3 py3-pip curl git openssl-dev pkgconf sqlite perl file)
          if $FULL; then
            append_unique alpine_packages racket
          fi
          if $WITH_OCORE; then
            append_unique alpine_packages clang lld llvm binutils qemu-system-x86_64 qemu-system-aarch64 qemu-img cmake
          fi
          if $WITH_GUEST_TOOLS; then
            append_unique alpine_packages qemu-system-x86_64 qemu-system-aarch64 qemu-img gzip xz zstd
          fi
          if $WITH_LINUX_KERNEL_TOOLS; then
            append_unique alpine_packages openssl bc bison flex elfutils-dev pahole cpio rsync kmod ncurses-dev xz zstd
          fi
          run_privileged apk add "${alpine_packages[@]}"
          ;;

        opensuse)
          if $WITH_OCORE || $WITH_GUEST_TOOLS; then
            echo "Error: the OpenSUSE Leap/Tumbleweed QEMU package split is not validated by this installer; install QEMU manually, then use --check." >&2
            return 2
          fi
          local suse_packages=(gcc gcc-c++ make python3 python3-pip curl git libopenssl-devel pkg-config sqlite3 perl file)
          if $FULL; then
            append_unique suse_packages racket
          fi
          if $WITH_OCORE; then
            append_unique suse_packages clang lld llvm binutils qemu qemu-tools cmake
          fi
          if $WITH_GUEST_TOOLS; then
            append_unique suse_packages qemu qemu-tools gzip xz zstd
          fi
          if $WITH_LINUX_KERNEL_TOOLS; then
            append_unique suse_packages openssl bc bison flex libelf-devel dwarves cpio rsync kmod ncurses-devel xz zstd
          fi
          run_privileged zypper --non-interactive install -l "${suse_packages[@]}"
          ;;

        void)
          local void_packages=(base-devel gcc make python3 python3-pip curl git openssl-devel pkg-config sqlite perl file)
          if $FULL; then
            append_unique void_packages racket
          fi
          if $WITH_OCORE; then
            append_unique void_packages clang lld llvm binutils qemu cmake
          fi
          if $WITH_GUEST_TOOLS; then
            append_unique void_packages qemu gzip xz zstd
          fi
          if $WITH_LINUX_KERNEL_TOOLS; then
            append_unique void_packages bc bison flex libelf-devel pahole cpio rsync kmod ncurses-devel xz zstd
          fi
          run_privileged xbps-install -Sy "${void_packages[@]}"
          ;;

        *)
          echo "Unknown Linux distro ($DISTRO_ID). Please manually install core build tools + python3 + sqlite3 + curl."
          if $WITH_OCORE || $WITH_GUEST_TOOLS || $WITH_LINUX_KERNEL_TOOLS; then
            echo "Required optional profile packages cannot be planned for this distro." >&2
            return 2
          fi
          ;;
      esac
      ;;

    bsd)
      echo "BSD ($DISTRO) detected."
      if has_cmd pkg; then
        local freebsd_packages=(gmake gcc python3 sqlite3 curl git pkgconf openssl perl5 file)
        if $FULL; then
          append_unique freebsd_packages racket
        fi
        if $WITH_OCORE || $WITH_GUEST_TOOLS; then
          append_unique freebsd_packages llvm binutils qemu cmake
        fi
        run_privileged pkg install -y "${freebsd_packages[@]}"
      elif has_cmd pkg_add || $DRY_RUN; then
        local openbsd_packages=(gmake gcc python sqlite3 curl git pkgconf)
        if $FULL; then
          append_unique openbsd_packages racket
        fi
        if $WITH_OCORE || $WITH_GUEST_TOOLS; then
          append_unique openbsd_packages llvm qemu cmake
        fi
        run_privileged pkg_add "${openbsd_packages[@]}"
      else
        echo "Error: no supported BSD package manager was found." >&2
        return 1
      fi
      ;;

    windows)
      echo "Windows bash env detected."
      echo "Best experience: use WSL2 (Ubuntu recommended) and re-run there."
      if has_cmd winget || $DRY_RUN; then
        run_cmd winget install --id Git.Git -e --silent --accept-package-agreements --accept-source-agreements
        run_cmd winget install --id Python.Python.3.12 -e --silent --accept-package-agreements --accept-source-agreements
        run_cmd winget install --id Rustlang.Rustup -e --silent --accept-package-agreements --accept-source-agreements
        run_cmd winget install --id SQLite.SQLite -e --silent --accept-package-agreements --accept-source-agreements
        run_cmd winget install --id Microsoft.VisualStudio.2022.BuildTools -e --silent --accept-package-agreements --accept-source-agreements --override "--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools"
        if $WITH_OCORE || $WITH_GUEST_TOOLS; then
          run_cmd winget install --id LLVM.LLVM -e --silent --accept-package-agreements --accept-source-agreements
          run_cmd winget install --id SoftwareFreedomConservancy.QEMU -e --silent --accept-package-agreements --accept-source-agreements
        fi
        if $FULL; then
          run_cmd winget install --id Racket.Racket -e --silent --accept-package-agreements --accept-source-agreements
        fi
        if $WITH_UBUNTU_VM; then
          run_cmd winget install --id Canonical.Multipass -e --silent --accept-package-agreements --accept-source-agreements
        fi
      else
        echo "Error: winget is required for automatic setup on Windows; use WSL2 or install prerequisites manually." >&2
        return 1
      fi
      echo "Restart your terminal after installs and re-run this script inside the project."
      if ! $DRY_RUN; then
        WINDOWS_RERUN_REQUIRED=true
      fi
      ;;

    *)
      echo "Unsupported platform. Install gcc/clang + make + python3 + sqlite3 + curl + git manually."
      return 2
      ;;
  esac
}

install_rust() {
  if has_cmd cargo && { ! $FULL || has_cmd rustup; }; then
    echo "Rust/Cargo already present."
  elif $DRY_RUN; then
    echo "[DRY] download the official rustup installer over TLS"
    echo "[DRY] run rustup-init -y --default-toolchain stable --no-modify-path"
  else
    echo ">>> Installing Rust (rustup)..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --no-modify-path
    # shellcheck disable=SC1090
    source "${CARGO_HOME:-$HOME/.cargo}/env" 2>/dev/null || true
    export PATH="$CARGO_BIN_DIR:$PATH"
    hash -r 2>/dev/null || true
  fi

  if has_cmd rustup || $DRY_RUN; then
    if $FULL; then
      run_cmd rustup component add rustfmt clippy
      run_cmd rustup target add wasm32-wasip1
    fi
  fi
}

install_hosted_runtime_extras() {
  if ! $WITH_HOSTED_RUNTIMES; then
    return
  fi
  if [[ "$PLATFORM" == "linux" && "$DISTRO" == "debian" ]] && \
      ! has_cmd wasmtime && ! has_cmd wasmer; then
    run_cmd cargo install --locked wasmtime-cli
  fi
}

source_nix_profile() {
  local profile
  for profile in \
    /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh \
    "$HOME/.nix-profile/etc/profile.d/nix.sh"; do
    if [[ -f "$profile" ]]; then
      set +u
      # shellcheck disable=SC1090
      source "$profile"
      set -u
      hash -r 2>/dev/null || true
      return 0
    fi
  done
  return 1
}

install_nix() {
  if ! $WITH_NIX; then
    echo ">>> Skipping Nix (select --with-nix or --full to install it)."
    return
  fi

  if $DRY_RUN; then
    local planned_nix
    planned_nix="$(find_tool_path nix 2>/dev/null || true)"
    if [[ -n "$planned_nix" ]]; then
      print_command "$planned_nix" --version
      print_command "$planned_nix" --extra-experimental-features nix-command eval --raw --expr 'toString (1 + 1)'
      return
    fi
  else
    source_nix_profile || true
  fi
  if has_cmd nix; then
    echo "Nix already present."
  elif $DRY_RUN; then
    echo "[DRY] download https://nixos.org/nix/install to a temporary file over TLS"
    if [[ "$PLATFORM" == "macos" || ( "$PLATFORM" == "linux" && -d /run/systemd/system ) ]]; then
      if $YES; then
        echo "[DRY] sh <nix-installer> --daemon --yes"
      else
        echo "[DRY] sh <nix-installer> --daemon"
      fi
    elif [[ "$PLATFORM" == "linux" ]]; then
      if $YES; then
        echo "[DRY] sh <nix-installer> --no-daemon --yes"
      else
        echo "[DRY] sh <nix-installer> --no-daemon"
      fi
    else
      echo "Error: the official Nix installer profile is supported here only on macOS and Linux." >&2
      return 2
    fi
    return
  else
    local installer
    local nix_install_args=()
    installer="$(mktemp "${TMPDIR:-/tmp}/ostadix-nix-install.XXXXXX")"
    if [[ "$PLATFORM" == "macos" || ( "$PLATFORM" == "linux" && -d /run/systemd/system ) ]]; then
      nix_install_args+=(--daemon)
    elif [[ "$PLATFORM" == "linux" ]]; then
      if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
        rm -f "$installer"
        echo "Error: refusing a root-owned single-user Nix install; configure a multi-user/systemd host or run as an unprivileged user." >&2
        return 2
      fi
      nix_install_args+=(--no-daemon)
    else
      rm -f "$installer"
      echo "Error: the official Nix installer profile is supported here only on macOS and Linux." >&2
      return 2
    fi
    if $YES; then
      nix_install_args+=(--yes)
    fi
    echo ">>> Downloading the current official Nix installer over TLS (nixos.org trust boundary)..."
    (
      trap 'rm -f "$installer"' EXIT HUP INT TERM
      curl --proto '=https' --tlsv1.2 -fsSL --retry 3 \
        https://nixos.org/nix/install -o "$installer"
      sh "$installer" "${nix_install_args[@]}"
    ) || return $?
    source_nix_profile || true
  fi

  if ! has_cmd nix; then
    echo "Error: Nix installation completed but nix is still unavailable; start a fresh shell and re-run --check." >&2
    return 1
  fi
  nix --version
  local nix_probe
  nix_probe="$(nix --extra-experimental-features nix-command eval --raw --expr 'toString (1 + 1)')"
  if [[ "$nix_probe" != "2" ]]; then
    echo "Error: network-free Nix evaluation returned an unexpected result: $nix_probe" >&2
    return 1
  fi
}

install_ubuntu_vm_tools() {
  if ! $WITH_UBUNTU_VM || [[ "$PLATFORM" == "macos" || "$PLATFORM" == "windows" ]]; then
    return
  fi
  if has_cmd multipass; then
    echo "Multipass already present."
    return
  fi
  if [[ "$PLATFORM" != "linux" ]]; then
    echo "Error: Multipass installation is supported by this script only on macOS, Linux, and Windows." >&2
    return 2
  fi
  if [[ "$DISTRO" != "debian" ]] && ! has_cmd multipass; then
    echo "Error: automatic Multipass installation is currently supported only on Debian-family Linux hosts." >&2
    return 2
  fi
  if has_cmd snap || $DRY_RUN; then
    if has_cmd systemctl || $DRY_RUN; then
      run_privileged systemctl enable --now snapd.socket
    fi
    run_privileged snap install multipass
  else
    echo "Error: --with-ubuntu-vm requires Multipass; install snapd/Multipass, then re-run --check." >&2
    return 1
  fi
}

build_rust() {
  echo ">>> Building Rust edition (--release)..."
  clean_rust_release_binaries
  local cargo_args=(build --release --locked)
  if $FULL; then
    cargo_args+=(--features notebook)
  fi
  for bin in "${RUST_BIN_TARGETS[@]}"; do
    cargo_args+=(--bin "$bin")
  done
  run_cmd cargo "${cargo_args[@]}"
  create_rust_alias_binaries
  refresh_cargo_bin_binaries
  echo "Rust build done → target/release/"
}

build_c() {
  echo ">>> Building C17 edition..."
  if [[ -d c_cpp ]]; then
    remove_managed_file "$PROJECT_ROOT/c_cpp/O"
    remove_managed_file "$PROJECT_ROOT/c_cpp/olangc"
    if $DRY_RUN; then
      echo "[DRY] Would build C17 edition in $PROJECT_ROOT/c_cpp"
      return
    fi
    local make_command="make"
    if [[ "$PLATFORM" == "bsd" ]] && has_cmd gmake; then
      make_command="gmake"
    fi
    (cd c_cpp && "$make_command" -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)" || "$make_command")
    echo "C edition done → ./c_cpp/O and ./c_cpp/olangc"
  fi
}

build_mcp_server() {
  if ! $INSTALL_MCP; then return; fi
  local mcp_dir="$PROJECT_ROOT/mcp/ostadix_lang_mcp_server"
  if [[ ! -f "$mcp_dir/Cargo.toml" ]]; then
    echo ">>> Skipping ostadix-mcp (no $mcp_dir/Cargo.toml)"
    return
  fi
  echo ">>> Building ostadix-mcp server (--release, --locked)..."
  # --locked: fail rather than silently re-resolve deps against crates.io, so
  # a build today and a build in a year off the same commit produce the same
  # binary modulo toolchain drift. This crate has its own Cargo.lock (rmcp is
  # pre-1.0 and moves fast) — deliberately not folded into the root
  # workspace so its dependency set can't bleed into the main O-lang build.
  run_cmd cargo build --release --locked --manifest-path "$mcp_dir/Cargo.toml"
  if $INSTALL_WRAPPERS && ! $DRY_RUN; then
    local mcp_bin="$mcp_dir/target/release/ostadix-mcp"
    if [[ -f "$mcp_bin" ]]; then
      mkdir -p "$HOME/.local/bin"
      remove_managed_file "$HOME/.local/bin/ostadix-mcp"
      cp "$mcp_bin" "$HOME/.local/bin/ostadix-mcp"
      chmod +x "$HOME/.local/bin/ostadix-mcp"
      echo "  wrapper → $HOME/.local/bin/ostadix-mcp"
    fi
  fi
  echo "MCP build done → $mcp_dir/target/release/ostadix-mcp"
}

setup_python() {
  echo ">>> Checking the repository-local Python reference edition..."
  if $DRY_RUN; then
    echo "[DRY] verify python3 can import the repository-local o_lang package"
    if ! $MINIMAL; then
      echo "[DRY] optional matplotlib availability check (no automatic install)"
    fi
    return
  fi
  if has_cmd python3; then
    if [[ -f o_lang/__init__.py ]]; then
      PYTHONPATH="$PROJECT_ROOT${PYTHONPATH:+:$PYTHONPATH}" \
        python3 -c 'import o_lang; print("  Python reference edition", o_lang.__version__)'
    fi
    if ! $MINIMAL && ! python3 -c 'import matplotlib' >/dev/null 2>&1; then
      echo "  matplotlib is optional for computed_plot.O; install it explicitly if needed"
    fi
  fi
}

create_wrappers() {
  if ! $INSTALL_WRAPPERS; then return; fi
  echo ">>> Creating convenience wrappers in ~/.local/bin (for runnable form)..."
  local BIN_DIR="$HOME/.local/bin"

  if $DRY_RUN; then
    echo "[DRY] mkdir -p $BIN_DIR"
    for wrapper in "${WRAPPER_TARGETS[@]}"; do
      remove_managed_file "$BIN_DIR/$wrapper"
      if [[ "$wrapper" == "o-notebook" ]]; then
        echo "[DRY] recreate wrapper $BIN_DIR/$wrapper if target/release/o-notebook is built"
      else
        echo "[DRY] recreate wrapper $BIN_DIR/$wrapper"
      fi
    done
    echo "[DRY] replace $BIN_DIR/$EVALUATOR_ALIAS from $PROJECT_ROOT/target/release/O"
    return
  fi

  mkdir -p "$BIN_DIR"

  for wrapper in "${WRAPPER_TARGETS[@]}"; do
    remove_managed_file "$BIN_DIR/$wrapper"
  done
  remove_managed_file "$BIN_DIR/$EVALUATOR_ALIAS"

  # Stable native evaluator identity for placement fingerprinting. This name
  # cannot collide with the O/o dispatcher on case-insensitive filesystems.
  cp "$PROJECT_ROOT/target/release/O" "$BIN_DIR/$EVALUATOR_ALIAS"
  chmod +x "$BIN_DIR/$EVALUATOR_ALIAS"

  # Rust evaluator (prefers release).
  cat > "$BIN_DIR/O" <<WRAP
#!/usr/bin/env bash
export O_BACKENDS_DIR="\${O_BACKENDS_DIR:-$PROJECT_ROOT/backends}"
exec "$PROJECT_ROOT/target/release/O" "\$@"
WRAP
  chmod +x "$BIN_DIR/O"

  # Lowercase `o` preserves evaluator compatibility and owns repo subcommands.
  # The installer also preserves uppercase evaluator behavior when O/o share
  # one filesystem entry on a case-insensitive host.
  "$PROJECT_ROOT/scripts/install-o-cli-wrapper.sh" "$BIN_DIR/o"

  cat > "$BIN_DIR/olangc" <<WRAP
#!/usr/bin/env bash
exec "$PROJECT_ROOT/target/release/olangc" "\$@"
WRAP
  chmod +x "$BIN_DIR/olangc"

  if [[ -x "$PROJECT_ROOT/target/release/o-notebook" ]]; then
    cat > "$BIN_DIR/o-notebook" <<WRAP
#!/usr/bin/env bash
export O_BACKENDS_DIR="\${O_BACKENDS_DIR:-$PROJECT_ROOT/backends}"
exec "$PROJECT_ROOT/target/release/o-notebook" "\$@"
WRAP
    chmod +x "$BIN_DIR/o-notebook"
  fi

  # C edition (often lighter)
  cat > "$BIN_DIR/o-c" <<WRAP
#!/usr/bin/env bash
BACKENDS_DIR="\${BACKENDS_DIR:-$PROJECT_ROOT/backends}"
exec "$PROJECT_ROOT/c_cpp/O" "\$@" "\$BACKENDS_DIR"
WRAP
  chmod +x "$BIN_DIR/o-c"

  cat > "$BIN_DIR/olangc-c" <<WRAP
#!/usr/bin/env bash
exec "$PROJECT_ROOT/c_cpp/olangc" "\$@"
WRAP
  chmod +x "$BIN_DIR/olangc-c"

  echo "Wrappers installed to $BIN_DIR"
  echo "Add to your shell rc if needed:"
  echo '  export PATH="$HOME/.local/bin:$PATH"'
}

find_tool_path() {
  local tool="$1"
  local resolved candidate
  resolved="$(command -v "$tool" 2>/dev/null || true)"
  if [[ -n "$resolved" ]]; then
    printf '%s\n' "$resolved"
    return 0
  fi
  for candidate in \
    "/opt/homebrew/opt/llvm/bin/$tool" \
    "/usr/local/opt/llvm/bin/$tool" \
    "/nix/var/nix/profiles/default/bin/$tool" \
    "$HOME/.nix-profile/bin/$tool" \
    "/opt/homebrew/bin/$tool" \
    "/usr/local/bin/$tool"; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

find_lld_path() {
  local candidate rust_sysroot rust_host
  if [[ -n "${OCORE_LLD:-}" && -x "${OCORE_LLD:-}" ]]; then
    printf '%s\n' "$OCORE_LLD"
    return 0
  fi
  if has_cmd rustc; then
    rust_sysroot="$(rustc --print sysroot 2>/dev/null || true)"
    rust_host="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"
    candidate="$rust_sysroot/lib/rustlib/$rust_host/bin/rust-lld"
    if [[ -n "$rust_sysroot" && -n "$rust_host" && -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  fi
  for candidate in \
    /opt/homebrew/opt/lld/bin/ld.lld \
    /opt/homebrew/opt/llvm/bin/ld.lld \
    /usr/local/opt/lld/bin/ld.lld \
    /usr/local/opt/llvm/bin/ld.lld; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  find_tool_path ld.lld || find_tool_path lld
}

CHECK_FAILURES=0

check_any_tool() {
  local label="$1"
  shift
  local tool path
  for tool in "$@"; do
    path="$(find_tool_path "$tool" 2>/dev/null || true)"
    if [[ -n "$path" ]]; then
      printf '  [ok] %-28s %s\n' "$label" "$path"
      return 0
    fi
  done
  printf '  [missing] %s (%s)\n' "$label" "$*" >&2
  ((CHECK_FAILURES+=1))
  return 0
}

check_lld() {
  local path
  path="$(find_lld_path 2>/dev/null || true)"
  if [[ -n "$path" ]]; then
    printf '  [ok] %-28s %s\n' "LLD linker" "$path"
  else
    echo "  [missing] LLD linker (rust-lld, ld.lld, or lld)" >&2
    ((CHECK_FAILURES+=1))
  fi
}

check_clang_target() {
  local target="$1"
  local clang_path temp_object
  clang_path="$(find_tool_path clang 2>/dev/null || true)"
  if [[ -z "$clang_path" ]]; then
    printf '  [missing] Clang target %s (clang unavailable)\n' "$target" >&2
    ((CHECK_FAILURES+=1))
    return
  fi
  temp_object="$(mktemp "${TMPDIR:-/tmp}/ostadix-clang-probe.XXXXXX")"
  if printf '.text\n.globl _start\n_start:\n' |
      "$clang_path" --target="$target" -x assembler -c -o "$temp_object" - >/dev/null 2>&1; then
    printf '  [ok] %-28s %s\n' "Clang target" "$target"
  else
    printf '  [missing] Clang cannot assemble target %s\n' "$target" >&2
    ((CHECK_FAILURES+=1))
  fi
  rm -f "$temp_object"
}

load_managed_environment() {
  if [[ -f "$ENV_FILE" ]]; then
    set +u
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set -u
    hash -r 2>/dev/null || true
  fi
}

check_c_header() {
  local label="$1"
  local header="$2"
  local compiler temp_object
  compiler="$(find_tool_path cc 2>/dev/null || find_tool_path clang 2>/dev/null || find_tool_path gcc 2>/dev/null || true)"
  if [[ -z "$compiler" ]]; then
    printf '  [missing] %s (no C compiler)\n' "$label" >&2
    ((CHECK_FAILURES+=1))
    return
  fi
  temp_object="$(mktemp "${TMPDIR:-/tmp}/ostadix-header-probe.XXXXXX")"
  if printf '#include <%s>\nint main(void) { return 0; }\n' "$header" |
      "$compiler" -x c -c -o "$temp_object" - >/dev/null 2>&1; then
    printf '  [ok] %-28s %s\n' "$label" "$header"
  else
    printf '  [missing] %s (%s)\n' "$label" "$header" >&2
    ((CHECK_FAILURES+=1))
  fi
  rm -f "$temp_object"
}

check_capabilities() {
  CHECK_FAILURES=0
  echo ">>> Checking selected capabilities..."
  check_any_tool "Rust compiler" rustc
  check_any_tool "Cargo" cargo
  check_any_tool "C compiler" cc clang gcc
  check_any_tool "Make" make gmake
  check_any_tool "Python 3" python3
  check_any_tool "Git" git
  check_any_tool "curl" curl
  check_any_tool "SQLite CLI" sqlite3

  if $FULL; then
    check_any_tool "Racket backend" racket
    if has_cmd rustup && rustup target list --installed | grep -Fxq wasm32-wasip1; then
      echo "  [ok] Rust WASI target             wasm32-wasip1"
    else
      echo "  [missing] Rust WASI target (wasm32-wasip1)" >&2
      ((CHECK_FAILURES+=1))
    fi
  fi

  if $WITH_NIX; then
    local nix_path nix_probe
    nix_path="$(find_tool_path nix 2>/dev/null || true)"
    check_any_tool "Nix" nix
    if [[ -n "$nix_path" ]]; then
      nix_probe="$("$nix_path" --extra-experimental-features nix-command eval --raw --expr 'toString (1 + 1)' 2>/dev/null || true)"
      if [[ "$nix_probe" == "2" ]]; then
        echo "  [ok] Nix evaluation              network-free 1 + 1"
      else
        echo "  [missing] Nix network-free evaluation failed" >&2
        ((CHECK_FAILURES+=1))
      fi
    fi
  fi

  if $WITH_OCORE; then
    check_any_tool "Clang" clang
    check_lld
    check_any_tool "ELF file inspector" file
    check_any_tool "nm" nm llvm-nm
    check_any_tool "objdump" objdump llvm-objdump
    check_any_tool "LLVM objdump" llvm-objdump
    check_any_tool "SHA-256 harness" shasum
    check_any_tool "CMake/CTest" cmake
    check_any_tool "x86_64 QEMU" qemu-system-x86_64
    check_any_tool "AArch64 QEMU" qemu-system-aarch64
    check_clang_target x86_64-unknown-none-elf
    check_clang_target aarch64-unknown-none-elf
  fi

  if $WITH_OCORE_MEDIA; then
    check_any_tool "GRUB x86_64 EFI builder" x86_64-elf-grub-mkstandalone grub-mkstandalone
    check_any_tool "FAT formatter" mformat
    check_any_tool "FAT copier" mcopy
    check_any_tool "committed source snapshot extractor" tar
    local firmware_candidate=""
    for firmware_candidate in \
      "${OSTADIX_OVMF_CODE:-}" \
      /opt/homebrew/opt/qemu/share/qemu/edk2-x86_64-code.fd \
      /usr/local/opt/qemu/share/qemu/edk2-x86_64-code.fd \
      /usr/share/OVMF/OVMF_CODE.fd \
      /usr/share/edk2/x64/OVMF_CODE.fd; do
      if [[ -f "$firmware_candidate" ]]; then
        printf '  [ok] %-28s %s\n' "OVMF/edk2 firmware" "$firmware_candidate"
        break
      fi
    done
    if [[ ! -f "$firmware_candidate" ]]; then
      echo "  [missing] OVMF/edk2 x86_64 code firmware" >&2
      ((CHECK_FAILURES+=1))
    fi
  fi

  if $WITH_GUEST_TOOLS; then
    check_any_tool "QEMU image tool" qemu-img
    check_any_tool "gzip" gzip
    check_any_tool "xz" xz
    check_any_tool "zstd" zstd
  fi

  if $WITH_HOSTED_RUNTIMES; then
    check_any_tool "Node.js" node nodejs
    check_any_tool "Ruby" ruby
    check_any_tool "Racket" racket
    check_any_tool "Haskell" runghc ghc
    check_any_tool "OCaml" ocaml ocamlopt ocamlc
    check_any_tool "Common Lisp" sbcl clisp
    if [[ -n "$(find_tool_path dotnet 2>/dev/null || true)" ]]; then
      check_any_tool "C# runtime" dotnet
    else
      check_any_tool "C# compiler" mcs
      check_any_tool "Mono runtime" mono
    fi
    check_any_tool "GNU Octave" octave
    check_any_tool "WebAssembly assembler" wat2wasm
    check_any_tool "WebAssembly runtime" wasmtime wasmer
    echo "  [excluded] Java backend (local no-JRE policy)"
    echo "  [excluded] MATLAB/Wolfram licensed runtimes (Octave covers MATLAB-compatible code)"
  fi

  if $WITH_UBUNTU_VM; then
    check_any_tool "Multipass" multipass
  fi

  if $WITH_LINUX_KERNEL_TOOLS; then
    check_any_tool "bc" bc
    check_any_tool "Bison" bison
    check_any_tool "Flex" flex
    check_any_tool "OpenSSL" openssl
    check_any_tool "pahole" pahole
    check_any_tool "cpio" cpio
    check_any_tool "rsync" rsync
    check_any_tool "kmod" kmod modprobe
    check_c_header "libelf headers" libelf.h
    check_c_header "OpenSSL headers" openssl/ssl.h
    check_c_header "ncurses headers" ncurses.h
  fi

  if [[ "$CHECK_FAILURES" -gt 0 ]]; then
    echo "Capability check: $CHECK_FAILURES missing requirement(s)." >&2
    return 1
  fi
  echo "Capability check: PASS"
}

render_environment_file() {
  printf '# Managed by Ostadix-lang setup.sh. Regenerated during setup.\n'
  printf 'export O_LANG_ROOT=%q\n' "$PROJECT_ROOT"
  cat <<'EOF'
export O_BACKENDS_DIR="$O_LANG_ROOT/backends"

# Activate either a daemon or single-user Nix installation when present.
if [ -f /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]; then
  . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
elif [ -f "$HOME/.nix-profile/etc/profile.d/nix.sh" ]; then
  . "$HOME/.nix-profile/etc/profile.d/nix.sh"
fi

_ostadix_prepend_path() {
  case ":$PATH:" in
    *":$1:"*) ;;
    *) PATH="$1:$PATH" ;;
  esac
}
_ostadix_prepend_path "$O_LANG_ROOT/target/release"
_ostadix_prepend_path "${CARGO_HOME:-$HOME/.cargo}/bin"
_ostadix_prepend_path "$HOME/.local/bin"
for _ostadix_llvm_bin in \
  /usr/local/opt/llvm/bin \
  /opt/homebrew/opt/llvm/bin \
  /usr/local/opt/lld/bin \
  /opt/homebrew/opt/lld/bin; do
  [ -d "$_ostadix_llvm_bin" ] && _ostadix_prepend_path "$_ostadix_llvm_bin"
done
export PATH

if [ -z "${OCORE_LLD:-}" ]; then
  for _ostadix_lld in \
    /opt/homebrew/opt/lld/bin/ld.lld \
    /opt/homebrew/opt/llvm/bin/ld.lld \
    /usr/local/opt/lld/bin/ld.lld \
    /usr/local/opt/llvm/bin/ld.lld; do
    if [ -x "$_ostadix_lld" ]; then
      export OCORE_LLD="$_ostadix_lld"
      break
    fi
  done
fi
unset _ostadix_llvm_bin _ostadix_lld
unset -f _ostadix_prepend_path 2>/dev/null || true
EOF
  printf 'if [ -z "${OSTADIX_GUESTS_DIR:-}" ]; then export OSTADIX_GUESTS_DIR=%q; fi\n' "$GUESTS_DIR"
}

write_environment_file() {
  if ! $WRITE_ENV; then
    return
  fi
  local env_dir temp_env
  env_dir="$(dirname "$ENV_FILE")"
  if [[ -e "$ENV_FILE" && ! -f "$ENV_FILE" ]]; then
    echo "Error: managed environment path is not a regular file: $ENV_FILE" >&2
    return 1
  fi
  if [[ -f "$ENV_FILE" ]] &&
      ! grep -Fq '# Managed by Ostadix-lang setup.sh.' "$ENV_FILE"; then
    echo "Error: refusing to overwrite unmanaged environment file: $ENV_FILE" >&2
    return 1
  fi
  if $DRY_RUN; then
    echo "[DRY] write managed environment file: $ENV_FILE"
    render_environment_file | sed 's/^/[DRY] | /'
    return
  fi
  mkdir -p "$env_dir"
  temp_env="$(mktemp "$env_dir/.ostadix-env.XXXXXX")"
  if ! render_environment_file > "$temp_env"; then
    rm -f "$temp_env"
    return 1
  fi
  chmod 0644 "$temp_env"
  mv -f "$temp_env" "$ENV_FILE"
  echo "Managed environment → $ENV_FILE"
}

resolve_shell_rc() {
  if [[ -n "${OSTADIX_SHELL_RC:-}" ]]; then
    printf '%s\n' "$OSTADIX_SHELL_RC"
    return
  fi
  case "$(basename "${SHELL:-}")" in
    bash) printf '%s\n' "$HOME/.bashrc" ;;
    zsh) printf '%s\n' "${ZDOTDIR:-$HOME}/.zshrc" ;;
    *)
      echo "Error: --persist-env supports bash and zsh; set OSTADIX_SHELL_RC explicitly for another compatible shell." >&2
      return 2
      ;;
  esac
}

preflight_setup() {
  if $WITH_LINUX_KERNEL_TOOLS && [[ "$PLATFORM" != "linux" ]]; then
    echo "Error: --with-linux-kernel-tools is supported only on a Linux host." >&2
    return 2
  fi
  if $WRITE_ENV && ! $CHECK_ONLY; then
    if [[ -e "$ENV_FILE" && ! -f "$ENV_FILE" ]]; then
      echo "Error: managed environment path is not a regular file: $ENV_FILE" >&2
      return 1
    fi
    if [[ -f "$ENV_FILE" ]] &&
        ! grep -Fq '# Managed by Ostadix-lang setup.sh.' "$ENV_FILE"; then
      echo "Error: refusing to overwrite unmanaged environment file: $ENV_FILE" >&2
      return 1
    fi
  fi
  if $PERSIST_ENV; then
    local shell_rc env_quoted source_line expected_block actual_block
    shell_rc="$(resolve_shell_rc)"
    printf -v env_quoted '%q' "$ENV_FILE"
    source_line="[ -f $env_quoted ] && . $env_quoted"
    expected_block="$(printf '%s\n%s\n%s' '# >>> Ostadix environment >>>' "$source_line" '# <<< Ostadix environment <<<')"
    if [[ -f "$shell_rc" ]] && grep -Fq '# >>> Ostadix environment >>>' "$shell_rc"; then
      actual_block="$(sed -n '/^# >>> Ostadix environment >>>$/,/^# <<< Ostadix environment <<<$/{p;}' "$shell_rc")"
      if [[ "$actual_block" != "$expected_block" ]]; then
        echo "Error: found an incomplete or different Ostadix block in $shell_rc; edit it manually." >&2
        return 1
      fi
    fi
  fi
}

persist_environment_hook() {
  if ! $PERSIST_ENV; then
    return
  fi
  local shell_rc env_quoted source_line begin_marker end_marker
  begin_marker="# >>> Ostadix environment >>>"
  end_marker="# <<< Ostadix environment <<<"
  shell_rc="$(resolve_shell_rc)"
  printf -v env_quoted '%q' "$ENV_FILE"
  source_line="[ -f $env_quoted ] && . $env_quoted"

  if [[ -f "$shell_rc" ]] && grep -Fq "$begin_marker" "$shell_rc"; then
    if grep -Fq "$source_line" "$shell_rc" && grep -Fq "$end_marker" "$shell_rc"; then
      echo "Shell environment hook already present → $shell_rc"
      return
    fi
    echo "Error: found an incomplete or different Ostadix block in $shell_rc; edit it manually." >&2
    return 1
  fi
  if $DRY_RUN; then
    echo "[DRY] append managed environment hook to $shell_rc"
    printf '[DRY] | %s\n[DRY] | %s\n[DRY] | %s\n' \
      "$begin_marker" "$source_line" "$end_marker"
    return
  fi
  mkdir -p "$(dirname "$shell_rc")"
  {
    [[ ! -s "$shell_rc" ]] || printf '\n'
    printf '%s\n%s\n%s\n' "$begin_marker" "$source_line" "$end_marker"
  } >> "$shell_rc"
  echo "Shell environment hook → $shell_rc"
}

prepare_guest_lab() {
  if ! $WITH_GUEST_TOOLS; then
    return
  fi
  run_cmd mkdir -p "$GUESTS_DIR"
  echo "Guest lab directory: $GUESTS_DIR"
  echo "  Supply checksum-pinned Linux/9front/OpenBSD media explicitly."
  echo "  No foreign OS image is downloaded or booted by setup.sh."
}

verify_ocore() {
  if ! $VERIFY_OCORE; then
    return
  fi
  if $DRY_RUN; then
    echo "[DRY] $PROJECT_ROOT/ocore/kernel/smoke-qemu.sh"
    return
  fi
  echo
  echo ">>> Verifying bounded x86_64 O-core boot under QEMU/TCG..."
  "$PROJECT_ROOT/ocore/kernel/smoke-qemu.sh"
}

verify_runnable() {
  if ! $VERIFY; then return; fi
  if $DRY_RUN; then
    echo "[DRY] Would verify Rust, Rust-native backends, C, AOT, and Python runnable forms"
    return
  fi
  echo
  echo ">>> Verifying runnable forms (this may take a moment)..."
  local ok=0 fail=0
  local missing_shims="/tmp/o-no-such-backends-setup-$$"
  local verify_bin="/tmp/verify-o-rust-$$"
  local hosted_verify_dir
  hosted_verify_dir="$(mktemp -d "${TMPDIR:-/tmp}/ostadix-hosted-verify.XXXXXX")"
  rm -rf "$missing_shims"
  rm -f "$verify_bin"

  echo -n "Rust release interpreter: "
  if target/release/O examples/hello.O 2>/dev/null | grep -qE "(2|Int)"; then echo "OK"; ((ok+=1)); else echo "FAIL"; ((fail+=1)); fi

  echo -n "Installed O on PATH: "
  if has_cmd O && "$(command -v O)" examples/bindings.O 2>/dev/null | grep -q "43"; then echo "OK"; ((ok+=1)); else echo "FAIL"; ((fail+=1)); fi

  echo -n "Installed hosted node/client CLIs: "
  if [[ -x "$CARGO_BIN_DIR/o-node" && -x "$CARGO_BIN_DIR/octl" ]] \
      && "$CARGO_BIN_DIR/o-node" serve --help >/dev/null 2>&1 \
      && "$CARGO_BIN_DIR/octl" node session --help >/dev/null 2>&1; then
    echo "OK"; ((ok+=1))
  else
    echo "FAIL"; ((fail+=1))
  fi

  echo -n "Hosted V2 identity and development-mTLS preflight: "
  if (
      trap 'rm -rf -- "$hosted_verify_dir"' EXIT HUP INT TERM
      "$CARGO_BIN_DIR/o-node" pki init \
        --directory "$hosted_verify_dir/pki" >/dev/null 2>&1 \
      && "$CARGO_BIN_DIR/o-node" identity init \
        --state-dir "$hosted_verify_dir/state" >/dev/null 2>&1 \
      && "$CARGO_BIN_DIR/octl" node session principal \
        --cert "$hosted_verify_dir/pki/client-cert.pem" \
        >"$hosted_verify_dir/principal.sha256" 2>/dev/null \
      && grep -Eq '^[0-9a-f]{64}$' "$hosted_verify_dir/principal.sha256" \
      && [[ -s "$hosted_verify_dir/state/node-signing-key.v2" \
         && -s "$hosted_verify_dir/state/node-signing-public.v2" ]]
    ); then
    echo "OK"; ((ok+=1))
  else
    echo "FAIL"; ((fail+=1))
  fi

  echo -n "Standalone O binary: "
  local standalone_bin
  standalone_bin="$(mktemp "${TMPDIR:-/tmp}/o-standalone-verify.XXXXXX")"
  rm -f "$standalone_bin"
  if cp target/release/O "$standalone_bin" && chmod +x "$standalone_bin" && (cd /tmp && "$standalone_bin" "$PROJECT_ROOT/examples/bindings.O" 2>/dev/null) | grep -q "43"; then
    echo "OK"
    ((ok+=1))
  else
    echo "FAIL"
    ((fail+=1))
  fi
  rm -f "$standalone_bin" 2>/dev/null || true

  echo -n "Rust-native Bash without shim dir: "
  if target/release/O examples/bash_hello.O "$missing_shims" 2>/dev/null | grep -q "hello from bash"; then echo "OK"; ((ok+=1)); else echo "FAIL"; ((fail+=1)); fi

  echo -n "Rust-native SQL without shim dir: "
  if target/release/O examples/sql_select.O "$missing_shims" 2>/dev/null | grep -q "2"; then echo "OK"; ((ok+=1)); else echo "FAIL"; ((fail+=1)); fi

  echo -n "Rust AOT (olangc): "
  if target/release/olangc examples/bash_hello.O -o "$verify_bin" >/dev/null 2>&1 && (cd /tmp && "$verify_bin" 2>/dev/null) | grep -q "hello from bash"; then
    echo "OK"; ((ok+=1))
  else
    echo "FAIL"; ((fail+=1))
  fi
  rm -f "$verify_bin" 2>/dev/null || true

  echo -n "C interp: "
  if [[ -x ./c_cpp/O ]] && ./c_cpp/O examples/hello.O ./backends 2>/dev/null | grep -q "2"; then echo "OK"; ((ok+=1)); else echo "FAIL"; ((fail+=1)); fi

  echo -n "C AOT (olangc): "
  if [[ -x ./c_cpp/olangc ]] && ./c_cpp/olangc examples/trailing_expr.O -o "$verify_bin" 2>&1 | tail -1 >/dev/null && "$verify_bin" 2>/dev/null | grep -q "42"; then
    echo "OK"; ((ok+=1))
  else
    echo "FAIL"; ((fail+=1))
  fi
  rm -f "$verify_bin" 2>/dev/null || true

  echo -n "Python: "
  if python3 -m o_lang examples/hello.O 2>/dev/null | grep -q "2"; then echo "OK"; ((ok+=1)); else echo "FAIL"; ((fail+=1)); fi

  echo "Verification: $ok passed, $fail failed."
  if [[ $fail -gt 0 ]]; then
    echo "Some verifications failed. Check output above." >&2
    return 1
  fi
}

# --- Main flow ---
preflight_setup

if $CHECK_ONLY; then
  check_capabilities
  exit $?
fi

install_system_deps
if $WINDOWS_RERUN_REQUIRED; then
  echo "Setup paused after winget installation. Restart the terminal and re-run setup.sh."
  exit 0
fi
install_rust
install_hosted_runtime_extras
install_ubuntu_vm_tools

export PATH="$CARGO_BIN_DIR:$PATH"
hash -r 2>/dev/null || true

install_nix
prepare_guest_lab
write_environment_file
if $WRITE_ENV && ! $DRY_RUN; then
  load_managed_environment
fi

if ! $DRY_RUN; then
  check_capabilities
fi
persist_environment_hook

if ! $DEPS_ONLY; then
  build_rust
  build_c
  build_mcp_server
  setup_python
  create_wrappers
  verify_runnable
  verify_ocore
fi

echo
if $DRY_RUN; then
  echo "=== Dry run complete. No changes were made. ==="
elif $DEPS_ONLY; then
  echo "=== Dependency and environment setup complete. Ostadix builds were skipped. ==="
else
  echo "=== All done! O-lang is set up and runnable on this machine. ==="
fi
echo
echo "Quick starts:"
echo "  o examples/hello.O                    # Rust (if wrapper installed)"
echo "  o-c examples/hello.O                  # C edition"
echo "  cargo run -- examples/hello.O"
echo "  ./c_cpp/O examples/hello.O ./backends"
echo "  ./c_cpp/olangc examples/hello.O -o /tmp/h && /tmp/h"
echo "  python3 -m o_lang examples/hello.O"
echo "  ostadix-mcp                           # MCP stdio server (includes o_runtimes, o_analyze_intent, o_execute_intent, o_run, and diagnostics)"
if $WRITE_ENV; then
  printf '  source %q                         # activate O_LANG_ROOT/tool paths now\n' "$ENV_FILE"
fi
if $WITH_OCORE; then
  echo "  ./setup.sh --with-ocore --check       # inspect native/QEMU capabilities"
  echo "  ./setup.sh --with-ocore --verify-ocore # bounded x86_64 QEMU/TCG smoke"
fi
if $WITH_OCORE_MEDIA; then
  echo "  o kernel media                         # deterministic GPT/UEFI image"
  echo "  o kernel smoke-media                   # exact image under OVMF/QEMU"
  echo "  o kernel prepare-write --image IMAGE --device DEVICE"
  echo "  # QEMU validation is not physical-machine or SMP evidence."
fi
if $WITH_GUEST_TOOLS; then
  echo "  Guest tools are for explicit, user-supplied media under:"
  echo "    $GUESTS_DIR"
  echo "  They do not establish Linux, Plan 9, or OpenBSD support in O-core."
fi
echo
echo "For clean testing in docker (as mentioned in history):"
echo '  docker run -it -v "$PWD:/ws" -w /ws debian bash -c "apt-get update && apt-get install -y sudo curl && ./setup.sh --minimal --verify"'
echo

echo
echo "Compatibility per-OS entrypoints delegate to this canonical setup in ./setup/os/:"
echo "  setup-macos.sh, setup-debian.sh, setup-arch.sh (incl. CachyOS), setup-fedora.sh,"
echo "  setup-gentoo.sh, setup-nixos.sh, setup-tinycore.sh, setup-alpine.sh, setup-opensuse.sh,"
echo "  setup-void.sh, setup-freebsd.sh, setup-windows.sh"
echo "  See ./setup/os/README.md for details and usage."
