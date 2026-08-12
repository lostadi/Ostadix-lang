#!/usr/bin/env bash
# Repository-owned lowercase `o` dispatcher. The setup wrapper delegates here
# so `o plan` and the historical lowercase evaluator alias cannot drift.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
OLANGC_BIN=${O_LANG_OLANGC_BIN:-"$ROOT/target/release/olangc"}
O_BIN=${O_LANG_EVALUATOR_BIN:-"$ROOT/target/release/O"}
KERNEL_CLI_BIN=${O_LANG_KERNEL_CLI_BIN:-"$ROOT/scripts/o-kernel.sh"}

usage() {
    cat <<'USAGE'
Usage: o <command> [arguments]

Repository commands:
  plan <project-or-.O> [options]  Print the OIR and execution plan
  why FILE.O P<N> [options]       Explain one admitted plan operation
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
    *)
        export O_BACKENDS_DIR="${O_BACKENDS_DIR:-$ROOT/backends}"
        exec "$O_BIN" "$@"
        ;;
esac
