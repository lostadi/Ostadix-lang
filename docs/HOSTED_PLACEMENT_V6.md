# Hosted Placement V6 profile

Hosted Placement V6 contains a transport-independent placement proof core and
two explicitly versioned direct-node transports. The proof records are
independently testable when no node is running. Frozen transport V1 executes one
fresh source document without consuming the placement proof. Durable transport
V2 carries the complete proof under a one-use signed authority envelope,
reconstructs the exact source locally as a sealed single-shim fragment, and
records session mutations in a node-signed hash-chain journal. Both transports
require an operator-selected node. None of these pieces implements an OSTADIX
World.

The profile has deliberately narrow authority:

- the placement core models requirements, targets, warrants, capacity, and
  one-use leases without depending on a transport;
- pinned mutual TLS authenticates one explicitly configured live node and its
  client certificate;
- V1 binds one fresh source/attempt and returns a self-digested outcome;
- V2 separates the TLS principal, high-entropy session bearer, logical state
  session, and physical actor generation;
- V2 recomputes placement eligibility against the current catalog and the
  exact locally prepared fragment before dispatch; and
- hard state quotas refuse pressure without silently evicting live actors.

It does not grant Governor authority, World membership, WorldFS access, native
O-core authority, scheduler-selected placement, global effect isolation,
cancelled-effect rollback, or an exactly-once external-effect guarantee.

## Admission coordinates and the Hosted Placement V6 milestone

Three deliberately separate names occur here:

- **Current local Evidence/Admission V6** means `oexec.evidence/v6` and
  `oexec.admission/v6`. Package 0.3 uses it through Graph V2 for uppercase `O`,
  the evaluator/coordinator, CLI, and MCP execution. It preserves typed
  `FidelityAssessmentV2`, exposes Schedule Explanation/Why V2, and freshly
  prepares `PreparedPlacementFragmentV2` for the hosted path.
- **Archival local coordinates** are Graph V1, Evidence/Admission V5, Schedule
  Explanation/Why V1, and `PreparedPlacementFragmentV1`. They remain explicit
  inspection and compatibility-verification surfaces only and are never
  uplifted, relabeled, authorized, or executed as package-0.3 V2/V6 authority.
  Execution Intent V1 remains bound to the frozen Graph V1 identity, but a
  matching intent still requires fresh Graph V2/V6 analysis and admission
  before dispatch.
- **Hosted Placement V6** is this document's placement milestone. It adds
  descriptor-based placement records plus frozen direct transport V1 and
  opt-in durable transport V2, exposed by `o-node` and `octl`. Its “V6” is not
  the `oexec.admission/v6` schema coordinate.

None is silently upgraded or translated into another, and none is World
admission. A generated executable embeds its runtime sources, including the
current Graph V2/Evidence and Admission V6 execution path and its V2 placement
fragment boundary.

## Backend catalog V5 hard rollover

Execution-admission coordinates, the Hosted Placement V6 milestone, and
backend-catalog V3/V4/V5 describe different things. An execution-admission
schema selects a local evidence contract; the placement milestone selects its
own records and transports. The catalog schema selects the hash domain for
backend metadata. The current authorizing catalog schema is
`ostadix.backend-catalog/v5`. Archival V4 added the backend-state support tier
and snapshot-compatibility identity. V5 preserves that exact hash prefix and
appends one self-identifying optional BackendMorphism V1 profile:
Python, JavaScript, and Rust have named profiles and the other 27 canonical
backends explicitly have none.

The profile label is catalog data, not direct crossing authority. The Catalog
V5 rollover itself is not projected into `BackendInterface`, does not rewrite
production `BackendCrossing` fidelity, does not change solved-graph hashing or
evidence/admission schemas, and does not alter dispatch. Separately, the
explicit local Graph V2/Evidence V6 API binds the complete solver assessment;
it does not enforce the catalog morphism profile. Both V5 and V6 evidence paths
bind the existing current Catalog V5 projection.

The schema string participates in the digest of the complete ordered catalog
and in every canonical backend-specification digest. Moving from V4 to V5 thus
changes those identities even when a backend keeps the same display name. A
name or alias is never substituted for that digest.

The rollover is enforced without rewriting the signed placement-record
schemas:

1. `TargetDescriptorV1::validate_current_backend_catalog()` checks every
   `BackendImplementationIdV1.backend_specification` against the specification
   identities minted by the process's current `BackendRegistry`.
