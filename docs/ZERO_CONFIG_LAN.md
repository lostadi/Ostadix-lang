# Zero-configuration LAN nodes

The ordinary Ostadix node path is designed around one rule:

> Transport coordinates, certificates, keys, node generations, capabilities,
> leases, operation identifiers, task digests, and proof artifacts are internal
> protocol state. They are not ordinary user input.

The expert interfaces still expose those values, but only when an operator
explicitly chooses manual mode.

## Ordinary use

Start a node on both machines:

```bash
o node start
```

The command provisions a stable node identity, LAN certificate material, a
Hosted V2 receipt identity, interface-aware discovery, and a detached server.
It does **not** expose the historical plaintext bootstrap service. Closing the
terminal or an ordinary remote-desktop terminal does not send the hosted server
the terminal's hangup signal.

On the first machine, create a foreground one-use pairing offer:

```bash
o node pair
```

The command prints its node ID and a ten-digit passcode, then waits for one
connection attempt for up to five minutes. On the second machine, name that
node and type the printed passcode at the hidden prompt:

```bash
o node pair NODE_ID
```

For noninteractive automation, `o node pair NODE_ID --passcode-stdin` reads one
line from standard input. The passcode is deliberately not accepted as a
positional argument, so the ordinary interface does not place it in shell
history or the process list.

After the exchange, both machines retain reciprocal pinned identities. Either
machine can later select the other by node ID, and ordinary commands reuse the
remembered identity without another passcode or transport flags:

```bash
o node list
o node profile --node NODE_ID
o node doctor --node NODE_ID
o node run --node NODE_ID examples/hello.O
o node session run --node NODE_ID examples/hello.O
```

When the paired node is the remembered preference, `--node NODE_ID` may be
omitted. Pairing records the current endpoint as a reconnect coordinate; a
later command can therefore reconnect from remembered state even when
discovery is unavailable.

Inspect or stop the local detached node with:

```bash
o node status
o node stop
o node restart
```

No CA, certificate, private key, receipt key, capability, placement lease,
operation ID, task digest, or attempt generation is ordinary user input.

The root `o-node-quickstart.sh` script is only a compatibility front door for
these same commands. It stores no parallel demo configuration and has no
localhost-only mode: no arguments starts the node, `--run FILE.O` delegates to
the automatically managed V2 session path, and `--manual` is the explicit
operator escape hatch.

When one paired node exists, Ostadix uses it. When several exist, Ostadix uses
the remembered preference when possible and otherwise chooses a stable,
deterministic first node. Selecting a different semantic node is the only
ordinary choice exposed to the user:

```bash
o node use ostadix-example-host-12ab34cd
```

That preference is remembered. It is not a transport configuration.

## What pairing and automatic reuse do

The offering node generates ten uniformly distributed decimal digits and
formats them as `00000-00000`. The offer accepts one connection attempt and
expires after 300 seconds by default. The two processes run SPAKE2 over the
short passcode, derive directional keys with HKDF-SHA-256, and exchange explicit
HMAC-SHA-256 confirmations. Those confirmations bind both node identities,
both public bundles, both SPAKE2 messages, both certificate requests, and both
issued certificates. A wrong code, changed transcript, replayed completed
offer, or late connection does not produce a stored peer.

Each node creates a fresh per-peer private client key locally and sends only
its certificate signing request. The pairing channel carries public server CA,
client-issuer CA, CSR, issued client certificate, and Hosted V2 receipt public
key material. Private keys never cross the pairing connection. Each destination
issues the other node a client certificate, so later traffic uses ordinary TLS
1.3 mutual X.509 authentication.

The resulting record pins the paired node's semantic ID, server CA, expected
server name, and receipt public key. Those identity coordinates are immutable:
an unauthenticated advertisement cannot rotate them, and conflicting material
for an existing node ID fails closed instead of silently replacing the pair.

Destination-issued client certificates are valid for 397 days. Renewing them,
or recovering when pairing state persisted on only one machine, requires a
fresh offer with explicit replacement enabled on both sides:

```bash
# offering machine
o node pair --replace
# joining machine
o node pair NODE_ID --replace
```

Ordinary pairing still refuses changed pins. `--replace` permits the staged
replacement of an existing paired record and lets the missing side perform its
ordinary first store; it does not authorize identity changes from discovery.

Every later ordinary node command follows one resolver pipeline:

1. Use UDP discovery, when available, only as a current routing hint.
2. Select the requested, remembered, or deterministic paired node identity.
3. Require an existing reciprocal pairing record for a pairing-required node.
4. Connect using the remembered CA, server name, receipt key, locally retained
   private key, and destination-issued client certificate.
