#!/usr/bin/env bash
# o-node-quickstart: one command to set up and run o-node
#
# Usage:
#   ./o-node-quickstart.sh                    # interactive setup
#   ./o-node-quickstart.sh --start            # start existing setup
#   ./o-node-quickstart.sh --run program.O    # run a program on the node
#   ./o-node-quickstart.sh --teardown         # clean up
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export O_LANG_ROOT="$ROOT"
export PATH="$HOME/.local/bin:$ROOT/target/release:$PATH"

# ── Colors ──────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

info()  { echo -e "${BLUE}▸${NC} $*"; }
ok()    { echo -e "${GREEN}✓${NC} $*"; }
warn()  { echo -e "${YELLOW}⚠${NC} $*"; }
fail()  { echo -e "${RED}✗${NC} $*" >&2; exit 1; }

# ── Defaults ────────────────────────────────────────────────────────────
DEMO="${DEMO:-${TMPDIR:-/tmp}/ostadix-node-$$}"
PKI="$DEMO/pki"
STATE="$DEMO/state"
AUTH="$DEMO/authority"
BIND="${BIND:-127.0.0.1:7337}"
NODE_ID="${NODE_ID:-demo-node}"
STATE_DIR="${STATE_DIR:-checkpoint-restore}"

# ── Helpers ─────────────────────────────────────────────────────────────
require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is not installed. $2"
}

ensure_built() {
  if [[ ! -f "$ROOT/target/release/O" ]]; then
    info "Building Ostadix tools..."
    "$ROOT/setup.sh" -y --minimal >/dev/null 2>&1
    ok "Built"
  fi
}

save_config() {
  cat > "$DEMO/config.env" <<EOF
DEMO=$DEMO
PKI=$PKI
STATE=$STATE
AUTH=$AUTH
BIND=$BIND
NODE_ID=$NODE_ID
STATE_DIR=$STATE_DIR
EOF
}

load_config() {
  if [[ -f "$DEMO/config.env" ]]; then
    source "$DEMO/config.env"
    ok "Loaded config from $DEMO"
  else
    fail "No setup found at $DEMO. Run without --start first."
  fi
}

# ── Commands ────────────────────────────────────────────────────────────

cmd_setup() {
  echo ""
  echo -e "${CYAN}╔══════════════════════════════════════════╗${NC}"
  echo -e "${CYAN}║     O-node Quickstart Setup              ║${NC}"
  echo -e "${CYAN}╚══════════════════════════════════════════╝${NC}"
  echo ""

  # Check dependencies
  require_cmd openssl "Install with: brew install openssl"
  require_cmd qemu-system-x86_64 "Install with: brew install qemu" || true

  # Build if needed
  ensure_built

  # Create workspace
  mkdir -p "$DEMO"
  info "Workspace: $DEMO"

  # Write a demo program
  if [[ ! -f "$DEMO/demo.O" ]]; then
    cat > "$DEMO/demo.O" <<'O'
python[7]^(
import json
data = {"greeting": "hello from o-node", "sum": sum(range(10))}
__oval_result__ = json.dumps(data, indent=2)
)_python[7]
O
    ok "Created demo program: $DEMO/demo.O"
  fi

  # Initialize PKI
  info "Initializing mTLS PKI..."
  o-node pki init --directory "$PKI" --server-name localhost >/dev/null 2>&1
  ok "PKI ready at $PKI"

  # Initialize node identity
  info "Initializing node identity..."
  o-node identity init --state-dir "$STATE" >/dev/null 2>&1
  ok "Node identity ready"

  # Initialize placement authority
  info "Initializing placement authority..."
  octl node authority init --directory "$AUTH" >/dev/null 2>&1
  ok "Authority ready"

  # Save config
  save_config
  ok "Config saved to $DEMO/config.env"

  echo ""
  echo -e "${GREEN}Setup complete!${NC}"
  echo ""
  echo "Next steps:"
  echo "  1. Start the node:   $0 --start"
  echo "  2. Run a program:    $0 --run $DEMO/demo.O"
  echo "  3. Clean up:         $0 --teardown"
  echo ""
}

