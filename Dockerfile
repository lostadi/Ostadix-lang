# ─────────────────────────────────────────────────────────────────────────────
# O-lang container image
#
# Multi-stage build: a pinned Rust builder compiles the hosted toolchain, and a
# slim runtime stage ships the four binaries, Python 3, and the backend shim
# adapters. The minimal image does not install every runtime those adapters can
# delegate to (for example rustc, Node.js, Ruby, a JDK, or C/C++ compilers).
#
# Build:
#   docker build -t o-lang:0.4.0-dev .
#
# Run a .O program from the host:
#   docker run --rm --mount type=bind,src="$PWD",dst=/work,readonly \
#       o-lang:0.4.0-dev examples/hello.O
#
# Literal-link the checked-in Python-only fixture and run it immediately:
#   docker run --rm \
#       --mount type=bind,src="$PWD/examples/docker_literal",dst=/work,readonly \
#       --entrypoint o-link o-lang:0.4.0-dev . -o /tmp/app.O
# Safe, nonexecuting lift of the actual repository root:
#   mkdir -p target/docker
#   docker run --rm --mount type=bind,src="$PWD",dst=/work,readonly \
#       --entrypoint o-link o-lang:0.4.0-dev --project . --stdout \
#       > target/docker/project.O
#
# Drop into an interactive REPL:
#   docker run --rm -it o-lang:0.4.0-dev --repl
# ─────────────────────────────────────────────────────────────────────────────

# ── Stage 1: build ───────────────────────────────────────────────────────────
FROM rust:1.97.1-slim-bookworm AS builder

WORKDIR /src

# Build dependencies for crates with native components (e.g. openssl-sys).
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ostadix-api ./crates/ostadix-api
COPY src ./src
COPY backends ./backends

# olangc embeds the runtime sources, Cargo.lock, and shim scripts at compile
# time (include_str!/include_bytes!), so everything above must be present.
RUN cargo build --release --locked --package o-lang \
    --bin O --bin o-cli --bin olangc --bin o-link

# ── Stage 2: runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && ln -sf /usr/bin/python3 /usr/local/bin/python

COPY --from=builder /src/target/release/O      /usr/local/bin/O
COPY --from=builder /src/target/release/o-cli  /usr/local/bin/o-cli
COPY --from=builder /src/target/release/olangc /usr/local/bin/olangc
COPY --from=builder /src/target/release/o-link /usr/local/bin/o-link

# Keep the lowercase compatibility front door as a dispatcher. Stateful intent
# commands reach the compiled orchestrator; every other shape retains the raw
# evaluator fallback, including shebang lines (`#!/usr/bin/env o`).
RUN printf '%s\n' \
    '#!/bin/sh' \
    'set -e' \
    'case "${1:-}" in' \
    '  run|optimize|plan|explain|inspect|help|--help|-h) exec o-cli "$@" ;;' \
    '  *) exec O "$@" ;;' \
    'esac' \
    > /usr/local/bin/o \
    && chmod +x /usr/local/bin/o

# Backend shim adapters. The environment variable is the image-wide authority
# for their stable path, including when callers override ENTRYPOINT with o-link.
COPY backends /opt/o-lang/backends
ENV O_BACKENDS_DIR=/opt/o-lang/backends

# Entrypoint wrapper: defaults the shim directory to the baked-in
# /opt/o-lang/backends so mounted work dirs don't need their own copy.
RUN printf '%s\n' \
    '#!/bin/sh' \
    'set -e' \
    'SHIMS=${O_BACKENDS_DIR:-/opt/o-lang/backends}' \
    'if [ "$#" -eq 0 ]; then exec O --repl "$SHIMS"; fi' \
    'case "$1" in --repl|-i) exec O "$1" "${2:-$SHIMS}";; esac' \
    'if [ "$#" -eq 1 ] && [ -f "$1" ]; then exec O "$1" "$SHIMS"; fi' \
    'exec o "$@"' \
    > /usr/local/bin/o-entrypoint \
    && chmod +x /usr/local/bin/o-entrypoint

WORKDIR /work
RUN ln -s /opt/o-lang/backends /work/backends

ENTRYPOINT ["o-entrypoint"]
