# Foreign-kernel QEMU lab

The foreign-kernel lab boots seven checksum-pinned, unmodified upstream
profiles: Alpine Linux 3.24.1 on both AArch64 and x86_64, FreeBSD
15.1-RELEASE on AArch64, and OpenBSD 7.9, 9front 11983, GNU Guix System 1.5.0,
and Redox OS 0.9.0 on x86_64. It is an opt-in host-side substrate test. It is
deliberately separate from O-core's portable evidence aggregate and from the
World G7 real-KernelWorld gate.

## What the lab establishes

For each run, the harness verifies every guest artifact before QEMU starts,
reopens admitted artifacts and firmware as descriptor-pinned inputs, forces
QEMU TCG with networking disabled, captures bounded serial output, and requires
exact ordered markers. The harness copies QEMU from its verified open descriptor
into a mode-0500 file inside a mode-0500 private launch directory and uses that
same snapshot for version inspection and boot. Linux executes the inherited
snapshot descriptor through `/proc/self/fd`; macOS, which cannot execute
`/dev/fd`, uses the protected snapshot path with inode checks before and after
launch. The observation JSON binds the manifest, artifacts, QEMU executable and
version, UEFI firmware, complete argv, raw and normalized transcript, QEMU
stderr, timeout result, and process-group cleanup action. Git checkouts bind the
commit and dirty state. Extracted source releases instead verify every declared
file and mode against canonical `SOURCE-MANIFEST.json` and `SHA256SUMS`, record
that provenance kind, and state that files outside the release manifest were
not audited.

The manifest-named QEMU executable discovered through the caller's host `PATH` and
any host UEFI firmware are explicit local trust anchors, not repository-signed
dependencies. A claim-admissible run requires a numeric QEMU version banner and
records the exact copied executable's digest, size, origin, banner, and
post-run identity. An explicit `run --qemu PATH` override remains useful for
diagnostics, but even successful mechanics are labeled `synthetic-passed` with
`claim_admissible: false`; an arbitrary override cannot mint a QEMU/TCG pass.

Both Alpine profiles reach the upstream initramfs `/bin/sh`, emit
architecture-specific readiness markers, answer `busybox uname -srm`, and
power down. The x86_64 profile therefore proves the exact x86_64 upstream
kernel and initramfs independently of the existing AArch64 evidence. FreeBSD
follows UEFI, its arm64 loader, the 15.1-RELEASE kernel, and rc startup into the
installer userland's terminal prompt.

OpenBSD boots the complete, unmodified `install79.iso` through its upstream
`OpenBSD/amd64 BOOTX64 3.71` loader. Its loader initially selects `pc0`, so the
manifest uses two counted actions on the same prompt: occurrence one sends
`set tty com0`, then occurrence two sends `boot` only after the loader has
reinitialized and emitted a second `boot>`. The proof requires the OpenBSD 7.9
`RAMDISK_CD` kernel, the `rd0` root, and the real installer question. The host
OVMF image is descriptor-pinned and recorded as an external local trust anchor;
it is not presented as an OpenBSD artifact.

9front boots its `pc64` Plan 9 kernel from the official read-only snapshot,
answers the root-device and user prompts as two ordered console actions, mounts
HJFS, starts rc, and reaches `term%`.

GNU Guix System is the Guile-based Linux target. The profile proves three
different layers instead of collapsing them into one claim: Linux-libre
6.17.12 is the kernel, GNU's early-boot Guile runs `/init`, and GNU Shepherd
1.0.9 runs with Guile 3.0.9. Guile defines and orchestrates the system; the
kernel itself is not implemented in Lisp. Because the upstream ISO's GRUB menu
selects graphical output, the deterministic serial profile extracts the exact
checksum-pinned kernel and initrd from that ISO, loads them directly, and keeps
the unmodified ISO attached read-only as the Guix store.

Redox boots from the expanded official livedisk, starts its kernel, mounts the
live RedoxFS, starts `ptyd`, and reaches `redox login:`. Its strict no-monitor
profile uses `-nodefaults` to avoid a hidden keyboard-injection control channel.
That intentionally excludes VGA, input, and networking, so their user-service
panics are recorded nonclaims rather than graphical or network readiness.