2. `NodeProfileV1::validate_at()` performs that check before freshness and
   detached-authentication checks. Candidate evaluation validates the profile
   before it attempts requirement or warrant discharge.
3. A V4, V3, or otherwise unknown specification fails with
   `PlacementValidationError::NonCurrentBackendCatalog`.

The exact diagnostic is:

```text
backend specification `<digest>` is not authorized by current catalog `ostadix.backend-catalog/v5`
```

Consequently, a self-consistent set of old profile, footprint, implementation,
warrant, and signature records cannot authorize V5 placement. Old warrants
also cannot discharge requirements freshly derived from V5 identities. The
runtime never edits, relabels, or silently uplifts an archival digest. A descriptor
with no backend implementations may remain structurally valid, but it cannot
satisfy a `BackendSpecification` or `BackendImplementation` requirement.

V4 and V3 records remain archive material. Their original bytes can be decoded and
their detached signatures can be checked against their original digests and
keys. Registry signature, namespace, and history verification establishes the
integrity of that archive; it is not current profile validation and is not
placement authorization. Keep the original records if they are needed for an
audit instead of modifying their signed contents.

Frozen direct-node V1 separately binds the exact whole-catalog digest in
`RemotePreparedOperationV1`, so peers built from different catalog generations
reject one another. That remains a protocol-compatibility check, not placement
authorization. Durable V2 carries the whole placement evidence bundle and
requires the operation's catalog digest to equal the current catalog before
local preparation and authorization.

### Executable-set and local-realization V2 rollover

Catalog V5 identity is necessary but not sufficient for a current backend
implementation. The implementation also uses:

- `ostadix/backend-executable-set/v2`, a path-independent digest over the
  selected catalog alternative, selection kind, logical command, executable
  role, and immutable artifact bytes; and
- `ostadix.local-realization/v2`, whose digest material binds the current
  backend specification, adapter kind and artifact, executable-set V2 identity,
  and `o-backend-cbor-v1` protocol ABI.

Physical invocation paths, file identities, and retained file handles stay in
process-local `AdmittedExecution` authority and are rechecked immediately
before launch. Moving the same immutable executable set to different paths does
not change the semantic V2 identity; changing bytes or launch coordinates does.
The runtime and `o-registry profile-local` use the same builder, so publication
and evaluator preflight cannot silently apply different realization formulas.

Local-realization V1 remains an archival digest domain. Even when paired with a
current V5 backend specification, it fails current target validation with
`NonCurrentBackendImplementation`. The runtime never relabels V1 material as
V2. Regenerate backend implementations, profiles, warrants, discharges, and
leases whenever either the catalog or realization formula changes.

### Regenerating current identities

After a catalog rollover, rebuild components that compile or embed the catalog:

```bash
./setup.sh --minimal --yes
```

Then restart long-running `O`, `o-node`, and MCP processes so they do not retain
an older compiled snapshot. Confirm that a rebuilt MCP `o_runtimes` report says
`runtime-catalog-schema=ostadix.backend-catalog/v5`.

For placement, generate and publish a new short-lived profile with
`o-registry profile-local` and `o-registry publish-profile`. Recompute the
operation footprint and every backend implementation, warrant/discharge,
capacity observation, reservation, and lease derived from catalog identity.
Do not copy old digest fields into new records. Re-run `olangc` for generated
or AOT executables intended to carry the current embedded runtime/catalog, for
example:

```bash
olangc program.O -o program --shim-dir backends
```

An installed V4 or V3 MCP snapshot reports its own compiled catalog, and a generated
executable retains its embedded build generation. Neither is a way to authorize
V5 placement.

The behavioral rollover seam is
`tests/placement_v6.rs::noncurrent_catalog_profiles_remain_inspectable_but_cannot_authorize`:
it round-trips a real archival V4 descriptor, then proves that current profile
validation returns `NonCurrentBackendCatalog`. The catalog hash-domain check is
covered by
`crates/ostadix-api/src/backend_catalog.rs::catalog_digests_are_stable_canonical_projections`, and
the MCP projection pins current V5 plus archival V4 and V3 in
`mcp/ostadix_lang_mcp_server/src/main.rs::runtime_inventory_is_a_complete_catalog_projection`.
The companion regression
`tests/placement_v6.rs::current_catalog_rejects_legacy_realization_with_a_current_specification`
proves the independent realization rollover. These tests establish the bounded
identity rules; frozen V1 still does not consume proof, while durable V2 does so
only through the pinned-authority adapter described below.

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
constraint. Sequential `LANG[*]` syntax and fresh-per-occurrence semantics are
supported by the Rust runtime, the Python reference, and the C17 edition.

