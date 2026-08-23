#!/usr/bin/env bash
# Prove the ordinary zero-configuration node path from a multi-homed Linux
# client whose real node is not on its default route. Ordinary reconnect uses
# no address, TLS, or manual override; initial pairing also exercises the
# explicit directly routed `--address` path.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "error: smoke-zero-config-lan-netns.sh must run inside a Git worktree" >&2
    exit 2
}
if [[ "$(uname -s)" != Linux ]]; then
    echo "error: zero-config LAN namespace smoke requires Linux" >&2
    exit 2
fi
for executable in ip openssl python3 sudo; do
    if ! command -v "$executable" >/dev/null 2>&1; then
        echo "error: required executable is unavailable: $executable" >&2
        exit 2
    fi
done
if ! openssl version >/dev/null 2>&1; then
    echo "error: required OpenSSL executable failed its version preflight" >&2
    exit 2
fi
if ! sudo -n true; then
    echo "error: zero-config LAN namespace smoke requires non-interactive sudo" >&2
    exit 2
fi

node_bin="$repo_root/target/debug/o-node"
client_bin="$repo_root/target/debug/octl"
evaluator_bin="$repo_root/target/debug/O"
for binary in "$node_bin" "$client_bin" "$evaluator_bin"; do
    if [[ ! -x "$binary" ]]; then
        echo "error: required hosted binary is not executable: $binary" >&2
        exit 2
    fi
done

smoke_tmp_root="${TMPDIR:-/tmp}"
work_dir="$(mktemp -d "$smoke_tmp_root/ostadix-lan-netns.XXXXXX")"
case "$work_dir" in
    "$smoke_tmp_root"/ostadix-lan-netns.*) ;;
    *)
        echo "error: refusing unexpected temporary directory: $work_dir" >&2
        exit 2
        ;;
esac

topology_suffix="${work_dir##*.}"
client_namespace="ostadix-lan-client-$topology_suffix"
node_namespace="ostadix-lan-node-$topology_suffix"
decoy_namespace="ostadix-lan-decoy-$topology_suffix"
client_node_veth="ocn$topology_suffix"
node_veth="onn$topology_suffix"
client_decoy_veth="ocd$topology_suffix"
decoy_veth="ond$topology_suffix"
client_created=0
node_created=0
decoy_created=0
node_link_created=0
decoy_link_created=0

node_home="$work_dir/node/home"
node_config="$work_dir/node/config"
node_state="$work_dir/node/state"
client_home="$work_dir/client/home"
client_config="$work_dir/client/config"
client_state="$work_dir/client/state"
mkdir -p \
    "$node_home" "$node_config" "$node_state" \
    "$client_home" "$client_config" "$client_state"

run_node() {
    sudo -n ip netns exec "$node_namespace" env \
        HOME="$node_home" \
        XDG_CONFIG_HOME="$node_config" \
        XDG_STATE_HOME="$node_state" \
        O_LANG_NODE_BIN="$node_bin" \
        O_LANG_OCTL_BIN="$client_bin" \
        O_LANG_EVALUATOR_BIN="$evaluator_bin" \
        "$repo_root/scripts/o-cli.sh" node "$@"
}

run_client() {
    sudo -n ip netns exec "$client_namespace" env \
        HOME="$client_home" \
        XDG_CONFIG_HOME="$client_config" \
        XDG_STATE_HOME="$client_state" \
        O_LANG_NODE_BIN="$node_bin" \
        O_LANG_OCTL_BIN="$client_bin" \
        O_LANG_EVALUATOR_BIN="$evaluator_bin" \
        "$repo_root/scripts/o-cli.sh" node "$@"
}