This proves those exact foreign kernels boot along the declared host-QEMU paths.
It does not prove that O-core admits, contains, supervises, or governs any of
them. It does not pass G7, export a KernelWorld service, exercise governed
withdrawal, or establish KVM, physical-hardware, PCI assignment, DMA, IOMMU,
Secure Boot, measured boot, or production-device evidence.

The runner gives QEMU a private working directory, private `HOME`/temporary
directories, a minimal fixed environment, no emulated NIC, and bounded admitted
devices. This is hygiene, not host containment: QEMU still runs with the
invoking VM user's uid and could access paths that uid can access. Use a
dedicated VM without unrelated sensitive mounts for adversarial-media work. On
`moral-gaur`, place `--output-dir` on native `/home/ubuntu` storage; do not treat
the SSHFS-mounted host checkout as a QEMU sandbox.

## Prepare the opt-in tools

The default setup still downloads no operating-system media. Automatic
`--with-guest-tools` installation is limited to the validated Debian/Ubuntu
package map and macOS/Homebrew. On Debian or Ubuntu, the guest profile installs
AArch64 and x86_64 emulators, AArch64 UEFI firmware, image tools,
`gzip`/XZ/Zstandard decompressors, and `xorriso` for checksum-verified Guix
ISO-member extraction:

```bash
./setup.sh --with-guest-tools --deps-only
```

The OpenBSD x86_64 profile additionally requires one complete OVMF image that
QEMU accepts through `-bios`. On Debian or Ubuntu, install `ovmf`; the proven
`moral-gaur` path is `/usr/share/ovmf/OVMF.fd`. Split code and variable-store
files such as `OVMF_CODE_4M.fd` are not interchangeable with that complete
image for this launch shape.

On Lee's macOS system, run the actual boots inside the existing Linux VM. The
`/home/ubuntu/Ostadix-host` path is the SSHFS-mounted host checkout, not a
disposable VM copy, so do not copy a repository over it or treat it as scratch.
Guest media stays in the VM's external data directory:

```bash
multipass exec moral-gaur -- bash -lc \
  'cd /home/ubuntu/Ostadix-host && python3 scripts/foreign_kernel_lab.py list'
```

`OSTADIX_GUESTS_DIR` selects the external cache. Its default is
`${XDG_DATA_HOME:-$HOME/.local/share}/ostadix/guests`. Large kernels, initramfs,
and media images never belong in Git.

No Cargo build is needed for this lab. The runner uses Python's standard
library plus the host's QEMU, extraction tools, and firmware; it neither
compiles nor links O-core. Python 3.14 can expand Zstandard frames directly;
older Python versions use the checked `zstd` executable. On other operating
systems or Linux distributions, install `qemu-system-aarch64`,
`qemu-system-x86_64`, `qemu-img`, `gzip`, XZ, Zstandard, `xorriso`, and AArch64
UEFI firmware manually, then run:

```bash
./setup.sh --with-guest-tools --check
```

Firmware discovery checks the manifest's known Debian/Ubuntu, edk2, and
Homebrew locations. If the runner's manifest candidates do not include the
installed path, pass it explicitly:

```bash
python3 scripts/foreign_kernel_lab.py run freebsd-15.1-release-aarch64 \
  --firmware aarch64_uefi=/absolute/path/to/QEMU_EFI.fd

python3 scripts/foreign_kernel_lab.py run openbsd-7.9-amd64 \
  --firmware x86_64_uefi=/absolute/path/to/complete/OVMF.fd
```

## Fetch, verify, and run

Fetching is explicit and is the only networked phase:

```bash
python3 scripts/foreign_kernel_lab.py fetch
```

Re-verify the complete cache without network access:

```bash
python3 scripts/foreign_kernel_lab.py verify
```

Run one profile or the complete matrix:

```bash
python3 scripts/foreign_kernel_lab.py run linux-alpine-3.24.1-aarch64
python3 scripts/foreign_kernel_lab.py run linux-alpine-3.24.1-x86_64
python3 scripts/foreign_kernel_lab.py run freebsd-15.1-release-aarch64
python3 scripts/foreign_kernel_lab.py run openbsd-7.9-amd64
python3 scripts/foreign_kernel_lab.py run plan9-9front-11983-amd64
python3 scripts/foreign_kernel_lab.py run guix-system-1.5.0-x86_64
python3 scripts/foreign_kernel_lab.py run redox-0.9.0-server-x86_64
python3 scripts/foreign_kernel_lab.py run-all
```

