#!/usr/bin/env bash
# o-node-quickstart: compatibility front door for zero-configuration LAN nodes.
#
# Ordinary use intentionally exposes only user intent. Discovery, routing,
# node identity, enrollment, certificates, capabilities, leases, operation
# identities, and task bindings are derived by Ostadix. Use --manual only when
# deliberately entering the raw protocol/operator surface.
set -euo pipefail

ROOT="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
O_CLI="$ROOT/scripts/o-cli.sh"
export O_LANG_ROOT="${O_LANG_ROOT:-$ROOT}"
export O_BACKENDS_DIR="${O_BACKENDS_DIR:-$ROOT/backends}"
export PATH="$HOME/.local/bin:$ROOT/target/release:$PATH"

info() { printf '▸ %s\n' "$*"; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }

ensure_built() {
    [[ -x "$O_CLI" ]] || fail "missing repository CLI: $O_CLI"
    if [[ ! -x "$ROOT/target/release/O" \
       || ! -x "$ROOT/target/release/o-node" \
       || ! -x "$ROOT/target/release/octl" ]]; then
        info "Building the Ostadix node tools"
        "$ROOT/setup.sh" -y --minimal
    fi
}

run_o() {
    "$O_CLI" "$@"
}

require_argument() {
    local label=$1
    local value=${2:-}
    [[ -n "$value" ]] || fail "$label is required"
}

show_example() {
    cat <<'O'
python[0]^(
import json
__oval_result__ = json.dumps({"greeting": "hello from an Ostadix LAN node"})
)_python[0]
O
}

usage() {
    cat <<'USAGE'
Usage: ./o-node-quickstart.sh [command]

Ordinary zero-configuration commands:
  (none), --start          Start this machine as a detached LAN node
  --stop                   Stop this machine's detached LAN node
  --restart                Restart this machine's detached LAN node
  --status                 Show this machine's node-process status
  --list                   Discover available LAN nodes
  --use NODE_ID            Remember which semantic node to use
  --profile                Inspect the automatically selected node
  --doctor                 Check the automatically selected node
  --run FILE.O [options]   Run FILE.O in a temporary managed V2 session
  --run-once FILE.O        Run one frozen V1 operation
  --session ARGS...        Forward to the managed session interface
  --example                Print a small .O example
  --teardown               Compatibility alias for --stop; identity is retained

Expert/operator escape hatch:
  --manual ARGS...         Forward to the raw `o node-host` service CLI

Ordinary commands never require an address, hostname, port, certificate, key,
capability, lease, operation ID, task digest, or attempt generation.
USAGE
}

case "${1:-}" in
    ""|--start)
        ensure_built
        run_o node start
        run_o node status
        ;;
    --stop)
        ensure_built
        exec "$O_CLI" node stop
        ;;
    --restart)
        ensure_built
        exec "$O_CLI" node restart
        ;;
    --status)
        ensure_built
        exec "$O_CLI" node status
        ;;
    --list)
        ensure_built
        exec "$O_CLI" node list
        ;;
    --use)
        require_argument "NODE_ID" "${2:-}"
        ensure_built
        shift
        exec "$O_CLI" node use "$@"
        ;;
    --profile)
        ensure_built
        shift
        exec "$O_CLI" node profile "$@"
        ;;
    --doctor)
        ensure_built
        shift
        exec "$O_CLI" node doctor "$@"
        ;;
    --run)
        require_argument "FILE.O" "${2:-}"
        ensure_built
        shift
        exec "$O_CLI" node session run "$@"
        ;;
    --run-once)
        require_argument "FILE.O" "${2:-}"
        ensure_built
        shift
        exec "$O_CLI" node run "$@"
        ;;
    --session)
        require_argument "session command" "${2:-}"
        ensure_built
        shift
        exec "$O_CLI" node session "$@"
        ;;
    --teardown)
        ensure_built
        run_o node stop
        info "Persistent node identity and enrollment state were retained"
        ;;
    --manual)
        require_argument "manual node-host command" "${2:-}"
        ensure_built
        shift
        exec "$O_CLI" node-host "$@"
        ;;
    --example)
        show_example
        ;;
    --help|-h)
        usage
        ;;
    *)
        fail "unknown command: $1 (try --help)"
        ;;
esac
