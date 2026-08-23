#!/usr/bin/env bash
# Prove the ordinary zero-configuration node path from a multi-homed Linux
# client whose real node is not on its default route. The client receives no
# address, TLS, or manual override.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "error: smoke-zero-config-lan-netns.sh must run inside a Git worktree" >&2
    exit 2
}
if [[ "$(uname -s)" != Linux ]]; then
    echo "error: zero-config LAN namespace smoke requires Linux" >&2
    exit 2
fi
for executable in ip python3 sudo; do
    if ! command -v "$executable" >/dev/null 2>&1; then
        echo "error: required executable is unavailable: $executable" >&2
        exit 2
    fi
done
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

run_node start >"$work_dir/node-start.out"
run_node status | grep -Eq '^running pid=[0-9]+ '

profile_json="$work_dir/profile.json"
attempt_error="$work_dir/profile-attempt.err"
profile_ready=0
for _attempt in {1..10}; do
    if run_client profile \
        --connect-timeout-seconds 2 \
        --io-timeout-seconds 5 \
        >"$profile_json" 2>"$attempt_error"; then
        profile_ready=1
        break
    fi
    sleep 0.2
done
if [[ "$profile_ready" != 1 ]]; then
    echo "error: ordinary client could not profile the non-loopback node" >&2
    cat "$attempt_error" >&2
    sudo -n cat "$node_state/ostadix/node/o-node.log" >&2 || true
    exit 1
fi

sudo -n python3 - "$profile_json" "$client_config/ostadix/peers" <<'PY'
import ipaddress
import json
import sys
from pathlib import Path

profile = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
peer_files = sorted(Path(sys.argv[2]).glob("*/peer.json"))
if len(peer_files) != 1:
    raise SystemExit(f"expected exactly one enrolled peer, found {len(peer_files)}")
peer = json.loads(peer_files[0].read_text(encoding="utf-8"))
if peer.get("address") != "192.0.2.2:7337":
    raise SystemExit(f"unexpected discovered route: {peer.get('address')!r}")
address_ip = ipaddress.ip_address(peer["address"].rsplit(":", 1)[0])
if address_ip.is_loopback:
    raise SystemExit("discovered peer route is loopback")
if peer.get("node_id") != profile.get("node_id"):
    raise SystemExit("enrolled peer identity differs from profiled node identity")
if profile.get("transport") != "tcp+tls1.3+mutual-x509":
    raise SystemExit(f"unexpected hosted transport: {profile.get('transport')!r}")
PY
printf 'zero-config LAN non-default non-loopback profile: PASS\n'

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
