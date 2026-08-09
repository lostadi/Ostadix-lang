# OSTADIX Alpha x86_64 UEFI boot media

This guide covers the implemented OSTADIX Alpha disk-image path for the
freestanding O-core kernel. The current path is **x86_64 UEFI only**. It builds
one deterministic GPT disk image, validates its bounded layout, boots that
exact disk through OVMF under QEMU/TCG, and can derive and write a
capacity-bound target plan for an external device on macOS or Linux. For a
larger target, the planner relocates the backup GPT to the target's final LBA
without enlarging or moving the admitted ESP.

The current automated evidence is virtual. Building an image, passing the
OVMF/QEMU smoke, or writing the bytes to removable media does **not** establish
that a physical machine booted OSTADIX Alpha. Mode 34 establishes one bounded
four-vCPU SMP bring-up under QEMU/TCG; it does not make the general kernel SMP
safe and does not establish physical SMP. Physical-machine boot remains
unpassed until separately observed under the authority-free workflow below.

## Implemented image

The media builder produces:

- a protective MBR and mirrored primary/backup GPT;
- exactly one EFI System Partition, beginning at LBA 2048 and labeled
  `OSTADIX`;
- the removable-media fallback executable `/EFI/BOOT/BOOTX64.EFI`;
- `/boot/kernel.elf`, built with `OCORE_PROBE_MODE=0`;
- an embedded GRUB configuration that selects the ESP by its deterministic
  FAT UUID and loads the kernel through Multiboot2; and
- deterministic disk and partition GUIDs derived from the ESP SHA-256 digest.

The default output is:

```text
target/ostadix-media/x86_64/ostadix-x86_64-uefi.img
```

The default ESP is 64 MiB. The builder fixes FAT staging timestamps, retains
`OSTADIX` as a descriptive FAT label, derives the FAT serial from the kernel
and boot configuration, and defaults `SOURCE_DATE_EPOCH` to `315532800`.
Determinism is claimed only for repeated builds under the same source,
toolchain, inputs, and relevant environment. The smoke command tests two such
local rebuilds byte for byte; it is not a cross-version GRUB or mtools
reproducibility claim.

### FAT boot-selection identity

The FAT serial is derived before the EFI executable is built. Let `K` be the
exact `kernel.elf` bytes and `C` be the exact canonical `grub.cfg` template,
including its single `@OSTADIX_FAT_UUID@` marker. The builder computes:

```text
H = SHA256(
      "OSTADIX/FAT-IDENTITY/V1\0" ||
      u64be(len(K)) || K ||
      u64be(len(C)) || C
    )
```

The first four bytes of `H`, interpreted as a big-endian integer, are the FAT
volume serial; the otherwise possible zero value is mapped to one. GRUB's FAT
UUID spelling renders the high and low 16-bit words, so serial `0x12345678` is
rendered as `1234-5678`. The builder replaces the marker with that exact
uppercase UUID, embeds the rendered configuration in `BOOTX64.EFI`, formats
the ESP with the same serial, and checks the on-media FAT32 volume-ID field
before packing GPT. The GRUB command is an exact
`search --no-floppy --fs-uuid --set=root`; the common `OSTADIX` label no longer
selects the boot filesystem.

The template rather than the completed ESP is in the derivation because the
completed ESP contains `BOOTX64.EFI`, which itself contains the derived UUID.
Hashing the completed ESP would create a circular fixed-point requirement.
The FAT serial is only a 32-bit locator, so it is deterministic but neither a
cryptographic authenticator nor globally collision-free. Firmware with another
attached FAT filesystem carrying the same serial may remain ambiguous; the
automated smoke attaches one OSTADIX data disk. The image SHA-256 and
digest-derived GPT GUIDs remain the stronger complete-image identities.

### BootInfoV1 boundary

`src/ocore/boot_info.rs` defines the architecture-neutral `BootInfoV1`
contract and a strict, bounded Multiboot2 normalizer. It validates memory-map,
module, ACPI RSDP, EFI, framebuffer, command-line, bootloader, serial, kernel
span, CPU, and artifact-digest inputs; it reserves kernel, module, and
framebuffer intersections before exposing usable memory.