The current scheduler's `ActorResourceId` remains the canonical backend name
plus persistent numeric environment. Its process registry separately adds the
sandbox policy and admitted launch generation. Durable hosted V2 does not
reinterpret that smaller key as remote identity: it uses
`ActorGenerationIdV1`, which binds the logical environment, exact backend
implementation, target descriptor, sandbox policy, launch context, and
physical generation. `StateSessionIdV2` separately names the logical durable
session. Linker-isolated `[*]` operations remain fresh and carry no persistent
`ActorState` dependency.

The default linker remains ordered. `o-link --parallel` is explicit autonomous
consent: eligible independent files are emitted as an autonomous batch, may
overlap, and each admitted parallel run returns an `OList` in input order.
Inlined `.O` roots and structural coordinator boundaries remain sequential and
split those runs. Hidden effects from already started operations can race and
are not rolled back. This syntax does not prove remote eligibility and does not
implicitly contact `o-node`.

The linker's existing import/include scan is a scheduling constraint, not just
an output sort: detected dependencies form topological barrier waves, and only
an antichain within one wave may share a batch. Dependency cycles retain stable
source order and are serialized conservatively.

The emitted `autonomous(batch(...))` call expression is currently executable
only by the authoritative Rust edition. C17 supports `LANG[*]` but schedules
operations serially; the Python reference supports sequential `LANG[*]` but
lacks the call-expression grammar used by that wrapper. Cross-edition users of
generated literal wrappers must therefore keep the ordered linker default.

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
the path-independent executable-set V2 projection, protocol ABI, and local-
realization V2 pipeline. `ActorGenerationIdV1` then binds the target, logical
environment, launch/sandbox context, and process generation.
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
boundary. Frozen `octl node run` V1 does not consume this policy or its
discharge record. V2 places the full policy, warrants, and discharge inside the
signed envelope and re-evaluates them at the node; it does not accept a lease
digest as a substitute for the underlying proof records.

`PlacementWarrantV1` carries an issuer-key digest, while signature bytes and
key resolution remain outside the transport-independent payload behind the
`RecordAuthenticatorV1` boundary. The core therefore verifies that an
authenticator accepted each record; it does not itself define a production key
enrollment system. The current V2 adapter pins exactly one Ed25519 placement-
authority public key and requires the envelope authority, profile, capacity
observation, warrants, and open-session state-capacity observation to name that
issuer. This is a bounded single-issuer policy, not a multi-key trust chain,
rotation, revocation, or production enrollment service.

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
`doctor` reports point-in-time transport and native-image preflight readiness;
it is not a backend-protocol probe, `PlacementWarrantV1`, or capacity
reservation. The first admitted hosted-backend launch exercises the selected
image's O protocol.

The placement core separately caps a node profile at 60 seconds, a capacity
observation at 5 seconds, and a placement lease at 30 seconds. These are maximum
record validity intervals, not promises that a node remains healthy for their
duration. V1 does not consume these records. V2 authenticates and revalidates
the records it carries at the exact command time; an operator-provided endpoint
still determines which node receives the request.

## Signed namespace registry: local and offline

Registry v1 carries `placement::NodeProfileV1` records in canonical-CBOR,
Ed25519-signed, append-only snapshots. A pinned `NamespaceRootV1` can delegate
only to a strict descendant namespace through `NamespaceDelegationV1`.
Verification checks signatures, scope, validity, sequence and previous-event
chains, monotonic profile generations, and snapshot continuity. Future-dated
events are rejected, and every profile validity interval must fit completely
inside one signer-authority interval. Imports merge only snapshots anchored by
a local trust pin or a current delegation and reject rollback, forks,
equivocation, and conflicting profiles before atomically replacing local state.

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
core's 60-second maximum; the CLI rejects values outside `1..=60` before it
touches registry state. It binds the registry signing-key identity and
fingerprints the exact adapter and installed runtime artifacts for each
selected `--backend`. Repeat `--backend`, `--capability`, or `--cpu-feature` as
needed; shim backends also require `--shim-dir`. Generate and publish the
short-lived profile together. Normal setup installs `ostadix-evaluator` as a
native, byte-identical evaluator alias so `profile-local` never mistakes the
case-insensitive `O`/`o` shell dispatcher for evaluator bytes. A script passed
through `--runtime-binary` is rejected rather than executed or fingerprinted.

