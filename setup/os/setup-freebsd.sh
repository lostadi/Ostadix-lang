#!/usr/bin/env bash
# O-lang setup for FreeBSD (and similar BSDs)
set -euo pipefail
echo "=== O-lang setup for FreeBSD ==="
sudo pkg update
sudo pkg install -y gmake gcc python3 curl git
# Note: may need pkg install rust if binary, but use rustup for latest
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release
(cd c_cpp && gmake -j$(sysctl -n hw.ncpu || echo 4) CC=gcc)
python3 -m pip install --user --upgrade pip 2>/dev/null || true
python3 -m pip install --user matplotlib 2>/dev/null || true
echo "=== Runnable forms ==="
echo "cargo run -- examples/hello.O"
echo "./c_cpp/O examples/hello.O ./backends"
echo "./c_cpp/olangc examples/hello.O -o /tmp/hello_c && /tmp/hello_c"
echo "python3 -m o_lang examples/hello.O"
echo "Done. Use gmake if make is BSD make."
