#!/usr/bin/env bash
# O-lang setup for Void Linux
set -euo pipefail
echo "=== O-lang setup for Void ==="
sudo xbps-install -Suy
sudo xbps-install -y base-devel gcc make python3 python3-pip curl git openssl-devel pkg-config sqlite
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
echo "Done."
