# Ostadix project mesh V1

Project mesh V1 lets `o-link` place source-closed project routes on authenticated
Ostadix nodes. It is a peer data and execution plane for whole heterogeneous
codebases: the original files remain ordinary Rust, C, Python, shell, or other
foreign-language sources and do not need to be rewritten as Ostadix-lang.

This document describes the implemented boundary. The protocol is
`ostadix.mesh-transport/v1`, negotiated over TLS 1.3 mutual authentication with
the `ostadix-mesh/1` ALPN.

## Quick start

On every participating Unix-like host (including WSL), build Ostadix and start
a node:

```bash
./setup.sh -y --minimal
source "$HOME/.config/ostadix/env.sh"
o node start
```

Pair the machines once as described in [Zero-configuration LAN
nodes](ZERO_CONFIG_LAN.md): run `o node pair` on the offering node, then run
`o node pair NODE_ID` and enter the one-use passcode on the joining node.

On the machine holding the codebase, inspect its routes and run one through the
mesh:

```bash
o-link ./large-codebase --project --list-routes

o-link ./large-codebase --project --run --route build \
  --mesh \
  --explain-mesh \
  --mesh-trace-out mesh-attempt.json
```

Bare `--mesh` means `--mesh=prefer`. It selects an eligible authenticated peer
when one is available and otherwise permits only the default fallback when
actor execution is proven not to have started. To require a remote result and
prohibit local execution:

```bash
o-link ./large-codebase --project --run --route build \
  --mesh=required \
  --mesh-local-fallback=never \
  --mesh-trace-out mesh-required.json
```

The selected route must name commands and runtimes that exist on the target
node. The bundle carries the codebase, route contracts, and files; it does not
install arbitrary host toolchains.

## Canonical execution unit

For foreign project bundles, the canonical mesh IR is the Project Logical
HGraph (`LogicalHGraphV1`). `o-link` first resolves the ordinary project route
selection. For each selected route it then treats that route and all transitive
prerequisites as one source-closed actor island with one materialized workspace.

Every actor request binds:

- the SHA-256 identity and byte length of the exact serialized `ProjectBundle`;
- the selected route ID and the exact route-contract digest;
- the canonical Project Logical HGraph digest for explicit execution of that
  route;
- the destination node ID and actor generation; and
- the portable execution limits used by the route.

The destination independently loads the bundle, recomputes the route-contract
and logical-graph digests, checks runtime availability and capacity, and rejects
substitution before admission. The mesh does not translate the foreign sources
into OIR or infer a new authority from file extensions. Hosted V1, durable
Hosted V2, and mesh V1 are separate protocols; a frozen V2 one-shim placement
record does not itself authorize a project-mesh actor.

For multi-alternative policies, ready route actors can execute concurrently on
different nodes. Target ordering is capacity first, then observed latency and
stable tie-breakers. Advertised slots bound the initial assignment; each node
still performs live admission against its own actor limit.

## Discovery and peer trust

The resolver joins two local sources of routing information:

1. live UDP LAN advertisements, used as endpoint hints; and
2. the durable paired-peer registry, which owns the pinned server CA, server
   name, client certificate/key, and remembered address, and retains the
   peer's receipt-signing identity metadata.

An advertisement may refresh the socket address, but it cannot replace the
stored TLS identity. A pairing-required advertisement is ineligible until the
user completes passcode pairing. Explicit legacy `--lan-open` nodes retain
their existing bootstrap enrollment path. `--mesh-peer-root PATH` selects a
different local paired-peer registry for one invocation.
`--mesh-no-lan-discovery` makes that registry a closed discovery set: no UDP
probe is sent and live advertisements cannot add or refresh endpoints.

Mesh V1 authorization is transport-scoped: any client certificate accepted by
the listener's configured client CA can submit project actors. Actor records
bind the exact actor specification but do not bind a separate client principal
or apply per-principal ACLs/quotas. Pair only machines that should receive this
execution capability, or use manual listener credentials and `--no-mesh`.

This is LAN and paired-record discovery, not Internet-wide peer discovery. A
stored peer may name a directly reachable routed address, but mesh V1 provides
no NAT traversal, hole punching, relay, DHT, Internet gossip, or federated/WAN
registry. Both endpoints still need ordinary TCP reachability to the node
listener.

## Wire and durable execution flow

The client and selected node perform these bounded steps:

1. Query the authenticated node profile and current free actor capacity.
2. Split the exact bundle into content-addressed chunks, upload missing chunks,
   and commit the ordered artifact manifest. The default chunk size is 512 KiB;
   the protocol maximum is 1 MiB per chunk.
3. Probe the exact route requirements. The node recomputes bundle, route, and
   Project Logical HGraph bindings and reports missing runtimes or capacity.
4. Submit a digest-bound actor ID/generation targeted to that node. The
   portable `RunOptions` projection is part of the actor digest, and the node
   rejects it before admission if it exceeds the node's configured execution
   ceiling. Mesh admission then durably records `Running` before a bounded
   background worker executes.
5. Poll status, request cancellation when needed, and fetch a terminal result
   through content-addressed result chunks. The client verifies the manifest,
   bytes, decoded result, exit status, and success summary.

