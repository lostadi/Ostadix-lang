#!/usr/bin/env bash
set -euo pipefail

REPO="${1:-$PWD}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH="$SCRIPT_DIR/ocore-independent-shift-count-types.patch"

if [[ ! -f "$REPO/Cargo.toml" || ! -f "$REPO/src/ocore/typeck.rs" ]]; then
  echo "error: '$REPO' is not an Ostadix-lang repository checkout" >&2
  echo "usage: $0 /path/to/Ostadix-lang" >&2
  exit 2
fi
if [[ ! -f "$PATCH" ]]; then
  echo "error: patch not found beside this script: $PATCH" >&2
  exit 2
fi

cd "$REPO"

if git apply --reverse --check "$PATCH" >/dev/null 2>&1; then
  echo "O-core shift-count patch is already applied."
elif git apply --check "$PATCH"; then
  git apply "$PATCH"
  echo "Applied O-core shift-count patch."
else
  echo "error: patch does not apply cleanly; your checkout differs from commit 830e395." >&2
  exit 1
fi

cargo fmt --all
cargo test accepts_integer_shift_counts_with_different_types -- --nocapture
cargo test emits_shifts_with_independent_integer_count_types -- --nocapture
cargo build --release --bin olangc --bin ocorec

cat <<EOF

Built successfully:
  $REPO/target/release/olangc
  $REPO/target/release/ocorec

For the terminal demo, force it to use these freshly built tools:
  export PATH="$REPO/target/release:\$PATH"
  hash -r
EOF