The generated placement profile is intentionally different from the
self-reported descriptive profile returned by `o-node profile`. It is not a
health probe, capacity observation, placement warrant, lease, or dispatch
authorization. The registry signer key must retain mode `0600` on Unix.

`o-registry export` verifies and writes a canonical portable store;
`o-registry import` verifies and atomically merges one. The core library
supports namespace-delegation records; the current CLI does not mint
delegations or edit trust roots, and it never serves snapshots over a network.
Publish and import transactions hold a persistent sibling advisory lock across
read, verification, append/merge, atomic replacement, and directory sync, so
cooperating CLI processes cannot silently lose one another's appends. Do not
delete that `.lock` file while writers are running; advisory locking does not
constrain programs that bypass the registry transaction API.
The lowercase wrapper exposes the same operations as `o registry ...`.
Verification rejects expired profiles by default. `--allow-stale-profiles` is
an explicit inspection/import policy that preserves the stale marker; it does
not turn an expired profile into current health or placement authority.

## Direct transports and prepared-operation boundaries

`o-node` uses synchronous TCP with TLS 1.3-only mutual X.509 authentication. It
requires a pinned trust root, expected server name, client certificate and key,
and a version-specific Ostadix ALPN value. There is no plaintext fallback,
opportunistic trust, 0-RTT path, or downgrade after ALPN selection. A dual node
offers frozen `ostadix-hosted/1` and durable `ostadix-hosted/2` on the same port,
but selects the decoder before reading application bytes.

The node binds `127.0.0.1:7337` by default, the client connects to that address
and validates the certificate name `localhost`, and the node permits at most 32
simultaneous connections. Use `o-node serve --bind`, `octl node ... --address`,
and `--server-name` for an explicitly configured non-loopback endpoint. Endpoint
selection remains an operator action; neither protocol performs discovery.

`o-node doctor` and `serve` also require a supported native evaluator image.
Normal setup installs the byte-identical `ostadix-evaluator` alias beside
`o-node`; development builds may resolve a sibling native `O`. Use
`--runtime-binary` to select another native image. Preflight checks its file
type, executable permission, and native image magic, but does not claim ABI or
`--o-backend` compatibility until an admitted hosted block launches. Script
dispatchers are rejected. Each operation's admission opens, hashes, and retains
the selected evaluator and its exact backend executable set rather than binding
the embedding `o-node` executable.

Control messages are canonical CBOR with a 2 MiB frame maximum. Operation
source is capped at 1 MiB and returned result data at 768 KiB. Connect and TLS
handshake timeout after 10 seconds by default; ordinary I/O after 60 seconds.
The placement core's immutable records use their domain-separated deterministic
JSON digest projection. A decodable CBOR or JSON record is not authority.

### Frozen direct transport V1

`octl node run` sends `RemotePreparedOperationV1`, which binds exact source
bytes through SHA-256, textual task and attempt identities, the complete
descriptive backend-catalog digest, an absolute deadline, and an output ceiling.
The node creates a fresh evaluator for every request. This is not a generic
shell-command RPC, does not retain an evaluator, and does not accept a project
bundle.

V1 checks the deadline before evaluation and before result publication. A late
success is suppressed, but effects already performed by a backend cannot be
cancelled or rolled back. `HostedOperationReceiptV1` binds request and outcome
with a canonical-CBOR SHA-256 self-digest. TLS authenticates the node only while
the response is received; because the receipt has no detached node signature,
a captured V1 receipt is tamper-evident but not independently attributable or
offline-verifiable.

V1 does not validate `RequirementFootprintV1`, `TargetDescriptorV1`, warrant
discharge, capacity admission, `PlacementLeaseV1`, `PlacementLeaseV2`, or a
verified registry profile. The catalog digest is compatibility, not placement
authority.

### Durable direct transport V2

V2 is an explicit session protocol, not an automatic upgrade of V1. The node
must be started with a durable state root, its Ed25519 receipt-signing key, an
exact node generation, the generation and values of all five state quotas, and
one pinned Ed25519 placement-authority public key. Without an authorizer the
safe library default denies every open, execute, and recovery command.

