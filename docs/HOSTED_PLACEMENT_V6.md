# Hosted Placement V6 profile

Hosted Placement V6 contains two deliberately separate pieces: a transport-
independent placement proof core and a bounded, direct-node execution channel.
The proof records are independently testable even when no node is running. The
current channel lets an operator explicitly select and authenticate one node;
it does not yet turn a placement proof into dispatch authority. Neither piece
is an implementation of an OSTADIX World.

The profile has deliberately narrow authority:

- the placement core models requirements, targets, warrants, capacity, and
  one-use leases without depending on a transport;
- pinned mutual TLS authenticates one explicitly configured live node;
- a bounded prepared-operation message binds the exact source and attempt; and
- a self-digested hosted-operation receipt makes a captured outcome tamper-
  evident.

It does not grant Governor authority, World membership, WorldFS access, native
O-core authority, or an exactly-once external-effect guarantee.

## Admission versions

Admission V5 and Hosted Placement V6 are dual current contracts:

- **V5** remains the supported legacy-local execution contract and the default
  for the uppercase `O` compatibility CLI and existing MCP execution tools.
- **V6** adds descriptor-based placement records and the bounded prepared-
  operation protocol. The current direct transport is exposed by `o-node` and
  `octl`; it is not an implicit mode of uppercase `O` or the MCP tools.

A V5 record is never silently upgraded to V6, and a V6 record is never treated
as World admission. A generated executable embeds the admission version it was
built against.

The ordinary HGraph coordinator is the default hosted executor. The ordered
OIR executor remains available through `O_EXECUTOR=serial` as a differential
oracle; it is not the default execution path.

## Environments and linker concurrency

O source has three environment forms:

```text
python^(...)_python          # fresh ephemeral evaluator
python[*]^(...)_python[*]    # fresh linker-isolated evaluator
python[7]^(...)_python[7]    # explicit persistent logical environment
```

`EnvironmentRefV2` preserves that distinction. An explicit numeric index is a
logical state-affinity constraint, not a physical process identifier or a
placement target. A verified stateless inline renderer does not acquire false
actor-state affinity merely because its source was indexed. A persistent shim
does retain its state dependency and is not remotely migrated by this profile.

Literal `o-link` output uses `[*]` for each wrapped input instead of synthesizing
numeric environment indices. That preserves per-file isolation while leaving
physical evaluator identity available to the runtime. Existing authored
numeric indices remain numeric and `o-unlink` preserves them.

`[*]` never means “choose an existing shared environment.” Each occurrence is
fresh. Programs that intentionally share evaluator state must continue to use
an explicit numeric environment and accept its locality/serialization
constraint.

The current V5 scheduler's `ActorResourceId` remains the canonical backend name
plus persistent numeric environment. Its process registry separately adds the
sandbox policy and admitted launch generation. V6's stronger
`ActorGenerationIdV1` can bind the backend implementation, target, logical
environment, sandbox/launch context, and process generation, but it is not yet
substituted for the V5 scheduler resource key. Linker-isolated `[*]` operations
are fresh and carry no `ActorState` dependency.

The default linker remains ordered. `o-link --parallel` is explicit autonomous
consent: eligible independent files are emitted as an autonomous batch, may
overlap, and each admitted parallel run returns an `OList` in input order.
Inlined `.O` roots and structural coordinator boundaries remain sequential and
split those runs. Hidden effects from already started operations can race and
are not rolled back. This syntax does not prove remote eligibility and does not
implicitly contact `o-node`.

Use `--parallel=verified` to admit only catalog-verified pure inline renderers,
`--parallel-required` to fail if any selected section cannot enter the chosen
parallel lane, and `--explain-parallel` to print each section's decision without
changing its semantics.

## Requirements, targets, and warrants

Placement never keys eligibility on a language or ISA display name.

`RequirementFootprintV1` is an order-independent union of the operation's:

- semantic capability atoms and value-preservation requirements;
- OS, ABI, endian, pointer-width, and backend implementation constraints;
- effect, replay, environment, and locality constraints; and
- bounded resource reservations.

