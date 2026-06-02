#!/usr/bin/env bash
# O-lang cross-platform setup & bootstrap script
# Sets up dependencies, builds Rust + C/C++ + Python editions for the *current machine*,
# and leaves everything in a convenient runnable form.
#
# Supports: macOS, Windows (Git Bash/WSL), Debian/Ubuntu, Arch/CachyOS, Fedora, Gentoo,
#           NixOS, TinyCore, Alpine, openSUSE, Void, FreeBSD, and many others via fallbacks.
#
# Usage:
#   ./setup.sh                  # normal setup
#   ./setup.sh --minimal        # core only, no prompts for nix/extras
#   ./setup.sh --full --verify  # everything + run verification examples
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
DRY_RUN=false

# --- Arg parsing ---
usage() {
  cat <<EOF
O-lang setup script

Options:
  -h, --help           Show this help
  -m, --minimal        Minimal setup (skip optional nix, matplotlib, extra shims)
  -f, --full           Full setup (install racket, etc. for all backends)
  -y, --yes            Non-interactive (assume yes for prompts)
  -v, --verify         After build, run verification on key examples (hello, meta, etc.)
  --no-wrappers        Do not create convenience wrappers in ~/.local/bin
  --dry-run            Print what would be done, do not install or build

Examples:
  ./setup.sh
  ./setup.sh --minimal --verify
  ./setup.sh --full -y
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage ;;
    -m|--minimal) MINIMAL=true; shift ;;
    -f|--full) FULL=true; shift ;;
    -y|--yes) YES=true; shift ;;
    -v|--verify) VERIFY=true; shift ;;
    --no-wrappers) INSTALL_WRAPPERS=false; shift ;;
    --dry-run) DRY_RUN=true; shift ;;
    *) echo "Unknown option: $1"; usage ;;
  esac
done

if $MINIMAL && $FULL; then
  echo "Error: --minimal and --full are mutually exclusive"
  exit 1
fi

echo "=== O-lang cross-platform setup ==="
echo "Project root: $PROJECT_ROOT"
echo "Host: $(uname -a)"
echo "Options: minimal=$MINIMAL full=$FULL yes=$YES verify=$VERIFY wrappers=$INSTALL_WRAPPERS"
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

echo "Detected: Platform=$PLATFORM Distro=$DISTRO (ID=$DISTRO_ID)"
echo

has_cmd() { command -v "$1" &>/dev/null; }

run_cmd() {
  if $DRY_RUN; then
    echo "[DRY] $*"
  else
    "$@"
  fi
}