cmd_start() {
  load_config
  ensure_built

  echo ""
  info "Starting o-node on $BIND..."

  # Start in background
  o-node serve \
    --node-id "$NODE_ID" \
    --shim-dir "$ROOT/backends" \
    --runtime-binary "$ROOT/target/release/O" \
    --bind "$BIND" \
    --cert "$PKI/node-cert.pem" \
    --key "$PKI/node-key.pem" \
    --client-ca "$PKI/ca.pem" \
    --v2-state-dir "$STATE" \
    --v2-authority-public-key "$AUTH/placement-public-key.v2" \
    &

  NODE_PID=$!
  echo "$NODE_PID" > "$DEMO/node.pid"
  sleep 1

  if kill -0 "$NODE_PID" 2>/dev/null; then
    ok "o-node running (PID $NODE_PID) on $BIND"
    echo ""
    echo "Check health:  $0 --status"
    echo "Run program:   $0 --run $DEMO/demo.O"
    echo "Stop node:     $0 --stop"
    echo ""
  else
    fail "o-node failed to start. Check: o-node doctor"
  fi
}

cmd_stop() {
  load_config
  if [[ -f "$DEMO/node.pid" ]]; then
    PID=$(cat "$DEMO/node.pid")
    if kill -0 "$PID" 2>/dev/null; then
      info "Stopping o-node (PID $PID)..."
      kill "$PID" 2>/dev/null || true
      sleep 1
      ok "Stopped"
    else
      warn "o-node (PID $PID) was already stopped"
    fi
    rm -f "$DEMO/node.pid"
  else
    warn "No node PID found"
  fi
}

cmd_status() {
  load_config
  info "Checking node status..."
  echo ""

  octl node profile \
    --address "$BIND" \
    --server-name localhost \
    --ca "$PKI/ca.pem" \
    --cert "$PKI/client-cert.pem" \
    --key "$PKI/client-key.pem" 2>/dev/null && ok "Node is healthy" || warn "Node may not be running"
}

cmd_run() {
  local source_file="${1:?Usage: $0 --run program.O}"
  load_config
  ensure_built

  if [[ ! -f "$source_file" ]]; then
    fail "File not found: $source_file"
  fi

  info "Running $source_file on node..."

  # Mint open lease
  octl node authority dev-mint open \
    --signing-key "$AUTH/placement-signing-key.v2" \
    --shim-dir "$ROOT/backends" \
    --runtime-binary "$ROOT/target/release/O" \
    --source "$source_file" \
    --node-id "$NODE_ID" \
    --state-tier "$STATE_DIR" \
    --client-cert "$PKI/client-cert.pem" \
    --capability-out "$DEMO/capability.json" \
    --out "$DEMO/open-lease.json" \
    --submit \
    --address "$BIND" \
    --server-name localhost \
    --ca "$PKI/ca.pem" \
    --key "$PKI/client-key.pem" \
    --node-receipt-public-key "$STATE/node-signing-public.v2" \
    2>/dev/null

  # Extract session ID
  SESSION_ID=$(python3 -c "
import json, sys
with open('$DEMO/open-lease.json') as f:
    data = json.load(f)
print(data.get('session_id', ''))
" 2>/dev/null || echo "")

  if [[ -z "$SESSION_ID" ]]; then
    fail "Failed to get session ID from open lease"
  fi

  ok "Session opened: $SESSION_ID"

  # Mint execute lease
  octl node authority dev-mint execute \
    --signing-key "$AUTH/placement-signing-key.v2" \
    --shim-dir "$ROOT/backends" \
    --runtime-binary "$ROOT/target/release/O" \
    --open-lease "$DEMO/open-lease.json" \
    --source "$source_file" \
    --operation-id "op-$(date +%s)" \
    --task-sha256 "$(shasum -a 256 "$source_file" | awk '{print $1}')" \
    --capability "$DEMO/capability.json" \
    --operation-out "$DEMO/operation.json" \
    --out "$DEMO/execute-lease.json" \
    --submit \
    --address "$BIND" \
    --server-name localhost \
    --ca "$PKI/ca.pem" \
    --cert "$PKI/client-cert.pem" \
    --key "$PKI/client-key.pem" \
    --node-receipt-public-key "$STATE/node-signing-public.v2" \
    2>/dev/null

  ok "Operation submitted"

  # Poll for result
  info "Waiting for result..."
  for i in $(seq 1 30); do
    STATUS=$(octl node session status \
      --address "$BIND" \
      --server-name localhost \
      --ca "$PKI/ca.pem" \
      --cert "$PKI/client-cert.pem" \
      --key "$PKI/client-key.pem" \
      --session-id "$SESSION_ID" \
      2>/dev/null | python3 -c "
import json, sys
try:
    data = json.load(sys.stdin)
    ops = data.get('operations', {})
    for op_id, op_data in ops.items():
        print(op_data.get('status', 'unknown'))
        break
    else:
        print('pending')
except:
    print('pending')
" 2>/dev/null || echo "pending")

    case "$STATUS" in
      succeeded)
        ok "Operation succeeded!"
        echo ""
        echo "Result:"
        octl node session status \
          --address "$BIND" \
          --server-name localhost \
          --ca "$PKI/ca.pem" \
          --cert "$PKI/client-cert.pem" \
          --key "$PKI/client-key.pem" \
          --session-id "$SESSION_ID" \
          2>/dev/null | python3 -c "
import json, sys
data = json.load(sys.stdin)
for op_id, op_data in data.get('operations', {}).items():
    if 'result' in op_data:
        print(json.dumps(op_data['result'], indent=2))
" 2>/dev/null || echo "(see raw status above)"
        exit 0
        ;;
      failed)
        fail "Operation failed"
        ;;
      *)
        sleep 1
        ;;
    esac
  done

  warn "Timed out waiting for result. Check: $0 --status"
}

