#!/usr/bin/env bash
# O-lang setup for Windows (Git Bash, MSYS2, Cygwin, WSL)
# For native Windows, prefer WSL + Ubuntu and use setup-debian.sh
set -euo pipefail
echo "=== O-lang setup for Windows bash env ==="
echo "Strongly recommended: use WSL2 with Ubuntu and run setup-debian.sh instead."
if command -v winget >/dev/null 2>&1; then
  echo "Trying winget for tools..."
  winget install --id Git.Git -e --silent || true
  winget install --id Python.Python.3.12 -e --silent || true
  winget install --id Rustlang.Rustup -e --silent || true
  winget install --id Microsoft.VisualStudio.2022.BuildTools -e --silent --override "--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools" || true
else
  echo "winget not found. Install Git, Python, Rustup, and MSVC Build Tools manually."
fi
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env" 2>/dev/null || true
fi
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release || echo "Build may need MSVC or MinGW setup."
if [[ -d c_cpp ]]; then
  (cd c_cpp && make -j4 || echo "C build may require mingw32-make or full MSVC.")
fi
python -m pip install --user --upgrade pip 2>/dev/null || true
python -m pip install --user matplotlib 2>/dev/null || true
echo "=== Runnable forms (adjust paths for your shell) ==="
echo "cargo run -- examples/hello.O"
echo "./c_cpp/O examples/hello.O ./backends   # if C built"
echo "./c_cpp/olangc examples/hello.O -o /tmp/hello_c && /tmp/hello_c"
echo "python -m o_lang examples/hello.O"
echo "Done. For best results use WSL."