The x86_64 Multiboot2 entry now implements the corresponding bounded
freestanding handoff. It copies admitted facts into kernel storage and chooses
one largest fully covered, page-aligned allocator subwindow of at least 4 MiB
inside 4--16 MiB. This is one bootstrap allocator window, not a general
firmware-discovered physical-memory allocator or arbitrary-platform contract.

## Prerequisites and setup

Run from the canonical repository:

```bash
export O_LANG_ROOT=/Users/ustad/Ostadix-lang
export O_BACKENDS_DIR="$O_LANG_ROOT/backends"
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$O_LANG_ROOT/target/release:$PATH"
cd "$O_LANG_ROOT"
```

The media setup profile includes the O-core profile and adds an x86_64 EFI
GRUB builder, mtools, and OVMF/edk2 firmware:

```bash
./setup.sh --with-ocore-media -y
./setup.sh --with-ocore-media --check
o kernel doctor-media
```

To install only dependencies and the managed environment, without building
OSTADIX Alpha:

```bash
./setup.sh --with-ocore-media --deps-only -y
```

Automatic installation of this profile is currently validated only for
macOS/Homebrew and Debian-family Linux. On another host, install the tools
manually and use the non-installing `--check` command.

The checked media prerequisites are:

- the ordinary O-core compiler/linker tools, including Clang, an
  LLD-compatible linker, ELF inspection tools, Python 3, and
  `qemu-system-x86_64`;
- `x86_64-elf-grub-mkstandalone` or `grub-mkstandalone`;
- `mformat` and `mcopy`;
- `tar` for challenged committed-source snapshots; and
- an x86_64 OVMF/edk2 code image.

The scripts search these firmware paths:

```text
/opt/homebrew/opt/qemu/share/qemu/edk2-x86_64-code.fd
/usr/local/opt/qemu/share/qemu/edk2-x86_64-code.fd
/usr/share/OVMF/OVMF_CODE.fd
/usr/share/edk2/x64/OVMF_CODE.fd
```

If firmware is elsewhere, bind the exact file explicitly:

```bash
export OSTADIX_OVMF_CODE=/absolute/path/to/OVMF_CODE.fd
```

## Build and inspect

Build the default image:

```bash
o kernel media
```

Or choose one output path:

```bash
IMAGE="$O_LANG_ROOT/target/ostadix-media/x86_64/ostadix-alpha.img"
o kernel media "$IMAGE"
```

The command rejects more than one path. A successful build prints the resolved
image path, total byte count, full-image SHA-256, ESP SHA-256, disk GUID,
derived FAT UUID, and complete FAT-identity preimage SHA-256.
Before reporting success, the builder invokes the strict inspector and requires
its metadata to match the pack metadata.

Inspect the default image:

```bash
o kernel inspect-media
```

Inspect an explicit image:

```bash
o kernel inspect-media "$IMAGE"
```

The inspector emits canonical JSON with schema `ostadix.boot-media/v1`. It
checks the protective MBR, both GPT headers and CRCs, partition-table CRCs,
fixed bounded geometry, exactly one ESP, the FAT-compatible boot signature,
and digest-derived disk and partition GUIDs. It also reports:

```text
schema, bytes, sha256, disk_guid, partition_guid, esp_sha256,
esp_bytes, esp_first_lba, esp_last_lba, sector_size
```

Inspection validates and hashes the bounded outer image. It does not provide a
signature, Secure Boot chain, measured boot, or independent authentication of
the producer.

## Boot the exact disk under OVMF/QEMU

Interactive boot rebuilds the default image, attaches it read-only as a
virtio block device, and starts x86_64 OVMF under QEMU/TCG:

```bash
o kernel boot-media
```

Exit the multiplexed serial monitor with `Ctrl-A X`.

To select an explicit image path or firmware file:

```bash
OSTADIX_MEDIA_IMAGE="$IMAGE" \
OSTADIX_OVMF_CODE=/absolute/path/to/OVMF_CODE.fd \
o kernel boot-media
```