Every authority envelope contains the canonical authority record, exact hosted
command binding, complete `HostedPlacementEvidenceV2`, and, for open, the
`StateCapacityObservationV2`. Open and recover use
`StateControlLeaseV2`; execute uses one-use `PlacementLeaseV2`. The signature
covers all of those records. The node verifies the pinned signer and same-issuer
proof records, freshness, target/node generations, current catalog and
realization, capacity, warrant discharge, trust policy, reservations, command
purpose, session, client sequence, operation or recovery digest, state quota
generation, and actor generation. It recomputes `CandidateDecisionV1`; an
echoed lease digest is not sufficient.

The placement lease binds:

- node, target/profile/capacity generations and candidate eligibility;
- operation OIR, complete footprint, warrant discharge, portable placement
  admission, and typed task attempt;
- backend implementation, realization pipeline, trust policy, and compute
  reservation; and
- exact hosted command plus open/existing state and actor-generation binding.

Before execute authorization, the node parses, lowers, solves, and admits the
submitted bytes into a non-cloneable `PreparedPlacementFragmentV2`. It accepts
one non-whitespace semantic root whose only executable node is exactly one shim
`Exec`; structural text may be a child. A second `Exec`, non-shim execution, `Load`, `Store`, `Call`,
`Request`, `Group`, `Schedule`, text-only source, or a nonempty coordinator
scope is rejected. Obvious `O.eval` is rejected during preparation, and a
dynamically hidden callback is settled as a typed semantic refusal without
granting recursive evaluator authority. Persistent actor state is retained when
that refusal settles cleanly; ambiguous infrastructure failure is not relabeled
as a semantic refusal.

The fragment handle is process-local, non-serializable, non-cloneable, and
fenced to the `Evaluator` instance that prepared it. After authorization, the
runtime consumes that same handle and rechecks retained executable authority
immediately before dispatch; it does not parse, solve, discover, or admit a
second program. Its portable placement-admission digest excludes ambient world,
PID, paths, and other process-local runtime facts so the authority and node can
derive the same semantic coordinate. The complete V6 admission retains those
local freshness facts and is still checked at dispatch.
`PreparedPlacementFragmentV1` remains an archival inspection type and cannot be
converted, authorized, or executed by the package-0.3 path. The session's
target, exact requirement footprint, backend,
realization, logical environment, trust policy, and compute reservation are
fixed at open and cannot drift on a later execute. Open has no physical actor
generation. A stateful first execute carries `None` for the not-yet-established
physical actor. After exact local preparation, the node derives and signs
`ActorGenerationIdV1`, including the exact sandbox and launch context; later
execute commands must carry that exact identity. Source, task attempt, OIR,
portable placement admission, and operation deadline are bound again by each
execute lease, so multiple separately authorized commands may use one persistent
environment only while those fixed coordinates continue to match.

Persistent opaque shim effects require the complete footprint to include
`execution/session-serialized-opaque-effects@1` and the selected target to
advertise it. This means the exact session serializes its command. It does not
mean the source is pure, replayable, deterministic, compensatable, or isolated
from effects in other sessions.

### V2 session authority and durable journal

At open, V2 binds the SHA-256 fingerprint of the authenticated TLS client leaf
certificate and a separate random 256-bit bearer. `octl` generates the bearer,
creates and fsyncs its mode-0600 capability file, and binds its domain-separated
commitment into the signed Open command before the first network write. Later
requests must present the same TLS principal and bearer. The node omits the
bearer from durable state and stores only its commitment, a random salt, and a
salted bearer hash.

The first signed `SessionOpened` record also binds the canonical digest of the
complete Open request, including its authority signature. If the connection or
receipt validation fails after durable commit, the caller retains the capability
and may resend the byte-identical request. The node returns the original signed
receipt even after restart or proof expiry; a changed principal, request,
signature, lease, tier, or bearer produces `open-retry-conflict`. A pre-send
failure never deletes a caller-owned capability because it may be retry material
for an earlier ambiguous attempt.

