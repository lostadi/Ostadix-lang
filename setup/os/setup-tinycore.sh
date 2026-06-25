#!/usr/bin/env bash
# O-lang setup for TinyCore Linux
set -euo pipefail
echo "=== O-lang setup for TinyCore ==="
echo "TinyCore is minimal. Ensure you have a persistent tce directory mounted."
echo "Installing packages (may need to run as tc user or with sudo if configured):"
tce-load -wi gcc make python3.12 sqlite3 curl git 2>/dev/null || {
  echo "Please run manually: tce-load -wi gcc make python3.12 sqlite3 curl git"
  echo "For full: also install any needed for your extensions."
}
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env" || true
fi
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release || echo "Rust build may need manual PATH adjustment."
(cd c_cpp && make -j2 || make) || echo "C build may need adjustment for minimal env."
python3 -m pip install --user --upgrade pip 2>/dev/null || true
echo "=== Runnable forms ==="
echo "cargo run -- examples/hello.O"
echo "./c_cpp/O examples/hello.O ./backends"
echo "./c_cpp/olangc examples/hello.O -o /tmp/hello_c && /tmp/hello_c"
echo "python3 -m o_lang examples/hello.O"
echo "Note: TinyCore may require reboot or tce-load for some libs. Test with hello.O"
echo "Done (best effort for minimal distro)."
