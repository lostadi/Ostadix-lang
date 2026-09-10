# Native network execution and real Linux integration

Author: Lee Daghlar Ostadi

These executable profiles add native Ethernet work dispatch and a real foreign
Linux kernel below O-core. They are independently runnable software integrations.
They do not complete OSTADIX Alpha, the general ProjectBundle executor, or the
physical distributed/device qualification gates.

## Native Ethernet execution

Two separately booted x86_64 O-core kernels communicate through RTL8139 Ethernet
devices. The native driver owns its receive ring and transmit buffers, accesses
PCI configuration and device registers directly, and disables device operation
and PCI bus mastering during shutdown. No Linux node daemon executes the work.

Each boot receives a fresh session nonce and pair key plus a finite graph
delegation. HMAC-SHA256 binds exact packet bytes, node identities, node
generations, the session, and graph digest. The independently provisioned graph
restricts the peer to two through four dependent additions. Possession of the
pair key cannot alter that delegation. The first input and each right operand
are u32; the accumulated result is u64.

The worker checks the preceding result digest, sequence, operation, operands,
and input digest before computing. It retains exact request/result bytes for
retries. A changed duplicate is rejected. Successful drain exchanges precede
NIC quiescence and erasure of the provisioned session storage.

The qualification harness runs separate guests on a direct QEMU Ethernet link.
Its fault lane corrupts authentication, sends authenticated stale-generation,
wrong-session, wrong-graph, out-of-order, and wrong-dependency packets, duplicates
a request, and drops replies. A longer retry case wraps the receive ring;
a permanent reply partition must terminate with native errors and verified NIC
abort, never successful completion. Native retransmission after bounded loss
completes the workload without extra executions. The verifier checks every ordered intermediate
input/result digest, final result, execution count, and both NIC shutdowns.
Private boot keys are never stored in the evidence directory.

```bash
# Use an isolated checkout; the compiler is a host build tool.
cargo build --locked --bin ocorec
bash ocore/kernel/native-cluster/build.sh
python3 ocore/kernel/native-cluster/verify.py \
  target/ocore-native-cluster/kernel.elf \
  target/ocore-native-cluster/evidence
```

The executable contract and wire layout are in
[the native cluster profile](../ocore/kernel/native-cluster/README.md).

## Real Linux and device withdrawal

The AArch64 profile runs a resident O-core EL2 monitor with a distinct stage-two
mapping for a real, unmodified upstream Linux 6.12.43 Image. Linux receives
512MiB of guest RAM mapped to a separate outer physical window. The guest cannot
map the monitor, its state, or its stage-two tables. The loader validates the
Image header, entry alignment, layout bounds, and recorded image digest.

O-core implements a modern virtio-MMIO block device. Linux uses its ordinary
built-in virtio-MMIO and virtio-block drivers and the emulated GIC interrupt
controller. The broker validates split-queue sizes, alignments, descriptor
chains, access directions, ranges, overlaps, feature negotiation, and available
indices before accessing guest buffers. Compiled adversarial selftests exercise
the production queue and withdrawal APIs before Linux entry.

The static Linux `/init` program verifies an uncached read, reports health, and
issues another read. O-core holds that accepted request, begins asynchronous
withdrawal, returns `VIRTIO_BLK_S_IOERR`, and retains the queue memory. The Linux
driver returns `EIO` to `/init`, which subsequently proves continued execution.
Only after guest consumption and interrupt acknowledgment does O-core complete
the device withdrawal and separately remove guest RAM mappings, invalidate
translations, and perform architectural barriers. Resuming the guest then
causes a real stage-two fault; the monitor contains it and demonstrates later
counter progress.

Linux presents no Ostadix authority hypercall or machine handle. The monitor
implements a bounded standard PSCI firmware subset; its guest health
observation currently uses trapped console lines. The broker and policy execute
at EL2 in this profile, so this is not the separate trusted-host-EL1 architecture
required by the full G7 contract.

Build the payload on Linux (native AArch64 or with an AArch64 cross compiler):

```bash
bash scripts/build-real-linux-payload.sh
OCORE_LINUX_GUEST_BUILD_DIR="$PWD/target/ocore-real-linux/payload" \
  bash ocore/guest/linux/build-initramfs.sh
```

Set `OCORE_LINUX_GUEST_CC=gcc` when building the guest natively on AArch64.
The kernel recipe downloads a checksum-pinned source archive, validates reused
source contents, builds outside the source tree, and records the compiler,
configuration, and Image digests. The guest recipe creates a deterministic
`newc` initramfs and rejects a wrong-architecture or dynamically linked `/init`.
Payload output directories can be selected using the scripts' environment
variables and transferred to the machine running QEMU.

```bash
OCORE_LINUX_OCOREC_BIN="$PWD/target/debug/ocorec" \
  bash ocore/kernel/build-aarch64-kernel-world-linux.sh
python3 ocore/kernel/smoke-aarch64-kernel-world-linux-qemu.py \
  --payload-dir target/ocore-real-linux/payload \
  --build-dir target/ocore-real-linux/monitor \
  --output-dir target/ocore-real-linux/evidence
```

The build follows the upstream [AArch64 boot ABI](https://docs.kernel.org/arch/arm64/booting.html).
The [upstream checksum index](https://cdn.kernel.org/pub/linux/kernel/v6.x/sha256sums.asc)
provides the pinned Linux archive digest.

## Qualification and remaining implementation

The `native-systems` CI job builds both profiles, runs their native QEMU checks,
and retains transcripts and payload provenance. It participates in Required CI.
This lane is separate from the existing portable QEMU manifest and the
append-only Alpha evidence ledger. Local transcripts do not establish a remote
CI result.

| Area | Implemented by these profiles | Required for full completion |
| --- | --- | --- |
| Native execution | Finite dependent graph, native compute, Ethernet transfer, authenticated per-session retry/cache and drain | Versioned native ProjectBundle route lowering, shared World identities and live capability-bound provider dispatch |
| Distributed authority | Explicit boot-provisioned two-peer delegation | Enrollment/key lifecycle, replicated Governor log, durable objects/checkpoints, recovery after node loss, exactly-one global commit |
| Linux containment | Real kernel, guest-only stage-two RAM, standard block driver, consumed EIO before memory removal | Separate host-EL1 broker, EL2-mediated bounded host views/pins, page-owner lifecycle, full composite acknowledgments and governed service publication |
| Devices | Emulated RTL8139 and virtio block, native register/queue access and bounded shutdown | Identified physical devices, IOMMU/SMMU mappings and drain, generation-bound interrupts, verified reset and failed-reset quarantine, isolation/reset-group handling |
| Integrated World | Two native network guests and a separate Linux guest profile | One integrated physical multinode scenario connecting these implementations to the general HGraph and authority services |

G4, G7, G8, and G10 remain unqualified. Physical work requires identified node
models and native boot/serial access, plus the device's PCI or platform identity,
IOMMU/SMMU topology, and isolation/reset group. These are not replaceable with
additional successful virtual runs. General graph lowering, replicated commit,
and the separate host-EL1 boundary are also software work still to be done.