5. If discovery is unavailable, try the remembered last endpoint.
6. For durable sessions, generate capabilities and exact protocol artifacts
   internally, submit them, poll the terminal outcome, and close temporary
   sessions automatically.

When a paired identity is known but its remembered route is stale, the caller
may override only the route for one invocation:

```bash
octl node profile --node NODE_ID --address HOST:7337
```

This retains the stored server CA, expected server name, client credential, and
receipt-key pin. It neither persists the supplied route nor replaces paired
identity material.

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

Discovery is unauthenticated and establishes no identity or permission. Direct
discovery is limited to connected IPv4 networks and does not cross routers,
VLAN boundaries, NAT, or the public Internet. When the route is already known,
the joiner can bypass discovery while preserving the passcode exchange:

```bash
o node pair NODE_ID --address HOST:7340
```

The supplied pairing endpoint must be directly reachable over TCP. This records
the observed address for later reconnect but provides no NAT traversal, relay,
hole punching, or persistent node mesh. Duplicate or spoofed advertisements for
a paired node ID can make route selection fail or select an unreachable route;
they cannot replace the stored identity pins, so their effect is availability
rather than silent re-enrollment.

## Security boundary

Passcode pairing authenticates the reciprocal transport identities used by
later mTLS connections. Its current boundary is intentionally narrower than a
general node-membership or authorization system:

- The repository uses the `spake2` crate's Ed25519-group implementation; that
  dependency has not been independently audited as part of Ostadix evidence.
- A pairing offer is consumed by its first connection attempt. There is no
  retry allowance under the same code.
- Version 1 issues a distinct client certificate per paired peer, but inbound
  trust is pairing-CA-wide: the server accepts certificates issued by that CA,
  not only the leaf stored for a current peer record. There is no leaf
  allowlist, unpair operation, or revocation mechanism; replacement therefore
  does not revoke an already issued certificate before its expiry.
- Pairing pins transport and receipt identity. It does not make the automatic
  Hosted V2 placement authorizer restrictive: that authorizer still accepts a
  self-consistent, unexpired lease signed by any key when its exact command
  fields match the live request.
- Pairing does not create a persistent background connection, replicated
  membership service, scheduler, or node mesh. An ordinary command opens a
  connection when work or inspection is requested.

## Explicit legacy LAN-open compatibility

The former reachability-is-authorization behavior remains available only by
explicit request:

```bash
o node start --lan-open
# or, for the raw surface:
o-node serve --lan-open
```

Legacy LAN-open exposes a plaintext bootstrap service and lets any
LAN-reachable caller download a shared client private key. It should be used
only when that weak boundary is deliberate. A paired peer cannot be downgraded
or overwritten by legacy bootstrap material.

## Expert and IT mode

Manual mode preserves the prior explicit trust surface. It is selected by
passing `--manual` or explicit trust-identity coordinates. The paired
`--node NODE_ID --address HOST:PORT` combination is a route-only exception: it
retains the stored identity and credential. Manual examples:

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

- Node configuration, pairing CA, and PKI:
  `$XDG_CONFIG_HOME/ostadix/lan-open`, otherwise
  `~/.config/ostadix/lan-open`
- Paired peers and client-side automatic authority:
  `$XDG_CONFIG_HOME/ostadix/peers`, otherwise `~/.config/ostadix/peers`
- Durable node state: `$XDG_STATE_HOME/ostadix/lan-open-v2`, otherwise
  `~/.local/state/ostadix/lan-open-v2`
- Automatic client sessions: `$XDG_STATE_HOME/ostadix/client-sessions`,
  otherwise `~/.local/state/ostadix/client-sessions`
- Detached node PID and log: `$XDG_STATE_HOME/ostadix/node`, otherwise
  `~/.local/state/ostadix/node`

Private key and capability files are written with owner-only permissions on
Unix systems. Per-peer private keys remain local; peer directories contain the
public trust material plus the local credential needed for that exact peer and
do not accept private material supplied by the remote node.

## Current operational boundary

`o node start` detaches the process from the invoking terminal. It is not yet a
system service installer. It therefore survives an ordinary shell or remote
terminal closing, but automatic restart after a machine reboot is outside this
patch. `o node pair` is intentionally foreground and temporary; only the
resulting trust record persists. A later service-manager adapter can make node
process persistence an internal projection without changing the ordinary
command surface.

## Verification status

The checks actually executed for this patch, together with the remaining Rust
build gate, are recorded in
[Zero-configuration LAN patch verification](ZERO_CONFIG_VERIFICATION.md).