Results are written beneath `target/foreign-kernel-lab/` by default. A
claim-admissible run has `status: passed`, `claim_admissible: true`,
`serial.raw`, `serial.normalized.txt`, `qemu.stderr`, and `observation.json`.
The observation records identities for stdout and stderr,
the source commit plus dirty-state evidence, and whether the isolated QEMU
process group exited, was terminated, or required a kill. A controlled harness
termination after the final marker is recorded as cleanup, not disguised as a
guest-initiated exit. Dirty source state is retained as provenance rather than
silently presented as a clean release run. In an extracted source release, the
same command works without `.git` only after the canonical source manifest,
checksums, all declared payload identities, and executable modes verify.

Use `--guest-dir PATH`, `--output-dir PATH`, or
`--firmware aarch64_uefi=/absolute/path` when those defaults do not match the
host. `run --qemu PATH` is only a diagnostic/test override: the exact override
and its digest are retained, but the command exits unsuccessfully with
`synthetic-passed` and `claim_admissible: false` even when every boot marker is
seen. For a claim-admissible nonstandard QEMU installation, put the reviewed
manifest-named `qemu-system-aarch64` or `qemu-system-x86_64` executable on the
caller `PATH`. `run-all` resolves that architecture-specific manifest name for
each profile and deliberately has no single `--qemu` override.

## Fail-closed conditions

The run fails before launch when a manifest field is unknown, an input is not a
regular non-symlink file, an artifact size or digest differs, an admitted file
changes while it is hashed, required firmware or QEMU is missing, or the QEMU
argv violates the TCG/no-network safety profile. A manifest-named executor
without the admitted numeric QEMU banner cannot establish the claim. During
execution the harness fails on a
timeout, capture overflow, forbidden marker, missing or out-of-order required
marker, duplicated marker declared unique, or an interactive profile that does
not complete every ordered prompt-triggered console action. Counted actions
fire only after the declared positive, bounded occurrence of their trigger;
occurrences for a repeated trigger must be strictly increasing. A nonzero
pre-cleanup exit, pre-cleanup QEMU
stderr, input mutation, unresolved process group, or incomplete pipe drain also
fails the run. Failure observations retain the bounded output and cleanup facts
for diagnosis; they do not establish the profile's boot claim.

## Pinned upstream artifacts

| Profile | Artifact | Bytes | SHA-256 |
|---|---|---:|---|
| Alpine Linux AArch64 | `vmlinuz-virt` | 10,351,104 | `47970e0ee0478fe5c60824a89f162d5a353fa29466e5d3bddb0f9c506f1ed756` |
| Alpine Linux AArch64 | `initramfs-virt` | 9,385,851 | `e47d38bc88509a3db11affc09f9762f9643b026bd29441724a4729ad8e97add6` |
| Alpine Linux x86_64 | `vmlinuz-virt` | 12,575,744 | `1e6bf9027720c75c3ed0d79171f21b5791ee40ca9795d07c7c6e04dc5ea2ae90` |
| Alpine Linux x86_64 | `initramfs-virt` | 9,637,032 | `6d80a739fedeeb6cd63e24dd208845e22199c41a5fb2054941ef61ec30264fa9` |
| FreeBSD | `bootonly.iso.xz` | 96,421,460 | `33e2dc303b5dce5a374727ba12c41c303db70fe0676e76333e09e0ea8cb2fbd0` |
| FreeBSD | expanded `bootonly.iso` | 460,095,488 | `359136c2af73e03da6f15ad59f0c67bc561ca8b69631d78bfb8f2225e2c9a5ef` |
| OpenBSD | `install79.iso` | 798,625,792 | `7a4a92e953618035097c796a90b54424a0f3ae775552e1e7d102cf8a5130449f` |
| 9front | `9front-11983.amd64.qcow2.gz` | 257,961,445 | `b96617b6eebcec8621a4c176e7acc29a1835ed09d4b61f9fe2e3e64c18d20867` |
| 9front | expanded qcow2 | 550,240,256 | `0326632e2d90f4038069edbadd2918f7662397ad879a97d91cdac474d31a9746` |
| GNU Guix System | installer ISO | 1,188,261,888 | `107e0a8082f03a10b15c1fb9383d2d752c1cdeda41b8db575a15550e1c2d8b4a` |
| GNU Guix System | detached ISO signature | 566 | `fac1d7af22f7dd6598f7599295544061202002617d2aee23cf56bd0a40a67d60` |
| GNU Guix System | extracted Linux-libre `bzImage` | 16,688,128 | `eba328fc22572bf8f6523fdec52e1f1ceeb8817f2d7caea3b312067f831f6e48` |
| GNU Guix System | extracted raw initrd | 14,104,469 | `bd3d4323d77e4ad94289c512bcccf4c712c3c401d707cb538364808d08287994` |
| Redox OS | server livedisk ISO.zst | 70,108,737 | `a73c3783a72a15eba8dd85dee941298cdd34e125a37008a3bcc7227b5f073e93` |
| Redox OS | expanded livedisk ISO | 536,870,912 | `33e85a8f9fc9207a6e075a170207a18bccd53ad00781e58d12b466a61958c994` |

