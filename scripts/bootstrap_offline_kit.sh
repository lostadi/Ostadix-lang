#!/bin/sh
# Verify and use one extracted, host-labelled Ostadix offline build kit.
set -eu

KIT_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
SOURCE_ROOT="$KIT_ROOT/source"
OFFLINE_ROOT="$KIT_ROOT/.offline"
PROFILE=${1:-check}

if [ ! -f "$SOURCE_ROOT/scripts/build_offline_kit.py" ]; then
    echo "offline bootstrap: missing source/scripts/build_offline_kit.py" >&2
    exit 2
fi

if ! python3 -c 'import sys; raise SystemExit(sys.version_info < (3, 10))'; then
    echo "offline bootstrap: Python 3.10 or newer is required" >&2
    exit 2
fi

python3 "$SOURCE_ROOT/scripts/build_offline_kit.py" extract \
    --kit-root "$KIT_ROOT" \
    --destination "$OFFLINE_ROOT"

export PATH="$OFFLINE_ROOT/toolchain/bin:$PATH"
export CARGO_HOME="$OFFLINE_ROOT/cargo-home"
export CARGO_NET_OFFLINE=true
export CARGO_TARGET_DIR="$OFFLINE_ROOT/target"
export CARGO_INCREMENTAL=0
CARGO_BIN="$OFFLINE_ROOT/toolchain/bin/cargo"
RUSTC_BIN="$OFFLINE_ROOT/toolchain/bin/rustc"
RUSTDOC_BIN="$OFFLINE_ROOT/toolchain/bin/rustdoc"
export CARGO="$CARGO_BIN"
export RUSTC="$RUSTC_BIN"
export RUSTDOC="$RUSTDOC_BIN"
unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER RUSTFLAGS CARGO_ENCODED_RUSTFLAGS \
    RUSTDOCFLAGS CARGO_ENCODED_RUSTDOCFLAGS RUSTUP_TOOLCHAIN RUSTC_BOOTSTRAP \
    CARGO_BUILD_RUSTC CARGO_BUILD_RUSTDOC CARGO_BUILD_RUSTC_WRAPPER \
    CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER

# Cargo discovers .cargo/config.toml from its invocation directory upward. Run
# from the filesystem root with absolute manifest paths so a recipient's
# ~/.cargo or extraction-parent configuration cannot enter this sealed build.
# A root-level Cargo config would still be visible, so fail closed if one exists.
if [ -e /.cargo/config.toml ] || [ -e /.cargo/config ]; then
    echo "offline bootstrap: refusing ambient root-level Cargo configuration" >&2
    exit 2
fi
if [ -e "$CARGO_HOME/config" ] || [ -L "$CARGO_HOME/config" ]; then
    echo "offline bootstrap: refusing legacy Cargo-home configuration" >&2
    exit 2
fi

root_cargo() {
    (
        cd /
        env -i \
            HOME="${HOME:-$KIT_ROOT}" \
            PATH="$PATH" \
            TMPDIR="${TMPDIR:-/tmp}" \
            LANG="${LANG:-C}" \
            CARGO="$CARGO_BIN" \
            CARGO_HOME="$CARGO_HOME" \
            CARGO_NET_OFFLINE=true \
            CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
            CARGO_INCREMENTAL=0 \
            RUSTC="$RUSTC_BIN" \
            RUSTDOC="$RUSTDOC_BIN" \
            "$CARGO_BIN" "$@"
    )
}

build_hosted() {
    root_cargo build --frozen --release --locked \
        --manifest-path "$SOURCE_ROOT/Cargo.toml" --package o-lang \
        --all-features --bins
}

build_mcp() {
    root_cargo build --frozen --release --locked --manifest-path \
        "$SOURCE_ROOT/mcp/ostadix_lang_mcp_server/Cargo.toml"
}

case "$PROFILE" in
    check)
        root_cargo metadata --frozen --locked --format-version 1 \
            --manifest-path "$SOURCE_ROOT/Cargo.toml" >/dev/null
        root_cargo metadata --frozen --locked --format-version 1 \
            --manifest-path "$SOURCE_ROOT/mcp/ostadix_lang_mcp_server/Cargo.toml" \
            >/dev/null
        root_cargo metadata --frozen --locked --format-version 1 \
            --manifest-path "$SOURCE_ROOT/apps/android-terminal/runtime/Cargo.toml" \
            >/dev/null
        root_cargo metadata --frozen --locked --format-version 1 \
            --manifest-path "$SOURCE_ROOT/fuzz/Cargo.toml" >/dev/null
        ;;
    hosted-rust)
        build_hosted
        ;;
    mcp)
        build_mcp
        ;;
    all-supported)
        build_hosted
        build_mcp
        ;;
    wasm-std-check)
        "$RUSTC_BIN" --print target-libdir --target wasm32-wasip1
        ;;
    *)
        echo "offline bootstrap: unknown profile: $PROFILE" >&2
        echo "profiles: check hosted-rust mcp all-supported wasm-std-check" >&2
        exit 2
        ;;
esac
