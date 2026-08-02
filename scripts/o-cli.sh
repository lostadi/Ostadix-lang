#!/usr/bin/env bash
# Repository-owned lowercase `o` dispatcher. The setup wrapper delegates here
# so `o plan` and the historical lowercase evaluator alias cannot drift.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
OLANGC_BIN=${O_LANG_OLANGC_BIN:-"$ROOT/target/release/olangc"}
O_BIN=${O_LANG_EVALUATOR_BIN:-"$ROOT/target/release/O"}

case "${1:-}" in
    plan)
        shift
        if [[ $# -eq 0 ]]; then
            printf 'usage: o plan <project-or-.O> [olangc options]\n' >&2
            exit 2
        fi
        exec "$OLANGC_BIN" "$1" --target ir "${@:2}"
        ;;
    *)
        export O_BACKENDS_DIR="${O_BACKENDS_DIR:-$ROOT/backends}"
        exec "$O_BIN" "$@"
        ;;
esac