cleanup() {
    local status=$?
    trap - EXIT HUP INT TERM
    set +e
    if [[ "$node_created" == 1 ]]; then
        run_node stop >/dev/null 2>&1
        while read -r namespace_pid; do
            if [[ "$namespace_pid" =~ ^[0-9]+$ ]]; then
                sudo -n kill -TERM "$namespace_pid" >/dev/null 2>&1
            fi
        done < <(sudo -n ip netns pids "$node_namespace" 2>/dev/null)
        sleep 0.1
        while read -r namespace_pid; do
            if [[ "$namespace_pid" =~ ^[0-9]+$ ]]; then
                sudo -n kill -KILL "$namespace_pid" >/dev/null 2>&1
            fi
        done < <(sudo -n ip netns pids "$node_namespace" 2>/dev/null)
        sudo -n ip netns delete "$node_namespace" >/dev/null 2>&1
    fi
    if [[ "$client_created" == 1 ]]; then
        run_client stop >/dev/null 2>&1
        while read -r namespace_pid; do
            if [[ "$namespace_pid" =~ ^[0-9]+$ ]]; then
                sudo -n kill -TERM "$namespace_pid" >/dev/null 2>&1
            fi
        done < <(sudo -n ip netns pids "$client_namespace" 2>/dev/null)
        sleep 0.1
        while read -r namespace_pid; do
            if [[ "$namespace_pid" =~ ^[0-9]+$ ]]; then
                sudo -n kill -KILL "$namespace_pid" >/dev/null 2>&1
            fi
        done < <(sudo -n ip netns pids "$client_namespace" 2>/dev/null)
        sudo -n ip netns delete "$client_namespace" >/dev/null 2>&1
    fi
    if [[ "$decoy_created" == 1 ]]; then
        sudo -n ip netns delete "$decoy_namespace" >/dev/null 2>&1
    fi
    if [[ "$node_link_created" == 1 ]]; then
        sudo -n ip link delete "$client_node_veth" >/dev/null 2>&1
        sudo -n ip link delete "$node_veth" >/dev/null 2>&1
    fi
    if [[ "$decoy_link_created" == 1 ]]; then
        sudo -n ip link delete "$client_decoy_veth" >/dev/null 2>&1
        sudo -n ip link delete "$decoy_veth" >/dev/null 2>&1
    fi
    sudo -n rm -rf -- "$work_dir"
    exit "$status"
}
trap cleanup EXIT HUP INT TERM

sudo -n ip netns add "$client_namespace"
client_created=1
sudo -n ip netns add "$node_namespace"
node_created=1
sudo -n ip netns add "$decoy_namespace"
decoy_created=1
sudo -n ip link add "$client_node_veth" type veth peer name "$node_veth"
node_link_created=1
sudo -n ip link set "$client_node_veth" netns "$client_namespace"
sudo -n ip link set "$node_veth" netns "$node_namespace"
sudo -n ip link add "$client_decoy_veth" type veth peer name "$decoy_veth"
decoy_link_created=1
sudo -n ip link set "$client_decoy_veth" netns "$client_namespace"
sudo -n ip link set "$decoy_veth" netns "$decoy_namespace"
sudo -n ip -n "$client_namespace" link set "$client_node_veth" name eth0
sudo -n ip -n "$client_namespace" link set "$client_decoy_veth" name eth1
sudo -n ip -n "$node_namespace" link set "$node_veth" name eth0
sudo -n ip -n "$decoy_namespace" link set "$decoy_veth" name eth0
sudo -n ip -n "$client_namespace" address add 192.0.2.1/30 dev eth0
sudo -n ip -n "$node_namespace" address add 192.0.2.2/30 dev eth0
sudo -n ip -n "$client_namespace" address add 198.51.100.1/30 dev eth1
sudo -n ip -n "$decoy_namespace" address add 198.51.100.2/30 dev eth0
sudo -n ip -n "$client_namespace" link set lo up
sudo -n ip -n "$node_namespace" link set lo up
sudo -n ip -n "$decoy_namespace" link set lo up
sudo -n ip -n "$client_namespace" link set eth0 up
sudo -n ip -n "$client_namespace" link set eth1 up
sudo -n ip -n "$node_namespace" link set eth0 up
sudo -n ip -n "$decoy_namespace" link set eth0 up
sudo -n ip -n "$client_namespace" route add default via 198.51.100.2 dev eth1
sudo -n ip -n "$client_namespace" route add 224.0.0.0/4 dev eth1
sudo -n ip -n "$client_namespace" route add 255.255.255.255/32 dev eth1

for route_expectation in \
    "192.0.2.2 eth0" \
    "239.255.73.37 eth1" \
    "255.255.255.255 eth1"; do
    read -r destination expected_device <<<"$route_expectation"
    route_output="$(sudo -n ip -n "$client_namespace" route get "$destination")"
    if [[ ! " $route_output " =~ [[:space:]]dev[[:space:]]$expected_device([[:space:]]|$) ]]; then
        echo "error: route to $destination did not use $expected_device: $route_output" >&2
        exit 1
    fi
done