Alpine publishes signatures and sidecar hashes for its complete release images
and netboot archive, but not separate sidecars for the two extracted netboot
members used here. Their versioned member digests are therefore labeled as
repository-observed pins rather than as separately signed upstream digests.
FreeBSD's manifest records the values published in its clearsigned release
checksum document. The 9front digest is a repository-observed pin for official
build 11983, which the build index ties to source commit
`50aefa0743c8cfd83fdc7f568d24e1bba8b9848e`.

During this audit, GPG reported `GOODSIG` and `VALIDSIG` for the Guix ISO from
Efraim Flashner's key fingerprint
`A28B F40C 3E55 1372 662D 14F7 41AA E7DC CA3D 8351`; it also reported an
expired historical subkey and undefined local owner trust. The committed lab
pins the ISO and detached-signature identities, but the runtime harness does not
invoke GPG or promote that one audit into a permanent trust decision. Redox's
compressed digest is from its published `SHA256SUM`; the expanded digest is a
repository pin verified after bounded Zstandard expansion. At runtime the
harness verifies every compressed, expanded, and extracted identity against the
committed manifest without refetching upstream checksum or signature documents.

Upstream references:

- [Alpine 3.24.1 AArch64 netboot members](https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/aarch64/netboot-3.24.1/)
- [Alpine 3.24.1 x86_64 netboot members](https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot-3.24.1/)
- [Alpine download and signing information](https://www.alpinelinux.org/downloads/)
- [FreeBSD 15.1-RELEASE image index](https://download.freebsd.org/releases/ISO-IMAGES/15.1/)
- [FreeBSD 15.1 AArch64 signed checksums](https://www.freebsd.org/releases/15.1R/checksums/CHECKSUM.SHA256-FreeBSD-15.1-RELEASE-arm64-aarch64.asc)
- [OpenBSD 7.9 amd64 release artifacts](https://cdn.openbsd.org/pub/OpenBSD/7.9/amd64/)
- [OpenBSD 7.9 amd64 signed checksums](https://cdn.openbsd.org/pub/OpenBSD/7.9/amd64/SHA256.sig)
- [9front official builds](https://build.9front.org/)
- [9front build 11983 source commit](https://git.9front.org/plan9front/plan9front/50aefa0743c8cfd83fdc7f568d24e1bba8b9848e/commit.html)
- [GNU Guix 1.5.0 downloads](https://ftp.gnu.org/gnu/guix/)
- [GNU Guix manual](https://guix.gnu.org/manual/en/guix.pdf)
- [Redox OS 0.9.0 x86_64 releases](https://static.redox-os.org/releases/0.9.0/x86_64/)
- [QEMU Arm `virt` machine](https://qemu.readthedocs.io/en/master/system/arm/virt.html)
- [QEMU x86 system emulator](https://qemu.readthedocs.io/en/master/system/target-i386.html)

## Evidence separation

`evidence/foreign_kernel_lab.toml` has its own schema and claim class. Do not
add these observations to `evidence/gates.toml`, rename them as `world-g7`, or
use them to relax the G7 acceptance contract. They are executable preparation
for later foreign-kernel integration, not evidence that the integration already
exists.