cmd_teardown() {
  load_config
  cmd_stop 2>/dev/null || true
  info "Removing $DEMO..."
  rm -rf "$DEMO"
  ok "Cleaned up"
}

cmd_example() {
  load_config
  cat <<'O'
# Example: Multi-language pipeline on o-node

python[0]^(
import json
data = [
    {"name": "Alice", "score": 95},
    {"name": "Bob", "score": 87},
    {"name": "Charlie", "score": 92},
]
__oval_result__ = json.dumps(data)
)_python[0]

sql[0]^(
CREATE TABLE scores (name TEXT, score INTEGER)
)_sql[0]

sql[0]^(
INSERT INTO scores VALUES ('Alice', 95), ('Bob', 87), ('Charlie', 92)
)_sql[0]

let stats = sql[0]^(
SELECT AVG(score) AS avg_score, MAX(score) AS max_score FROM scores
)_sql[0]

html^(
<h1>Score Report</h1>
<p>Average: $stats</p>
)_html
O
}

# ── Main ────────────────────────────────────────────────────────────────
case "${1:-}" in
  --setup|"")
    cmd_setup
    ;;
  --start)
    cmd_start
    ;;
  --stop)
    cmd_stop
    ;;
  --status)
    cmd_status
    ;;
  --run)
    shift
    cmd_run "$@"
    ;;
  --teardown)
    cmd_teardown
    ;;
  --example)
    cmd_example
    ;;
  --help|-h)
    echo "Usage: $0 [command]"
    echo ""
    echo "Commands:"
    echo "  (none)       Interactive setup (default)"
    echo "  --start      Start the node"
    echo "  --stop       Stop the node"
    echo "  --status     Check node health"
    echo "  --run FILE   Run a .O file on the node"
    echo "  --teardown   Clean up everything"
    echo "  --example    Show a multi-language example"
    echo ""
    echo "Environment:"
    echo "  DEMO=$DEMO"
    echo "  BIND=$BIND"
    echo "  NODE_ID=$NODE_ID"
    ;;
  *)
    fail "Unknown command: $1. Try: $0 --help"
    ;;
esac
