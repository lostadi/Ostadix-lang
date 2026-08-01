# O-core Native Live-System Contract

Status: a bounded native Milestone 5 slice is implemented and gated by
`ocore/kernel/smoke-live-qemu.sh`. The separately gated hosted semantic oracle
in [`HOSTED_LIVE_REFERENCE.md`](HOSTED_LIVE_REFERENCE.md) remains the broader
package-lifecycle differential oracle. The native gate is host-built,
fixed-capacity, single-CPU, and kernel-mediated; it is not yet the complete
unprivileged service architecture described as the target contract below.

This document defines the native live-system layer that must exist after the
native loader and minimal VFS, and before a foreign personality is presented as
an installable O-Domain runtime. The layer keeps O-core source-extensible and
interactive instead of making it only a compatibility substrate.

## 1. Required boundary

The live system consists of unprivileged native services over existing O-core
mechanisms. The kernel continues to own scheduling, IPC, memory protection,
capability transfer, and process teardown. Package names, manifests, service
names, hashes, and REPL text are metadata. None of them grant authority.

The target architecture provides:

- a native `init` process and crash-isolated service supervisor;
- a serial O control REPL, with framebuffer support left as a later transport;
- a content-addressed, read-only package-object store;
- versioned package manifests and dependency resolution;
- service registration through capability-returning namespace lookups;
- explicit capability requests checked by activation policy;
- transactional activation, health checking, and rollback; and
- a host-assisted build path that does not claim native self-hosting.

The REPL, package daemon, builder endpoint, and service supervisor are separate
principals with separate CSpaces. Compromise of one must not imply the ambient
authority of another.

The current executable slice loads `init`, supervisor, package-daemon, and REPL
ELFs into four separate address spaces and CSpaces from one immutable OVFS
image. The REPL runs the real serial command loop. After all four principals
publish their private health tokens and activation commits, the package daemon
deliberately faults in CPL3. The mode-16 scheduler contains that generation,
preserves the three unrelated principals, and executes one fresh package-daemon
generation from the same verified image. Package-root, health-gated activation,
recovery, and control-submit state machines currently execute in the privileged
O-core runtime behind the REPL's typed control capability. This is a real
protection and authority boundary, but not yet four cooperating user-space
daemons over endpoint RPC.

## 2. Package identity and storage

A package object is immutable and identified by a digest of a canonical
manifest plus the complete payload tree. The initial digest algorithm is
SHA-256, recorded with its algorithm name so a future format can migrate.
Package aliases such as `personality/linux` resolve to immutable digests under
an explicit repository policy. An alias is not identity and a digest is not a
statement of trust.

The VFS exposes package objects read-only. Writable configuration, logs,
service state, and domain overlays live outside the package object. Garbage
collection may reclaim only objects that are unreachable from active
generations, rollback roots, pinned manifests, and in-flight build or
activation transactions.

The canonical package manifest contains at least:

```toml
schema = "ocore.package/v1"
name = "personality/linux"
version = "0.1.0"
architecture = "x86_64"
payload_sha256 = "<64 lowercase hexadecimal characters>"

[runtime]
kind = "personality"
entry = "/bin/linux-personality"
abi = "ocore.personality/linux-x86_64-v1"

[[services]]
name = "personality.linux"
protocol = "ocore.personality-rpc/v1"

[[capability_requests]]
kind = "endpoint"
rights = ["send", "receive"]
purpose = "personality syscall request channel"

[health]
protocol = "ocore.health/v1"
timeout_ms = 2000

[build]
source_sha256 = "<64 lowercase hexadecimal characters>"
builder = "ocorec-host/v1"
```

The current native slice uses a fixed-capacity numeric `PACKAGE_SCHEMA_V1`
record and exact immutable digest words; it does not parse this illustrative
TOML manifest. The hosted reference implements the richer strict manifest and
canonical identity. A future native manifest parser must continue to reject
unknown required fields and prevent optional extensions from adding authority.

## 3. Capability requests and service registration

A capability request declares the minimum object kind and rights a package
wants. Activation policy resolves each request to one of four outcomes:
grant, attenuated grant, explicit denial, or operator decision. The package
never selects a kernel slot and cannot turn a declared request into a live
handle by itself.

Services register a versioned protocol name only after the process starts and
passes its health gate. Lookup returns an attenuated capability to an endpoint,
not a process pointer or an ambient singleton. Re-registration after restart
creates new generations and makes stale endpoint handles fail closed.

## 4. Activation transaction

Activation is an explicit transaction with these durable states:

```text
resolved -> staged -> started -> healthy -> active
                      |           |
                      +-> failed <-+
                              |
                         rolled_back
```

The package daemon must:

1. resolve every alias to a digest and verify the complete dependency closure;
2. validate architecture, ABI versions, payload digests, quotas, and policy;
3. allocate fresh process, address-space, CSpace, and endpoint generations;
4. start services without publishing their names;
5. run bounded health checks;
6. atomically publish the new service generation and activation record; and
7. retain the prior healthy generation as the rollback root until policy
   releases it.

Failure before publication tears down the staged generation. Failure after
publication either rolls back the complete activation set or records a
terminal degraded state. Partial alias updates and mixed service generations
are not successful activation.

## 5. Serial O control REPL

