# KernelWorld Contract and Bounded Native Object Slice

Status: Stage 0's strict host-side semantic contract, verified-package binding,
and lifecycle oracle are implemented. A bounded native supervisor-admission
follow-on now consumes a hash-pinned normal-form record, applies independent
default-deny policy, and configures nonexecuting VM/vCPU/guest-page objects. No
foreign kernel is executed by either stage.

`src/kernel_world.rs` defines the common public boundary for the two planned
foreign-kernel integration tracks:

| Track | Image mechanism | Required execution mechanism |
|---|---|---|
| `source_integrated` | source-built kernel in an immutable package payload | `paravirtual` direct entry |
| `binary_contained` | package-payload or hash-pinned user-supplied image | `hardware_virtualized` machine |

Both tracks use the same world identity, generation, health, export, quota,
request, failure, replacement, and provenance rules. A client binds to a typed
export such as `o.net-port/v1`; it does not receive provider-internal handles or
infer authority from manifest metadata.

## Strict manifest

The accepted schema is `ocore.kernel-world/v1`. Unknown fields are rejected.
Names, paths, digests, list lengths, vCPU and memory envelopes, request counts,
shared-memory bytes, and device counts are bounded before a world instance is
created. Declaration order is non-semantic and `canonical_toml()` produces a
stable human-readable form.

`VerifiedKernelWorld::from_package()` binds that document to an already
verified `ocore.package/v1` object. The package must use runtime kind
`kernel-world` and ABI `ocore.kernel-world-control/v1`; its non-executable
runtime entry supplies the world manifest. Package name, version, architecture,
health contract, service names/protocols, and capability requests must exactly
match the inner world declaration. For `package_payload` images, the referenced
image must exist in the captured payload and its bytes must match the declared
SHA-256. The resulting instance identity carries the verified package digest.

An illustrative binary-contained driver world is:

```toml
schema = "ocore.kernel-world/v1"
name = "kernel/linux-driver"
version = "1.0.0"
integration = "binary_contained"

[image]
kind = "user_supplied"
expected_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[machine]
guest_architecture = "x86_64"
profile = "o-machine-pc/v1"
execution = "hardware_virtualized"
firmware = "uefi"
min_vcpus = 1
max_vcpus = 4
min_memory_mib = 512
max_memory_mib = 4096
requirements = ["iommu", "vmx"]

[lifecycle]
health_protocol = "ocore.kernel-world-health/v1"
health_timeout_ms = 5000
restart = "on_failure"

[quotas]
max_outstanding_requests = 64
max_requests_per_generation = 100000
max_shared_memory_bytes = 67108864
max_devices = 1

[[exports]]
name = "network.default"
plane = "device"
protocol = "o.net-port/v1"

[[exports]]
name = "linux.exec"
plane = "abi"
protocol = "linux.exec/v1"

[[capability_requests]]
kind = "vm.machine"
rights = ["run", "stop"]
purpose = "contained guest execution"

[[capability_requests]]
kind = "device.net"
rights = ["dma", "reset"]
purpose = "exclusive network provider"

[license]
redistribution = "user_supplied_only"
external_acceptance_required = true
```

A user-supplied image must carry an exact SHA-256 constraint, use
`user_supplied_only`, and require external acceptance. Those fields record
deployment constraints; they are not a legal determination. A
binary-contained world must request `vm.machine` authority. A device-plane
export also requires an explicit `device.*` request and a nonzero device quota.
The manifest requests authority but never grants it.

## Lifecycle and request invariant

The reference instance follows this bounded state machine:

```text
Installed -> Starting -> Healthy -> Failed
                         |             |
                         +-> Stopped   +-> Starting (policy permitting)
                               |
                               +-> Starting (`always` only)
```

Exports are unavailable until the exact current generation is marked healthy.
Each request ID contains that generation and a bounded sequence. Reply,
cancellation, timeout, world failure, and world stop compete for one terminal
record. A second completion is rejected with the first terminal disposition.
World failure or stop drains every outstanding request once. Replacement
increments the generation, clears the bounded prior-generation tombstones, and
rejects every old request as stale.

Resolved exports carry immutable provenance:

```text
package digest + world name + generation + integration mode
    + export name + export plane + protocol
```

This metadata describes origin. It is not a capability and cannot recreate
authority.

Run the executable contract gate with:

```bash
cargo test --test kernel_world_contract --no-default-features
```

The gate covers strict/unknown-field parsing, canonicalization, hash pinning,
exact content-addressed package binding, package-payload image verification,
cross-track execution constraints, explicit VM/device authority, quota
exhaustion, health-gated publication, one-terminal-result behavior, failure
fan-out, restart policy, and stale-generation denial.

## Native normal form, admission, and objects

