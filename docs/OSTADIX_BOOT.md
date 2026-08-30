# OSTADIX Alpha x86_64 UEFI boot media

This guide covers the implemented OSTADIX Alpha boot containers. Every path is
**x86_64 UEFI only**:

- a deterministic raw GPT disk image with one EFI System Partition for the
  freestanding O-core kernel;
- a deterministic ISO9660 image with an El Torito UEFI no-emulation boot
  image for the freestanding O-core kernel; and
- a staged-tree-addressed combined OSTADIX ISO with Hosted Linux as its
  default, direct O-core and Alpine entries, and explicit nested QEMU/TCG
  entries for Guix, OpenBSD, 9front, and Redox.

Each path validates its own bounded structure. The raw O-core and ordinary ISO
paths boot their admitted artifact through OVMF/QEMU TCG. The combined ISO
strictly admits all 14 artifacts, while its current automated gates execute
Hosted and O-core only. Only the raw GPT image participates in
the capacity-bound external-device writer on macOS or Linux. For a larger raw
target, the planner relocates the backup GPT to the target's final LBA without
enlarging or moving the admitted ESP. Both ISO forms remain read-only optical
artifacts and are not accepted by the raw removable-media writer. The combined
ISO uses the separate guarded Ventoy file-copy workflow.

The current automated evidence is virtual. Building an image, passing an
OVMF/QEMU smoke, or writing/copying bytes to removable media does **not**
establish that those exact bytes booted a physical machine. Mode 34 establishes
one bounded four-vCPU SMP bring-up under QEMU/TCG; it does not make the general
kernel SMP safe and does not establish physical SMP. One earlier CLI-only
Hosted Live artifact has an operator-observed physical boot, but that result
does not transfer to the newer workstation bytes.

## Implemented raw disk image

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

`crates/ostadix-api/src/ocore/boot_info.rs` defines the architecture-neutral `BootInfoV1`
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

The media setup profile includes the O-core profile and adds x86_64 EFI GRUB
standalone and rescue-image builders, mtools, xorriso, and OVMF/edk2 firmware:

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
- `x86_64-elf-grub-mkrescue` or `grub-mkrescue`, plus its x86_64-efi platform
  directory;
- `mformat` and `mcopy`;
- `xorriso`;
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

## Boot the exact disk and ISO under OVMF/QEMU

### Raw GPT disk

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

### Build, inspect, and boot the UEFI ISO

The ISO builder packages the mode-0 x86_64 kernel at `/boot/kernel.elf`, the
exact committed ISO-specific GRUB configuration at `/boot/grub/grub.cfg`, and
an embedded FAT EFI image containing `/EFI/BOOT/BOOTX64.EFI`. Unlike the raw
disk configuration, the ISO configuration performs no FAT UUID search; GRUB
loads the kernel directly from the mounted ISO filesystem.

Build the default ISO:

```bash
o kernel iso
```

The default output is:

```text
target/ostadix-iso/x86_64/ostadix-x86_64-uefi.iso
```

Or build and inspect one explicit output path:

```bash
ISO="$O_LANG_ROOT/target/ostadix-iso/x86_64/ostadix-alpha.iso"
o kernel iso "$ISO"
o kernel inspect-iso "$ISO"
```

The builder uses `grub-mkrescue` with an explicit x86_64-efi GRUB platform
directory and a repository-owned xorriso canonicalization wrapper. It builds a
private candidate, strictly inspects it, and only then atomically publishes the
requested regular-file output. Source, build-directory, and output symlinks
are rejected rather than followed. As with the raw disk, deterministic rebuild
claims are limited to the same source, toolchain, inputs, and relevant
environment; they are not a cross-version GRUB or xorriso guarantee.

The wrapper replaces GRUB's wall-clock token exactly once in `efi.img`, the
private `efi/boot/bootx64.efi`, and exactly one admitted auxiliary EFI layout:
`boot.efi` or `System/Library/CoreServices/boot.efi`. It rejects both or neither
layout and never scans or rewrites `/boot/kernel.elf` or arbitrary rescue-tree
payloads.