`boot-media` accepts no positional arguments. The image path is selected by
`OSTADIX_MEDIA_IMAGE`; the command rebuilds that path before booting it.

The interactive VM is deliberately narrow: QEMU TCG, `q35`, 128 MiB RAM,
read-only pflash firmware, read-only disk media, `-nodefaults`, `-nic none`, no
display, and the serial/monitor multiplexer on standard I/O. It does not use
QEMU's `-kernel` shortcut.

Run the automated media smoke with:

```bash
o kernel smoke-media
```

The smoke command:

1. builds two images in separate build directories;
2. requires the complete image bytes to be identical;
3. strictly inspects the first image;
4. requires both derivations to match and independently reads the exact ESP's
   FAT volume ID back from the built disk;
5. boots it as a read-only disk through x86_64 OVMF and QEMU/TCG, thereby
   exercising the embedded `--fs-uuid` search; and
6. requires these serial markers:

```text
O-core kernel: serial online
page protections: W^X online
CPL3 native[0]: online
timer CPL3 return: online
CPL3 heartbeat: online
```

The kernel is expected to remain alive, so the harness treats its bounded QEMU
timeout as part of success after all markers appear. The default timeout is 12
seconds and can be changed for a slow local TCG host:

```bash
OSTADIX_MEDIA_TIMEOUT_SECONDS=30 o kernel smoke-media
```

### Firmware handoff and challenged lifecycle gate

Run the stricter BootInfo gate with:

```bash
o kernel smoke-boot-info
```

This gate builds a challenged mode-33 image twice, requires byte-identical
media, and boots it through GRUB and OVMF. The freestanding kernel accepts only
a bounded Multiboot2 record (at most 64 KiB and 64 tags), validates a sorted
non-overlapping memory map, records bounded ACPI/EFI status, selects one fully
covered page-aligned allocator subwindow inside 4--16 MiB, and rejects modules.
It copies the admitted facts into kernel storage and removes the temporary
boot-information mapping before W^X and page-allocation checks.

The same gate then boots a challenged ordinary mode-0 image. It requires the
exact random challenge and clean Git commit to be echoed once and proves that
the admitted allocator continues through CPL3 entry, timer return, and the
CPL3 heartbeat. Its final positive and wrong-challenge negative checks use the
same exact transcript grammar as the authority-free physical-observation
tool.

This is a bounded x86_64 OVMF/QEMU TCG handoff proof. It is not a general
firmware allocator, ACPI table consumer, initrd loader, Secure Boot chain,
physical-machine observation, or hardware trust result.

### Bounded SMP bring-up gate

Run the exact four-vCPU positive and one-vCPU negative controls with:

```bash
o kernel smoke-smp
```

Mode 34 revalidates one exact four-CPU ACPI/MADT topology, validates PIT
progress before startup, copies the admitted 0x8000 trampoline while it is
RW/NX, changes it to R/X, and sends x2APIC INIT/SIPI to three APs. Once all APs
have entered kernel RX text on distinct stacks, the BSP erases and unmaps the
trampoline. Four unique APIC identities then cross one atomic release/progress
barrier before a later PIT transition and heartbeat. The same challenged image
under one vCPU must emit one rejection and none of the topology, startup, CPU,
barrier, or heartbeat success markers.

This gate is exactly QEMU q35/TCG + OVMF with four type-0, 8-bit APIC IDs. APs
park after the barrier. It is not physical-machine evidence, arbitrary CPU
topology, a general SMP scheduler, interrupt balancing, per-CPU allocation, or
proof that existing process/IPC/syscall subsystems are SMP safe.

## Prepare and write removable media

**Writing is destructive. It replaces the target's partition-table authority,
writes the admitted image body, retires the source backup GPT, and writes a new
backup GPT at the selected whole device's end. There is no automatic backup or
rollback. Keep the image on a different disk, verify the device identity
yourself, and never use an internal or active system disk.**

The public workflow is intentionally two-step. `prepare-write` does not write
the device. It captures and validates one private image snapshot, probes one
whole device, and derives an `ostadix.boot-media-target-plan/v2` document for
that exact reported capacity. The target must be 512-byte aligned, at least as
large as the canonical image, and no larger than the bounded 16 TiB v2 limit.