The target REPL is a serial control service using a small, versioned O command
surface; it is not the hosted polyglot evaluator running in ring 0. In the
current slice, line assembly and command issuance happen in the unprivileged
REPL, while the bounded parser and activation transaction run in the privileged
runtime after a typed-capability check and fault-aware copy. Moving that control
state machine into a user-space daemon is a remaining boundary.

The mode-16 gate pins this first command spelling:

```text
o> install <64-lowercase-hex-sha256> 5 1
o> activate <the-same-64-lowercase-hex-sha256>
```

The 192-byte bounded parser also recognizes canonical `status`, `resolve`, and
`upgrade` forms, but the QEMU interaction gate sends only malformed input,
`install`, and `activate`. Both serial byte reads and control submission require
the exact `OBJECT_CONTROL`/`RIGHT_CONTROL` capability installed only in the
REPL CSpace. The command buffer crosses through fault-aware bounded user copy;
text and digest bytes never choose a capability slot.

## 6. Compiler bootstrap

Self-hosting is a progression, not a prerequisite for the first usable system.

### Stage 1: host-built image injection

The host runs the pinned `ocorec` toolchain, emits native ELF packages, and
constructs the initial read-only package image. The M4 artifact builder builds
two separately linked personalities and checks deterministic image repacking.
The M5 builder performs two full service builds and requires byte-identical
ELFs and images. Both verify image SHA-256 before boot. O-core also runs its
freestanding NIST-vector-tested SHA-256 implementation over the complete image
before OVFS import and publication, then validates OVFS and ELF structure. The
mode-16 artifact is pinned at 62,056 bytes with SHA-256
`388b9253ce6f92bef1e1f986b46aabbeb728604cc73589d12105031f5f6b780a`;
both the smoke harness and kernel require that exact identity. A fuller receipt
must also name the compiler revision and source digest.

### Stage 2: capability-bounded build service

An O-core package daemon submits a versioned build request to a trusted builder
endpoint. The request contains source-object capabilities or immutable source
digests, target, compiler identity, declared inputs, resource limits, and a
unique operation ID. The builder returns an immutable result digest and build
receipt. It receives no ambient domain filesystem or package-manager authority.
Retry rules distinguish a request that never started from one whose result was
already committed.

The builder may initially run on the development host. A remote builder is not
required and does not become trusted merely because it is remote.

### Stage 3: native compiler domain

A pinned O-core compiler and linker run inside a dedicated O-Domain and publish
packages through the same build protocol. Native self-hosting is accepted only
after a clean bootstrap rebuild reproduces the declared outputs or records and
explains every permitted source of nondeterminism.

## 7. Personality and rootfs packages

The Linux personality and each root filesystem are different package kinds.
One Linux personality package may serve multiple domain instances. Alpine and
Debian packages contain root filesystem content and policy, not duplicate
personality implementations.

Activating `personality/linux` registers a versioned personality service.
Installing `rootfs/alpine` makes an immutable root available. Creating
`linux[alpine]` binds the personality capability, rootfs digest, writable
overlay, namespace, quotas, and persistent-state version into one domain
transaction.

## 8. Implemented acceptance gate

`ocore/kernel/smoke-live-qemu.sh` performs a fresh mode-16 build and proves:

- the four service ELFs and their OVFS image rebuild identically, the host
  checks the exact SHA-256, and O-core recomputes it before import;
- none of the four `_start` symbols is linked into the kernel image;
- QEMU imports the read-only OVFS object, creates four distinct loaded W^X
  address spaces and isolated CSpaces, and executes all four ELFs in CPL3;
- a real serial `o> ` loop rejects malformed install text without publishing
  state, then accepts an exact-digest install and exact-digest activation;
- only the REPL's typed control capability authorizes serial read and command
  submission;
- the immutable package root publishes all four service-generation records only
  after exact capability requests are granted and each health gate succeeds;
- the package daemon deliberately faults in CPL3 after activation; only its
  process generation is contained while `init`, supervisor, and REPL survive;
- the old process, thread, CSpace, address-space, debug-capability, and service
  generations become stale, and the service stays withdrawn throughout
  `CONTROL_RECOVERING`;
- a freshly loaded package-daemon generation runs from the verified image and
  is republished only after its exact restart health token is observed;
- the control plane reaches `CONTROL_DEACTIVATED`, then control-capability
  revocation, process and namespace teardown, and complete dynamic-frame
  reclamation succeed; and
- a post-lifecycle timer fires and QEMU survives the following observation
  window.

The independent mode-17 `smoke-live-semantics-qemu.sh` boot executes the finite
package/supervisor corpus without borrowing mode-16 markers: two immutable
roots, overgrant and incomplete-set denial, failed-health nonpublication,
complete-set rollback and stale references, abstract crash/restart with
unaffected state, strict serial parsing, invariant checks, and a later timer.

The gate is deliberately narrower than the eventual live-system contract. It
does not yet prove two-package dependency resolution, a user-space endpoint
health RPC, failed-upgrade rollback through the real serial path, general or
unbounded retry/backoff, or independently operating daemon supervision. The
real native restart proof covers exactly one package-daemon generation. A
replacement that faults or omits its exact health token remains withdrawn and
fails closed; a further recovery attempt is not claimed. The gates also do not
prove reboot reconstruction or native compiler receipts, and do not claim a
Linux ABI, foreign rootfs, native compiler/self-hosting, dynamic linker,
framebuffer, arbitrary hosted O backend execution inside O-core, SMP, or
unbounded capacity.