`inspect-iso` emits canonical JSON with schema `ostadix.boot-iso/v1`. The
strict inspector independently validates the ISO9660 volume, El Torito catalog
checksum, exactly one UEFI platform `0xef` entry using no-emulation media, the
embedded FAT EFI image, and `/EFI/BOOT/BOOTX64.EFI`. The EFI executable must
have a PE signature, x86_64 COFF machine `0x8664`, PE32+ optional-header magic
`0x20b`, EFI application subsystem `10`, and a nonzero entry point in a
file-backed executable section. It also requires an executable x86_64 ELF at
`/boot/kernel.elf`, at least one valid `PT_LOAD` segment, a file-backed
executable entry point, and exactly one valid Multiboot2 header in the admitted
header window. The exact committed ISO GRUB configuration is required, and the
inspector reports hashes for the complete ISO, EFI boot image, EFI bootloader,
kernel, and GRUB configuration. Structural admission and hashing do not
authenticate the producer or establish a signing, Secure Boot, or measured
boot chain.

Interactive boot rebuilds and strictly inspects the selected ISO, then attaches
those exact bytes as a read-only IDE CD-ROM behind read-only OVMF firmware:

```bash
o kernel boot-iso

# Select a non-default output; boot-iso accepts no positional arguments.
OSTADIX_ISO_IMAGE="$ISO" o kernel boot-iso
```

Exit the multiplexed serial monitor with `Ctrl-A X`. The VM uses QEMU TCG,
`q35`, 128 MiB RAM, firmware boot order `d`, `-nodefaults`, `-nic none`, and no
QEMU `-kernel` shortcut.

Run the automated ISO gate with:

```bash
o kernel smoke-iso
```

The gate builds two complete ISOs in separate kernel and container build
directories, requires full byte identity, strictly inspects both, and requires
the exact first ISO to be published with all write-permission bits cleared. It
opens that ISO once without following links, hashes and validates the held
descriptor, and gives QEMU an inherited descriptor for the same bytes as
read-only CD media. It likewise descriptor-pins OVMF firmware as read-only
media. The streamed validator requires the ordered kernel, W^X, CPL3, timer, and
heartbeat markers, rejects fatal output, and then requires continued process
liveness after the heartbeat for
`min(1 second, timeout / 4)`. The configured timeout is the total deadline: the
ordered markers and the complete post-heartbeat window must both finish before
it. A positive deadline override must be supplied as:

```bash
OSTADIX_ISO_TIMEOUT_SECONDS=30 o kernel smoke-iso
```

This is deterministic-container and firmware-mediated boot evidence for one
exact ISO under QEMU q35/OVMF/TCG. It is not physical-machine, KVM, SMP,
Secure Boot, measured-boot, PCI, DMA, IOMMU, hardware-driver, installer,
external-media-write, or release-qualification evidence.

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

## Combined staged-tree OSTADIX ISO

The Hosted Workstation release is a separate staged-index product. Its default
name is:

```text
target/ostadix-hosted-live/x86_64/ostadix-hosted-live-x86_64-uefi-<tree12>_VTGRUB2.iso
```

`<tree12>` identifies the first 12 hexadecimal characters of the exact staged
Git tree. `_VTGRUB2.iso` selects Ventoy's GRUB2 handling path. The release is
built in the Linux Multipass guest named `moral-gaur`, under a run-owned path,
rather than compiling in the mounted macOS checkout.

Review and stage only the intended source changes, then build and inspect the
tree-addressed result:

```bash
git status --short
git add -- <reviewed-source-paths>
o kernel hosted-live-release

HOSTED_LIVE_ISO='<exact hosted-live-output path>'
o kernel smoke-hosted-live "$HOSTED_LIVE_ISO"
```

Staging is neither committing nor pushing. The release orchestrator snapshots
the index with `git write-tree`, rejects source drift, and uses that snapshot as
the common identity for three different roles:

| Role | Boot representation | Authority boundary |
|---|---|---|
| Inspectable source | Complete staged tree at `/usr/src/ostadix` | Excludes `.git`, `target/`, caches, and untracked files; boot self-bind-remounts this tree read-only and proves a write fails. |
| First-class objects | Read-only CAS at `/usr/share/ostadix/boot-objects/v1` | `o object root/list/stat/get/verify` inspect or materialize verified bytes; boot remounts the tree read-only and v1 does not execute objects. |
| Runnable products | Admitted x86_64 executables at `/usr/local/bin` | Built separately from the same tree and individually receipt-bound by size and SHA-256. |
| Derived O artifact | `/usr/share/ostadix/wasm/hello.wasm` plus `hello.release.json` | The descriptor binds the staged tree, O input, installed `olangc`, generated Cargo-project closure, fixed offline build profile, and module identity. |

The 14 declared root products are `O`, `o-cli`, `olangc`, `ocorec`, `o-link`,
`o-unlink`, `o-notebook`, `ogit`, `o-live-host`, `o-node`, `octl`,
`o-registry`, `o-info`, and `ocore-kernel-world-record`. `ostadix-mcp` is a
fifteenth executable built from its separately locked MCP crate. The hosted
workstation also contains `apk`, Rust, Cargo, rustfmt, Clippy, `rust-wasm`, the
`wasm32-wasip1` standard library and WASI libc, Git, OpenSSL, Firefox ESR,
xdg-open, and the C/C++ build tools. The zero-argument
`o-notebook` honors the installed `O_BACKENDS_DIR`; the boot gate evaluates a
Python-backed cell, and the graphical gate requires a real Firefox notebook
window before declaring the Openbox desktop ready. A fresh isolated
`o node start` must provision its OpenSSL PKI, publish listener readiness,
report running status, and stop cleanly. The cross-architecture gate selects
the explicit `--fresh-pki-key-algorithm ec-p256` profile, which avoids RSA
prime-generation latency under TCG while retaining comparable classical
security. Ordinary `o node start` remains RSA-3072 by default.

The cross-architecture release harness bounds that one-time fresh provisioning
to 900 seconds while preserving the node CLI's separate 30-second
post-provisioning listener-readiness deadline. The full Hosted serial and
graphical gates each remain fail-closed under a 1,800-second deadline; direct
O-core keeps its independent 900-second maximum. The Hosted deadline can be
lowered, but not raised beyond that ceiling, with
`OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT`. Direct O-core uses the separate
`OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT` override. The host release
orchestrator validates finite values and forwards both assignments explicitly
through the Multipass boundary; ambient guest values cannot silently select the
release policy.

Before those products are built, the worker runs `cargo vendor --locked
--versioned-dirs` for the root manifest and synchronizes
`mcp/ostadix_lang_mcp_server/Cargo.toml`. Its canonical
`ostadix.cargo-vendor-manifest/v1` binds both staged `Cargo.lock` files and a
sorted `{path,bytes,sha256}` record for every vendored file. The currently
observed closure has 285 package directories, 17,593 files, and 376,759,069
payload bytes. Root, MCP, and O-core release builds use that source replacement
with Cargo network access forced offline.

The verified vendor tree is available in the boot at
`/usr/share/ostadix/cargo/vendor`; its manifest is
`/usr/share/ostadix/cargo/cargo-vendor-manifest.json`. The installed
`/root/.cargo/config.toml` replaces crates.io with that absolute directory and
sets Cargo offline. These counts bind the current pair of lockfiles rather than
establishing a permanent count for future releases.

The strict ISO closure has 14 artifacts: the Hosted LTS kernel, bootstrap
initramfs, digest-bound `/boot/hosted/rootfs.squashfs`, and exact
`/boot/modloop-lts`; the direct O-core kernel; the shared capacity-host kernel
and initramfs; the direct Alpine initramfs; the Guix kernel, initramfs, and ISO;
the OpenBSD ISO; the 9front qcow2; and the Redox ISO. The hosted entry pins both
`rootfs_path` and `modloop_path`; GRUB passes
them as `ostadix.rootfs=` and `modloop=` kernel arguments. Neither artifact is
on GRUB's `initrd` line, which contains only the bootstrap cpio. Stage one
discovers media with the `OSTADIX_CAPACITY` volume label, checks the independently
embedded SquashFS byte count and SHA-256, attaches it read-only through loop,
mounts it as SquashFS, constructs a volatile tmpfs overlay, moves the retained
mounts under the new root, and invokes `switch_root`. It cannot enter the hosted
gate script before those steps succeed. The package database, Rust/Cargo
toolchain, Firefox/Openbox GUI, complete staged source, offline Cargo vendor
closure, boot-object CAS, and installed Ostadix executables are all first-class
contents of that verified root.