The plan keeps the partition and ESP geometry fixed, updates the protective
MBR and primary GPT for the target capacity, zeros the canonical image's now
stale backup-GPT location, and places the replacement backup partition table
and header at the target's final LBAs. Every range OSTADIX will mutate appears
as an ordered, sector-aligned extent with its own SHA-256. Every gap appears in
`unwritten_ranges` under the policy
`preserve-unhashed-unverified-may-be-recoverable`.

`target_plan_sha256` is the authority for the proposed mutation: it binds the
canonical source identity, exact target capacity and geometry, ordered extent
descriptors and hashes, and explicit unwritten ranges. It is **not** a hash of
the complete target device. `target_image_sha256` is populated only when every
target byte is determined by the plan. It is therefore `null` for an ordinary
larger sparse target. Bytes in `unwritten_ranges` are not erased, read-back
verified, or authenticated; prior data there may remain recoverable. Treat the
entire device as sensitive even after a successful write.

Set an image and a deliberately incomplete placeholder device, then replace
the placeholder only after checking the operating-system inventory:

```bash
IMAGE="$O_LANG_ROOT/target/ostadix-media/x86_64/ostadix-x86_64-uefi.img"
DEVICE=/dev/diskN  # macOS: replace N with the verified external whole disk
```

On Linux, use the verified removable or USB whole-disk path, such as a
confirmed `/dev/sdX`; do not use a partition path. An internal NVMe device is
rejected even when it is not the active root disk.

Inspect the image again, then prepare the exact pairing:

```bash
o kernel inspect-media "$IMAGE"
o kernel prepare-write --image "$IMAGE" --device "$DEVICE"
```

Review every returned field, especially `device`, `target_bytes`,
`target_plan_sha256`, `target_extents`, and `unwritten_ranges`. Copy the exact
`confirmation` value from that output; it has the form `OSTADIX-WRITE-`
followed by 32 uppercase hexadecimal characters. Then run:

```bash
TOKEN=OSTADIX-WRITE-COPY_THE_EXACT_PREPARE_TOKEN
o kernel write-media \
  --image "$IMAGE" \
  --device "$DEVICE" \
  --confirm "$TOKEN"
```

The placeholder token above is intentionally invalid. Do not turn this into a
one-step command that skips human review.

### macOS guards

The writer accepts only a whole `/dev/diskN` or `/dev/rdiskN`. It requires
`diskutil` to report the device as external, writable, and not read-only, and
rejects the whole disk containing the active root filesystem. A stable device
serial or media UUID is mandatory; a `DeviceTreePath` or USB-port topology is
not accepted as device identity. Immediately before writing,
it repeats image/device validation, unmounts the external disk, and writes
through its raw `/dev/rdiskN` path.

### Linux guards

The writer obtains inventory from `lsblk` and the active root source from
`findmnt`. It requires one exact writable whole-disk record, rejects any
mountpoint on the target or any direct or nested descendant, and rejects the
detected root disk. It additionally requires `lsblk` to report either `RM=true`
or USB transport. Internal NVMe, internal SATA, and unknown non-removable
transports fail closed. A stable `SERIAL` or `WWN` identity is mandatory.
There is no dangerous-device override in writer v2.

### Confirmation and post-write verification

The v2 token is bound to:

- operating system;
- canonical device path and operating-system-reported device identity;
- device byte capacity;
- the complete canonical `ostadix.boot-media-target-plan/v2`, including source
  and ESP identities, target capacity, GPT geometry, extent hashes, and
  unwritten ranges.

`write-media` validates the image and device again before mutation. A changed
path, reported identity, capacity, image, or stale token fails closed. A
replacement device that the operating system reports with exactly the same
path, stable identity, device number, and capacity cannot be distinguished by
this interface, so the operator must still keep physical custody between
prepare and write.