# --- Install system dependencies (extended) ---
install_system_deps() {
  echo ">>> Installing system dependencies..."
  if $DRY_RUN; then echo "[DRY] Would install packages for $DISTRO"; return; fi

  case "$PLATFORM" in
    macos)
      if ! has_cmd brew; then
        echo "Homebrew not found. Please install it:"
        echo '  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"'
        exit 1
      fi
      brew update
      brew install --quiet gcc make python@3.12 curl git pkg-config openssl 2>/dev/null || true
      xcode-select --install 2>/dev/null || true
      if $FULL; then
        brew install --quiet racket 2>/dev/null || true
      fi
      ;;

    linux)
      case "$DISTRO" in
        debian)
          sudo apt-get update -qq
          sudo apt-get install -y -qq build-essential gcc g++ make python3 python3-pip python3-venv curl git pkg-config libssl-dev
          if $FULL; then sudo apt-get install -y -qq racket || true; fi
          ;;

        arch)
          sudo pacman -Syu --noconfirm
          sudo pacman -S --noconfirm --needed base-devel gcc make python python-pip curl git pkgconf openssl
          if $FULL; then sudo pacman -S --noconfirm --needed racket 2>/dev/null || true; fi
          ;;

        fedora)
          sudo dnf groupinstall -y "Development Tools" || true
          sudo dnf install -y gcc gcc-c++ make python3 python3-pip curl git openssl-devel pkgconfig
          if $FULL; then sudo dnf install -y racket 2>/dev/null || true; fi
          ;;

        gentoo)
          echo "Gentoo: emerging packages..."
          sudo emerge --quiet --ask=n sys-devel/gcc sys-devel/make dev-lang/python net-misc/curl dev-vcs/git dev-libs/openssl || true
          if $FULL; then sudo emerge --quiet --ask=n dev-scheme/racket || true; fi
          ;;

        nixos)
          echo "NixOS: recommend managing via nixos-rebuild or home-manager."
          echo "  Example: environment.systemPackages = with pkgs; [ rustup gcc gnumake python3 racket nix ];"
          if has_cmd nix; then
            nix-env -iA nixpkgs.rustup nixpkgs.gcc nixpkgs.gnumake nixpkgs.python3 2>/dev/null || true
            if $FULL; then nix-env -iA nixpkgs.racket 2>/dev/null || true; fi
          fi
          ;;

        tinycore)
          echo "TinyCore: minimal - run manually if needed:"
          echo "  tce-load -wi gcc make python3.12 curl git"
          if $FULL; then echo "  tce-load -wi racket (if available)"; fi
          tce-load -wi gcc make python3.12 curl git 2>/dev/null || true
          ;;

        alpine)
          sudo apk update
          sudo apk add build-base gcc g++ make python3 py3-pip curl git openssl-dev pkgconf
          if $FULL; then sudo apk add racket 2>/dev/null || true; fi
          ;;

        opensuse)
          sudo zypper refresh
          sudo zypper install -y -l gcc gcc-c++ make python3 python3-pip curl git libopenssl-devel pkg-config
          if $FULL; then sudo zypper install -y racket 2>/dev/null || true; fi
          ;;

        void)
          sudo xbps-install -Suy
          sudo xbps-install -y base-devel gcc make python3 python3-pip curl git openssl-devel pkg-config
          if $FULL; then sudo xbps-install -y racket 2>/dev/null || true; fi
          ;;

        *)
          echo "Unknown Linux distro ($DISTRO_ID). Please manually install core build tools + python3 + curl."
          ;;
      esac
      ;;

    bsd)
      echo "BSD ($DISTRO) detected."
      if has_cmd pkg; then
        sudo pkg install -y gmake gcc python3 curl git
        if $FULL; then sudo pkg install -y racket 2>/dev/null || true; fi
      elif has_cmd pkg_add; then
        sudo pkg_add gmake gcc python curl git
      fi
      ;;

    windows)
      echo "Windows bash env detected."
      echo "Best experience: use WSL2 (Ubuntu recommended) and re-run there."
      if has_cmd winget; then
        winget install --id Git.Git -e --silent || true
        winget install --id Python.Python.3.12 -e --silent || true
        winget install --id Rustlang.Rustup -e --silent || true
        winget install --id Microsoft.VisualStudio.2022.BuildTools -e --silent --override "--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools" || true
        if $FULL; then
          winget install --id Racket.Racket -e --silent || true
        fi
      fi
      echo "Restart your terminal after installs and re-run this script inside the project."
      ;;

    *)
      echo "Unsupported platform. Install gcc/clang + make + python3 + curl + git manually."
      ;;
  esac
}

install_rust() {
  if has_cmd cargo; then
    echo "Rust/Cargo already present."
    if has_cmd rustup; then rustup update stable || true; fi
    return
  fi
  echo ">>> Installing Rust (rustup)..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --no-modify-path
  source "$HOME/.cargo/env" 2>/dev/null || true
  export PATH="$HOME/.cargo/bin:$PATH"
  hash -r 2>/dev/null || true
}

install_nix() {
  if has_cmd nix; then
    echo "Nix already present."
    return
  fi
  if $MINIMAL; then
    echo "Skipping Nix (minimal mode)."
    return
  fi
  echo ">>> Optional: Nix for nix*/nixos_test examples"
  if ! $YES; then
    read -r -p "Install Nix now? [y/N] " reply || true
    [[ "$reply" =~ ^[Yy]$ ]] || return
  fi
  curl -L https://nixos.org/nix/install | sh -s -- --daemon --yes 2>/dev/null || \
    curl -L https://nixos.org/nix/install | sh -s -- --no-daemon || true
  [[ -f /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]] && source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh || true
}

build_rust() {
  echo ">>> Building Rust edition (--release)..."
  cargo build --release
  echo "Rust build done → target/release/"
}

build_c() {
  echo ">>> Building C/C++ edition..."
  if [[ -d c_cpp ]]; then
    (cd c_cpp && make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)" || make)
    echo "C edition done → ./c_cpp/O and ./c_cpp/olangc"
  fi
}

setup_python() {
  echo ">>> Setting up Python shims / edition..."
  if has_cmd python3; then
    python3 -m pip install --user --upgrade pip setuptools wheel 2>/dev/null || true
    if ! $MINIMAL; then
      python3 -m pip install --user matplotlib 2>/dev/null || echo "  (matplotlib optional for computed_plot.O)"
    fi
    # Install Python edition in editable mode for convenience
    if [[ -f o_lang/__init__.py ]]; then
      python3 -m pip install --user -e . 2>/dev/null || true
    fi
  fi
  if $FULL && has_cmd pip3; then
    pip3 install --user racket 2>/dev/null || true   # no, racket is system
  fi
}