For the Ventoy 1.1.17 Alpine hook contract, bootstrap init exposes the exact
`ebegin 'Mounting boot media'` / `eend 0` insertion marker. The separate minimal
gzip SquashFS modloop contains the pinned LTS `dm-mod.ko`, and stage one requires
`dm_mod` after the hook point. Media discovery is bounded to 30 one-second
attempts. It invokes BusyBox-compatible `blkid "$device"` and parses the full
output for an exact `LABEL="OSTADIX_CAPACITY"` token; it expressly avoids the
unsupported `blkid -s` and `blkid -o` forms.

The boot gate separately checks the exact Rust/Cargo package versions, runs
rustfmt and Clippy with warnings denied, and compiles and executes a
dependency-free Rust program with `cargo run --offline`. It then launches the
installed MCP server from `/workspace` and requires `o_olangc` to regenerate
the exact no-build `wasm32-wasip1` Cargo project. The project closure must match
the release descriptor, and the descriptor must bind the packaged
`/usr/share/ostadix/wasm/hello.wasm` to the staged source and installed
compiler. A separate live `rustc --target wasm32-wasip1` probe validates the
installed Rust/WASI target and the resulting core-WASM envelope. The gate then
executes the packaged Olangc module with Wasmtime and runs the installed
`webassembly^` route through `wasm-tools parse` and Wasmtime. The expensive
Olangc-generated WASI entrypoint retains OIR projection, evidence analysis, V6
admission, schedule validation, and executable-lease checks, then selects the
serial reference executor because WASI Preview 1 cannot create native worker
threads. Native generated binaries remain graph-first. The module is compiled
once during native release construction with Cargo offline, LTO disabled, and
16 codegen units; it is not cold-compiled under nested x86 TCG at every boot.
The MCP exchange also proves installed-root
discovery, bundled search execution, and search-path rejection. Alpine v3.24 currently ships these
commands from Rust 1.96.1 packages, while the staged
`rust-toolchain.toml` pins 1.97.1 for canonical repository development. The
complete pinned source and dual-lock vendor closure remain available, but the
in-image gate is not a second full root-plus-MCP rebuild and does not claim the
packaged compiler is byte-identical to the pinned 1.97.1 toolchain.

The object index binds staged-tree and base-commit identities, path modes, Git
blob SHA-1 values, raw SHA-256 values, logical and deduplicated byte counts, and
a domain-separated root. The convenient source view is checked against the same
bindings. See [OSTADIX_BOOT_OBJECTS.md](OSTADIX_BOOT_OBJECTS.md) for the exact
format and limits.

### Combined ISO boot entries and gates

The strict ISO profile has exactly seven entries:

1. Hosted Linux is first and default. Its small Alpine LTS bootstrap initramfs
   verifies and mounts the read-only SquashFS workstation root before launching
   the Openbox/Xterm desktop and local Firefox O-notebook. The resulting system
   exposes the staged source/object store, admitted Ostadix tools, Cargo, and
   `apk`.
2. O-core loads `/boot/ocore/kernel.elf` directly with GRUB Multiboot2. It is a
   freestanding, serial-only kernel. It does not inherit Linux, X11, Cargo,
   `apk`, or the hosted source filesystem.
3. Alpine Linux 3.24.1 loads its virt kernel and initramfs directly.
4. GNU Guix System 1.5.0 launches through the shared Alpine capacity host under
   nested QEMU/TCG with its Linux-libre kernel, initramfs, and ISO.
5. OpenBSD 7.9 launches its offline installer ISO through nested QEMU/TCG.
6. 9front build 11983 launches its qcow2 through nested QEMU/TCG.
7. Redox OS 0.9.0 launches its server livedisk through nested QEMU/TCG.

