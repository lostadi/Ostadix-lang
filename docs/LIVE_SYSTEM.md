# O-core Native Live-System Contract

Status: design contract for a planned milestone. Nothing in this document is
implemented merely because a manifest field, command, or package name is
specified here.

This document defines the native live-system layer that must exist after the
native loader and minimal VFS, and before a foreign personality is presented as
an installable O-Domain runtime. The layer keeps O-core source-extensible and
interactive instead of making it only a compatibility substrate.

## 1. Required boundary

The live system consists of unprivileged native services over existing O-core
mechanisms. The kernel continues to own scheduling, IPC, memory protection,
capability transfer, and process teardown. Package names, manifests, service
names, hashes, and REPL text are metadata. None of them grant authority.

The first implementation must provide:

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

The concrete parser and canonical encoding are Milestone 5 deliverables. The
example fixes the semantic fields, not a claim that the parser exists today.
Unknown required fields fail closed. Optional extension fields are namespaced
and cannot add authority.

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

The first REPL is a serial control service using a small, versioned O command
surface. It is not the hosted polyglot evaluator running in ring 0. Parsing and
evaluation happen in an unprivileged process, and every operation is limited by
that process's CSpace.

Illustrative commands are:

```text
o> pkg.install("personality/linux")
o> pkg.install("rootfs/alpine")
o> world.create("alpine", personality="linux", rootfs="alpine")
o> service.status("personality.linux")
o> pkg.rollback("personality/linux")
```

These spellings become public API only when parser, policy, and runtime tests
pin them. REPL history must not serialize live handles. Inspection may render
opaque handle metadata, but replay must reacquire authority through policy.

## 6. Compiler bootstrap

Self-hosting is a progression, not a prerequisite for the first usable system.

### Stage 1: host-built image injection

The host runs the pinned `ocorec` toolchain, emits native ELF packages, hashes
the source and outputs, and constructs the initial read-only package image.
O-core verifies the manifest and payload digest before loading. Evidence names
the compiler revision, source digest, target, and produced package digest.

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

## 8. Acceptance gate

Milestone 5 is complete only when a fresh QEMU boot can:

- load `init`, the supervisor, package daemon, and serial REPL from native ELF
  files rather than kernel-linked payloads;
- install two host-built native packages by immutable digest;
- deny one undeclared or over-broad capability request;
- activate a service, resolve it to a capability, and pass a request/reply
  health probe;
- inject a failed upgrade and restore the prior healthy service generation
  without reviving stale handles;
- restart a crashed service without stopping an unrelated native process;
- reboot and reconstruct the active package set from versioned metadata; and
- emit package, source, compiler, and activation digests in the evidence log.

The gate does not claim a Linux ABI, a foreign rootfs, native self-hosting, a
graphical environment, or arbitrary hosted O backend execution inside O-core.
