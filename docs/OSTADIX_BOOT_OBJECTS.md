# OSTADIX boot object contract

The OSTADIX workstation ISO admits the exact staged Git tree as a read-only,
content-addressed boot object set. This is the release boundary for the phrase
"all of Ostadix-lang is available in the boot": it does not mean that generated
`target/` files, `.git`, caches, or untracked workstation files are copied into
the image.

## Exact inclusion boundary

The release orchestrator runs `git write-tree`, creates a deterministic archive
of that tree, and rejects tracked worktree drift before the Linux guest builds
anything. Every regular blob in that archive is present in both forms:

- at its original path below `/usr/src/ostadix`, for normal source-oriented
  tools; and
- once by raw SHA-256 below
  `/usr/share/ostadix/boot-objects/v1/objects/sha256`, with every Git path and
  executable mode bound through `index.bin`.

The index binds the staged tree and base commit, both Git blob SHA-1 and raw
SHA-256 identities, logical and deduplicated byte counts, and a domain-separated
root digest. Paths, records, and bindings are canonically ordered. The builder
rejects symbolic links, hard links, special files, unsafe paths, unsupported Git
modes, oversized blobs, and count or byte-limit violations.

The copy below `/usr/src/ostadix` is verified against the same bindings during
the boot gate. Thus the convenient source view cannot silently diverge from the
content-addressed object view.

Both views reside in the digest-bound read-only workstation SquashFS. The small
bootstrap initramfs verifies that root by byte count and SHA-256, mounts it
through a read-only loop device, and creates a volatile tmpfs overlay before
`switch_root`. Before readiness, stage-two init self-bind-mounts both
`/usr/src/ostadix` and `/usr/share/ostadix/boot-objects/v1`, remounts each bind
read-only, checks the live mount flags, and proves direct writes fail. Runtime
work belongs below `/workspace`, volatile `/tmp`, or the overlay. This
establishes the default live-system posture; it is not an adversarial guarantee
against privileged root deliberately changing mounts.

## First-class operations

The released lowercase CLI exposes the store without granting it execution
authority:

```text
o object root
o object list [PREFIX]
o object stat PATH
o object get PATH [OUTPUT]
o object verify
```

`root`, `list`, and `stat` are typed inspection operations. `get` materializes
one admitted blob after identity verification. `verify` checks the complete
index and object closure. Version 1 deliberately has no execute operation:
being present in the Git tree is not itself permission to run a file.

## Runnable and bootable roles

Runnable products are separately built from the same staged tree and installed
under `/usr/local/bin`; their exact size and SHA-256 identities are recorded in
the release receipt. The combined ISO has 14 separately typed artifacts: four
Hosted components, direct O-core, the capacity-host kernel/initramfs, direct
Alpine initramfs, Guix kernel/initramfs/ISO, OpenBSD ISO, 9front qcow2, and Redox
ISO. Hosted, O-core, and Alpine are direct routes; the four foreign systems are
explicit nested QEMU/TCG routes. The hosted entry names the rootfs and modloop
as kernel arguments; neither is another GRUB initrd.
Rust, Cargo, `rust-wasm`, the `wasm32-wasip1` standard library, the Alpine
package manager, build tools, `wasm-tools`, Wasmtime, and the offline Cargo
source closure are workstation capabilities, not evidence that every source
blob is executable.
The separately admitted `/usr/share/ostadix/wasm/hello.wasm` is paired with a
read-only release descriptor. That descriptor binds the exact staged
`examples/wasm_hello.O` input,
installed `olangc`, generated Cargo-project closure, fixed offline build
profile, and final module identity. Boot regenerates the project through MCP
without Cargo, verifies the descriptor/module, and independently compiles a
small Rust/WASI probe. It then executes the admitted module under Wasmtime and
runs a `webassembly^` O object through WAT conversion and Wasmtime. These are
derived-object, runtime, and toolchain proofs; they do not grant execution
authority to arbitrary source objects.

O-core remains a freestanding kernel with its own implemented boundary. Its menu
entry does not turn the hosted Linux userspace, Cargo, APK, or graphical desktop
into native O-core facilities.

The foreign guest images are typed combined-ISO artifacts, not blobs from the
staged-tree CAS. Their independent pinned-download provenance is receipt-bound.
Embedding and menu routing do not turn them into native O-core facilities.

## Evidence boundary

The automated release proves exact staged-tree inclusion, object-store closure,
the verified SquashFS handoff, seven-entry/14-artifact structural admission,
x86_64 Hosted binary and APK execution, Rust/Cargo operation, graphical QEMU
input, and direct O-core selection under OVMF/QEMU TCG with a 4 GiB regression
bound. Those gates do not by themselves prove execution of direct Alpine or the
four nested foreign routes, their package managers or GUIs, Secure Boot,
persistence, Ventoy 1.1.17 boot, or arbitrary physical hardware support.
The hook-compatible marker, minimal `dm-mod.ko` SquashFS, and bounded
BusyBox-label discovery are implementation properties, not hardware evidence.
Physical boot is recorded separately because only a real machine can establish
it.