The release keeps their evidence independent. The hosted serial gate verifies
the digest-bound SquashFS/tmpfs-overlay handoff, read-only source/CAS mounts,
APK presence, O execution, Bash, SQLite, OIR generation,
CLI/link operations, Rust, Cargo, rustfmt, Clippy, an offline Cargo hello,
MCP-mediated exact Olangc project materialization, the source-bound packaged
Olangc WASM module, its execution under Wasmtime, the O `webassembly^` backend,
a live Rust/WASI compile, a fresh `o-node` lifecycle, a Python-backed O-notebook
cell, all declared binaries, the complete staged source view, the MCP
executable, and the boot object closure before `OSTADIX HOSTED LIVE READY`. A
second OVMF/QEMU TCG boot starts Openbox, Firefox,
O-notebook, and Xterm; requires the Firefox notebook window; rejects a black,
unchanged, or insufficiently chromatic framebuffer; injects USB-keyboard input;
and verifies the typed command over serial. A third gate selects the O-core
entry and requires its ordered serial liveness markers. Passing one route does
not stand in for another. These gates bind the whole ISO identity but do not
select direct Alpine or the four nested guest routes. Release receipt v6 and
boot-gates v6 require the exact 14-artifact closure. Serial result v4 and VGA
result v7 repeat the exact inspected ISO byte count and SHA-256; O-core remains
independently bound to those same bytes.
Serial obtains its identity from the same pinned descriptor passed to QEMU;
graphical and O-core hash their own held descriptors. A stale or mixed gate
record therefore cannot be adopted as evidence for another ISO.

The public `o kernel smoke-hosted-live` command is the corresponding aggregate
re-smoke. It copies the selected regular non-symlink ISO into one private,
read-only snapshot, then runs the serial, graphical, and direct O-core gates in
that order against those same bytes. It rejects a failed child, malformed or
unexpected result schema, changed snapshot, or any ISO/firmware identity
disagreement and emits one `ostadix.hosted-live-qemu-smoke-all/v2` result. Its
graphical monitor connect, command, response, keyboard, capture, quit, and
success-exit operations all consume one absolute Hosted deadline rather than
receiving a fresh relative socket timeout per operation.

These are QEMU gates, not physical or Secure Boot proof. The ISO is unsigned,
and its intended development path has Secure Boot disabled. The live root is a
read-only SquashFS with a volatile tmpfs overlay; the release does not claim an
installer, durable home directory, or durable package installation.

The hosted QEMU gates deliberately retain 4 GiB of guest RAM as a regression
bound for the split-root design. The large immutable filesystem stays on
SquashFS media and is paged on demand rather than expanded into the initial
rootfs; only writable overlay and build state consume tmpfs memory. This does
not establish a physical RAM minimum.

Guix, OpenBSD, 9front, and Redox are embedded as exact, typed guest artifacts
with explicit menu routes. The shared Alpine capacity host launches them under
nested QEMU/TCG; they are not direct GRUB kernels or personalities running
inside O-core. Current combined-release gates do not execute those menu routes
and therefore do not prove their package managers, GUIs, or Ventoy routing.

### Physical-artifact distinction

The earlier CLI-only artifact remains a distinct operator-observed physical
success:

```text
path:   target/ostadix-hosted-live/x86_64/ostadix-hosted-live-x86_64-uefi-12037d21a394_VTGRUB2.iso
bytes:  80306176
sha256: f25622d0e562ec5c95230653a1ab0e9edf65bb33b44750602b0d41237c44481b
scope:  physical boot observed; CLI-only payload
```

That observation does not prove a later, larger workstation artifact. The
workstation receipt remains `physical_hardware_proof:false` after its virtual
gates and until the exact copied bytes are separately observed on the target
machine.

The previously retained 3,227,592,704-byte seven-entry attempt with SHA-256
`247a51f296e8b07238df32870a44c2a582ff802daafab631be822ea0f4539c6e`
passed an older serial gate but reproduced a black VGA screen. It remains a
failure record, not boot evidence for the workstation or its individual menu
entries.

### Guarded Ventoy copy

The combined OSTADIX ISO uses a guarded file copy, not the raw GPT writer.
Resolve the currently connected external whole disk and mounted Ventoy data
volume immediately before preparing the operation:

```bash
VENTOY_DEVICE=/dev/diskN       # example only; replace with the live external disk
VENTOY_VOLUME=/Volumes/Ventoy
VENTOY_NAME=OSTADIX-Hosted-Workstation-x86_64-UEFI_VTGRUB2.iso

o kernel prepare-ventoy \
  --iso "$HOSTED_LIVE_ISO" --device "$VENTOY_DEVICE" \
  --volume "$VENTOY_VOLUME" --name "$VENTOY_NAME"

TOKEN='<exact prepare-ventoy token>'
o kernel install-ventoy \
  --iso "$HOSTED_LIVE_ISO" --device "$VENTOY_DEVICE" \
  --volume "$VENTOY_VOLUME" --name "$VENTOY_NAME" \
  --confirm "$TOKEN"

o kernel verify-ventoy \
  --iso "$HOSTED_LIVE_ISO" --device "$VENTOY_DEVICE" \
  --volume "$VENTOY_VOLUME" --name "$VENTOY_NAME"
```

Preparation is read-only. Installation re-identifies the same removable,
writable target, copies through a private `.part` file or a guarded ExFAT
fallback, synchronizes it, and verifies the complete ISO identity and structure.
It never overwrites a divergent destination. Re-resolve the device after every
disconnect; an old `/dev/diskN` value is not persistent device identity.
Successful copy and digest verification, hook-compatible markers, and the
minimal modloop do not prove that Ventoy or a physical firmware path can
rediscover the ISO and mount its SquashFS root. That boot substrate remains
unproven until the exact copied bytes are observed separately on hardware.

## Prepare and write raw O-core removable media

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
| `o kernel iso` succeeds | A mode-0 x86_64 kernel was packed into a strictly admitted ISO9660/El Torito UEFI no-emulation container and atomically published. | A second reproducible build, any boot, physical hardware, SMP, authenticity, or suitability for the raw-media writer. |
| `o kernel inspect-iso` succeeds | The supplied bytes conform to `ostadix.boot-iso/v1`, including the admitted UEFI PE32+ application, x86_64 kernel ELF, exact GRUB configuration, and reported component hashes. | That the ISO was produced by a trusted party, that its payloads execute, or that it is signed, measured, or authenticated. |
| `o kernel smoke-iso` succeeds | Two local ISO rebuilds are byte-identical and the descriptor-pinned first ISO boots as read-only CD media under x86_64 OVMF/QEMU TCG, emits the five required markers in order without fatal output, and remains live for the bounded post-heartbeat window without changing identity. | Physical boot, KVM, SMP, secure/measured boot, hardware driver support, PCI/DMA/IOMMU isolation, external-media writing, or a release qualification gate. |
| `o kernel hosted-live-release` succeeds | One exact staged Git tree was embedded as both `/usr/src/ostadix` and a verified read-only boot-object CAS; the declared x86_64 root/MCP executables and Hosted package closure were admitted with exact direct Hosted/O-core/Alpine and nested Guix/OpenBSD/9front/Redox entries, 14 typed artifacts, and pinned foreign-media verification. | A commit, push, physical boot, persistence, Secure Boot, execution of every source file or backend runtime, selection of the other five menu routes, guest GUI/package-manager execution, or Ventoy foreign-route behavior. |
| `o kernel smoke-hosted-live` succeeds | The exact combined ISO passes the Hosted serial/object/toolchain/node/notebook gate, Openbox/Firefox/Xterm framebuffer and USB-input gate, and separate direct-selection O-core serial gate under OVMF/QEMU TCG. | Physical hardware, Secure Boot, KVM/SVM, a persistent desktop, direct Alpine or nested guest execution, an O-core GUI or Linux userspace, the canonical Rust 1.97.1 toolchain, or guest package-manager/GUI execution. |
| `install-ventoy` and `verify-ventoy` succeed | The same currently identified external Ventoy target contains a synchronized, hash-identical, structurally inspected copy under the requested new basename. | Firmware acceptance, a boot, persistence, authenticity, or a safe basis for reusing the device path after disconnection. |
| `o kernel smoke-boot-info` succeeds | A bounded challenged Multiboot2 handoff derives the page allocator's admitted subwindow, closes its temporary mapping before W^X, and a challenged mode-0 boot reaches CPL3/timer/heartbeat while the shared transcript parser rejects a wrong challenge. | Physical execution, general firmware/ACPI support, initrd loading, KVM, secure/measured boot, or hardware trust. |
| `o kernel smoke-smp` succeeds | One challenged four-vCPU QEMU/TCG image performs bounded ACPI/MADT admission, PIT-timed x2APIC INIT/SIPI, unique-stack AP entry, trampoline retirement, and one atomic barrier; the same image rejects under one vCPU. | Physical SMP, KVM, arbitrary topologies, a general SMP scheduler, interrupt balancing, per-CPU allocation, or SMP safety of other kernel subsystems. |
| `prepare-write` succeeds | One privately snapshotted and validated source image, one stable external/removable device identity and capacity, and one canonical sparse target plan are bound by `target_plan_sha256` and a confirmation token. | A device write, the contents of `unwritten_ranges`, or a boot. |
| `write-media` returns `written:true` | Every admitted v2 extent was written and individually read-back verified through one held target descriptor after identity and capacity rechecks. | Erasure, authentication, or verification of unwritten ranges; firmware acceptance; physical execution; or SMP. |
| `prepare-physical` succeeds | A clean source commit, challenged image, successful writer-v2 record and target plan, declared x86_64 machine profile, expected CPU count, and marker set were sealed into an authority-free intent. | A boot, trusted build, machine authenticity, freshness, or admission. |
| `record-physical` succeeds | The current challenged image and target plan still match the intent, the operator supplied the exact assertion, and one bounded ASCII transcript satisfies the selected causal `mode0` or `smp4` grammar without a rejection marker. | Independent proof of physical execution, authenticated hardware or operator identity, replay prevention, physical SMP qualification, release-gate credit, or authority. |

