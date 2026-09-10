# Native Ethernet execution profile V1

This directory builds a standalone freestanding O-core image. Two copies run
as separate QEMU x86_64 machines with RTL8139 NICs. The coordinator transfers
each operation to its peer over Ethernet, checks the returned value and digest,
and supplies that output as the next operation's input. The worker independently
checks the provisioned graph and the prior result before executing an addition.
The host launches the machines, provisions private boot configuration, perturbs
opaque Ethernet frames in fault cases, and verifies observations. It does not
execute graph operations or supply their results.

This is a finite native execution profile, not the general project/HGraph
executor, a native IP/TCP stack, a replicated Governor, or physical qualification.
It supports one provisioned pair and an add chain containing two through four
operations. The initial value and each right-hand operand are u32; intermediate
and final values are u64. Each operation is total within this finite delegation.
There is no compiler change or change to existing kernel probe modes.

Build with an existing compiler (the script never invokes Cargo):

```sh
OCOREC_BIN=/absolute/path/to/ocorec \
OCORE_LLD=/absolute/path/to/ld.lld \
OCORE_NATIVE_CLUSTER_BUILD_DIR=/absolute/output \
  ocore/kernel/native-cluster/build.sh
python3 ocore/kernel/native-cluster/verify.py --help
```

The output is `kernel.elf`, an ELF64 image with the existing native bootstrap's
Multiboot2/Xen 32-bit entry convention. It enters long mode, identity maps the
first GiB, and keeps code, stack, and private DMA buffers below 16 MiB. It has
no hosted runtime, Linux guest, filesystem, or CPL3 provider. The RTL8139 driver
uses PCI configuration I/O, four transmit descriptors, and an 8 KiB receive
ring; ring-wrapped frames are copied into a bounded buffer. Register semantics
were checked against [QEMU's RTL8139 implementation](https://raw.githubusercontent.com/qemu/qemu/master/hw/net/rtl8139.c).
The software profile uses polling and fixed TSC retry/deadline bounds.

## Provisioning and authority

After `NATIVE_CLUSTER BOOT_READY`, the trusted boot controller writes exactly
320 lowercase hexadecimal digits and a newline to the serial port. These encode
the 160-byte configuration below. All integer fields are unsigned big-endian.
The MAC is `52:54:00:12:34:<node>`; local and peer IDs are distinct values 1–254.

| Offset | Bytes | Meaning |
|---|---:|---|
| 0 | 8 | `OCNBOOT1` |
| 8, 16 | 8 each | Local node, peer node |
| 24, 32 | 8 each | Local generation, peer generation; both nonzero |
| 40 | 32 | Fresh provisioned session nonce |
| 72 | 32 | Private pair key |
| 104 | 8 | Role: coordinator 1, worker 2 |
| 112 | 8 | Task count, 2–4 |
| 120 | 8 | Initial u32 value encoded in u64 |
| 128 | 32 | Four u32 operands encoded in u64; unused entries must be zero |

The graph digest is SHA-256 of the exact 48 bytes at offsets 112–159. Keys and
nonces must be generated anew by the provisioning harness and are not evidence
fields. Authentication uses HMAC-SHA256 with a constant-work tag comparison;
the native image runs RFC 4231 test case 1 before accepting provisioning.
This authenticates the explicitly provisioned pair. It does not provide
encryption, native entropy, enrollment, persistent key storage, public-key
attestation, key rotation, or authentication of an untrusted boot controller.

The authenticated peer is still restricted to the separately provisioned graph:
exact task number, operation kind, operand, predecessor number, predecessor
result digest, and expected accumulator. A pair key is not a CSpace capability.
This boot-controlled delegation must not be presented as Governor authorization
or as a transferable native capability grant.

## Ethernet record

Frames have Ethernet type `0x88b5` and contain exactly 320 payload bytes. The
driver supplies the 334 bytes excluding Ethernet CRC. No IP layer or serial
relay carries these operation records. Integers are unsigned big-endian.

| Offset | Bytes | Meaning |
|---|---:|---|
| 0 | 8 | `OCNDIS01` |
| 8, 10 | 2 each | Schema 1, message kind |
| 12 | 4 | Exact payload length 320 |
| 16, 24 | 8 each | Sender node, recipient node |
| 32, 40 | 8 each | Sender generation, recipient generation |
| 48 | 32 | Provisioned session nonce |
| 80 | 8 | Sequence |
| 88, 96 | 8 each | Task number, delegated add opcode 1 |
| 104, 112 | 8 each | Left and right operands |
| 120 | 8 | Predecessor task number, zero for the first task |
| 128 | 32 | Input digest |
| 160 | 32 | Predecessor result digest, zero for the first task |
| 192 | 8 | Result value; zero in a request |
| 200, 208, 216 | 8 each | Execution, duplicate, rejection counters in responses |
| 224 | 32 | Result digest; zero in a request |
| 256 | 32 | Provisioned graph digest |
| 288 | 32 | HMAC-SHA256 of payload bytes 0–287 |

The input digest is SHA-256 of `opcode || left || right || predecessor ||
predecessor_result_digest`, 64 bytes. The result digest is SHA-256 of its
eight-byte big-endian representation. Generation, nonce, graph, direction,
length, and exact bytes are authenticated; authentication alone is never the
operation admission check.

Kinds are request 1, result 2, drain 3, drain acknowledgment 4, and drain
confirmation 5. Task sequences are 1 through the task count. Only the next
dependent task can create an execution. An exact duplicate request returns the
retained result bytes. A substituted request using an old sequence is rejected.
The native result counter advances once per accepted task; retry never runs the
addition again. Four retained attempt slots are independent of the existing
single-wait personality RPC implementation.

After all tasks, sequence `task_count + 1` drains the worker. Task bytes 88–255
must be zero in control requests. The worker fences new work, retains the exact
drain acknowledgment for retries, and reports its counters. `COMPLETE` is emitted
only after the coordinator validates that authenticated acknowledgment and its
final value/digest. Sequence `task_count + 2` confirms drain. Both nodes then
drain transmit ownership, disable the NIC and its PCI bus mastering, clear the
session key storage, and emit `NIC_QUIESCED`. This is the lifecycle of a dedicated
emulated device, not IOMMU isolation, reset recovery, or physical-device ownership.
On terminal failure, an idempotent abort path also disables the emulated device
and its PCI bus mastering, including partially initialized and malformed-RX
states. It reports `NIC_ABORTED`, separately from successful `NIC_QUIESCED`;
failed disable verification reports `NIC_ABORT_FAILED` and cannot pass a gate.
The native coordinator additionally tests that an authentic drain acknowledgment
with nonzero unused task fields, or a wrong byte length, is rejected. Those
local decoder probes are not counted as network fault injections.

The cache and sequence fence exist only in RAM for one boot session. Loss of a
node or confirmation is bounded by the harness deadline and is not silently
reported as success. New boots require fresh provisioning. These records make
no persistent exactly-once, replicated commit, crash-recovery, World membership,
G4, G8, G10, or Alpha claim. Keep physical-node and device gates open.