Every accepted session mutation is appended to a canonical-CBOR, Ed25519-
signed, previous-hash-chained journal before it is acknowledged. The client
pins the node receipt public key and verifies every returned journal receipt.
The separate signed placement-authority journal records refused lease nonces
and explicit closed-session GC. On restart, the node verifies both journals,
reconstructs consumed accepted/refused nonces and exact client commits, and
rejects nonce replay. An exact duplicate client sequence/request-id/request-
digest triple returns the prior commit receipt; a conflicting reuse fails.
GC first signs an authorization binding the closed journal's terminal head,
raw digest, and byte length, then atomically relocates that already-durable
journal into a permanent tombstone archive before deleting operation and
checkpoint payloads. The archive therefore preserves the retired session
identity and every consumed nonce without a quota-expanding copy. Retained
journal bytes are never reported as reclaimed, and a fixed 16 KiB authority-
control reserve keeps authorization and completion reachable at the hard total
state boundary. Missing, replaced, or corrupt archives fail startup closed.

Cooperating server and offline-admin processes retain an exclusive advisory
lock on the persistent state-root lock file for their complete lifetime. Every
root containing durable state must also carry the exact package-0.3
`.execution-authority-v1` marker for Graph V2, Evidence/Admission V6, and
placement-admission V2. A root with durable sessions or an authority journal
but no exact marker, or with a different authority coordinate, is rejected
without mutation; archival journals are never uplifted, resumed, relabeled, or
executed. A root without existing durable sessions or an authority journal may
be initialized with the marker. This is process coordination under the host
account, not protection from a hostile same-UID process that can mutate the
state directory. On Unix, state directories are mode 0700 and files mode 0600;
trusted inputs reject symlinks and non-regular files, and file plus
parent-directory updates are synchronized. A new session and its first journal
frame are published by one staged-directory rename.
Immutable operation and checkpoint blobs are written and fsynced in a private
same-filesystem staging directory, then installed through atomic no-clobber
publication; an exact existing canonical blob is idempotent and conflicting or
partial content fails closed. Durable operation source, outcomes, journals, and
checkpoints are protected by filesystem ownership but are deliberately **not
encrypted at rest**.

Each journal is fully verified once when the exclusive store opens. Appends then
advance a cached sequence/head/byte coordinate in constant work and reject any
out-of-band length change. Startup repairs only an incomplete final frame;
complete invalid CBOR, signatures, or hash links remain corruption. The repair
is followed by a signed `JournalTailRepaired` authority event. A second crash in
the narrow interval after truncation is fsynced but before that event is appended
can leave the valid retained prefix without the audit event; V2 does not claim a
transactional repair-intent log for that interval.

`session status` and `session actors` verify and correlate a node-signed receipt
for the exact current journal head. The projected view fields are authenticated
by the live mTLS channel but are not individually signed by that receipt; offline
consumers should verify the underlying journal events rather than treating the
convenience projection as a detached attestation.

Submission is asynchronous: acceptance and terminal outcome are separate
journal events, and `session status` collects the current durable view. On
restart, accepted-but-not-started work becomes `NotStarted`; work that started
without a terminal event becomes `Ambiguous` and puts the session into
`RecoveryRequired`. This is durable evidence and reconnect status, not exactly-
once execution or exactly-once external-effect publication.

### V2 state tiers and hard quotas

`SessionStateTierV2` has four wire labels, but the current release authorizes
only these exact catalog mappings:

| Session tier | Required source environment | Required catalog support | Restart behavior |
|---|---|---|---|
| `Stateless` | fresh ephemeral or `[*]` | `BackendStateSupportV2::Stateless` | new evaluator; no retained state |
| `CheckpointRestore` | explicit persistent environment | `SemanticSnapshot` with exact codec and compatibility | validate the last durable snapshot, fence the lost generation, then require signed acknowledged recovery before more user execution |
| `LiveActorOnly` | explicit persistent environment | `ExternalPinned` | state is node/process-bound; loss requires explicit handling |
| `ReplayReconstructible` | n/a | none currently | rejected before open |

