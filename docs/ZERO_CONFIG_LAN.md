# Zero-configuration LAN nodes

The ordinary Ostadix node path is designed around one rule:

> Transport coordinates, certificates, keys, node generations, capabilities,
> leases, operation identifiers, task digests, and proof artifacts are internal
> protocol state. They are not ordinary user input.

The expert interfaces still expose those values, but only when an operator
explicitly chooses manual mode.

## Ordinary use

On the machine that contributes execution capacity:

```bash
o node start
```

The command provisions a stable node identity, LAN certificate material, a
Hosted V2 receipt identity, discovery, enrollment, and a detached server. It
binds the ordinary hosted service to the LAN and returns to the shell. Closing
the terminal or an ordinary remote-desktop terminal does not send the hosted
server the terminal's hangup signal.

Inspect or stop it with:

```bash
o node status
o node stop
o node restart
```

On another machine in the same local network:

```bash
o node list
o node profile
o node doctor
o node run examples/hello.O
o node session run examples/hello.O
```

No address, host name, port, CA, certificate, private key, receipt key,
capability, placement lease, operation ID, task digest, or attempt generation
is required.

The root `o-node-quickstart.sh` script is only a compatibility front door for
these same commands. It stores no parallel demo configuration and has no
localhost-only mode: no arguments starts the node, `--run FILE.O` delegates to
the automatically managed V2 session path, and `--manual` is the explicit
operator escape hatch.

When one reachable node exists, Ostadix uses it. When several exist, Ostadix
uses the remembered preference when possible and otherwise chooses a stable,
deterministic first node. Selecting a different semantic node is the only
ordinary choice exposed to the user:

```bash
o node use ostadix-example-host-12ab34cd
```

That preference is remembered. It is not a transport configuration.

## What automatic mode does

The client follows one resolver pipeline for every ordinary node command:

1. Discover LAN advertisements over Ostadix UDP discovery.
2. Select the requested, remembered, or deterministic node identity.
3. Fetch enrollment material from the node when no usable enrollment exists.
4. Store the resulting peer record and credentials in the user's private
   Ostadix configuration directory.
5. Connect to the current advertised address through TLS 1.3.
6. For durable sessions, generate capabilities and exact protocol artifacts
   internally, submit them, poll the terminal outcome, and close temporary
   sessions automatically.

A DHCP address change therefore changes routing, not semantic node identity.
The node ID is generated once and persisted independently of the current host
address.

Discovery is interface-aware. On multi-homed machines, including hosts whose
default route is a VPN, Ostadix opens a probe socket on every active IPv4
interface. It sends directed broadcast and multicast probes from each one, and
the node joins the multicast group on every active IPv4 interface. The source
address of the node's reply remains the routing coordinate, so a client on the
physical LAN does not receive a VPN-only or loopback address merely because
that route is preferred by the host.

This is zero-configuration discovery for directly connected IPv4 networks; it
does not cross routers, VLAN boundaries, NAT, or the public Internet. For a
routed node, use the expert `--address` and TLS identity flags or provide a
network-level multicast relay deliberately.

## Deliberately weak LAN trust model

Automatic mode is named `lan-open` because its boundary is intentionally
simple:

- Any machine that can reach the node's enrollment port can obtain the shared
  LAN client identity.
- Discovery is unauthenticated.
- Enrollment is plaintext.
- All automatically enrolled clients currently share one client certificate
  and private key.
- TLS still encrypts ordinary hosted traffic and authenticates the node after
  enrollment, but it does not distinguish one enrolled LAN client from
  another.
- The LAN-open Hosted V2 authorizer accepts a self-consistent, unexpired lease
  signed by any key when its exact command fields match the live request.

In practical terms, access to the local network is treated as permission to
use the node. This mode is appropriate only where that is the intended policy.
It prioritizes immediate access and low cognitive overhead over resistance to
another machine already present on the LAN.

## Expert and IT mode

Manual mode preserves the prior explicit trust surface. It is selected by
passing `--manual` or any explicit connection override. Examples:

```bash
o-node serve --manual \
  --node-id controlled-node \
  --bind 192.0.2.10:7337 \
  --cert /secure/node-cert.pem \
  --key /secure/node-key.pem \
  --client-ca /secure/client-ca.pem \
  --v2-state-dir /secure/hosted-v2 \
  --v2-node-signing-key /secure/node-signing-key.v2 \
  --v2-authority-public-key /secure/placement-public-key.v2

octl node profile --manual \
  --address 192.0.2.10:7337 \
  --server-name node.example \
  --ca /secure/ca.pem \
  --cert /secure/client-cert.pem \
  --key /secure/client-key.pem
```

The high-level wrapper leaves the raw server CLI available as `o node-host`.
Manual flags are overrides and diagnostic controls, not prerequisites for
ordinary use.

## Storage

Automatic node material follows XDG locations where available:

- Node configuration and PKI: `$XDG_CONFIG_HOME/ostadix/lan-open`, otherwise
  `~/.config/ostadix/lan-open`
- Enrolled peers and client-side automatic authority:
  `$XDG_CONFIG_HOME/ostadix/peers`, otherwise `~/.config/ostadix/peers`
- Durable node state: `$XDG_STATE_HOME/ostadix/lan-open-v2`, otherwise
  `~/.local/state/ostadix/lan-open-v2`
- Automatic client sessions: `$XDG_STATE_HOME/ostadix/client-sessions`,
  otherwise `~/.local/state/ostadix/client-sessions`
- Detached node PID and log: `$XDG_STATE_HOME/ostadix/node`, otherwise
  `~/.local/state/ostadix/node`

Private key and capability files are written with owner-only permissions on
Unix systems.

## Current operational boundary

`o node start` detaches the process from the invoking terminal. It is not yet a
system service installer. It therefore survives an ordinary shell or remote
terminal closing, but automatic restart after a machine reboot is outside this
patch. A later service-manager adapter can make persistence itself an internal
projection without changing the ordinary command surface.

## Verification status

The checks actually executed for this patch, together with the remaining Rust
build gate, are recorded in
[Zero-configuration LAN patch verification](ZERO_CONFIG_VERIFICATION.md).