For the destructive invocation, the writer opens the source without following
symbolic links, copies exactly its admitted length into a private temporary
file, validates that snapshot, and retains one read-only descriptor through
the device write. It rejects source growth, truncation, replacement, trailing
bytes, or content/identity change before opening the raw target. The original
source pathname is never reopened as the source of device bytes. Ensure the
host's temporary filesystem has free space for one complete image snapshot.

The raw mutation target is opened once. The writer validates that held device
descriptor's kernel identity and capacity, re-probes the public path while the
descriptor remains held, and then uses only that descriptor for mutation and
verification. It writes exactly the admitted extents, calls `fsync`, and reads
each extent back through the same held descriptor. Each read-back SHA-256 must
match the extent declared in `target_plan_sha256`. It deliberately does not
read, hash, zero, or verify an `unwritten_range`. Success emits schema
`ostadix.media-write/v2` with `written:true`.

If opening the device is denied, re-run the exact already-reviewed command only
with the minimum local privilege appropriate for that host. Do not weaken the
confirmation step or change the target while adding privilege.

## Physical serial-console checklist

This workflow records an operator observation; it does not authenticate the
machine or make physical boot a passed OSTADIX Alpha gate. Use a non-production
x86_64 test machine whose firmware can boot an unsigned UEFI removable-media
fallback executable and which exposes a 16550-compatible COM1 serial port at
I/O base `0x3f8`.

First work from a clean committed tree and create a private operator-artifact
directory **outside** the repository. Every image, build directory, writer
record, machine profile, intent, transcript, and observation below stays under
that directory so creating the evidence does not make the source tree dirty:

```bash
git status --short                 # must be empty
EVIDENCE_DIR="$HOME/ostadix-physical-$(date -u +%Y%m%dT%H%M%SZ)"
umask 077
mkdir -m 700 "$EVIDENCE_DIR"       # must be a new path outside O_LANG_ROOT
MACHINE_JSON="$EVIDENCE_DIR/machine.json"
```

For challenged media, the builder exports the exact named Git commit to a
private source snapshot and compiles from that export, then rechecks that the
canonical checkout still names the same clean commit before publishing the
image. This closes the mutable-worktree build race; it does not authenticate
the compiler, linker, firmware tools, host, or resulting artifact.

Create an exact machine profile. `serial_identity_sha256` is the SHA-256 of a
stable inventory identity retained by the operator; do not publish the raw
serial if it is sensitive. The profile has no optional or additional keys:

```json
{
  "schema": "ostadix.physical-machine-profile/v1",
  "architecture": "x86_64",
  "manufacturer": "REPLACE_WITH_MANUFACTURER",
  "model": "REPLACE_WITH_MODEL",
  "board": "REPLACE_WITH_BOARD",
  "cpu_model": "REPLACE_WITH_CPU_MODEL",
  "firmware": "REPLACE_WITH_VENDOR_VERSION_AND_RELEVANT_SETTINGS",
  "serial_identity_sha256": "REPLACE_WITH_64_LOWERCASE_HEX_DIGITS"
}
```

Save that completed record as `$MACHINE_JSON`; replace every `REPLACE_...`
value, including the digest placeholder, with real bounded inventory data.

### Mode 0 physical-attempt flow

This is the complete Mode 0 chain. The prepare and write records are generated
only after the challenged Mode 0 image exists, and every output remains outside
the repository:

```bash
MODE0_CHALLENGE="$(o kernel boot-challenge)"
MODE0_IMAGE="$EVIDENCE_DIR/mode0-$MODE0_CHALLENGE.img"
MODE0_PREPARE_JSON="$EVIDENCE_DIR/mode0-write-prepare.json"
MODE0_WRITE_JSON="$EVIDENCE_DIR/mode0-write-result.json"
MODE0_INTENT_JSON="$EVIDENCE_DIR/mode0-intent.json"
MODE0_TRANSCRIPT="$EVIDENCE_DIR/mode0.serial"
MODE0_OBSERVATION_JSON="$EVIDENCE_DIR/mode0-observation.json"

OCORE_MEDIA_PROBE_MODE=0 \
OSTADIX_BOOT_CHALLENGE="$MODE0_CHALLENGE" \
OSTADIX_MEDIA_ROOT="$EVIDENCE_DIR/mode0-media-build" \
OCORE_MEDIA_KERNEL_BUILD_DIR="$EVIDENCE_DIR/mode0-kernel-build" \
  o kernel media "$MODE0_IMAGE"
o kernel inspect-media "$MODE0_IMAGE"

o kernel prepare-write --image "$MODE0_IMAGE" --device "$DEVICE" \
  | tee "$MODE0_PREPARE_JSON"
MODE0_TOKEN="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["confirmation"])' "$MODE0_PREPARE_JSON")"

# Destructive: recheck DEVICE before running this command.
o kernel write-media --image "$MODE0_IMAGE" --device "$DEVICE" \
  --confirm "$MODE0_TOKEN" | tee "$MODE0_WRITE_JSON"

o kernel prepare-physical \
  --image "$MODE0_IMAGE" \
  --media-write "$MODE0_WRITE_JSON" \
  --machine "$MACHINE_JSON" \
  --profile mode0 \
  --expected-cpus 1 \
  --output "$MODE0_INTENT_JSON"
```

`prepare-physical` emits `ostadix.physical-boot-intent/v1` with
`authority:"none"`. It refuses to replace an existing output. The only
accepted transcript contracts are `--profile mode0 --expected-cpus 1` and
`--profile smp4 --expected-cpus 4`; omitting `--profile` infers it from that
exact count. The `smp4` grammar is shared with the QEMU gate, but selecting it
does not promote an operator transcript into independent physical evidence.

### Mode 34 physical-attempt flow

Mode 34 is a separate complete chain with its own challenge, image, fresh
prepare record, completed write record, and intent. Never reuse the Mode 0
writer record after rebuilding different bytes:

```bash
MODE34_CHALLENGE="$(o kernel boot-challenge)"
MODE34_IMAGE="$EVIDENCE_DIR/smp4-$MODE34_CHALLENGE.img"
MODE34_PREPARE_JSON="$EVIDENCE_DIR/smp4-write-prepare.json"
MODE34_WRITE_JSON="$EVIDENCE_DIR/smp4-write-result.json"
MODE34_INTENT_JSON="$EVIDENCE_DIR/smp4-intent.json"
MODE34_TRANSCRIPT="$EVIDENCE_DIR/smp4.serial"
MODE34_OBSERVATION_JSON="$EVIDENCE_DIR/smp4-observation.json"

OCORE_MEDIA_PROBE_MODE=34 \
OSTADIX_BOOT_CHALLENGE="$MODE34_CHALLENGE" \
OSTADIX_MEDIA_ROOT="$EVIDENCE_DIR/smp4-media-build" \
OCORE_MEDIA_KERNEL_BUILD_DIR="$EVIDENCE_DIR/smp4-kernel-build" \
  o kernel media "$MODE34_IMAGE"
o kernel inspect-media "$MODE34_IMAGE"

o kernel prepare-write --image "$MODE34_IMAGE" --device "$DEVICE" \
  | tee "$MODE34_PREPARE_JSON"
MODE34_TOKEN="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["confirmation"])' "$MODE34_PREPARE_JSON")"

# Destructive: recheck DEVICE before running this command.
o kernel write-media --image "$MODE34_IMAGE" --device "$DEVICE" \
  --confirm "$MODE34_TOKEN" | tee "$MODE34_WRITE_JSON"

o kernel prepare-physical \
  --image "$MODE34_IMAGE" \
  --media-write "$MODE34_WRITE_JSON" \
  --machine "$MACHINE_JSON" \
  --profile smp4 \
  --expected-cpus 4 \
  --output "$MODE34_INTENT_JSON"
```

This remains an operator workflow, not a physical qualification claim. No
physical observation is bundled or claimed by this release.

Then perform the observation:

1. Wait for the verified write to finish before removing or ejecting the
   device.
2. Connect the first serial port at **38400 baud, 8 data bits, no parity, 1
   stop bit**. GRUB and the kernel both use serial output.
3. Enter x86_64 UEFI firmware and select the external device. The fallback file
   is `EFI/BOOT/BOOTX64.EFI`. If firmware enforcement rejects the unsigned
   image, use only an explicitly authorized development setting.
