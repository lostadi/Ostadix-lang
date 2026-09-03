#!/bin/sh
# Repository-owned lowercase `o` dispatcher. The setup wrapper delegates here
# so `o plan` and the historical lowercase evaluator alias cannot drift.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OCLI_BIN=${O_LANG_OCLI_BIN:-"$ROOT/target/release/o-cli"}
OLANGC_BIN=${O_LANG_OLANGC_BIN:-"$ROOT/target/release/olangc"}
O_BIN=${O_LANG_EVALUATOR_BIN:-"$ROOT/target/release/O"}
KERNEL_CLI_BIN=${O_LANG_KERNEL_CLI_BIN:-"$ROOT/scripts/o-kernel.sh"}
CAPACITY_BIN=${O_LANG_CAPACITY_BIN:-"$ROOT/scripts/ostadix_capacity.py"}
LIVE_BIN=${O_LANG_LIVE_BIN:-"$ROOT/target/release/o-live-host"}
OGIT_BIN=${O_LANG_OGIT_BIN:-"$ROOT/target/release/ogit"}
NODE_BIN=${O_LANG_NODE_BIN:-"$ROOT/target/release/o-node"}
OCTL_BIN=${O_LANG_OCTL_BIN:-"$ROOT/target/release/octl"}
REGISTRY_BIN=${O_LANG_REGISTRY_BIN:-"$ROOT/target/release/o-registry"}
INFO_BIN=${O_LANG_INFO_BIN:-"$ROOT/target/release/o-info"}
DEVICE_BIN=${O_LANG_DEVICE_BIN:-"$ROOT/target/release/ostadix-device"}

case "${1:-}" in
    device)
        shift
        if [ ! -x "$DEVICE_BIN" ]; then
            printf 'error: Android device controller is missing or not executable: %s\n' "$DEVICE_BIN" >&2
            exit 1
        fi
        exec "$DEVICE_BIN" "$@"
        ;;
    help|--help|-h|run|routes|optimize|plan|explain|inspect|object|operation|realizations|observe|replan)
        if [ ! -x "$OCLI_BIN" ]; then
            printf 'error: compiled Ostadix front door is missing or not executable: %s\n' "$OCLI_BIN" >&2
            exit 1
        fi
        export O_BACKENDS_DIR="${O_BACKENDS_DIR:-$ROOT/backends}"
        exec "$OCLI_BIN" "$@"
        ;;
    why)
        shift
        if [ "$#" -lt 2 ]; then
            printf 'usage: o why FILE.O P<N> [olangc options]\n' >&2
            exit 2
        fi
        source=$1
        operation=$2
        shift 2
        exec "$OLANGC_BIN" "$source" --target ir --why "$operation" "$@"
        ;;
    kernel)
        shift
        if [ ! -x "$KERNEL_CLI_BIN" ]; then
            printf 'error: O-core kernel CLI is missing or not executable: %s\n' "$KERNEL_CLI_BIN" >&2
            exit 1
        fi
        exec "$KERNEL_CLI_BIN" "$@"
        ;;
    capacity)
        shift
        if [ ! -x "$CAPACITY_BIN" ]; then
            printf 'error: absorbed-capacity package manager is missing or not executable: %s\n' "$CAPACITY_BIN" >&2
            exit 1
        fi
        exec "$CAPACITY_BIN" "$@"
        ;;
    node)
        shift
        case "${1:-}" in
            start|stop|status|restart|pair|serve|pki|identity|admin)
                if [ ! -x "$NODE_BIN" ]; then
                    printf 'error: hosted node service is missing or not executable: %s\n' "$NODE_BIN" >&2
                    exit 1
                fi
                exec "$NODE_BIN" "$@"
                ;;
            host)
                shift
                if [ ! -x "$NODE_BIN" ]; then
                    printf 'error: hosted node service is missing or not executable: %s\n' "$NODE_BIN" >&2
                    exit 1
                fi
                exec "$NODE_BIN" "$@"
                ;;
            *)
                if [ ! -x "$OCTL_BIN" ]; then
                    printf 'error: hosted node client is missing or not executable: %s\n' "$OCTL_BIN" >&2
                    exit 1
                fi
                exec "$OCTL_BIN" node "$@"
                ;;
        esac
        ;;
    node-host)
        shift
        if [ ! -x "$NODE_BIN" ]; then
            printf 'error: hosted node service is missing or not executable: %s\n' "$NODE_BIN" >&2
            exit 1
        fi
        exec "$NODE_BIN" "$@"
        ;;
    registry)
        shift
        if [ ! -x "$REGISTRY_BIN" ]; then
            printf 'error: signed registry CLI is missing or not executable: %s\n' "$REGISTRY_BIN" >&2
            exit 1
        fi
        exec "$REGISTRY_BIN" "$@"
        ;;
    info)
        shift
        if [ ! -x "$INFO_BIN" ]; then
            printf 'error: information CLI is missing or not executable: %s\n' "$INFO_BIN" >&2
            exit 1
        fi
        exec "$INFO_BIN" "$@"
        ;;
    live)
        shift
        if [ ! -x "$LIVE_BIN" ]; then
            printf 'error: hosted live-system CLI is missing or not executable: %s\n' "$LIVE_BIN" >&2
            exit 1
        fi
        exec "$LIVE_BIN" "$@"
        ;;
    receipt)
        shift
        if [ ! -x "$OGIT_BIN" ]; then
            printf 'error: O-Git CLI is missing or not executable: %s\n' "$OGIT_BIN" >&2
            exit 1
        fi
        if [ "$#" -eq 0 ]; then
            exec "$OGIT_BIN" demo semantic-receipt
        fi
        exec "$OGIT_BIN" "$@"
        ;;
    *)
        export O_BACKENDS_DIR="${O_BACKENDS_DIR:-$ROOT/backends}"
        exec "$O_BIN" "$@"
        ;;
esac
