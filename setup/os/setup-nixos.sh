#!/usr/bin/env bash
# O-lang setup for NixOS
set -euo pipefail
echo "=== O-lang setup for NixOS ==="
echo "NixOS is declarative. Add to your configuration.nix or use nix-env temporarily."
echo "Recommended in configuration.nix:"
cat <<NIX
  environment.systemPackages = with pkgs; [
    rustup gcc gnumake python3 sqlite
    # optional
    # nix  (already there)
  ];
NIX
if command -v nix-env >/dev/null 2>&1; then
  nix-env -iA nixpkgs.rustup nixpkgs.gcc nixpkgs.gnumake nixpkgs.python3 nixpkgs.sqlite || true
fi
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release
(cd c_cpp && make -j$(nproc || echo 4))
python3 -m pip install --user --upgrade pip 2>/dev/null || true
python3 -m pip install --user matplotlib 2>/dev/null || true
echo "=== Runnable forms ==="
echo "cargo run -- examples/hello.O"
echo "./c_cpp/O examples/hello.O ./backends"
echo "./c_cpp/olangc examples/hello.O -o /tmp/hello_c && /tmp/hello_c"
echo "python3 -m o_lang examples/hello.O"
echo "For full reproducibility, consider adding a shell.nix or flake to the project."
echo "Done."
