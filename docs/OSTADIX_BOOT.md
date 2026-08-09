# OSTADIX Alpha x86_64 UEFI boot media

This guide covers the implemented OSTADIX Alpha disk-image path for the
freestanding O-core kernel. The current path is **x86_64 UEFI only**. It builds
one deterministic GPT disk image, validates its bounded layout, boots that
exact disk through OVMF under QEMU/TCG, and can write the validated bytes to a
confirmation-bound, exact-capacity external device on macOS or Linux.

The current automated evidence is virtual. Building an image, passing the
OVMF/QEMU smoke, or writing the bytes to removable media does **not** establish
that a physical machine booted OSTADIX Alpha. The current kernel path is also
single-CPU: none of these commands establishes SMP. Physical boot and SMP must
remain unpassed until they are separately observed and admitted as qualifying
evidence.

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

That implementation is currently a hosted conformance foundation. The
freestanding `boot.S` path does not yet hand its normalized result to the
O-core allocator: the running kernel still uses its documented fixed bootstrap
memory window. Therefore the media is not yet a general firmware-discovered
memory, arbitrary-platform, or SMP boot path. Connecting equivalent bounded
normalization inside the freestanding kernel is a prerequisite for promoting
physical portability claims.

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
- `mformat` and `mcopy`; and
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

## Prepare and write removable media

**Writing is destructive. It overwrites the beginning and partition table of
the selected whole device. There is no automatic backup or rollback. Keep the
image on a different disk, verify the device identity yourself, and never use
an internal or active system disk.**

The public workflow is intentionally two-step. `prepare-write` does not write
the device. It captures and validates one private image snapshot, probes one
exact whole device, requires the reported device capacity to equal the complete
image byte count, and returns JSON containing the device identity, image
identity, and a confirmation token.

The exact-capacity rule is a deliberate bounded-v1 restriction. The GPT backup
header is at the image's final LBA; copying that image to a larger device would
leave the backup header before the physical device's final LBA. This writer
does not yet repack the GPT for a target device and therefore rejects both
smaller and larger targets. Most ordinary USB media will not exactly match the
generated image and will be rejected. Do not pad the image or bypass the
inspector: padded bytes violate the admitted `ostadix.boot-media/v1` geometry.

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

Review every returned field. Copy the exact `confirmation` value from that
output; it has the form `OSTADIX-WRITE-` followed by 16 uppercase hexadecimal
characters. Then run:

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
rejects the whole disk containing the active root filesystem. Immediately
before writing, it repeats image/device validation, unmounts the external disk,
and writes through its raw `/dev/rdiskN` path.

### Linux guards

The writer obtains inventory from `lsblk` and the active root source from
`findmnt`. It requires one exact writable whole-disk record, rejects any
mountpoint on the target or any direct or nested descendant, and rejects the
detected root disk. It additionally requires `lsblk` to report either `RM=true`
or USB transport. Internal NVMe, internal SATA, and unknown non-removable
transports fail closed. There is no dangerous-device override in writer v1.

### Confirmation and post-write verification

The token is bound to:

- operating system;
- canonical device path and operating-system-reported device identity;
- device byte capacity;
- image SHA-256; and
- image byte count.

`write-media` validates the image and device again before mutation. A changed
path, reported identity, capacity, image, or stale token fails closed. A
replacement device that the operating system reports with exactly the same
path, identity, and capacity cannot be distinguished by this v1 interface, so
the operator must still keep physical custody between prepare and write.

For the destructive invocation, the writer opens the source without following
symbolic links, copies exactly its admitted length into a private temporary
file, validates that snapshot, and retains one read-only descriptor through
the device write. It rejects source growth, truncation, replacement, trailing
bytes, or content/identity change before opening the raw target. The original
source pathname is never reopened as the source of device bytes. Ensure the
host's temporary filesystem has free space for one complete image snapshot.

After copying exactly the admitted bytes, the writer calls `fsync`, reads back
the exact target prefix, and requires its SHA-256 to equal the held snapshot.
Because writer v1 also requires exact device/image capacity, that prefix is the
complete reported target. Success emits schema `ostadix.media-write/v1` with
`written:true`.

If opening the device is denied, re-run the exact already-reviewed command only
with the minimum local privilege appropriate for that host. Do not weaken the
confirmation step or change the target while adding privilege.

## Physical serial-console checklist

This is an observation procedure, not a claim that physical boot has passed.
Use a non-production x86_64 test machine whose firmware can boot an unsigned
UEFI removable-media fallback executable and which exposes a 16550-compatible
COM1 serial port at I/O base `0x3f8`.