wait_for_offer() {
    local output_file=$1
    local offer_pid=$2
    for _attempt in {1..100}; do
        if grep -q '^Passcode: [0-9]\{5\}-[0-9]\{5\}$' "$output_file" 2>/dev/null; then
            return 0
        fi
        if ! kill -0 "$offer_pid" 2>/dev/null; then
            echo "error: pairing offer exited before printing a passcode" >&2
            cat "$output_file" >&2 || true
            return 1
        fi
        sleep 0.05
    done
    echo "error: pairing offer did not become ready" >&2
    return 1
}

run_node start >"$work_dir/node-start.out"
run_client start >"$work_dir/client-start.out"
run_node status | grep -Eq '^running pid=[0-9]+ '
run_client status | grep -Eq '^running pid=[0-9]+ '
node_id="$(sed -n 's/^o-node started: //p' "$work_dir/node-start.out")"
client_id="$(sed -n 's/^o-node started: //p' "$work_dir/client-start.out")"
if [[ -z "$node_id" || -z "$client_id" || "$node_id" == "$client_id" ]]; then
    echo "error: detached nodes did not produce distinct stable identities" >&2
    exit 1
fi

# A syntactically valid wrong passcode consumes one offer but installs no trust
# state on either side.
wrong_offer_out="$work_dir/wrong-offer.out"
wrong_offer_err="$work_dir/wrong-offer.err"
run_node pair --offer-timeout-seconds 30 --io-timeout-seconds 5 \
    >"$wrong_offer_out" 2>"$wrong_offer_err" &
wrong_offer_pid=$!
wait_for_offer "$wrong_offer_out" "$wrong_offer_pid"
offered_code="$(sed -n 's/^Passcode: //p' "$wrong_offer_out")"
if [[ "${offered_code:0:1}" == 0 ]]; then
    wrong_code="1${offered_code:1}"
else
    wrong_code="0${offered_code:1}"
fi
if printf '%s\n' "$wrong_code" | run_client pair "$node_id" \
    --passcode-stdin --discovery-timeout-millis 3000 --io-timeout-seconds 5 \
    >"$work_dir/wrong-join.out" 2>"$work_dir/wrong-join.err"; then
    echo "error: wrong pairing passcode was accepted" >&2
    exit 1
fi
if ! grep -Fq 'pairing authentication failed' "$work_dir/wrong-join.err"; then
    echo "error: wrong-code join failed without the expected authentication error" >&2
    cat "$work_dir/wrong-join.err" >&2
    exit 1
fi
if wait "$wrong_offer_pid"; then
    echo "error: offering side accepted a wrong pairing passcode" >&2
    exit 1
fi
if ! grep -Fq 'pairing attempt failed; the one-use offer was consumed' "$wrong_offer_err"; then
    echo "error: wrong-code offer failed without the expected one-use-consumption error" >&2
    cat "$wrong_offer_err" >&2
    exit 1
fi
if sudo -n find "$node_config/ostadix/peers" "$client_config/ostadix/peers" \
    -name peer.json -print -quit 2>/dev/null | grep -q .; then
    echo "error: wrong pairing passcode left durable peer state" >&2
    exit 1
fi
printf 'passcode pairing wrong-code no-state boundary: PASS\n'

# A fresh offer authenticates both public bundles over the direct routed
# pairing endpoint and issues reciprocal client certificates while each
# private key stays on the machine that generated it.
offer_out="$work_dir/offer.out"
offer_err="$work_dir/offer.err"
run_node pair --offer-timeout-seconds 30 --io-timeout-seconds 5 \
    >"$offer_out" 2>"$offer_err" &
offer_pid=$!
wait_for_offer "$offer_out" "$offer_pid"
pairing_code="$(sed -n 's/^Passcode: //p' "$offer_out")"
printf '%s\n' "$pairing_code" | run_client pair "$node_id" \
    --passcode-stdin --address 192.0.2.2:7340 --io-timeout-seconds 5 \
    >"$work_dir/join.out" 2>"$work_dir/join.err"
wait "$offer_pid"

# The one-use offer process has exited, so its listener is no longer available.
# This proves only listener consumption; no captured network frame is reused.
if printf '%s\n' "$pairing_code" | run_client pair "$node_id" \
    --passcode-stdin --address 192.0.2.2:7340 --io-timeout-seconds 2 \
    >"$work_dir/consumed-listener.out" 2>"$work_dir/consumed-listener.err"; then
    echo "error: consumed one-use pairing listener accepted another connection" >&2
    exit 1
fi
if ! grep -Fq 'could not reach pairing offer' "$work_dir/consumed-listener.err"; then
    echo "error: consumed pairing listener failed without the expected reachability error" >&2
    cat "$work_dir/consumed-listener.err" >&2
    exit 1