`VerifiedKernelWorld::encode_native_record()` is the only host API that can
encode a verified world into the bounded `OKWORLD1` binary normal form. The
record carries the verified package digest separately from the canonical world
manifest digest and has fixed bounds of 16 KiB, four exports, and eight
capability requests. Untrusted bytes decode only as the distinct descriptive
`InspectedNativeKernelWorldRecord`; its `from_bytes()` rejects malformed,
noncanonical, wrong-version, all-zero-package, and over-limit encodings and
cannot be substituted for an authority-bearing record. The
`ocore-kernel-world-record` tool produces the embedded mode-20 artifact from an
exact package manifest and captured payload; the build produces it twice and
requires byte identity.

The native `kernel_world_record.oc` parser first verifies the exact record
SHA-256 and then validates its complete bounded structure and bytewise canonical
ordering for requirements, exports, requests, and rights. Mode 20 also presents
an independently hash-pinned record whose final rights are reversed and proves
native rejection. The parser does not parse TOML or turn serialized metadata
into authority. The kernel-resident supervisor
admission state in `kernel_world_admission.oc` stages at most two
generation-tagged worlds and admits one only when every
declared request has an independently registered policy rule keyed by exact
package digest plus copied, byte-for-byte request kind and purpose, with no
rights overgrant. A 64-bit string summary may reject a nonmatch early but can
never grant authority without the exact-byte comparison. Missing policy is
denial. Package and manifest digests remain distinct through admission.

`vm_object.oc` supplies at most two VM identities, four vCPU identities, and
eight aligned guest-page attachments backed by real anonymous 4 KiB memory
objects. Every child is bound to the exact admitted-world generation; duplicate
or sparse vCPU ordinals, unaligned or overlapping guest addresses, and quota
excess fail closed. Sealing is local to this bounded pilot graph: package
admission remains `ADMITTED` until revocation, and there is no public
`mark_configured` admission transition. Exact-world revocation reclaims its
vCPUs, page objects, and frames without disturbing an unrelated admitted VM.

Mode 20's `smoke-kernel-world-qemu.sh` gate parses the actual embedded 440-byte
fixture record (SHA-256
`36ebffa374631fc51e70cc20e0512fd899f3703fe15d200a33e330482a707671`),
rejects a wrong record digest, proves package/manifest identity and default-deny
grants, locally seals a binary-contained pilot graph without entering it,
checks VM/vCPU/guest-page generation and quotas, revokes and reclaims that exact
world while an unrelated VM survives, and reaches a later timer. The native
parser consumes one embedded verified fixture record; the second admitted
identity is a bounded synthetic peer used only for isolation and reclamation
testing. Neither is provider start or full manifest-resource fulfillment.

The exact-policy negative uses the correct device rule plus a same-package,
same-kind, same-length VM rule whose purpose differs by one byte; sealing is
denied until the exact VM-purpose bytes are registered.

## Next native slices

The remaining dependency order is:

1. complete the M6B boundary beyond its current bounded-copy mechanism by
   integrating the CPL3 personality RPC, pinned windows, signals, fuzzing, and
   allocation-failure/race gates;
2. connect the native normal-form admission surface to the running package
   supervisor's start, health, stop, replacement, and export-publication
   lifecycle;
3. add an actual paravirtual or hardware-virtualized execution backend,
   interrupt injection, and virtual devices behind the current nonexecuting
   VM/vCPU/guest-memory identities;
4. carry the same request and export contracts over a bounded guest-agent and
   shared-queue protocol; and
5. add device assignment only after separate IOMMU isolation, interrupt
   revocation, DMA-window teardown, device-reset, and hostile-failure gates.

The first driver-compatibility proof should use a bounded resettable device
class and must show that O-core has no native driver for it, a foreign kernel
exports the service, unrelated worlds survive provider failure, old generation
handles fail closed, DMA and interrupts are revoked, and a health-gated
replacement can be rebound.

## Explicit non-claims

The current native slice does not:

- start, stop, health-check, replace, or publish exports from a provider;
- boot Linux, BSD, Windows, macOS, or another foreign kernel;
- enter a VM through VMX or SVM, build EPT/NPT mappings, or execute a vCPU;
- implement firmware execution, a guest agent, shared queue, UEFI, ACPI, or a
  foreign ABI;
- assign PCI hardware, configure an IOMMU, map DMA, route interrupts, or reset a
  device;
- install a public `vm.machine` run capability or convert manifest identifiers
  into ambient authority; or
- establish source-integrated or binary-contained isolation.

`KernelWorldInstance` is an executable semantic oracle for later native state
machines. Mode 20 establishes only the bounded native record, admission, and
nonexecuting object claims above. Separate source, artifact, QEMU, and hardware
gates remain necessary for provider execution and isolation claims.