4. Capture the complete ASCII serial transcript. The challenged kernel must
   emit exactly one `OSTADIX boot challenge: <challenge>` line, exactly one
   `OSTADIX source commit: <commit>` line, and every marker in the selected
   profile in causal order, with no BootInfo or SMP rejection marker. The
   `smp4` profile additionally requires four ordered, unique APIC and aligned
   stack identity lines between stack admission and barrier completion.
5. Preserve the media, machine inventory, firmware settings, cold/warm boot
   context, timestamps, and raw transcript under operator custody.

Finally, save the complete serial capture to the matching external transcript
path and make the deliberate operator assertion. Use exactly one of these
profile-matched commands:

```bash
# Mode 0
o kernel record-physical \
  --intent "$MODE0_INTENT_JSON" \
  --transcript "$MODE0_TRANSCRIPT" \
  --image "$MODE0_IMAGE" \
  --assert-physical I-OBSERVED-OSTADIX-ON-PHYSICAL-X86_64 \
  --output "$MODE0_OBSERVATION_JSON"

# Mode 34
o kernel record-physical \
  --intent "$MODE34_INTENT_JSON" \
  --transcript "$MODE34_TRANSCRIPT" \
  --image "$MODE34_IMAGE" \
  --assert-physical I-OBSERVED-OSTADIX-ON-PHYSICAL-X86_64 \
  --output "$MODE34_OBSERVATION_JSON"
```

The literal `--assert-physical` phrase prevents accidental record creation; it
does not authenticate the operator or substrate. The resulting
`ostadix.physical-boot-observation/v1` record has `authority:"none"` and
`admission:"not-performed"`. Its canonical `record_sha256` is an unkeyed
integrity seal. The challenge has no trusted clock or one-shot registry, so
copied images, transcripts, intents, and observations can be replayed. The
machine profile, writer output, assertion, and transcript are all
self-reported; together they still do not independently prove physical
execution, trusted firmware, a trusted source build, or a completed write.

## Evidence boundary

| Result | What it establishes | What it does not establish |
|---|---|---|
| `o kernel media` succeeds | A mode-0 x86_64 kernel was packed into a bounded GPT/ESP image and the resulting container passed strict structural inspection. | A second reproducible build, any boot, physical hardware, SMP, or authenticity. |
| `o kernel inspect-media` succeeds | The supplied bytes conform to `ostadix.boot-media/v1` and produce the reported hashes and GUIDs. | That the image was produced by a trusted party, that its EFI/kernel payloads execute, or that it is safe to write to an arbitrary device. |
| `o kernel smoke-media` succeeds | Two local rebuilds are byte-identical and one exact disk boots under x86_64 OVMF/QEMU TCG far enough to emit the five required kernel/CPL3/timer markers and remain alive to timeout. | Physical boot, KVM, SMP, secure/measured boot, hardware driver support, PCI/DMA/IOMMU isolation, or a release qualification gate. |
| `o kernel smoke-boot-info` succeeds | A bounded challenged Multiboot2 handoff derives the page allocator's admitted subwindow, closes its temporary mapping before W^X, and a challenged mode-0 boot reaches CPL3/timer/heartbeat while the shared transcript parser rejects a wrong challenge. | Physical execution, general firmware/ACPI support, initrd loading, KVM, secure/measured boot, or hardware trust. |
| `o kernel smoke-smp` succeeds | One challenged four-vCPU QEMU/TCG image performs bounded ACPI/MADT admission, PIT-timed x2APIC INIT/SIPI, unique-stack AP entry, trampoline retirement, and one atomic barrier; the same image rejects under one vCPU. | Physical SMP, KVM, arbitrary topologies, a general SMP scheduler, interrupt balancing, per-CPU allocation, or SMP safety of other kernel subsystems. |
| `prepare-write` succeeds | One privately snapshotted and validated source image, one stable external/removable device identity and capacity, and one canonical sparse target plan are bound by `target_plan_sha256` and a confirmation token. | A device write, the contents of `unwritten_ranges`, or a boot. |
| `write-media` returns `written:true` | Every admitted v2 extent was written and individually read-back verified through one held target descriptor after identity and capacity rechecks. | Erasure, authentication, or verification of unwritten ranges; firmware acceptance; physical execution; or SMP. |
| `prepare-physical` succeeds | A clean source commit, challenged image, successful writer-v2 record and target plan, declared x86_64 machine profile, expected CPU count, and marker set were sealed into an authority-free intent. | A boot, trusted build, machine authenticity, freshness, or admission. |
| `record-physical` succeeds | The current challenged image and target plan still match the intent, the operator supplied the exact assertion, and one bounded ASCII transcript satisfies the selected causal `mode0` or `smp4` grammar without a rejection marker. | Independent proof of physical execution, authenticated hardware or operator identity, replay prevention, physical SMP qualification, release-gate credit, or authority. |