The footprint is explicitly `Complete`, `ConservativeUnknown`, or
`Unsatisfiable`. Unknown information cannot join away and cannot authorize
strict remote placement. Complete joins take set union and the maximum bound
for duplicate resource kinds, so composition is associative, commutative, and
idempotent and does not depend on graph-coarsening order.

`TargetDescriptorV1` binds the full platform descriptor, semantic and raw CPU
features, backend implementations, sandbox properties, and realization
provenance. Stable target facts are separate from short-lived capacity so
capacity churn neither changes artifact identity nor turns a legal operation
into an illegal one.

A backend specification digest is semantic catalog identity, not process
identity. `BackendImplementationIdV1` additionally binds adapter bytes,
executable-set manifest, and protocol ABI. `ActorGenerationIdV1` then binds the
target, logical environment, launch/sandbox context, and process generation.
Logical persistent-environment serialization therefore remains stable while
physical actors can be retired and replaced without aliasing one another.
Artifact-cache identity binds OIR, analyzer/compiler, backend implementation,
the target's code-generation capability projection, and optimization policy.
It excludes target display names and fast-changing capacity observations.

Every required atom has an exact discharge through `PlacementWarrantV1`:

| Tier | Meaning | Strict default |
|---|---|---|
| Static footprint | Compiler-derived operation requirement | accepted |
| Runtime discovered | Fresh node probe or enforced runtime fact | accepted |
| Provider declared | Authenticated provider assertion | restriction-only |
| Historical observation | Exact prior successful realization | corroboration-only |

The placement-core trust policy can opt into provider-declared, historical, or
both positive warrant tiers. Neither tier may override a fresh discovered
negative, unknown requirements, insufficient capacity, or a World authority
boundary. The direct `octl node run` path does not yet consume this policy or
its discharge record.

`PlacementWarrantV1` carries an issuer-key digest, while signature bytes and
key resolution remain outside the transport-independent payload behind the
`RecordAuthenticatorV1` boundary. The core therefore verifies that an
authenticator accepted each record; it does not itself define a production key
enrollment system.

Runtime-discovered warrants have a 60-second maximum lifetime, provider-
declared warrants 5 minutes, and historical warrants 24 hours. Historical
authorization additionally requires at least three successes and exact binding
to the operation OIR, target descriptor, backend implementation, realization
pipeline, and input equivalence class.

The V6 target model accepts downward-closed, general-purpose capability sets.
Accelerators whose primitive support is not downward-closed require a later
non-ideal solver and are outside this profile.

Inspect the current compiler projection for one exact plan node without
executing it:

```bash
o why program.O P3
# exact compiler spelling:
olangc program.O --target ir --why P3
```

The focused report appends its `RequirementFootprintV1`. Hosted shim effects
without an explicit enclosing `autonomous(...)` policy, coordinator-local
control, and unpackaged scope state remain `ConservativeUnknown`. The report is
descriptive inspection, not a warrant, candidate decision, lease, or dispatch.

## Discovery status

The direct node channel performs no network registry discovery. The operator
supplies the endpoint, trusted CA, expected server name, client certificate,
and client key to `octl`. A node profile is useful inspection data; it is not a
lease and does not automatically authorize dispatch.

Specifically, `o-node profile` and `octl node profile` expose the descriptive
`hosted_remote::NodeProfileV1` catalog/limit record. It is distinct from the
authenticated, short-lived `placement::NodeProfileV1` used by the proof core.
`doctor` reports point-in-time transport/runtime readiness; it is not a
`PlacementWarrantV1` or capacity reservation.

The placement core separately caps a node profile at 60 seconds, a capacity
observation at 5 seconds, and a placement lease at 30 seconds. These are maximum
record validity intervals, not promises that the direct transport consumes the
records or that a node remains healthy for their duration.

## Signed namespace registry: local and offline