The node retains artifacts, actor records, fences, and result chunks beneath
its mesh state root. Automatic `o node start` enables this runtime at
`V2_STATE_DIR/mesh-v1`. `o-node serve --no-mesh` disables it; manual serving is
mesh-off unless `--mesh-state-dir PATH` is supplied alongside durable V2 setup.

## Retry, replay, and local fallback

`--mesh-retries N` is the number of additional remote generations after the
first attempt (default `2`, maximum `64`). A retry chooses from the ordered peer
pool and creates a new actor generation. It never moves a running process.

The scheduler classifies the preceding attempt as:

- **proven not started**: no actor submission was delivered, the node rejected
  it before admission, or an atomic `FenceActorIfAbsent` durably proved and
  fenced that exact actor generation absent;
- **ambiguous**: the actor may have started, but the authenticated destination
  could not be reconciled; or
- **executed**: execution produced a terminal result or execution-stage
  failure.

Remote replay is allowed after a proven-not-started outcome. Ambiguous delivery
always fails closed because another generation could overlap the old actor.
After a confirmed terminal execution failure, replay is allowed only when every
member of the route and prerequisite island carries the bundle-bound
`failure_continuation = "declared_idempotent"` contract. Omission defaults to
fail-closed, unproven replay. `declared_idempotent` is an author declaration,
not independently verified purity, sandboxing, or exactly-once effects.
A nonzero route result is such a settled execution failure: it is retained in
the trace and may advance to another generation only under that island-wide
contract.

Local fallback is additionally controlled by the mesh mode:

| Mode | Local execution rule |
| --- | --- |
| `--mesh=prefer --mesh-local-fallback=pre-send` | Allowed only after proven-not-started delivery; this is the default. |
| `--mesh=prefer --mesh-local-fallback=idempotent` | Also allowed after a confirmed terminal execution failure when the entire island is declared idempotent; ambiguity still fails closed. |
| `--mesh=prefer --mesh-local-fallback=never` | Never execute locally. |
| `--mesh=required` | Never execute locally, regardless of the fallback flag. |

The same island-wide replay rule governs continuing to another fallback route
after a route settles unsuccessfully.

## Migration boundary

Mesh V1 migration is settled-boundary actor replay: a new generation
rematerializes the immutable bundle on a selected node and begins the route
island again under its bound limits. It is not live process migration. The
protocol does not capture or transfer process memory, threads, file
descriptors, sockets, child processes, backend interpreter state, GPU/device
state, or a mutable filesystem snapshot.

If a node restarts while a durable actor record still says `Running`, reopening
the state reports it as `Indeterminate`; the old worker is not silently resumed.
The scheduler may replay only under the delivery and island-wide replay rules
above. A durable absent fence prevents a delayed request for the same
node/actor/generation from starting, but it does not make arbitrary effects
exactly once across different generations or nodes.

## `o-link` mesh options

All tuning options require both `--mesh` and `--run`.

| Option | Meaning |
| --- | --- |
| `--mesh[=prefer|required]` | Enable project-mesh execution. Bare `--mesh` selects `prefer`; a value requires `=`. |
| `--mesh-retries N` | Additional remote attempts after the first; `0..=64`, default `2`. |
| `--mesh-local-fallback pre-send|idempotent|never` | Select the local fallback rule; default `pre-send`. |
| `--mesh-discovery-timeout-ms N` | LAN discovery window; `1..=60000`, default `750`. |
| `--mesh-no-lan-discovery` | Use only the selected paired-peer registry and disable live UDP discovery. |
| `--mesh-peer-root PATH` | Override the local paired-peer registry root. |
| `--mesh-trace-out PATH` | Write the unsigned discovery/placement/retry/fallback trace as JSON. |
| `--explain-mesh` | Explain candidates and decisions on standard error. |

Mesh execution is project-only. It conflicts with literal execution and
`--project-trace-out`; the literal `--parallel` lane is not a project-mesh
switch. Route policies remain the project policies selected by
`--routes-policy`.

## Explicit nonclaims

Project mesh V1 provides authenticated, content-addressed remote project
execution with bounded concurrency, retry, and policy-controlled fallback. It
does not by itself provide:

- NAT traversal, public-Internet discovery/gossip, relays, or a global peer
  registry;
- live process/checkpoint migration or a distributed shared filesystem;
- ordinary `OIrProgram` operation-level placement or automatic decomposition
  of one route across nodes; the schedulable unit is a Project Logical HGraph
  route/prerequisite island;
- weighted CPU, memory, or GPU reservations: V1 atomically reserves one actor
  slot, while nonzero memory/GPU requirements fail closed as unsupported;
- independent proof that declared-idempotent foreign commands really have
  those effects;
- exactly-once arbitrary host effects, compensation, or transaction rollback;
- automatic installation of foreign runtimes, containers, or device drivers;
- CAS garbage collection or eviction, and per-client authorization/quotas;
- a signed World receipt, World mutation, Governor admission, or conversion of
  the unsigned mesh trace into execution authority.

The pairing and transport boundaries are detailed in
[Zero-configuration LAN nodes](ZERO_CONFIG_LAN.md). Frozen V1/V2 placement
contracts remain documented separately in [Hosted Placement
V6](HOSTED_PLACEMENT_V6.md).