Additional current nonclaims:

- No AArch64, Apple Silicon, BIOS/CSM, ISO, PXE, or network-boot image is
  implemented by this path.
- The default mode-0 media path remains single-CPU. Mode 34 is a bounded
  four-vCPU QEMU proof; the observation schema cannot turn it into physical SMP
  evidence.
- The generated EFI executable and kernel have no implemented signing,
  measured-boot, or TPM evidence chain.
- The image is not an installer and does not authorize writing an internal
  disk.
- It does not boot Linux, Plan 9, or another foreign kernel.
- QEMU/TCG observation cannot be promoted into physical-hardware evidence.
- Physical intent and observation records are unkeyed, authority-free,
  operator-asserted records. They are neither attestations nor authenticators.

## Recovery and troubleshooting

### Missing build tools

Run:

```bash
./setup.sh --with-ocore-media --check
```

The builder also permits explicit tool paths through
`OSTADIX_GRUB_MKSTANDALONE`, `OSTADIX_MFORMAT`, `OSTADIX_MCOPY`, and
`OSTADIX_PYTHON`. Use them only for the exact executables you inspected.

### Missing OVMF firmware

Set the code image and retry:

```bash
OSTADIX_OVMF_CODE=/absolute/path/to/OVMF_CODE.fd o kernel boot-media
OSTADIX_OVMF_CODE=/absolute/path/to/OVMF_CODE.fd o kernel smoke-media
```

### Inspection or determinism failure

Do not write the image. Remove only the known generated output, rebuild it with
`o kernel media`, and inspect it again. A malformed GPT, CRC mismatch, geometry
change, digest/GUID mismatch, or non-identical smoke rebuild is a hard failure,
not a warning to bypass.

### QEMU boot failure

Use `Ctrl-A X` to leave an interactive boot. For the smoke, require both PASS
lines; a timeout without every required marker is a failure. Retain QEMU stdout
and stderr, confirm the exact OVMF path, rebuild, and inspect before retrying.

### Prepare or confirmation failure

Do not reuse the old token. Recheck the image, whole-device path, device
stable identity, capacity, removable/USB classification, mount state, and
root-disk relationship, then run `prepare-write` again. Hot-plug replacement
and image changes are intentionally expected to invalidate confirmation. A
target smaller than the canonical image, above 16 TiB, not 512-byte aligned, or
lacking the required stable device identity is rejected rather than overridden.

### Write or read-back failure

Treat the target media as incomplete and do not boot it. The writer has no
rollback. Unwritten ranges may still contain recoverable prior bytes, but that
does not restore the old GPT or make the medium safe to use. Reinspect the
source image, replace suspect removable media if necessary, obtain a new
confirmation token, and repeat the complete planned-extent write and read-back
process.

### No physical serial output

Return to the QEMU smoke first. If it passes, verify x86_64 UEFI mode, external
media selection, firmware policy for unsigned EFI executables, and the first
16550-compatible serial port at COM1 base `0x3f8`. Use 38400 8N1 for both GRUB
and kernel entry. Preserve the failed attempt as an observation; do not report
a physical pass.

To recover the test machine, power it down, remove the external OSTADIX Alpha
media, and restore its prior firmware boot order. Never troubleshoot by writing
the host's internal system disk.