Registry v1 carries `placement::NodeProfileV1` records in canonical-CBOR,
Ed25519-signed, append-only snapshots. A pinned `NamespaceRootV1` can delegate
only to a strict descendant namespace through `NamespaceDelegationV1`.
Verification checks signatures, scope, validity, sequence and previous-event
chains, monotonic profile generations, and snapshot continuity. Imports merge
only snapshots anchored by a local trust pin or a current delegation and reject
rollback, forks, equivocation, and conflicting profiles before atomically
replacing local state.

`o-registry` is the durable local-file CLI for this core. All state, key, and
trust paths are explicit. A minimal local flow after normal setup is:

```bash
REGISTRY_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/ostadix/registry"
mkdir -p "$REGISTRY_DIR"
chmod 700 "$REGISTRY_DIR"

o-registry init \
  --state "$REGISTRY_DIR/store.cbor" \
  --key "$REGISTRY_DIR/root.key" \
  --trust "$REGISTRY_DIR/trust.cbor" \
  --namespace local

o-registry profile-local \
  --key "$REGISTRY_DIR/root.key" \
  --output "$REGISTRY_DIR/node.json" \
  --node-id local-node \
  --backend html

o-registry publish-profile \
  --state "$REGISTRY_DIR/store.cbor" \
  --key "$REGISTRY_DIR/root.key" \
  --trust "$REGISTRY_DIR/trust.cbor" \
  --namespace local \
  --profile "$REGISTRY_DIR/node.json"

o-registry verify \
  --state "$REGISTRY_DIR/store.cbor" \
  --trust "$REGISTRY_DIR/trust.cbor"
o-registry list \
  --state "$REGISTRY_DIR/store.cbor" \
  --trust "$REGISTRY_DIR/trust.cbor"
```

`profile-local` defaults to a 45-second profile lifetime, up to the placement
core's 60-second maximum. It binds the registry signing-key identity and
fingerprints the exact adapter and installed runtime artifacts for each
selected `--backend`. Repeat `--backend`, `--capability`, or `--cpu-feature` as
needed; shim backends also require `--shim-dir`. Generate and publish the
short-lived profile together.

The generated placement profile is intentionally different from the
self-reported descriptive profile returned by `o-node profile`. It is not a
health probe, capacity observation, placement warrant, lease, or dispatch
authorization. The registry signer key must retain mode `0600` on Unix.

`o-registry export` verifies and writes a canonical portable store;
`o-registry import` verifies and atomically merges one. The core library
supports namespace-delegation records; the current CLI does not mint
delegations or edit trust roots, and it never serves snapshots over a network.
The lowercase wrapper exposes the same operations as `o registry ...`.
Verification rejects expired profiles by default. `--allow-stale-profiles` is
an explicit inspection/import policy that preserves the stale marker; it does
not turn an expired profile into current health or placement authority.

## Direct transport and prepared-operation boundary

`o-node` uses synchronous TCP with TLS 1.3-only mutual X.509 authentication. It
requires a pinned trust root, expected server name, client certificate and key,
and an Ostadix ALPN value. There is no plaintext fallback, opportunistic trust,
or 0-RTT path.

The node binds `127.0.0.1:7337` by default, the client connects to that address
and validates the certificate name `localhost`, and the node permits at most 32
simultaneous connections. Use `o-node serve --bind`, `octl node ... --address`,
and `--server-name` for an explicitly configured non-loopback endpoint.

Control messages are canonical CBOR with a 2 MiB frame maximum. Operation
source is capped at 1 MiB and returned result data at 768 KiB. Connect and TLS
handshake timeout after 10 seconds by default; ordinary I/O after 60 seconds.

`RemotePreparedOperationV1` binds exact source bytes through SHA-256, task and
attempt identities, the complete descriptive backend-catalog digest, a
deadline, and an output ceiling. The node checks those bounds and creates a
fresh evaluator for every request. This is not a generic shell-command RPC,
does not preserve a remote evaluator between operations, and does not accept a
project bundle.

The deadline is checked before evaluation and again before result publication.
A late success is suppressed and returned as a deadline failure, but the
current evaluator cannot safely cancel effects that were already running.