fi

sudo -n python3 - \
    "$node_config/ostadix/peers" \
    "$client_config/ostadix/peers" \
    "$node_id" "$client_id" <<'PY'
import ipaddress
import json
import stat
import sys
from pathlib import Path

node_root, client_root = map(Path, sys.argv[1:3])
node_id, client_id = sys.argv[3:5]

def require_directory_mode(path: Path):
    metadata = path.lstat()
    if not stat.S_ISDIR(metadata.st_mode):
        raise SystemExit(f"stored peer path is not a real directory: {path}")
    actual = stat.S_IMODE(metadata.st_mode)
    if actual != 0o700:
        raise SystemExit(f"stored peer directory {path} has mode {actual:04o}, expected 0700")

def require_regular_file(path: Path):
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode):
        raise SystemExit(f"stored peer path is not a regular file: {path}")
    return stat.S_IMODE(metadata.st_mode)

def load_one(root: Path, expected_id: str, expected_address: str):
    require_directory_mode(root)
    peer_files = sorted(root.glob("*/peer.json"))
    if len(peer_files) != 1:
        raise SystemExit(f"expected one paired peer under {root}, found {len(peer_files)}")
    peer = json.loads(peer_files[0].read_text(encoding="utf-8"))
    if peer.get("node_id") != expected_id:
        raise SystemExit(f"unexpected paired node id: {peer.get('node_id')!r}")
    if peer.get("address") != expected_address:
        raise SystemExit(f"unexpected paired route: {peer.get('address')!r}")
    if peer.get("security_mode") != "paired-public-key":
        raise SystemExit(f"unexpected paired security mode: {peer.get('security_mode')!r}")
    if ipaddress.ip_address(peer["address"].rsplit(":", 1)[0]).is_loopback:
        raise SystemExit("paired route unexpectedly used loopback")
    directory = peer_files[0].parent
    require_directory_mode(directory)
    for public_name in ("peer.json", "ca.pem", "client-cert.pem", "node-signing-public.v2"):
        public_path = directory / public_name
        public_mode = require_regular_file(public_path)
        if public_mode & 0o022:
            raise SystemExit(
                f"public paired material {public_path} is group/other-writable: {public_mode:04o}"
            )
        if b"PRIVATE KEY" in public_path.read_bytes():
            raise SystemExit(f"private key marker leaked into public file {public_name}")
    client_key = directory / "client-key.pem"
    client_key_mode = require_regular_file(client_key)
    if client_key_mode != 0o600:
        raise SystemExit(
            f"local paired client key {client_key} has mode {client_key_mode:04o}, expected 0600"
        )
    return directory

node_peer = load_one(node_root, client_id, "192.0.2.1:7337")
client_peer = load_one(client_root, node_id, "192.0.2.2:7337")
node_client_key = node_peer.joinpath("client-key.pem").read_bytes()
client_client_key = client_peer.joinpath("client-key.pem").read_bytes()
if b"PRIVATE KEY" not in node_client_key or b"PRIVATE KEY" not in client_client_key:
    raise SystemExit("paired client key file did not contain private-key PEM material")
if node_client_key == client_client_key:
    raise SystemExit("both nodes retained the same client private key")
PY
printf 'reciprocal public-key pairing, private storage, distinct keys, and one-use-listener boundary: PASS\n'

# Neither default service may expose the historical shared-key bootstrap.
if sudo -n ip netns exec "$client_namespace" python3 - <<'PY'
import socket
s = socket.socket()
s.settimeout(0.5)
s.connect(("192.0.2.2", 7338))
PY
then
    echo "error: paired-default node namespace exposed the legacy bootstrap port" >&2
    exit 1
fi
if sudo -n ip netns exec "$node_namespace" python3 - <<'PY'
import socket
s = socket.socket()
s.settimeout(0.5)
s.connect(("192.0.2.1", 7338))
PY
then
    echo "error: paired-default client namespace exposed the legacy bootstrap port" >&2
    exit 1
fi
printf 'paired-default legacy-bootstrap-disabled in both namespaces: PASS\n'

run_node restart >"$work_dir/node-restart.out"
run_client restart >"$work_dir/client-restart.out"

