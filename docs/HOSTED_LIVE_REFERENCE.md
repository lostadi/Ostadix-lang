# Hosted Live-World Reference (Stage 1)

Status: executable hosted semantic oracle. This is deliberately **not** the
O-core native live-system milestone.

The reference closes the control-plane loop before O-core gains a public
blocking IPC ABI, a native ELF loader, and a VFS. It lets the package, service,
activation, rollback, restart, and cross-world value semantics be exercised in
ordinary local processes without pretending those processes already run inside
booted O-core.

## Implemented boundary

The hosted reference provides:

- strict `ocore.package/v1` manifests with unknown-field rejection and bounded
  metadata;
- deterministic SHA-256 identities over a canonical manifest and the complete
  regular-file payload tree;
- a local content-addressed store with verification on read, atomic object
  publication, read-only objects, and policy-controlled aliases that are never
  authority;
- default-deny validation of declared capability requests before a runtime is
  spawned or published;
- one fixed-protocol local child process per service;
- bounded health checks before an activation becomes discoverable;
- generation-bound, CSPRNG-backed service capabilities whose descriptive
  metadata cannot recreate authority;
- retained healthy rollback roots, stale-bearer denial, targeted service
  restart, and active-set reconstruction from immutable package digests; and
- sequential composition of packaged runtime worlds through structural,
  boot-persistable `OValue`s.

Package runtime entries use package-internal absolute notation. For example,
`/bin/live.toml` means `bin/live.toml` beneath the verified immutable payload
root. It never means `/bin/live.toml` on the host. The one resolver shared by
manifest validation and worker launch rejects `..`, empty components,
backslashes, NUL bytes, symlinks, and escapes from the package object.

Admission is quantitatively bounded before large reads or process launches:

- manifests and activation-policy files are at most 64 KiB;
- one payload has at most 4,096 files, 8,192 total directory entries, 64 MiB
  per file, and 256 MiB total bytes;
- one manifest has at most 32 services, 64 capability requests, and 16 rights
  per request;
- one runtime program and each protocol frame are at most 1 MiB, with at most
  256 declared operations;
- the hosted active set has at most 64 active packages, 64 rollback roots, and
  256 active services; and
- one supervisor session has at most 4,096 live bearers and 256 composition
  steps. Its default structural-value ceiling is 64 KiB.

## Authority and isolation

The private service-bearer table is authority; the serialized
`OValue::Capability` metadata is not. Lookup binds a random bearer to one
supervisor session, service, protocol, rights set, and generation. Activation,
rollback, and restart rotate generations and revoke affected bearers. Runtime
arguments must have a pure runtime boundary and be safe to persist across a
boot; capabilities, scopes, requests, systems, and other live references are
rejected as data. Live authority must cross through an explicit broker
operation instead.

The reference starts separate local host processes and limits their
control-plane grants. It does **not** yet provide processor-enforced O-core
address spaces or CSpaces for those workers, and it does not claim complete
containment of arbitrary host syscalls. Host syscall containment remains
best-effort and operating-system dependent until the same state machines run
over O-core process, IPC, and capability mechanisms.

The state directory is same-user trusted control-plane authority. Immutable
objects are verified when opened, aliases never grant authority, and the
active-set file is strict, bounded metadata containing digests rather than live
tokens. A principal that can replace the state directory itself is nevertheless
inside the hosted control-plane trust boundary; use a directory that is not
writable by another account.

On Unix, every stateful `o-live-host` command takes a process-shared advisory
lock in that authority directory before any reconstruction or mutation and
holds it through the complete command. Cooperating CLI processes therefore
cannot commit competing stale snapshots. Direct `HostedSupervisor` users have
an additional checked boundary: the persisted active set carries a monotonic
revision (legacy files enter at revision zero), and read-only reconstruction
records that revision without changing service generations. Each publishing
activation, rollback, or service restart takes an active-set-specific
process-shared lock, rereads the durable revision, and commits only the exact
observed-to-next transition. A stale instance gets an explicit revision
conflict and must reconstruct before retrying.

These are cooperative same-host transaction boundaries, not a general storage
consensus protocol. A non-cooperating writer inside the same-user trust boundary
is not contained by either advisory lock. The public-boundary integration test
constructs two supervisors at the same revision, commits through one, and
proves the stale second instance cannot overwrite it:

```bash
cargo test --test hosted_supervisor_transactions --no-default-features
```

## CLI and acceptance gate

`o-live-host` exposes the hosted lifecycle surface:

```bash
cargo run --bin o-live-host -- pack --manifest package.toml --payload payload/
cargo run --bin o-live-host -- install --state .o-live \
  --manifest package.toml --payload payload/ --alias runtime/example
cargo run --bin o-live-host -- activate --state .o-live sha256:<digest>
cargo run --bin o-live-host -- upgrade --state .o-live sha256:<digest>
cargo run --bin o-live-host -- invoke --state .o-live \
  service.example ocore.runtime-service/v1 compute value.json
cargo run --bin o-live-host -- compose --state .o-live \
  --plan composition.json --input value.json
cargo run --bin o-live-host -- rollback --state .o-live runtime/example
cargo run --bin o-live-host -- restart --state .o-live service.example
cargo run --bin o-live-host -- status --state .o-live
```

Activation is default-deny when a manifest requests authority. An operator can
provide an exact-match policy with the global `--policy policy.toml` option:

```toml
schema = "ocore.hosted-activation-policy/v1"

[[grants]]
package = "runtime/example"
kind = "endpoint"
purpose = "request channel"
rights = ["send", "receive"]
```

There are no wildcard grants. The policy must be supplied again when a later
command reconstructs packages that require it. A composition plan is a bounded
JSON array of `{service, protocol, operation}` records; its input and every
intermediate result must remain pure, boot-persistable OValue data.

The self-contained demonstration installs two fixed hosted test-runtime
packages. A source world produces the structural object `{lhs: 20, rhs: 22}`
and a second world consumes it to produce `42`. The same run denies an
over-broad capability request, keeps version 1 active after an unhealthy
version 2 is staged, rejects
stale bearers after publication and rollback, restarts one crashed service
without rotating an unrelated service, and reconstructs the active set with
fresh session authority:

```bash
./scripts/smoke-hosted-live-reference.sh
```

All success markers begin `HOSTED live reference:` so they cannot be confused
with evidence from a booted kernel.

## Native handoff

This oracle fixes semantics; it does not waive the native dependency order or
stand in for native evidence. Bounded native Milestone 3, 4, and 5 slices now
have separate QEMU gates for public endpoint IPC and lifecycle cleanup, strict
static ELF/package loading, and package activation, health-gated publication,
crash withdrawal, restart, and teardown. The remaining handoff is:

1. move the privileged Milestone 5 state machines into independently operating
   endpoint-RPC daemons and supervise personality services through that path;
2. carry native structural OValues and capsule composition across those
   service boundaries, including durable reboot reconstruction; and
3. only after those substrate properties hold, begin a Linux personality as a
   package-managed runtime rather than as a kernel-wide compatibility mode.

The hosted reference therefore supports one precise claim:

> Package-addressed runtime services can be supervised as local child
> processes, published only after health checks, addressed through revocable
> service bearers, rolled back by generation, reconstructed from immutable
> digests, and composed through structural OValues. This is an executable
> semantic oracle for the native control plane. The hosted oracle itself is not
> evidence for any native milestone; the bounded Milestone 3, 4, and 5 claims
> are established by their separate source, artifact, and QEMU gates.