The placement core's immutable semantic records use their own domain-separated,
deterministic JSON digest projection. The direct protocol uses canonical CBOR;
neither deserialization alone grants authority.

`HostedOperationReceiptV1` binds the request and outcome with a canonical-CBOR
SHA-256 self-digest. The live TLS session authenticates the node while the
result is received. The receipt has no detached node signature, so after
capture it is tamper-evident but not independently attributable or offline-
verifiable.

The direct run path does not validate `RequirementFootprintV1`,
`TargetDescriptorV1`, warrant discharge, capacity admission, or
`PlacementLeaseV1`. Those remain a tested placement-proof core awaiting an
authority-preserving adapter into transport dispatch.

## Failure behavior

A direct run is one synchronous request to one operator-selected node. A
connection, authentication, timeout, protocol, or execution failure is returned
to the caller. There is no durable attempt ledger, reconnect status query,
automatic retry, alternate-node selection, or local fallback. Task and attempt
identities bind the request; they do not make it idempotent or exactly once.

## User-facing commands

The implemented direct-node surface is intentionally small:

```text
o-node pki init ...
o-node profile ...
o-node doctor ...
o-node serve ...

octl node profile ...
octl node doctor ...
octl node run ...
```

### Development PKI quickstart

Provision a development-only CA and the node/client identities used by the
default loopback configuration:

```bash
o-node pki init
```

The helper invokes OpenSSL without a shell, refuses to overwrite any existing
PKI file, installs private keys with mode `0600` on Unix, and proves the result
with a real loopback TLS 1.3 mutual-authentication handshake before returning.
It writes to `${XDG_CONFIG_HOME:-$HOME/.config}/ostadix/hosted` by default.
Use `--directory` to select another location and `--server-name` to generate a
certificate whose DNS or IP SAN matches a non-default connection name.

This command is a development-PKI convenience, not enrollment in a production
trust domain. It leaves `ca-key.pem` beside the generated runtime files so the
initial set can be inspected. Move that CA private key to protected offline
storage after issuance; the node and client need only `ca.pem` and their own
certificate/private-key pair at runtime.

With the default file locations and loopback endpoint:

```bash
o-node doctor --shim-dir backends
o-node serve --shim-dir backends

# In another terminal:
octl node doctor
octl node run examples/hello.O
```

The lowercase wrapper exposes the same paths as `o node-host ...` and
`o node ...`. Local execution remains an explicit operator choice through
`o run examples/hello.O`; a failed remote operation is never retried locally
without a new command.

Use each command's `--help` for certificate, endpoint, timeout, limit, and
output options. `profile` and `doctor` inspect; only `octl node run` executes an
operation. The uppercase `O`, existing MCP tools, and `o-link` do not contact a
node implicitly.

## Explicit non-claims

Hosted Placement V6 does **not** establish:

- OSTADIX World membership, a replicated Governor, Governor admission or
  commit, WorldFS, or passage of G1 or any G0--G13 gate;
- automatic network discovery, a network registry service, scheduler-selected
  placement, transport enforcement of the proof core, capacity reservation,
  durable attempts, retry, alternate-node or local fallback;
- a detached node signature or independently verifiable attribution for a
  captured hosted-operation receipt;
- physical-machine, Secure Boot, measured-boot, device-assignment, DMA/IOMMU,
  or O-core hardware-isolation evidence;
- coherent cross-node memory, transparent pointers, persistent-actor
  migration, mid-operation migration, safepoints, rematerialization, or
  holonomy-flat target-neutral semantics;
- arbitrary HGraph-island, project, accelerator, foreign-kernel, or operating-
  system placement; or
- exactly-once external effects, distributed transactions, or transparent
  recovery from every accepted attempt.

The current 26-gate portable QEMU component manifest remains separate evidence
for its exact O-core and bounded World-codec scenarios. An older sealed Alpha
constitution comment that names 24 gates is historical sealed evidence; it is
not the current component gate count and is not rewritten by this profile.
