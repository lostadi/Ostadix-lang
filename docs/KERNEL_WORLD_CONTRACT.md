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
authority_request = "device.net"

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

This is a schema-only future illustration, not the current native fixture or
evidence of an implemented driver service.

A user-supplied image must carry an exact SHA-256 constraint, use
`user_supplied_only`, and require external acceptance. Those fields record
deployment constraints; they are not a legal determination. A
binary-contained world must request `vm.machine` authority.

Every `capability_requests.kind` is a unique manifest key. Every device-plane
export must carry `authority_request` naming one exact existing `device.*`
request; ABI and semantic exports must omit it. The field is explicit because
neither an export name nor a protocol is authority-bearing, and neither can
unambiguously select a request when a world declares several device requests
or exports. Here `device.*` requires a non-empty suffix; the bare `device.`
prefix is rejected. Multiple device exports may share one authority request.
`quotas.max_devices` counts distinct bound authority-request kinds, not export
rows, so that shared binding consumes one device slot.

The reserved rights matrix is:

| Request kind | Permitted reserved rights |
|---|---|
| `vm.machine` | `run`, `stop` |
| `device.*` | `reset`, `dma` |

Other request kinds cannot use those four reserved rights. The bounded native
V2 record accepts only this four-right pilot vocabulary. These declarations
remain requests: the manifest and its `authority_request` strings never grant
or synthesize a capability.

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
cross-track execution constraints, unique request kinds, the typed rights
matrix, exact device-export authority binding, distinct-authority device-quota
accounting, quota exhaustion, health-gated publication, one-terminal-result
behavior, failure fan-out, restart policy, and stale-generation denial.

## Native normal form, admission, and objects

`VerifiedKernelWorld::encode_native_record()` is the only host API that can
encode a verified world into record-version V2 of the bounded `OKWORLD1` binary
normal form. The magic remains `OKWORLD1`; the version field is 2. V2 carries
each export's exact authority-request key, the verified package digest
separately from the canonical world manifest digest, and fixed bounds of 16
KiB, four exports, and eight capability requests. Untrusted bytes decode only
as the distinct descriptive `InspectedNativeKernelWorldRecord`; its
`from_bytes()` rejects malformed, noncanonical, V1 or other wrong-version,
all-zero-package, and over-limit encodings and cannot be substituted for an
authority-bearing record. The
`ocore-kernel-world-record` tool produces the embedded mode-20 artifact from an
exact package manifest and captured payload; the build produces it twice and
requires byte identity.

The native `kernel_world_record.oc` parser first verifies the exact record
SHA-256 and then validates its complete bounded structure, retains each exact
authority-request key, and enforces bytewise canonical ordering for
requirements, exports, requests, and rights. It rejects duplicate request
kinds, missing or nonexact device-export bindings, cross-kind reserved rights,
and device-authority use beyond the distinct-authority quota. Mode 20 also
presents an independently hash-pinned record whose final rights are reversed
and proves native rejection. The parser does not parse TOML or turn serialized
metadata into authority. The
kernel-resident supervisor
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

Mode 20's `smoke-kernel-world-qemu.sh` gate parses the actual embedded 459-byte
V2 fixture record (SHA-256
`0ece5f7f37ebe203d03cc7e5213dc8f9257a9a225a73e52d37d1f718424b9232`).
The fixture declares exactly the canonical requirements `["npt", "svm"]`.
Native semantics compare both retained strings byte for byte, require
`hardware_virtualized`, and reject a missing, extra, reordered, or `vmx`
requirement before either Mode 20 object configuration or Mode 21 SVM entry.
The gate rejects a wrong record digest, proves package/manifest identity and
default-deny grants, exact export/request binding, typed rights, and
distinct-authority accounting, locally seals a binary-contained pilot graph
without entering it, checks VM/vCPU/guest-page generation and quotas, revokes
and reclaims that exact world while an unrelated VM survives, and reaches a
later timer. The native parser consumes one embedded verified fixture record;
the second admitted identity is a bounded synthetic peer used only for
isolation and reclamation testing. Neither is provider start or full
manifest-resource fulfillment.

The exact-policy negatives include a device export forged to name
`device.audio` instead of its admitted `device.net` request and a same-package,
same-kind, same-length VM rule whose purpose differs by one byte. Sealing is
denied until the exact device authority and exact VM-purpose bytes are
registered.

## Next native slices

The remaining dependency order is:

1. complete the M6B boundary beyond its current bounded-copy mechanism by
   integrating the CPL3 personality RPC, pinned windows, signals, fuzzing, and
   allocation-failure/race gates;
2. connect the native normal-form admission surface to the running package
   supervisor's start, health, stop, replacement, and export-publication
   lifecycle;
3. extend the Mode 21 AMD SVM/NPT synthetic execution backend into a pinned
   foreign-kernel boot and add capability-backed virtual devices;
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

- start, stop, health-check, replace, or publish exports from a provider, and
  it installs no live device service or device capability;
- boot Linux, Plan 9, BSD, Windows, macOS, or another foreign kernel;
- implement firmware execution, a guest agent, shared queue or shared ring,
  UEFI, ACPI, 9P, or another foreign ABI or filesystem protocol;
- implement a virtual device, assign PCI hardware, configure an IOMMU, map DMA,
  route device interrupts, or reset a device;
- install a public `vm.machine` run capability or convert manifest identifiers
  into ambient authority; or
- establish source-integrated or binary-contained isolation.

`KernelWorldInstance` is an executable semantic oracle for later native state
machines. Mode 20 establishes the bounded native record, admission, and
nonexecuting object claims. Hardware-only Mode 21 enters a real AMD SVM vCPU
with NPT, executes a two-page synthetic guest, injects one vector, handles a
controlled hypercall, denies an unmapped GPA, and tears down the exact NPT
context while another VM survives. Mode 21 executes only that synthetic guest;
it does not boot or supervise Linux, Plan 9, or any other foreign kernel, and
it publishes no service. Separate source, artifact, guest-agent, shared-ring,
device, 9P, and hardware gates remain necessary for provider execution and
isolation claims beyond this first instruction-execution substrate.