Additional current nonclaims:

- No AArch64, Apple Silicon, BIOS/CSM, PXE, or network-boot image is
  implemented. The ISO path is x86_64 UEFI El Torito only.
- The default mode-0 media path remains single-CPU. Mode 34 is a bounded
  four-vCPU QEMU proof; the observation schema cannot turn it into physical SMP
  evidence.
- The generated EFI executable and kernel have no implemented signing,
  measured-boot, or TPM evidence chain.
- The image is not an installer and does not authorize writing an internal
  disk.
- The O-core-only raw disk and ISO do not boot Linux, Plan 9, or another
  foreign kernel. The combined ISO separately contains three direct and four
  nested QEMU/TCG routes; this does not turn any guest into an O-core
  personality.
- The workstation's complete staged-tree/object closure does not install every
  declared backend runtime. Foreign guest media bytes are included, but current
  combined-release gates do not execute or qualify their package managers or
  GUIs.
- The workstation is RAM-backed and has no claimed persistence or installer.
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
`OSTADIX_PYTHON`. The ISO builder additionally accepts
`OSTADIX_GRUB_MKRESCUE`, `OSTADIX_GRUB_EFI_DIRECTORY`, and
`OSTADIX_XORRISO`. Use them only for the exact executables or platform
directory you inspected.

### Missing OVMF firmware

Set the code image and retry:

```bash
OSTADIX_OVMF_CODE=/absolute/path/to/OVMF_CODE.fd o kernel boot-media
OSTADIX_OVMF_CODE=/absolute/path/to/OVMF_CODE.fd o kernel smoke-media
OSTADIX_OVMF_CODE=/absolute/path/to/OVMF_CODE.fd o kernel boot-iso
OSTADIX_OVMF_CODE=/absolute/path/to/OVMF_CODE.fd o kernel smoke-iso
```

### Inspection or determinism failure

Do not write the image. Remove only the known generated output, rebuild it with
`o kernel media`, and inspect it again. A malformed GPT, CRC mismatch, geometry
change, digest/GUID mismatch, or non-identical smoke rebuild is a hard failure,
not a warning to bypass.

For an ISO failure, remove only the known generated `.iso`, rebuild it with
`o kernel iso`, and inspect it again. An invalid ISO9660/El Torito/FAT/PE/ELF
structure, changed committed GRUB configuration, component-hash mismatch, or
non-identical smoke rebuild is likewise a hard failure.

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
