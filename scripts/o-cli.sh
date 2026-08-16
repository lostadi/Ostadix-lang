#!/usr/bin/env bash
# Repository-owned lowercase `o` dispatcher. The setup wrapper delegates here
# so `o plan` and the historical lowercase evaluator alias cannot drift.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
OLANGC_BIN=${O_LANG_OLANGC_BIN:-"$ROOT/target/release/olangc"}
O_BIN=${O_LANG_EVALUATOR_BIN:-"$ROOT/target/release/O"}
KERNEL_CLI_BIN=${O_LANG_KERNEL_CLI_BIN:-"$ROOT/scripts/o-kernel.sh"}
LIVE_BIN=${O_LANG_LIVE_BIN:-"$ROOT/target/release/o-live-host"}
OGIT_BIN=${O_LANG_OGIT_BIN:-"$ROOT/target/release/ogit"}
NODE_BIN=${O_LANG_NODE_BIN:-"$ROOT/target/release/o-node"}
OCTL_BIN=${O_LANG_OCTL_BIN:-"$ROOT/target/release/octl"}
REGISTRY_BIN=${O_LANG_REGISTRY_BIN:-"$ROOT/target/release/o-registry"}
INFO_BIN=${O_LANG_INFO_BIN:-"$ROOT/target/release/o-info"}

usage() {
    cat <<'USAGE'
Usage: o <command> [arguments]

Repository commands:
  run FILE.O [backends]          Run one local O document
  plan <project-or-.O> [options]  Print the OIR and execution plan
  why FILE.O P<N> [options]       Explain one admitted plan operation
  node <profile|doctor|run|session> ...
                                  Inspect or invoke one explicit hosted node
  node-host <command> ...         Provision or serve a hosted node
  registry <command> ...          Manage the local signed node registry
  info <command> ...              Manage the local authority-free information store
  live <command> ...              Run the hosted live-system control plane
  receipt [ogit arguments]        Emit the O-Git semantic receipt demo
  kernel <command>               Build, boot, or verify the O-core kernel
  help                           Show this help

Any other arguments retain the historical lowercase evaluator behavior and
are forwarded to O. Uppercase O always invokes the evaluator directly.
USAGE
}

case "${1:-}" in
    help)
        usage
        ;;
    run)
        shift
        if [[ $# -eq 0 ]]; then
            printf 'usage: o run FILE.O [backends]\n' >&2
            exit 2
        fi
        export O_BACKENDS_DIR="${O_BACKENDS_DIR:-$ROOT/backends}"
        exec "$O_BIN" "$@"
        ;;
    plan)
        shift
        if [[ $# -eq 0 ]]; then
            printf 'usage: o plan <project-or-.O> [olangc options]\n' >&2
            exit 2
        fi
        exec "$OLANGC_BIN" "$1" --target ir "${@:2}"
        ;;
    why)
        shift
        if [[ $# -lt 2 ]]; then
            printf 'usage: o why FILE.O P<N> [olangc options]\n' >&2
            exit 2
        fi
        exec "$OLANGC_BIN" "$1" --target ir --why "$2" "${@:3}"
        ;;
    kernel)
        shift
        if [[ ! -x "$KERNEL_CLI_BIN" ]]; then
            printf 'error: O-core kernel CLI is missing or not executable: %s\n' "$KERNEL_CLI_BIN" >&2
            exit 1
        fi
        exec "$KERNEL_CLI_BIN" "$@"
        ;;
    node)
        shift
        if [[ ! -x "$OCTL_BIN" ]]; then
            printf 'error: hosted node client is missing or not executable: %s\n' "$OCTL_BIN" >&2
            exit 1
        fi
        exec "$OCTL_BIN" node "$@"
        ;;
    node-host)
        shift
        if [[ ! -x "$NODE_BIN" ]]; then
            printf 'error: hosted node service is missing or not executable: %s\n' "$NODE_BIN" >&2
            exit 1
        fi
        exec "$NODE_BIN" "$@"
        ;;
    registry)
        shift
        if [[ ! -x "$REGISTRY_BIN" ]]; then
            printf 'error: signed registry CLI is missing or not executable: %s\n' "$REGISTRY_BIN" >&2
            exit 1
        fi
        exec "$REGISTRY_BIN" "$@"
        ;;
    info)
        shift
        if [[ ! -x "$INFO_BIN" ]]; then
            printf 'error: information CLI is missing or not executable: %s\n' "$INFO_BIN" >&2
            exit 1
        fi
        exec "$INFO_BIN" "$@"
        ;;
    live)
        shift
        if [[ ! -x "$LIVE_BIN" ]]; then
            printf 'error: hosted live-system CLI is missing or not executable: %s\n' "$LIVE_BIN" >&2
            exit 1
        fi
        exec "$LIVE_BIN" "$@"
        ;;
    receipt)
        shift
        if [[ ! -x "$OGIT_BIN" ]]; then
            printf 'error: O-Git CLI is missing or not executable: %s\n' "$OGIT_BIN" >&2
            exit 1
        fi
        if [[ $# -eq 0 ]]; then
            exec "$OGIT_BIN" demo semantic-receipt
        fi
        exec "$OGIT_BIN" "$@"
        ;;
    *)
        export O_BACKENDS_DIR="${O_BACKENDS_DIR:-$ROOT/backends}"
        exec "$O_BIN" "$@"
        ;;
esac