1. Run `o kernel smoke-media` first to separate image/firmware-path failures
   from hardware-specific failures.
2. Run `o kernel inspect-media "$IMAGE"` and retain its complete JSON output.
3. Run the two-step `prepare-write` and `write-media` sequence. Retain both JSON
   records, especially the image SHA-256, device identity, token, and
   `written:true` result.
4. Wait for the verified write command to finish before removing or ejecting
   the device.
5. Connect the machine's first serial port. Start the terminal at **38400
   baud, 8 data bits, no parity, 1 stop bit**. GRUB and the kernel use the same
   serial rate; GRUB uses serial only, so there is no graphical menu.
6. Enter x86_64 UEFI firmware and select the external device. The expected
   fallback file is `EFI/BOOT/BOOTX64.EFI`. If firmware enforcement rejects the
   unsigned image, use only an explicitly authorized development setting; the
   current builder does not sign it.
7. When GRUB transfers control, keep the terminal at **38400 8N1**. The current
   kernel reinitializes the first 16550-compatible serial port at COM1 base
   `0x3f8` with divisor 3. Capture the complete transcript and look for the
   same five markers required by the QEMU smoke.
8. Record the machine model/serial or inventory identity, CPU model, firmware
   vendor/version/settings, cold or warm boot type, media/device identity,
   image and ESP digests, timestamp, and full transcript.
9. Keep the observation labeled **physical x86_64, single CPU, provisional**
   until the repository's physical evidence admission path explicitly accepts
   it. A transcript or successful manual boot does not by itself pass an
   OSTADIX Alpha qualification gate.

## Evidence boundary

| Result | What it establishes | What it does not establish |
|---|---|---|
| `o kernel media` succeeds | A mode-0 x86_64 kernel was packed into a bounded GPT/ESP image and the resulting container passed strict structural inspection. | A second reproducible build, any boot, physical hardware, SMP, or authenticity. |
| `o kernel inspect-media` succeeds | The supplied bytes conform to `ostadix.boot-media/v1` and produce the reported hashes and GUIDs. | That the image was produced by a trusted party, that its EFI/kernel payloads execute, or that it is safe to write to an arbitrary device. |
| `o kernel smoke-media` succeeds | Two local rebuilds are byte-identical and one exact disk boots under x86_64 OVMF/QEMU TCG far enough to emit the five required kernel/CPL3/timer markers and remain alive to timeout. | Physical boot, KVM, SMP, secure/measured boot, hardware driver support, PCI/DMA/IOMMU isolation, or a release qualification gate. |
| `prepare-write` succeeds | One privately snapshotted and validated image exactly matches one external/removable whole device's reported capacity, and a token is bound to both identities. | A device write or a boot. |
| `write-media` returns `written:true` | Exactly the admitted snapshot bytes were copied to the re-probed exact-capacity device and its exact prefix was read back with the source SHA-256. | Firmware acceptance, physical execution, SMP, or preservation of previous device contents. |
| A manual serial transcript is captured | A provisional observation tied to the recorded machine, firmware, media, and transcript. | Automatic evidence admission, other hardware, multicore correctness, or general physical support. |

Additional current nonclaims:

- No AArch64, Apple Silicon, BIOS/CSM, ISO, PXE, or network-boot image is
  implemented by this path.
- The kernel path is single-CPU and contains no SMP qualification.
- The generated EFI executable and kernel have no implemented signing,
  measured-boot, or TPM evidence chain.
- The image is not an installer and does not authorize writing an internal
  disk.
- It does not boot Linux, Plan 9, or another foreign kernel.
- QEMU/TCG observation cannot be promoted into physical-hardware evidence.

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
identity, exact capacity, removable/USB classification, mount state, and
root-disk relationship, then run `prepare-write` again. Hot-plug replacement
and image changes are intentionally expected to invalidate confirmation. A
larger target is not an error to override: target-capacity GPT repacking is not
implemented in bounded writer v1.

### Write or read-back failure

Treat the target media as incomplete and do not boot it. The writer has no
rollback. Reinspect the source image, replace suspect removable media if
necessary, obtain a new confirmation token, and repeat the complete write and
read-back process.

### No physical serial output

Return to the QEMU smoke first. If it passes, verify x86_64 UEFI mode, external
media selection, firmware policy for unsigned EFI executables, and the first
16550-compatible serial port at COM1 base `0x3f8`. Use 38400 8N1 for both GRUB
and kernel entry. Preserve the failed attempt as an observation; do not report
a physical pass.

To recover the test machine, power it down, remove the external OSTADIX Alpha
media, and restore its prior firmware boot order. Never troubleshoot by writing
the host's internal system disk.
