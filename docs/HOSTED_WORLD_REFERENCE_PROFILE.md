# Hosted World Reference Profile

**Status:** design/reference profile with partial hosted foundations;
non-qualifying for native Ostadix World release gates.

This profile preserves the useful hosted design that preceded the native
full-stack constitution in [`OSTADIX_WORLD.md`](OSTADIX_WORLD.md). It specifies
how hosted simulators, differential oracles, protocol-fuzz targets, and
development consoles may support native work. It is not the product definition
and cannot satisfy G0 through G13 by being scaled across more Linux processes
or machines.

The currently executable hosted surface consists of the narrower lifecycle
oracle and the partial libraries identified below; there is no complete hosted
World node, Governor, or World protocol implementation.

## What the profile models

The hosted profile may model:

- one named `WorldId` and monotonically increasing `WorldEpoch`;
- hosted node and domain generations, renewable leases, and stale-reference
  denial;
- a single authoritative development Governor;
- logical HGraphs, inert snapshots, placement inputs, and deployment records;
- OValue, capability, and capsule crossing classifications;
- separate namespace/control, authority, execution, and bulk-data planes;
- node admission/withdrawal and exactly-one task-result fencing; and
- explicit resource locality rather than coherent cross-node RAM.

Names, inventory, serialized capability descriptions, and planner annotations
remain descriptive. They do not grant authority. A live bearer must be bound
to a private authority implementation, and unknown portability defaults to a
capsule.

The narrower package and service lifecycle oracle is documented in
[`HOSTED_LIVE_REFERENCE.md`](HOSTED_LIVE_REFERENCE.md).

## Reference topology and consistency

The profile may use Linux-hosted node daemons and one development Governor.
That Governor can persist epochs, issue leases, and reject stale generations.
It does not implement the replicated authoritative plane required by the
native constitution. During a partition, the reference profile may fail
closed, but that behavior is not evidence for quorum, fencing across replicas,
island mode, or recovery of a replicated log.

The profile can produce deterministic fixtures and expected results for later
native implementations. Native results must still pass their own qualifying
gate under the evidence class required by
[`world_alpha_gates.toml`](../evidence/world_alpha_gates.toml).

## Current repository boundary

The repository currently has a shared identity foundation plus a partial
hosted World foundation:

- all 20 constitutional identity atoms typed in Rust and `.oc`, with bounded
  `OWIDENT` v1 byte-exact native convergence, strict invalid-record rejection,
  and separate hierarchical stale/mismatch rejection;
- a bounded `OWPROTO` v1 Rust/`.oc` record codec with deterministic framing,
  strict size and canonical-form rejection, and offline schema-range selection;
- a separate bounded `OWVALUE` v1 Rust/`.oc` oracle with an explicit portable
  allowlist, canonical record and scalar-key-map ordering, a root-only inert
  versioned extension envelope, and a byte-exact 19-record, 928-byte corpus
  whose concatenated SHA-256 is
  `264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc`;
- governed planner vocabulary that remains separate from ambient `HostWorld`;
- deterministic grounding views for OValues, capabilities, capsules, and
  selected governed resources;
- a descriptive KernelWorld-to-domain binding; and
- local hosted lifecycle, project, HGraph, and capability-broker components.

The `OWIDENT` record remains identity-only, and serialized capability IDs are
descriptive non-authority. The separate `OWPROTO` v1 slice is a record codec
with a pure bounded negotiation function; it is not a stream or network
transport, live peer handshake, authenticated session, OValue envelope, or
receipt codec and supplies no authority, Governor, or consensus. The repository
now also has the separate Mode 29 `OWVALUE` codec/hash oracle. It does not make
the full hosted `OValue` enum portable: hosted capabilities, capsules, live
references, requests, and other effectful values fail its explicit projection.
Decoded extensions stay inert, and descriptive code or object references do not
resolve into authority. The repository does not yet have a live replicated
Governor, an authoritative World snapshot service, governed distributed lowering, native World transport,
WorldFS, physical multinode convergence, or an Alpha evidence bundle. No
production lowering currently eliminates ambient `HostWorld` for arbitrary
hosted effects.

## Non-claims

No hosted reference result, including a multinode Linux-hosted deployment,
establishes:

- a native O-core node fabric or native network transport;
- AArch64 O-core boot or SMP safety;
- replicated Governor authority;
- a live per-process WorldFS namespace;
- Linux or Plan 9 kernel boot;
- a general Linux ABI or Debian personality;
- KVM/SVM hardware isolation merely from QEMU TCG;
- PCI or physical-device assignment, DMA/IOMMU isolation, interrupt remapping,
  or hardware reset;
- transparent remote pointers, coherent distributed RAM, or transparent
  migration; or
- G12, G13, or the name **Ostadix World Alpha**.

Modes 20 through 29 remain separately scoped native or QEMU evidence. Their
bounded claims and exclusions are recorded in [`CLAIMS.md`](CLAIMS.md) and
[`gates.toml`](../evidence/gates.toml); they are not promoted by this profile.