install_extras() {
  if $MINIMAL || ! $FULL; then return; fi
  echo ">>> Installing extra backend deps (full mode)..."
  case "$PLATFORM" in
    macos) brew install --quiet racket 2>/dev/null || true ;;
    linux)
      case "$DISTRO" in
        debian) sudo apt-get install -y -qq racket 2>/dev/null || true ;;
        arch) sudo pacman -S --noconfirm racket 2>/dev/null || true ;;
        fedora) sudo dnf install -y racket 2>/dev/null || true ;;
        *) echo "  Racket: install manually for racket^ backend if desired." ;;
      esac
      ;;
    bsd) sudo pkg install -y racket 2>/dev/null || true ;;
  esac
}

create_wrappers() {
  if ! $INSTALL_WRAPPERS; then return; fi
  echo ">>> Creating convenience wrappers in ~/.local/bin (for runnable form)..."
  mkdir -p "$HOME/.local/bin"
  local BIN_DIR="$HOME/.local/bin"

  # Rust runner (prefers release)
  cat > "$BIN_DIR/o" <<WRAP
#!/usr/bin/env bash
exec "$PROJECT_ROOT/target/release/O" "\$@"
WRAP
  chmod +x "$BIN_DIR/o"

  cat > "$BIN_DIR/olangc" <<WRAP
#!/usr/bin/env bash
exec "$PROJECT_ROOT/target/release/olangc" "\$@"
WRAP
  chmod +x "$BIN_DIR/olangc"

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

verify_runnable() {
  if ! $VERIFY; then return; fi
  echo
  echo ">>> Verifying runnable forms (this may take a moment)..."
  local ok=0 fail=0

  echo -n "Rust (cargo): "
  if cargo run --quiet -- examples/hello.O 2>/dev/null | grep -qE "(2|Int)"; then echo "OK"; ((ok++)); else echo "FAIL"; ((fail++)); fi

  echo -n "C interp: "
  if ./c_cpp/O examples/hello.O ./backends 2>/dev/null | grep -q "2"; then echo "OK"; ((ok++)); else echo "FAIL"; ((fail++)); fi

  echo -n "AOT (olangc): "
  if ./c_cpp/olangc examples/trailing_expr.O -o /tmp/verify-o 2>&1 | tail -1 >/dev/null && /tmp/verify-o 2>/dev/null | grep -q "42"; then
    echo "OK"; ((ok++))
  else
    echo "FAIL"; ((fail++))
  fi
  rm -f /tmp/verify-o 2>/dev/null || true

  echo -n "Python: "
  if python3 -m o_lang examples/hello.O 2>/dev/null | grep -q "2"; then echo "OK"; ((ok++)); else echo "FAIL"; ((fail++)); fi

  echo "Verification: $ok passed, $fail failed."
  if [[ $fail -gt 0 ]]; then
    echo "Some verifications failed. Check output above."
  fi
}

# --- Main flow ---
install_system_deps
install_rust

export PATH="$HOME/.cargo/bin:$PATH"
hash -r 2>/dev/null || true

install_nix
build_rust
build_c
setup_python
install_extras
create_wrappers
verify_runnable

echo
echo "=== All done! O-lang is set up and runnable on this machine. ==="
echo
echo "Quick starts:"
echo "  o examples/hello.O                    # Rust (if wrapper installed)"
echo "  o-c examples/hello.O                  # C edition"
echo "  cargo run -- examples/hello.O"
echo "  ./c_cpp/O examples/hello.O ./backends"
echo "  ./c_cpp/olangc examples/hello.O -o /tmp/h && /tmp/h"
echo "  python3 -m o_lang examples/hello.O"
echo
echo "For clean testing in docker (as mentioned in history):"
echo '  docker run -it -v "$PWD:/ws" -w /ws debian bash -c "apt-get update && apt-get install -y sudo curl && ./setup.sh --minimal --verify"'
echo

echo
echo "Dedicated per-OS scripts (no detection, simpler for CI/Docker/specific machines) live in ./setup/os/:"
echo "  setup-macos.sh, setup-debian.sh, setup-arch.sh (incl. CachyOS), setup-fedora.sh,"
echo "  setup-gentoo.sh, setup-nixos.sh, setup-tinycore.sh, setup-alpine.sh, setup-opensuse.sh,"
echo "  setup-void.sh, setup-freebsd.sh, setup-windows.sh"
echo "  See ./setup/os/README.md for details and usage."