profile_json="$work_dir/profile.json"
reverse_profile_json="$work_dir/reverse-profile.json"
attempt_error="$work_dir/profile-attempt.err"
profile_ready=0
for _attempt in {1..10}; do
    if run_client profile --node "$node_id" \
        --connect-timeout-seconds 2 \
        --io-timeout-seconds 5 \
        >"$profile_json" 2>"$attempt_error" \
        && run_node profile --node "$client_id" \
        --connect-timeout-seconds 2 \
        --io-timeout-seconds 5 \
        >"$reverse_profile_json" 2>>"$attempt_error"; then
        profile_ready=1
        break
    fi
    sleep 0.2
done
if [[ "$profile_ready" != 1 ]]; then
    echo "error: paired nodes did not reconnect bidirectionally after restart" >&2
    cat "$attempt_error" >&2
    sudo -n cat "$node_state/ostadix/node/o-node.log" >&2 || true
    sudo -n cat "$client_state/ostadix/node/o-node.log" >&2 || true
    exit 1
fi

sudo -n python3 - "$profile_json" "$reverse_profile_json" "$node_id" "$client_id" <<'PY'
import json
import sys
from pathlib import Path

forward = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
reverse = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
if forward.get("node_id") != sys.argv[3] or reverse.get("node_id") != sys.argv[4]:
    raise SystemExit("profile node identities did not match the paired targets")
for profile in (forward, reverse):
    if profile.get("transport") != "tcp+tls1.3+mutual-x509":
        raise SystemExit(f"unexpected hosted transport: {profile.get('transport')!r}")
PY
printf 'paired bidirectional restart reconnect over non-loopback links: PASS\n'

# Simulate the crash window in which only one machine retained the completed
# exchange. Both operators deliberately use --replace: the retained side
# replaces its pin, while the missing side performs an ordinary first store.
lost_client_peer="$work_dir/lost-client-peer"
sudo -n mv "$client_config/ostadix/peers/$node_id" "$lost_client_peer"
replacement_offer_out="$work_dir/replacement-offer.out"
replacement_offer_err="$work_dir/replacement-offer.err"
run_node pair --replace --offer-timeout-seconds 30 --io-timeout-seconds 5 \
    >"$replacement_offer_out" 2>"$replacement_offer_err" &
replacement_offer_pid=$!
wait_for_offer "$replacement_offer_out" "$replacement_offer_pid"
replacement_code="$(sed -n 's/^Passcode: //p' "$replacement_offer_out")"
printf '%s\n' "$replacement_code" | run_client pair "$node_id" \
    --replace --passcode-stdin --address 192.0.2.2:7340 --io-timeout-seconds 5 \
    >"$work_dir/replacement-join.out" 2>"$work_dir/replacement-join.err"
wait "$replacement_offer_pid"

sudo -n python3 - \
    "$lost_client_peer/client-key.pem" \
    "$client_config/ostadix/peers/$node_id/client-key.pem" <<'PY'
import sys
from pathlib import Path

old_key, new_key = (Path(value).read_bytes() for value in sys.argv[1:3])
if old_key == new_key:
    raise SystemExit("explicit one-sided recovery reused the prior local private key")
PY

run_node restart >"$work_dir/node-replacement-restart.out"
run_client restart >"$work_dir/client-replacement-restart.out"
replacement_profile_ready=0
for _attempt in {1..10}; do
    if run_client profile --node "$node_id" \
        --connect-timeout-seconds 2 \
        --io-timeout-seconds 5 \
        >"$work_dir/replacement-profile.json" 2>"$work_dir/replacement-profile.err" \
        && run_node profile --node "$client_id" \
        --connect-timeout-seconds 2 \
        --io-timeout-seconds 5 \
        >"$work_dir/replacement-reverse-profile.json" \
        2>>"$work_dir/replacement-profile.err"; then
        replacement_profile_ready=1
        break
    fi
    sleep 0.2
done
if [[ "$replacement_profile_ready" != 1 ]]; then
    echo "error: explicit one-sided pairing recovery did not reconnect bidirectionally" >&2
    cat "$work_dir/replacement-profile.err" >&2
    exit 1
fi
printf 'explicit replacement recovers one-sided pairing persistence: PASS\n'

sudo -n ip -n "$client_namespace" link set eth0 down
if run_client profile \
    --connect-timeout-seconds 1 \
    --io-timeout-seconds 2 \
    >"$work_dir/disconnected-profile.out" \
    2>"$work_dir/disconnected-profile.err"; then
    echo "error: ordinary client reached the node after its real-node link was removed" >&2
    exit 1
fi
printf 'zero-config LAN severed-link boundary: PASS\n'
