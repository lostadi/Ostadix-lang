# ─────────────────────────────────────────────────────────────────────────────
# O-lang container image
#
# Multi-stage build: a Rust builder stage compiles the toolchain, and a slim
# runtime stage ships the three binaries with Python 3 (the runtime most
# backends shim through) and the backend shim scripts.
#
# Build:
#   docker build -t o-lang .
#
# Run a .O program from the host:
#   docker run --rm -v "$PWD:/work" o-lang my_program.O
#
# Link a codebase into one .O file, then run it:
#   docker run --rm -v "$PWD:/work" --entrypoint o-link o-lang src/ -o app.O
#   docker run --rm -v "$PWD:/work" o-lang app.O
#
# Drop into an interactive REPL:
#   docker run --rm -it o-lang --repl
# ─────────────────────────────────────────────────────────────────────────────

# ── Stage 1: build ───────────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

WORKDIR /src

# Build dependencies for crates with native components (e.g. openssl-sys).
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY backends ./backends

# olangc embeds the runtime sources, Cargo.lock, and shim scripts at compile
# time (include_str!/include_bytes!), so everything above must be present.
RUN cargo build --release --bin O --bin olangc --bin o-link

# ── Stage 2: runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && ln -sf /usr/bin/python3 /usr/local/bin/python

COPY --from=builder /src/target/release/O      /usr/local/bin/O
COPY --from=builder /src/target/release/olangc /usr/local/bin/olangc
COPY --from=builder /src/target/release/o-link /usr/local/bin/o-link

# `o` wrapper so shebang lines (`#!/usr/bin/env o`) and docs work unchanged.
RUN ln -s /usr/local/bin/O /usr/local/bin/o

# Backend shim scripts, available at the default ./backends lookup path via
# the /work symlink below and at a stable absolute path for explicit use.
COPY backends /opt/o-lang/backends

# Entrypoint wrapper: defaults the shim directory to the baked-in
# /opt/o-lang/backends so mounted work dirs don't need their own copy.
RUN printf '%s\n' \
    '#!/bin/sh' \
    'set -e' \
    'SHIMS=/opt/o-lang/backends' \
    'if [ "$#" -eq 0 ]; then exec O --repl "$SHIMS"; fi' \
    'case "$1" in --repl|-i) exec O "$1" "${2:-$SHIMS}";; esac' \
    'if [ "$#" -eq 1 ] && [ -f "$1" ]; then exec O "$1" "$SHIMS"; fi' \
    'exec O "$@"' \
    > /usr/local/bin/o-entrypoint \
    && chmod +x /usr/local/bin/o-entrypoint

WORKDIR /work
RUN ln -s /opt/o-lang/backends /work/backends

ENTRYPOINT ["o-entrypoint"]