The catalog currently declares semantic snapshots for Python and SQL and an
external-pinned manifest for `ubuntu_vm`; other current entries are stateless.
A checkpoint refusal or checkpoint/restore incompatibility does not become a
false durability claim. Startup validates compatible snapshot bytes, durably
records the old physical generation as lost, and exposes `RecoveryRequired`;
ordinary submission never performs a hidden restore or executes user code on a
replacement first. Recovery may be triggered either by an ambiguous operation
or by actor loss at an exact signed checkpoint head, and requires an exact
`RecoveryWarrantV2` plus a recover-purpose state-control lease. Before spawning
the replacement, V2 appends `RecoveryAttemptStarted`, consuming the lease nonce,
client sequence reservation, and unique next actor generation. For the current
Python and SQL semantic-snapshot codecs, it then stages the exact snapshot in a
fresh evaluator and executes a reviewed state-neutral probe to force the
RestoreV1 receipt. Only that acknowledgement permits `RecoveryCommitted` and a
return to `Ready`. A failed, expired, unsupported, or crash-interrupted
handshake closes or forgets the replacement, durably refuses the attempt, and
keeps the session in `RecoveryRequired`; its attempted generation is never
reused. Replay, live-only recovery, and future unreviewed snapshot codecs remain
fail-closed; there is no automatic replay or external-effect publication
adapter.

`StateQuotaLimitsV2` contains exactly five hard limits:

1. maximum open sessions;
2. maximum actors per session;
3. maximum snapshot bytes per actor;
4. maximum durable state bytes per session; and
5. maximum durable state bytes in the complete root.

Open carries a fresh signed capacity observation and exact reservation. The
initial V2 runtime realizes exactly one actor per session. When a reservation,
journal, checkpoint, output, session, or total-state bound would be exceeded,
the operation is refused; no existing actor is evicted to make room. Closing a
session stops its actor and releases the reservation but retains its signed
journal. Only the explicit offline `o-node admin gc-closed` command can remove
a durably closed session. It acquires the cooperating state-root lock and writes
signed `ClosedSessionGcAuthorized` and `ClosedSessionGcCompleted` anchors to the
authority journal around deletion. There is no automatic TTL or pressure GC.
Per-session terminal, generation-fence, and close headroom makes the mandatory
failure records reachable after ordinary work reaches its limit. A separate
16-KiB authority-control debit covers tail repair and the two GC anchors;
verified bytes reclaimed by a completed GC recycle that debit, while a
zero-reclaim history can eventually exhaust it and is refused rather than
evicted. If a filesystem barrier cannot be reconciled to exact durable bytes,
the store enters `store-reopen-required`: mutation and current-head views stop
until a fresh open revalidates the journals.

### Deadlines and cancellation boundary

V2 checks the absolute operation deadline before admission, before evaluator
entry, and while waiting on the prepared backend. A
deadline already expired at prepared-fragment entry produces the typed
`PreparedPlacementDeadlineExpiredV1` result and proves that no backend command
was sent. After dispatch, a timeout bounds the evaluator wait and suppresses a
late value, but it cannot safely cancel, compensate, or roll back external
effects. Output encoding, checkpoint creation, journal and directory fsync, and
terminal response publication may finish after the deadline; V2 makes no
end-to-end publication-deadline claim. A lost in-flight attempt may remain
`Ambiguous`; neither V1 nor V2 claims cancellation or exactly-once effects.

## Failure behavior

Frozen V1 is one synchronous request to one operator-selected node. A
connection, authentication, timeout, protocol, or execution failure is returned
to the caller. V1 has no durable attempt ledger or reconnect-status query.

V2 first durably accepts an operation and returns a signed commit receipt; the
client then uses `session status` to collect the terminal record. Exact duplicate
sequence, request-id, and request-digest input returns the prior commit rather
than starting a second operation. This is control-plane idempotence, not a claim
that arbitrary backend effects occur exactly once. The signed terminal record
also binds whether actor state was touched and whether it remains durable, so
restart does not infer those facts from the session tier. After restart, an
accepted operation with no start record is classified `NotStarted` and its
physical generation is retired or fenced according to prior state; a start
record with no terminal record is `Ambiguous` and blocks ordinary session
mutation until an authorized recovery path resolves the state. Current recover
support is limited to compatible checkpoint/restore sessions; reset does not
erase an ambiguous attempt.

Neither version automatically retries, selects an alternate node, or falls back
to local execution. Retryability and durable classifications are evidence for a
caller; they are not an automatic retry policy. Task, attempt, operation, request,
and sequence identities bind exact work and reject conflicts, but do not make
external effects idempotent or exactly once.

## User-facing commands

The implemented direct-node surface is explicit:

```text
o-node pki init ...
o-node identity init ...
o-node admin gc-closed ...
o-node profile ...
o-node doctor ...
o-node serve ...

octl node profile ...
octl node doctor ...
octl node run ...
octl node session principal|open|exec|status|actors|reset|recover|close ...
octl node authority init|issue ...
octl node authority dev-mint open|execute|recover ...
```

`profile`, `doctor`, and `session principal` inspect. `node run` executes the
frozen one-operation V1 path. V2 requires an explicitly enabled server, a signed
lease, and an explicit `session` command; it is never selected by a V1 request.
`authority issue` envelopes caller-supplied exact expectation, command, evidence,
and optional open-capacity records. The `authority dev-mint` commands are a
co-located development bridge that prepares one exact source fragment against
the same local shim directory and native evaluator image as the selected node;
they are not network discovery, enrollment, or a production scheduler. The
development helper can derive open, execute, and current checkpoint-recovery
authority and accepts `--submit` to send the freshly minted envelope immediately.
That integrated path avoids spending most of the four-second development
capacity-observation lifetime between separate human commands. It remains a
co-located self-attested authority, not an independent observation, discovery,
enrollment, scheduler, production key policy, or automatic recovery service.
The deterministic source-release closure includes
`crates/ostadix-api/src/hosted_remote/v2/dev.rs` together with the V2 protocol, cryptography,
authorizer, client, server, runtime, and store modules; the release validator
rejects an archive that omits the development bridge.

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

With the default file locations and loopback endpoint, this exercises frozen V1:

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
output options. The uppercase `O`, existing MCP tools, and `o-link` do not
contact a node implicitly.

On Unix, a V2-enabled `o-node serve` registers SIGINT/SIGTERM before acquiring
the durable state-root lock. The first signal stops new connection admission,
drains the explicit `HostedV2RuntimeOwner::shutdown()` barrier, joins accepted
connection workers, and releases the root before a clean return. The server
distributes only cloneable `HostedV2RuntimeHandle` request/query access; those
handles have no shutdown authority and report the typed closed state after the
owner barrier, without retaining the durable-root lock. A second
termination signal is an operator escape hatch that restores the default signal
action and terminates immediately. Neither path closes a session or performs
offline GC; a forced interruption is interpreted using the ordinary durable
restart classifications.

The historical cloneable `HostedV2Runtime` and `serve_node_dual` entry points
remain source-compatible through the 0.2 line. They are documentation-deprecated
for new embedders, without a Rust `#[deprecated]` attribute; new code should
pair `HostedV2RuntimeOwner` with `HostedV2RuntimeHandle` and use
`serve_owned_node_dual`.

## Explicit non-claims

Hosted Placement V6 does **not** establish:

- OSTADIX World membership, a replicated Governor, Governor admission or
  commit, WorldFS, or passage of G1 or any G0--G13 gate;
- automatic network discovery, a live network registry service, scheduler-
  selected placement, or production placement-authority enrollment, multi-key
  policy, rotation, revocation, or provider admission;
- proof enforcement, state-capacity reservation, a durable attempt ledger, or
  detached node attribution on frozen V1; these are bounded V2 properties and
  must not be read back into V1;
- automatic retry, alternate-node selection, local fallback, timeout
  cancellation, effect compensation or rollback, automatic actor eviction, or
  automatic TTL/pressure garbage collection;
- a global exactly-once execution/effect guarantee, distributed transaction,
  global effects-isolation protocol, or transparent recovery from every
  accepted or ambiguous attempt;
- encrypted-at-rest session state, hardware-protected node or authority keys,
  production secret distribution, or confidential-computing attestation;
- physical-machine, Secure Boot, measured-boot, device-assignment, DMA/IOMMU,
  or O-core hardware-isolation evidence;
- coherent cross-node memory, transparent pointers, persistent-actor
  migration, mid-operation migration, safepoints, rematerialization, or
  holonomy-flat target-neutral semantics;
- arbitrary HGraph-island, multi-backend project, accelerator, foreign-kernel,
  or operating-system placement; or
- a general replay/publication adapter for `ReplayReconstructible`, cross-node
  migration for `CheckpointRestore`, or recovery of lost `LiveActorOnly` state.

The current 26-gate portable QEMU component manifest remains separate evidence
for its exact O-core and bounded World-codec scenarios. An older sealed Alpha
constitution comment that names 24 gates is historical sealed evidence; it is
not the current component gate count and is not rewritten by this profile.
