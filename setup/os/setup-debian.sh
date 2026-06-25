#!/usr/bin/env bash
# O-lang setup for Debian / Ubuntu / Linux Mint / Pop!_OS etc.
set -euo pipefail
echo "=== O-lang setup for Debian-based ==="
sudo apt-get update -qq
sudo apt-get install -y -qq build-essential gcc g++ make python3 python3-pip python3-venv curl git pkg-config libssl-dev sqlite3
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
echo "Done. Make sure ~/.cargo/bin is in PATH."
