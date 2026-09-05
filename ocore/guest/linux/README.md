# Pinned Linux virtio block probe

This initramfs runs a real Linux kernel as the guest. Its static `/init` uses
Linux's built-in `virtio-mmio` and `virtio-blk` drivers through `/dev/vda`, checks
an uncached read, then requires a later uncached read to return `EIO` after the
O-core broker withdraws its accepted request. Console output reports guest
observations; it grants no O-core or O-Machine authority.

Build on an AArch64 Linux machine with a static libc development package:

```sh
OCORE_LINUX_GUEST_CC=gcc bash ocore/guest/linux/build-initramfs.sh
```

Or use `aarch64-linux-gnu-gcc` (the default) or an explicit musl cross compiler.
The script rejects a non-AArch64 or dynamically linked executable and writes
an uncompressed, deterministic `newc` archive without requiring root. Its
output directory is selected by `OCORE_LINUX_GUEST_BUILD_DIR`.

Pin the exact Linux source release, configuration, `Image` digest, compiler,
`init` and archive digests in the run manifest. Required built-in configuration:

```text
CONFIG_ARM64=y
CONFIG_OF=y
CONFIG_BLOCK=y
CONFIG_BLK_DEV=y
CONFIG_VIRTIO=y
CONFIG_VIRTIO_MMIO=y
CONFIG_VIRTIO_BLK=y
CONFIG_DEVTMPFS=y
CONFIG_DEVTMPFS_MOUNT=y
CONFIG_SERIAL_AMBA_PL011=y
CONFIG_SERIAL_AMBA_PL011_CONSOLE=y
CONFIG_BINFMT_ELF=y
CONFIG_BLK_DEV_INITRD=y
```

The uncompressed archive does not require `CONFIG_RD_GZIP`. Use the monitor's
generated device tree with guest RAM, PL011, timer, GICv2 and one standard
`virtio,mmio` block endpoint at `0x0a000000`, size `0x200`, SPI 48
(`interrupts = <0 16 4>`). Example command line:

```text
console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init panic=-1 maxcpus=1
```

The endpoint implements modern virtio 1.x MMIO and one split virtqueue with
32 direct descriptors, 4 KiB segments, read-only media, and 512-byte sectors.
Its 1 MiB immutable content is defined by `byte(offset) = (offset XOR 0x5a) & 255`.
It negotiates neither indirect descriptors nor packed rings or event indices.
The Linux driver receives ordinary GIC interrupts. The host can poll for queued
work, but the guest is not a custom polling `/dev/mem` driver. The transport
follows the [Virtio 1.2 specification](https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html).

The monitor must arm the broker only after the complete `KW LINUX SERVICE
HEALTHY` line. A return of `2` from `poll()` proves an actual descriptor was
captured without being committed. The monitor calls `begin_withdraw(1)`, retains
its returned completion handle, and calls `poll()` to publish `IOERR`. It must
keep guest execution and queue memory alive until Linux reports `KW LINUX
IOERR CONSUMED`, call `note_guest_consumed` for the current generation, and
verify `query_completion(handle) == 1` before separate memory teardown. The
post-withdrawal liveness line proves that the guest continued after its error.

Malformed chains, queue overruns, unsupported feature negotiation and invalid
RAM ranges fail closed. A queue reset cannot resurrect a withdrawn generation.
The completion represents this broker's drained synchronous backing work and
retained guest observation; it is not a physical DMA acknowledgment, an
O-Machine `HostResourceAck`, or a durable completion journal. This probe alone
does not qualify G7 or G8, a governed service capability, physical-device reset,
IOMMU isolation, or independent host-EL1 memory mediation.
