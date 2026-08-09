<p align="center">
  <img src="./assets/olang-logo.png" alt="Ostadix-lang" width="900" />
</p>

# Ostadix-lang

*By Lee Daghlar Ostadi*

[![CI](https://github.com/lostadi/Ostadix-lang/actions/workflows/ci.yml/badge.svg)](https://github.com/lostadi/Ostadix-lang/actions/workflows/ci.yml)
[![Parser fuzz campaign](https://github.com/lostadi/Ostadix-lang/actions/workflows/fuzz.yml/badge.svg)](https://github.com/lostadi/Ostadix-lang/actions/workflows/fuzz.yml)

> **Every expression carries its own interpreter as part of its syntax.**

This repository is the compatibility-preserving integration monorepo for
**OSTADIX**, the umbrella system. Its component names remain distinct:

- **Ostadix-lang** is the hosted polyglot language and evidence-bound HGraph
  runtime.
- **O-core** is the freestanding native systems language.
- **OKernel** is the sovereign kernel built through O-core.
- **O-Machine** is the architecture-specific machine-resource and
  virtualization substrate.
- A governed **World** is OSTADIX's distributed runtime ontology.

The first integrated system release is named **OSTADIX Alpha**. `World`
continues to name runtime identities, resources, namespaces, contracts, and an
elastic governed computer; it is not part of the release name. Existing
Ostadix-lang commands, package identities, URLs, and citation metadata remain
compatible.

Ostadix-lang is a language system built on one
radical idea: the language an expression is written in is a structural part
of the expression itself, not a file extension, not a global mode switch, not
a pragma. You write the language name directly around the code, and the
runtime dispatches to that language's evaluator on the spot.

```O
html^(
  <p>The answer is python^(
__oval_result__ = sum(x*x for x in range(10))
)_python.</p>
)_html
```

The `python^( ... )_python` block is not a string, not a template, not a code
fence. It is an *expression*. Its parenthesis shape, `LANG^(` ... `)_LANG`, is
the syntax that says "evaluate this in Python." The result is an OValue that
HTML can embed directly, without either side knowing about the other's type
system.

Ostadix-lang now has two computation layers that share one project but do different
jobs:

1. **O orchestration**, written in `.O` files, composes real hosted languages,
   persistent environments, deferred computations, Nix operations, and
   operating-system values through typed parentheses and OValue.
2. **O-core**, written in `.oc` files, is the statically typed native systems
   language. It compiles through typed HIR and SSA MIR into freestanding
   ELF64 object files for its primary x86_64 target and bounded AArch64 G2
   subset. It is capable of building a kernel without Python, JSON,
   subprocesses, a filesystem, libc, or Rust `std` in the target image.

This separation is deliberate. OIR describes orchestration between language
runtimes. O-core MIR describes machine computation, control flow, memory, and
hardware. Hosted blocks such as `python^`, `rust^`, `nix^`, and `sql^` remain
available in user space without becoming kernel dependencies.

**Portability.** Hosted Ostadix-lang is architecture-portable through its
Rust/Cargo, C17, and Python implementations, subject to availability of the
evaluator runtimes used by a program. It has been developed and run on macOS
ARM64, Android ARM64 on a rooted Pixel 8 Pro, and Intel x86_64 Linux. O-core's
broad compiler/kernel target remains x86_64, while G2 adds a bounded,
conservative `aarch64-unknown-none` scalar backend and single-vCPU QEMU/TCG
execution. Those native target boundaries do not apply to hosted `.O`
execution.

---

## Using Ostadix-lang with AI agents

Ostadix-lang is designed to work well as a *primary* language for AI coding
agents: one `.O` program can delegate each subtask to the best hosted language
while every result flows through the typed OValue boundary. The `O` CLI has an
agent-oriented surface:

- `O --json program.O` — run and emit a single-line JSON result or structured
  error (`{"ok":false,"stage":"parse"|"eval","error":...}`) on stdout.
- `O --check program.O` — parse-only validation without executing anything
  (combine with `--json` for a machine-readable verdict).
- `O --eval '<source>'` / `O -e '<source>'` — evaluate an inline expression
  without a file.

See [docs/AI_GUIDE.md](docs/AI_GUIDE.md) for the full agent workflow, syntax
pitfalls, and recipes, and [llms.txt](llms.txt) for an LLM-oriented index of
this repository.

### Local MCP server for AI-agent tools

The repository also includes `ostadix-mcp`, a local stdio
[Model Context Protocol](https://modelcontextprotocol.io/) server under
`mcp/ostadix_lang_mcp_server/`. It exposes a small, typed tool surface over the
existing local `O` and `olangc` binaries so an MCP-capable agent can discover
the Ostadix environment, run a smoke test, execute a `.O` program, or inspect a
compiler target without reconstructing the backend path by hand.

| MCP tool | Current behavior |
|----------|------------------|
| `o_env` | Reports the resolved Ostadix root, backend directory, `O` and `olangc` paths, and Python shim status. |
| `o_doctor` | Checks the local toolchain and inventories compatibility shims. |
| `o_smoke` | Runs `examples/hello.O` with an absolute backend path and expects `2`. |
| `o_run` | Runs one local `.O` file with an explicit working directory and timeout. |
| `o_olangc` | Runs `olangc` with the resolved shim directory; supports `ir`, `dot`, `script`, and `wasm`, or the default target. |
| `o_search_run` | Runs a named search program from an external `a18re` work tree when that optional tree is present. |

The normal setup builds this separate, lockfile-pinned Rust crate and, unless
wrappers are disabled, copies the executable to
`~/.local/bin/ostadix-mcp`:

```bash
./setup.sh --minimal --yes
```

For a build without the rest of setup, build the two Ostadix commands used by
the server and then the server itself:

```bash
cargo build --release --bin O --bin olangc
cargo build --release --locked \
  --manifest-path mcp/ostadix_lang_mcp_server/Cargo.toml
```

The second command writes
`mcp/ostadix_lang_mcp_server/target/release/ostadix-mcp`. The server crate is
deliberately separate from the root Cargo package, so a root `cargo build` or
`cargo test` does not build or test it. Its dedicated CI job uses the crate's
own lockfile, rejects Clippy warnings, and exercises initialization, exact tool
discovery, `o_env`, and `o_smoke` over the real stdio transport with
`scripts/smoke_ostadix_mcp.py`. That smoke also calls `o_run` with both forms of
relative path and calls `o_olangc`, so transport discovery alone cannot mask a
broken execution tool. Use `./setup.sh --no-mcp` when the MCP server is not
wanted. The deterministic source release includes `mcp/`, `.mcp.json`, and the
smoke client, and its link/schema/metadata verifier rejects an incomplete MCP
release surface without executing archive payloads. The separate crate uses the
repository's LGPL-2.1-only license.

The checked-in `.mcp.json` registers the server as `ostadix` using the
`ostadix-mcp` wrapper. MCP clients that support repository-local stdio server
configuration can load that file after `~/.local/bin` is visible in the
client's `PATH`. It deliberately contains no shell-expanded environment values:
the server discovers a valid Ostadix root from its working directory (or its
ancestors), then checks `O_LANG_ROOT` and conventional home-directory checkouts
when needed. For clients that launch outside the repository or need an explicit
configuration, use absolute paths rather than relying on shell expansion inside
JSON:

```json
{
  "mcpServers": {
    "ostadix": {
      "command": "/absolute/path/to/Ostadix-lang/mcp/ostadix_lang_mcp_server/target/release/ostadix-mcp",
      "args": [],
      "env": {
        "O_LANG_ROOT": "/absolute/path/to/Ostadix-lang",
        "O_BACKENDS_DIR": "/absolute/path/to/Ostadix-lang/backends"
      }
    }
  }
}
```

After adding the configuration, reload the client's MCP servers and use a
short discovery-to-execution workflow such as:

```text
o_env {}
o_smoke {}
o_run {"path":"/absolute/path/to/Ostadix-lang/examples/hello.O"}
o_olangc {"path":"/absolute/path/to/Ostadix-lang/examples/hello.O","target":"ir"}
```

These are MCP tool names and argument objects, not shell commands. `o_smoke`
should return `SMOKE_OK` and show the program result `2`.

`ostadix-mcp` is a local child process, not a hosted service. It has no network
listener or authentication layer and executes local programs with the
authority and `PATH` inherited from the MCP client. Its current tool surface
does not expose `o-link`, `o-unlink`, `ocorec`, kernel boot gates, repository
editing, or a separate arbitrary-shell tool. A `.O` program run through
`o_run` can still invoke any configured backend, including shell backends.

---

## Getting Started: Full Setup Guide

There are three implementations of the hosted `.O` language and one native
compiler path in this repository:

- The **Rust edition** is the authoritative hosted runtime and contains the
  interpreter, REPL, OIR, scheduler, linker tools, notebook, `olangc`, and
  `ocorec`.
- The **C17 edition** is the small standalone hosted runtime and AOT compiler.
- The **Python edition** is the readable reference implementation used for
  semantic cross-checking.
- **O-core** is compiled by the Rust `ocorec` binary, but the code it produces
  is freestanding and has no Rust runtime dependency.

You only need the Rust edition for the full current feature set. The C17 and
Python editions remain useful when you want a smaller substrate or a direct
comparison of the evaluator semantics.

### Prerequisites

The base Rust build needs:

- Rust and Cargo
- A C compiler and system linker
- Python 3 for the `python^` compatibility bridge and Python-backed legacy adapters
- Git and standard POSIX command-line tools

Each hosted backend uses the real local runtime named in the backend table.
You only install the runtimes your `.O` program actually uses. Nix is needed
for the Nix lattice and NixOS tests. Node.js is needed for `javascript^`.
Racket is needed for `racket^`. Rust is needed for `rust^`. The same rule
applies to the other language backends. The automatic installer keeps Nix and
native/kernel tooling behind explicit profiles so a normal hosted setup does
not install them unexpectedly.

The portable O-core build and QEMU gates additionally need:

- Clang with the `x86_64-unknown-none-elf` and
  `aarch64-unknown-none-elf` assembler targets
- An LLD-compatible linker, either `rust-lld`, `ld.lld`, or Homebrew `lld`
- ELF inspection tools, CMake/CTest, and x86_64 plus AArch64 QEMU

The kernel build probes the active Rust toolchain, `PATH`, and common Homebrew
LLD prefixes. If your linker lives somewhere custom, set
`OCORE_LLD=/absolute/path/to/rust-lld-or-ld.lld`.

Python is used by the four-second QEMU smoke-test harness. It is not linked
into the kernel and is not used after the machine starts executing O-core.

Linux kernel development, foreign guest experiments, and O-core are separate
scopes. The Linux-only kernel profile installs host build dependencies, not a
Linux source tree, kernel, root filesystem, or boot image. Guest tooling is for
user-supplied, checksum-pinned Linux, 9front, or OpenBSD media; installing it
does not mean that O-core boots or supports those foreign kernels or operating
systems.

### Option A: Automatic setup

The included `setup.sh` script detects the host, installs the ordinary hosted
runtime dependencies, builds the Rust and C17 editions, prepares the Python
reference, builds the local MCP server, and creates convenience wrappers:

```bash
git clone https://github.com/lostadi/Ostadix-lang.git Ostadix-lang
cd Ostadix-lang
./setup.sh
```

The script supports composable setup profiles and non-installing checks:

```bash
./setup.sh --minimal                         # hosted build without matplotlib
./setup.sh --full --verify                   # full hosted profile + hosted checks
./setup.sh --with-nix --deps-only            # Nix plus environment, no builds
./setup.sh --with-ocore --verify-ocore       # tools + bounded x86 QEMU smoke
./setup.sh --full --with-hosted-runtimes     # broad open-source backend pack
./setup.sh --with-linux-kernel-tools --deps-only
./setup.sh --with-guest-tools --with-ubuntu-vm --deps-only
./setup.sh --with-ocore --check              # non-installing capability check
./setup.sh --env-file /path/to/env.sh --persist-env
./setup.sh --no-wrappers
./setup.sh --no-mcp
./setup.sh --dry-run
./setup.sh --help
```

`--minimal` skips optional matplotlib while still allowing explicit
`--with-*` profiles. `--full` adds the notebook, Racket, Nix, and the complete
O-core build/QEMU tool set on the validated host package maps; use `--no-nix`
to exclude Nix. It does **not**
select guest tooling, install guest media, or select the Linux kernel tool
profile.

`--with-nix` installs or verifies Nix on supported macOS and Linux hosts.
`--with-ocore` adds Clang, LLD, ELF tools, CMake/CTest, and x86_64/AArch64 QEMU.
`--with-hosted-runtimes` is the deliberately heavy, currently macOS/Homebrew
and Debian-family profile for Node.js, Ruby, Racket, GHC, OCaml, Common Lisp,
Mono, GNU Octave, WABT, and Wasmtime. It excludes Java by local policy and does
not install licensed MATLAB or Wolfram products; Octave covers only
MATLAB-compatible code. Use `--with-hosted-runtimes --check` to inventory those
executables without installing packages.
`--with-linux-kernel-tools` is Linux-only and installs build prerequisites such
as Bison, Flex, libelf/pahole, CPIO, rsync, and kmod; it does not fetch kernel
sources. `--with-guest-tools` adds QEMU image and compression tools and prepares
`${XDG_DATA_HOME:-$HOME/.local/share}/ostadix/guests` for media supplied by the
user. `--with-ubuntu-vm` also selects guest tools and installs Multipass for the
`ubuntu_vm^` backend where the host package manager supports it.

`--verify` checks the hosted Rust, C17, AOT, and Python forms after a build.
`--verify-ocore` implies `--with-ocore` and runs the bounded x86_64 O-core
QEMU/TCG smoke; it is not a foreign-OS test. `--check` performs a non-installing,
no-persistent-change capability check for the selected profiles. `--deps-only` installs dependencies
and writes the environment but skips all Ostadix builds, so it cannot be
combined with either verification option.

By default setup writes a managed environment file at
`~/.config/ostadix/env.sh`. It exports `O_LANG_ROOT`, `O_BACKENDS_DIR`, the
Ostadix/Cargo tool paths, detected Homebrew LLVM/LLD paths, and
`OSTADIX_GUESTS_DIR`, and activates Nix when present. Use `--env-file PATH` to
choose another location, `--no-env` to disable the file, or `--persist-env` to
add an idempotent source block to `~/.zshrc` or `~/.bashrc`. Each normal setup
run removes stale generated Ostadix-lang binaries before rebuilding them,
refreshes installed Rust copies in `~/.cargo/bin`, and recreates wrappers in
`~/.local/bin`; `--no-mcp` skips the separately locked `ostadix-mcp` crate.

After setup:

```bash
o examples/hello.O
cargo run -- examples/hello.O
./c_cpp/O examples/hello.O ./backends
python3 -m o_lang examples/hello.O
```

### Option B: Manual Rust setup

```bash
git clone https://github.com/lostadi/Ostadix-lang.git Ostadix-lang
cd Ostadix-lang
cargo build --release

./target/release/O examples/hello.O backends
./target/release/olangc examples/hello.O -o target/hello
./target/hello
```

### O-Git semantic receipt demo

The smallest O-Git surface is a one-command receipt demo. It lowers one tiny
Ostadix-lang group pipeline to C, runs the compiled artifact, records what semantic
structure survived or disappeared, and shows a policy-aware diff:

```bash
cargo run --bin ogit -- demo semantic-receipt
```

The checked-in source lives in `examples/group_pipeline/main.O`. The generated
C target, visible HTML graph, and DOT graph are written to
`examples/group_pipeline/generated/`, and the receipt is written to
`.ogit/receipts/semantic-receipt-001.json`.

The usual package-manager prerequisites for a manual hosted build are below.
For the validated O-core, Linux-kernel-build, or guest-lab
dependency sets, prefer the corresponding `setup.sh --with-* --deps-only`
profile; those optional sets are deliberately not all included here.

#### macOS

```bash
xcode-select --install
brew install rust python sqlite qemu
cargo build --release
```

#### Debian, Ubuntu, Mint, and Pop!_OS

```bash
sudo apt-get update
sudo apt-get install -y build-essential clang lld python3 python3-pip sqlite3 \
    curl git pkg-config libssl-dev qemu-system-x86
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
cargo build --release
```

#### Arch, CachyOS, Manjaro, and EndeavourOS

```bash
sudo pacman -Syu
sudo pacman -S --needed base-devel clang lld python sqlite rustup qemu-full git
rustup default stable
cargo build --release
```

#### Fedora, RHEL, Rocky, and related systems

```bash
sudo dnf groupinstall -y "Development Tools"
sudo dnf install -y clang lld python3 sqlite rustup qemu-system-x86 git openssl-devel pkgconfig
rustup default stable
cargo build --release
```

#### NixOS

```bash
nix-shell -p rustup clang lld python3 sqlite qemu
rustup default stable
cargo build --release
```

#### Other systems

Dedicated setup scripts are provided for Alpine, openSUSE, Void, Gentoo,
FreeBSD, TinyCore, Windows, macOS, Debian, Arch, Fedora, and NixOS under
`setup/os/`. Windows development is best done through WSL2 when the program
needs POSIX backends or the QEMU kernel proof.

### Option C: C17 edition only

The C17 edition requires only a C17 compiler, make, and whatever hosted
language runtimes the program calls:

```bash
cd c_cpp
make
./O ../examples/hello.O ../backends

./olangc ../examples/hello.O -o /tmp/hello-c
/tmp/hello-c
```

The C17 port implements the core typed-parenthesis evaluator, structural
backends, Python execution, the Nix value ladder, lazy and deferred requests,
shebang handling, and AOT packaging. The Rust edition remains authoritative
for OIR planning, coordination-group concurrency, the full backend registry,
the notebook, and O-core.

### Option D: Build and boot O-core

```bash
# Non-installing host capability check.
o kernel doctor

# Build and inspect the baseline freestanding ELF.
o kernel build
o kernel image

# Bounded automated boot proof.
o kernel smoke

# Interactive baseline boot, or the native capability-gated `o> ` console.
o kernel boot
o kernel console

# Drive the console lifecycle automatically, or run the complete portable set.
o kernel smoke-live
o kernel gates

# Build, inspect, and boot the deterministic x86_64 UEFI disk path.
o kernel doctor-media
o kernel media
o kernel smoke-media
o kernel smoke-boot-info
o kernel smoke-smp

# Capacity-bound removable-media and authority-free observation workflow.
o kernel prepare-write --image "$IMAGE" --device "$DEVICE"
o kernel write-media --image "$IMAGE" --device "$DEVICE" --confirm "$TOKEN"
o kernel boot-challenge
o kernel prepare-physical --image "$IMAGE" --media-write write.json \
  --machine machine.json --profile mode0 --expected-cpus 1 --output intent.json
o kernel record-physical --intent intent.json --transcript serial.log \
  --image "$IMAGE" \
  --assert-physical I-OBSERVED-OSTADIX-ON-PHYSICAL-X86_64 \
  --output observation.json
```

`o kernel console` builds probe mode 16 and prints the exact embedded package
digest plus the currently usable commands before entering QEMU:

```text
o> status
o> install <printed-sha256> 5 1
o> activate <printed-sha256>
```

Use `Ctrl-A X` to leave either interactive QEMU session. The native console is
a bounded control plane, not a general shell: installation and activation are
the implemented interactive lifecycle. The smoke command is the finite,
asserted alternative suitable for automation.

For the OVMF path and confirmation-gated removable-media writer, see
[OSTADIX Alpha x86_64 UEFI boot media](docs/OSTADIX_BOOT.md). That image is
structurally suitable for raw physical media. Writer v2 derives a
capacity-bound `ostadix.boot-media-target-plan/v2`, relocates the backup GPT to
a larger target's final LBA, and writes and verifies only its exact admitted
extents through one held device descriptor. `target_plan_sha256` binds that
mutation; `target_image_sha256` is `null` when sparse unwritten ranges remain,
and those ranges can retain recoverable prior data. A stable, nonempty device
identity is mandatory; depending on the platform it must be a device serial,
WWN, or media UUID. USB-port topology alone is rejected, and accepted identity
values are not necessarily unclonable hardware identities. The physical-intent
and observation commands produce unkeyed,
authority-free, replayable operator records; even with the exact
`--assert-physical` phrase, they do not authenticate a physical-machine boot,
trusted source build, or physical SMP execution.

`o kernel smoke-smp` is the separate Mode 34 control. It boots one challenged
image under QEMU q35/TCG + OVMF with exactly four vCPUs, starts three APs with
x2APIC INIT/SIPI, verifies distinct APIC identities and stacks across one
atomic barrier, and then reruns the same bytes with one vCPU as a fail-closed
negative control. APs park after that barrier: this proves bounded bring-up,
not a general SMP scheduler or a physical-machine boot.

The lower-level compiler and boot scripts remain available:

```bash
cargo build --bin ocorec

# Compile one or more O-core modules to an ELF relocatable object.
target/debug/ocorec ocore/examples/minimal.oc --emit obj -o target/minimal.o

# Build the included freestanding kernel.
./ocore/kernel/build.sh

# Boot interactively or run all manifest-defined portable release gates.
./ocore/kernel/run-qemu.sh
./boot-and-test.sh smoke
```

The repository-root `boot-and-test.sh` script sequences these same layers
through one entrypoint with tool-presence guards, so a missing optional
dependency skips rather than aborts:

```bash
./boot-and-test.sh setup    # ./setup.sh --minimal --verify
./boot-and-test.sh hosted   # hosted .O runtime + differential + cross-checks
./boot-and-test.sh ocore    # ocorec build, HIR/MIR dump, freestanding object
./boot-and-test.sh kernel   # build the kernel ELF and boot it in QEMU
./boot-and-test.sh smoke    # asserted smoke gate, then the full probe matrix
./boot-and-test.sh tests    # Rust tests, proptest, reproducibility, examples
./boot-and-test.sh quick    # hosted + ocore + default smoke (default)
./boot-and-test.sh full     # everything above
```

The asserted default `smoke-qemu.sh` output is:

```text
O-core kernel: serial online
page protections: W^X online
page allocator: online
M03 frames: reclaim PASS
M03 frames: zero-reuse PASS
M03 frames: stale-double-free denied
M03 frames: injected-failure rollback PASS
M03 memory objects: typed-generation PASS
address space: online
capability: online
user copy faults: recovered
entry state: CPU-local online
T
CPL3 native[0]: online
user zero-fill: online
capability bounds: denied
forged capability: denied
stale capability: denied
wrong rights: denied
wrong type: denied
closed capability: denied
user ranges: denied
kernel pointer: denied
unknown syscall: denied
register preservation: online
cap_copy reserved: denied
process exit gated: denied
M03 page_alloc: capability online
M03 quota: enforced-recovered
M03 memory stale close: denied
M03 memory lifecycle: PASS
oversized buffer: denied
RFLAGS sanitization: online
timer CPL3 return: online
yield hook: online
CPL3 heartbeat: online
QEMU smoke: PASS
```

The `T` is emitted by the actual IRQ0 timer handler after the IDT, PIC, and
PIT are installed. The smoke gate requires that standalone timer line before a
post-interrupt CPL3 return marker and a later CPL3 heartbeat. User-boundary
markers are emitted by the CPL3 task through the architectural syscall path;
the M0.3 allocator/object markers come from kernel self-tests. Only the final
`QEMU smoke: PASS` line is added by the host harness.

The Milestone 0.2 fault gate performs eight fresh QEMU boots. It covers divide
error, invalid opcode, canonical non-present and supervisor reads, a stack-guard
write, NX instruction fetch, a noncanonical target, and an excluded syscall
return RIP. Each run must mark the scenario's only process `FAULTED`, clear the
current process, and reach a later kernel timer marker. It also performs a ninth
boot with a deliberate leaf-page hole, requires `ERR_USER_COPY_FAULT`, and
observes a later CPL3 heartbeat without faulting the process. This wording is
specific to that bootstrap gate and is not the current kernel ceiling.

Milestone 1 is complete for two bounded native processes on one CPU.
`smoke-processes-qemu.sh` boots independent normal-exit and contained-fault
scenarios and proves separate CR3s, same-VA physical isolation, atomic
PCB/domain/address-space/CSpace switching, split teardown, stale identity
denial, sibling survival, complete dynamic-frame reclamation, and a later timer.

Milestone 2 is complete for four TCBs across two processes on one CPU.
`smoke-scheduler-qemu.sh` proves one million forced identity transactions, FIFO
runnable and blocked queues, two CPU-bound and two sleeping CPL3 threads,
cooperative yield, timer preemption, cross-thread hostile-RFLAGS sanitization,
wake-once sleep, priority/accounting, hostile saved-RSP TCB containment, idle
entry, exit during preemption, sibling progress, stale TCB denial, frame
reclamation, and a post-lifecycle timer. The million-iteration stress installs
and verifies CR3/TSS/GS plus PCB/domain/address-space/CSpace identity and a saved
frame canary without entering CPL3; the separate IRQ/SYSCALL phase proves real
save/IRET context switching.

Milestone 3 now has a bounded native IPC gate. The earlier
`smoke-ipc-foundation-qemu.sh` remains a kernel-mechanism regression;
`smoke-ipc-qemu.sh` adds public CPL3 endpoint create/send/receive/cancel,
cross-domain request/reply, real TCB blocking and wake-once retry on a full
four-message FIFO, exact attenuated capability transfer, creating-process-bound
ticket abort, recovery after all 16 ticket slots are exhausted, automatic
dead-sender cleanup, exception-driven personality crash containment,
unrelated-world progress, transactional reclamation, and a post-lifecycle
timer.

Milestone 4 is gated by `smoke-loader-qemu.sh`. A deterministic host builder
produces two independently linked static personality ELFs plus malformed,
overlapping, and W+X corpus entries in a read-only OVFS image. A fresh QEMU boot
imports those bytes as data, rejects the corpus before start, executes both
ELFs in isolated W^X address spaces, returns an attenuated service capability,
tears the namespace down transactionally, reclaims all frames, and reaches a
later timer.

Milestone 5 is gated by `smoke-live-qemu.sh`. Four separately built native ELFs
(`init`, supervisor, package daemon, and REPL) load from a deterministic,
content-addressed OVFS image into isolated CSpaces. The real CPL3 serial loop is
authorized by one typed control capability. Its asserted interaction rejects a
malformed command, installs one immutable package root by exact SHA-256, and
health-gates all four service-generation records before atomic activation. The
package daemon then faults in CPL3; O-core contains only that generation,
withdraws its service while `CONTROL_RECOVERING`, runs a freshly loaded package
daemon, and republishes only after the replacement's exact health token. The
gate finishes by deactivating the control plane, revoking control authority,
tearing down and reclaiming the complete scenario, observing a later timer, and
checking that QEMU remains alive. `smoke-live-semantics-qemu.sh` is the separate
mode-17 rollback, denial, stale-generation, restart, and parser corpus.

M6A is gated separately by `smoke-personality-qemu.sh` in mode 18. A
deterministic digest-pinned read-only OVFS image supplies a test client, native
personality daemon, native supervisor daemon, and unrelated observer as four
independently loaded CPL3 ELFs. Its canonical image is 62,104 bytes with
SHA-256
`f5924eeb64b5a3d332e20b5d0fae7b233ae2714eb58b72ea07f08a4d26334417`;
the gate verifies that identity, the exact four `/sbin/m6-*.elf` paths, and the
absence of their module symbols from the kernel. The supervisor health-gates
publication and chooses cancellation, one crash-driven generation restart, and
cooperative stop policy; O-core performs containment, reload, and capability
rebind as mechanism. Scalar calls cross the generic
personality router and endpoint-backed request/reply path; timeout, service
death, supervisor cancellation, stale/late/duplicate reply, and wake-once
terminal arbitration are asserted. Consumed terminals enter a 16-record exact-
handle history; this trace requires all nine records to remain present with zero
eviction, while an older evicted reply would remain denied as stale. The
supervisor also queues its fault watch before releasing the cancelled client,
making watch-before-timeout/crash an endpoint-FIFO contract. This is a bounded
scalar supervision slice, not full Milestone 6: pointer-bearing calls and
request-scoped foreign memory views remain disabled, and no Linux or other
foreign ABI is implemented.

M6B's first native mechanism slice is gated separately by
`smoke-m6b-qemu.sh` in mode 19. It creates generation-tagged request-scoped
bounded-copy views over a real kernel process/address space, with kernel-owned
staging, direction-attenuated nontransferable capabilities, snapshot input, and
written-prefix-only output commit. The fixed limits are four views, 128 bytes
per view, and 256 charged bytes total. Reply, cancellation, timeout,
service-death, process-exit, unmap, and delegated-resource terminal hooks close
the view capability before one terminal result and one wake publication. A
reply that can no longer be delivered after a later process-exit or unmap hook
is released without publishing a second result or wake. Typed revocable leases
carry the same nonzero request identity as any bound view, cover memory,
filesystem, timer, network, and device classes, and support request-wide
revocation while unrelated requests survive without ambient fallback. Lease
creation and view binding are one transaction: an injected bind failure proves
the just-published capability is revoked and the exact resource generation is
destroyed before failure returns.

That gate is a mechanism slice, not complete M6B: it is not routed through the
M6A CPL3 daemon or a public pointer-bearing personality call. It has no pinned
windows, streaming output, signal/restart integration, Linux ABI oracle, schema
fuzzing, allocation-failure matrix, or concrete filesystem, network, timer, or
device service.

Mode 24 adds a separately gated live composition without broadening that claim
to a general foreign ABI. `build-m6b-live-artifacts.sh` deterministically packs
four independently linked CPL3 ELFs into a 65,152-byte OVFS image with SHA-256
`5b9d2526da2abd75ec90b4770ded5923d856132fad736fb13f241c34f1579887`.
The client issues each bounded call once; one exact four-byte `INOUT` request
crosses the public call, view lookup/read/write, and reply syscalls. The
generation-1 daemon is fault-contained, the unrelated observer survives, and
the supervisor health-gates generation 2 before selecting pre-terminal unmap,
request-revoke, delegated-device-resource-revoke, and caller-exit dispositions
for waiting requests. Those actions do not mutate a mapping or report an
external resource event; the device resource is one internal delegated lease,
not a physical device. `smoke-live-bounded-personality-qemu.sh` does not cover
the post-reply/pre-consume process-exit or unmap race, pinned windows,
streaming, signals, Linux or Plan 9 boot, a general foreign ABI or guest agent,
KVM, PCI, DMA, IOMMU, or physical-device isolation.

The common foreign-kernel contract begins in `src/kernel_world.rs`. Strict
`ocore.kernel-world/v1` manifests put
source-integrated and binary-contained providers behind the same bounded world
identity, generation, health, export, quota, request, replacement, and
provenance rules. Verified normal-form encoding requires the inner declaration
to match package identity, health, services, and capability requests exactly.
Package-payload image bytes must match their declared SHA-256; a user-supplied
image records an expected digest and acceptance metadata but this stage does
not receive or verify those external bytes. `tests/kernel_world_contract.rs`
executes those semantic rules and the deterministic `OKWORLD1` normal-form
codec.

Mode 20's `smoke-kernel-world-qemu.sh` parses the actual embedded hash-pinned
normal form, keeps verified package and canonical manifest digests distinct,
and applies native default-deny supervisor admission keyed by exact package
digest plus copied, byte-exact request kind and purpose. It then locally seals
a generation-bound,
nonexecuting VM/vCPU/guest-page pilot graph, with aligned anonymous page
backing, quota and overlap denial, exact-world reclaim, and unrelated-VM
survival. The package remains in native `ADMITTED` state; this VM-local seal is
not a provider lifecycle transition or proof that the manifest's complete
machine and memory declaration has been fulfilled. It
does not boot a guest, enter VMX/SVM, construct EPT/NPT, execute firmware,
inject interrupts, publish provider exports, assign a device, map DMA, or
establish IOMMU isolation; see
[`docs/KERNEL_WORLD_CONTRACT.md`](docs/KERNEL_WORLD_CONTRACT.md).

Mode 21's hardware-only `smoke-kernel-world-execution-qemu.sh` adds the first
AMD SVM/NPT backend behind those same generation-bound objects. On an x86-64
host with nested SVM and writable `/dev/kvm`, a two-page real-mode synthetic
guest receives one injected vector, computes and commits a known result,
exits through `VMMCALL`, and then faults closed on an unmapped guest-physical
access. The gate tears down all NPT entries, stops and restarts the vCPU
context, revokes the world, proves the unrelated VM remains current, and
observes a host timer afterward. It is not a Linux boot, provider lifecycle,
guest-agent, service export, virtual-device, or device-assignment proof.

### Hosted Live-World reference

`o-live-host` closes the package and service control-plane loop as an
executable **hosted semantic oracle**. It installs strict manifests and payloads
into an immutable SHA-256 content-addressed store, checks exact default-deny
capability policy, starts one local child per declared service, health-gates
publication, rotates generation-bound service bearers on upgrade, rollback, and
restart, reconstructs the active set from verified digests, and composes
packaged runtime worlds through pure, boot-persistable OValues.

On Unix, every stateful `o-live-host` command holds one process-shared advisory
lock from before any reconstruction or mutation through the complete operation,
serializing cooperating CLI writers for that state directory. The direct
`HostedSupervisor` API has a second stale-writer boundary: read-only
reconstruction records the persisted monotonic active-set revision, and each
publishing activation, rollback, or service restart locks the active set,
compares that revision, and advances it atomically. A stale supervisor receives
an explicit revision conflict and must reconstruct before retrying.

Run its two-world transaction, failed-upgrade, crash-isolation, stale-bearer,
and reconstruction gate with:

```bash
./scripts/smoke-hosted-live-reference.sh
```

Those workers are host processes. This reference does not run inside booted
O-core and is not evidence for the independently gated native IPC, loader/VFS,
or live-system claims above. See
[`docs/HOSTED_LIVE_REFERENCE.md`](docs/HOSTED_LIVE_REFERENCE.md) for its exact
boundary and lifecycle CLI.

### OSTADIX Alpha native constitution

The normative target is a native, replicated, capability-governed World whose
boundary is governed membership rather than a chassis. The full-stack program,
crossing kinds, consistency model, physical OSTADIX Alpha requirements, G0--G13 gate
ladder, and explicit non-claims are pinned in
[`docs/OSTADIX_WORLD.md`](docs/OSTADIX_WORLD.md). The machine-readable
qualification registry is
[`evidence/world_alpha_gates.toml`](evidence/world_alpha_gates.toml).

Hosted World work is retained only as a simulator, differential oracle,
protocol-fuzz target, and development console under
[`docs/HOSTED_WORLD_REFERENCE_PROFILE.md`](docs/HOSTED_WORLD_REFERENCE_PROFILE.md).
It earns no OSTADIX Alpha gate credit. The current repository does not yet
provide a replicated Governor, native node-membership transport, WorldFS,
distributed HGraph execution, real Linux KernelWorld boot, physical-device
assignment and DMA/IOMMU isolation, a native Debian personality, or physical
multinode convergence. Names, inventory, and namespace lookup do not grant
authority, and aggregate node memory is never presented as coherent local RAM.

Validate the constitution and its evidence-class substitution rules with:

```bash
python3 scripts/world_alpha_evidence.py
```

The checked-in result defines 14 entries--the G0 constitutional baseline plus
13 integration gates through G13. Registry schema v4 admits typed,
content-addressed attestations and currently passes only G0 and its dependent
AArch64 QEMU/TCG gate G2; the other 12 remain defined. The executable G0
composition is [`evidence/world_contract_v2.toml`](evidence/world_contract_v2.toml):
it imports the byte-frozen
[`evidence/world_contract_v1.toml`](evidence/world_contract_v1.toml) vocabulary
and the separately versioned O-Machine contract. Unrelated
bounded gates do not become G0--G13 evidence by proximity or addition, and G2
is not physical AArch64, KVM/SVM, SMP, Linux/Plan 9 boot, foreign-ABI,
PCI/DMA/IOMMU, or device-assignment evidence.

Run the bounded shared World-identity gate with:

```bash
./ocore/kernel/smoke-world-identity-qemu.sh
```

Mode 27 gives all 20 constitutional identity atoms typed Rust and `.oc`
definitions. Its strict `OWIDENT` v1 identity-only corpus converges
byte-for-byte between Rust and native O-core under QEMU TCG. Strict decoding
rejects malformed and zero-valued records; separate hierarchical
current/reference checks reject stale generations and same-generation logical
mismatches. Serialized capability IDs are descriptive non-authority. `OWIDENT`
remains the identity-only nested format rather than a transport, OValue
envelope, or receipt codec; it implements no Governor or consensus and passes
no G0--G13 gate.

Run the bounded canonical World-protocol gate with:

```bash
./ocore/kernel/smoke-world-protocol-qemu.sh
```

Mode 28 implements the PR3 `OWPROTO` v1 record codec in Rust and `.oc`. Its
fixed 20-record, 1254-byte corpus combines two offers, one canonical v1
selection, one disjoint rejection, and all 16 identity conformance records and
converges byte-for-byte under QEMU TCG. The codec uses deterministic big-endian
framing, four fixed kinds, a 16 KiB hard maximum, caller/negotiated limits,
strict canonical decoding, and an offline function that chooses the highest
common schema and smaller record limit or an exact no-overlap rejection. It is
not a stream or network transport, live
handshake, authenticated session, authority channel, OValue/extension envelope,
receipt codec, Governor, consensus implementation, or Workstream A acceptance;
it passes no G0--G13 gate.

Run the bounded canonical World-value gate with:

```bash
./ocore/kernel/smoke-world-value-qemu.sh
```

Mode 29 implements the PR4 `OWVALUE` v1 value and full-record SHA-256 oracle in
Rust and native `.oc`. It is a separate self-framed format, not a fifth
`OWPROTO` v1 kind. The portable schema has a 4096-byte record maximum, depth-16
and 128-node limits, an explicit allowlist, canonical ordered records and
scalar-key maps, and a root-only inert versioned extension whose payload must
also be portable. Its fixed 19-record, 928-byte corpus is 1856 lowercase hex
digits with concatenated SHA-256
`264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc` and
converges byte-for-byte and by full-record SHA-256 under QEMU TCG; strict
decode/reencode rejects malformed and noncanonical data, and
hosted conversion rejects capabilities, capsules, live references, requests,
and other effectful values. This is an offline codec/hash oracle, not transport,
a live M9 crossing, authority, receipt, Governor, consensus, WorldFS,
Workstream A acceptance, or G0--G13 passage. It does not make the full hosted
`OValue` enum portable or replace the canonical-CBOR shim wire format.

Run the bounded canonical World-receipt gate with:

```bash
./ocore/kernel/smoke-world-receipt-qemu.sh
```

Mode 30 implements the PR5 `OWRECEIPT` v1 canonical receipt and signing-preimage
oracle in Rust and native `.oc`. The separate self-framed record binds bounded
descriptive World identities and generations, SHA-256 content references,
capability-right descriptions, terminal and commit fields, evidence-gate
identity, and an algorithm-tagged signature envelope. Rust and native O-core
produce the same fixed two-record, 3,239-byte corpus (6,478 lowercase hex
digits; SHA-256
`1edd90bf881cd42d08e2031482baae4e7c9a95bd78cfa65f0cbe14147c0a2604`) and
the same 1,575-byte current and 1,546-byte stale signing preimages.
The hosted oracle uses a pinned, explicitly non-secret conformance key to test
real Ed25519 signing, verification, tamper rejection, and wrong-key rejection;
native Mode 30 validates the canonical receipt and signature-envelope structure
but is not a freestanding Ed25519 verifier.

This Mode 30 corpus is constructed offline and is not evidence that another
subsystem emitted a receipt. A valid signature does not grant authority,
establish trusted signer policy, prove current World state, or enforce
replay/commit fencing. Mode 30
provides no production key lifecycle, transport, Governor, consensus, WorldFS,
typed OSTADIX Alpha attestation, Acceptance A, or G0--G13 passage, and QEMU TCG is
not physical or hardware-isolation evidence.

The separate World-project hosted-reference path now emits a live canonical
OWRECEIPT after terminal project coordination. It uses a caller-supplied
Ed25519 signer and always records
`ReceiptCommitFenceV1::Uncommitted`. Native Mode 32 consumes that emitted record
as bounded canonical lowercase hex, performs full canonical decode, exact
re-encoding, validated signing-preimage construction, requires the uncommitted
fence, and compares a domain-separated SHA-256 over the complete unsigned
canonical body with the hosted value. It also proves that a malformed envelope
clears success-only validation tags when native scratch storage is reused. The
required no-argument gate generates the hosted fixture before entering Mode 32:

```bash
./ocore/kernel/smoke-world-project-runtime-qemu.sh
# Direct caller-vector interface:
./ocore/kernel/smoke-world-project-receipt-qemu.sh RECEIPT_HEX_FILE EXPECTED_SEMANTIC_SHA256
```

Mode 32 does not execute the project or verify Ed25519 natively. This path is no
Governor admission/commit, capability or lease grant, reservation, remote
dispatch, recovery, or exactly-once protocol. QEMU TCG is not physical hardware,
and the slice passes neither G1 nor Workstream A acceptance.

The shared identity/effect/grounding foundation can also be inspected without
execution:

```bash
olangc examples/hello.O --target ir --grounding \
  --world-id desk --world-epoch 4
```

PR6 adds one hosted typed `ResourceKey` class for World, Governor, node, domain,
process, generic resource, object, descriptive capability, namespace,
task-attempt, artifact publication, device, and accelerator state. Device and
accelerator views also touch the same canonical generic resource chain, so a
trusted lowering cannot make one identity independent by changing its view.
Source `reads=`/`writes=` declarations cannot mint any governed key.

```bash
./scripts/smoke-world-resource-keys.sh
```

The World ResourceKey hosted repository-conformance gate checks the exact
vocabulary and display, underlying identity helpers' caller-pair stale/logical
comparison, HGraph state chaining, alias-aware governed/ambient projection,
source-forgery rejection, and the real CLI's residual `HostWorld` output. The
grounding report checks only the caller-supplied bound World epoch and World
membership; it does not consult a live snapshot or prove authoritative nested
generation freshness. No production lowering currently emits these
governed keys, so ordinary hosted `.O` plans retain residual `HostWorld` and
report `governed-effects none`.

This hosted gate is not O-core Mode 31, a ResourceKey wire format, native or
QEMU evidence, device assignment, DMA/IOMMU isolation, Governor authority,
Acceptance gate A, G0/G1 passage, project placement, or a World-execution
surface. `CapabilityState` is descriptive identity only and carries no grant.

### Project HGraph planning and ordered hosted execution gates

PR7 constructs real project operations from a directory or lifted
`ProjectBundle`. The direct IR planning commands below do not run project
commands:

```bash
olangc tests/fixtures/project_hgraph --target ir --route main
./scripts/o-cli.sh plan tests/fixtures/project_hgraph --route main
# After setup.sh installs the wrapper:
o plan tests/fixtures/project_hgraph --route main
```

The composite smoke checks those nonexecution properties, then compiles a
project binary and runs bounded opt-in AnySuccess short-circuit and
explicitly admitted nonzero-to-success cases in disposable workspaces:

```bash
./scripts/smoke-project-hgraph.sh
```

The typed project plan is bound to an exact bundle digest and the resolved route
policy. It builds logically separate materialization branches with prerequisite routes and
projects `MaterializeProject`, `BuildRoute`, `RunRoute`, `SelectRoute`, and,
for `verify_equivalent`, `CompareRouteResults` into a validated HGraph. Guards,
environment key names, inputs, outputs, declared effects, cancellation, and
equivalence policy remain visible in stable inspection output. The route facts
also show the `failure-continuation` contract; command strings and environment
values do not. Project-bundle format v2 carries that contract. Legacy v1 bundles
migrate only when every route omits the field, in which case all routes become
fail-closed `unproven`; a v1 document carrying the v2 field is rejected.

World PR8-1 adds a versioned project-profile `LogicalHGraphV1`. It normalizes
the exact validated plan/HGraph projection into strict canonical JSON with a
domain-separated SHA-256 identity. The schema records exact bundle/selection
binding, typed value-versus-success dependencies, project operations, route
facts including executable/evaluator/entrypoint requirements, declared
input/output paths, and the complete effect-resource vocabulary.
This is an exact-source-bound projection identity, not a whitespace-insensitive
source-semantic hash: source bytes, file modes, and manifest formatting feed the
bundle digest and therefore change the logical digest. Permissive decode plus
canonical re-encoding normalizes only the `LogicalHGraphV1` JSON record.
Unknown hosted work remains an explicit `HostWorld` read/write, and the hosted
profile emits no governed authority requirements. Planner-local logical IDs are
not World task identities.

World PR8-2 adds canonical `PlacementSnapshotV1` and `DeploymentPlanV1`
records. For policies implemented by the current hosted coordinator, the
ordinary hosted plan binds workspace/route work to `ambient_host` and
in-process work to `hosted_coordinator`; unsupported hosted policies remain
explicit `unresolved`. The hosted plan preserves residual `HostWorld` and
contains no World, task, node, domain, process, or provider identity.
Optionally, a caller may supply an exact World-epoch placement snapshot plus
one exact `TaskIdentity` per logical operation. The deterministic
single-provider reference policy checks the exact project bundle,
bundle-scoped role/path declarations, policy/runtime classes,
executable/evaluator requirements, platform/environment guards, authority
absence, and residual-`HostWorld` admission. Architecture, package, and
failure-domain constraints are schema vocabulary but are currently
unconstrained or empty in the project logical profile. The planner records
rejected providers and either emits a `proposed_provider` with an exact
node/domain/service generation and optional exact process, or remains
`unresolved`. Snapshot/provider metadata and the proposal are descriptive and
non-authorizing. They do not prove authenticated membership, freshness,
health, Governor admission, capability grant, reservation, dispatch, or actual
runtime placement. `require_current_world` checks only the supplied World
identity and epoch, not nested generation freshness. Structural/canonical
decode is not a trusted source comparison; callers must use the trusted hosted
or snapshot validator. `olangc --target ir` prints both canonical digests.
The ordinary opt-in executor binds the canonical hosted-unbound plan into trace
v5. A separate explicit hosted-reference World entry point consumes the exact
snapshot-derived plan. `HostedWorldLaunchV1` plus a caller-supplied current view
re-derive and fence the logical/deployment/snapshot bindings, exact
World/Governor, selected provider node/domain/optional-process/service and
implementation, a separate caller-supplied coordinator observation
node/domain/optional-process, a dedicated coordinator attempt, and every
operation task attempt inside `ProjectCoordinator` before schedule derivation,
workspace materialization, or child-process launch. The coordinator observer and
attempt are descriptive current-view identities, not authenticated membership.

After terminal coordination, `RuntimeGraphV1` semantically replays the trusted
project schedule and binds those exact artifact digests and identities to trace
event ordinals/outcomes and aggregate observed residual `HostWorld`;
never-started operations remain explicitly unobserved. Its neutral
`RouteSettlement` terminal distinguishes success, nonzero, and guard skip.
`execute_world_project_with_receipt` uses a caller-supplied Ed25519 signer to
emit canonical OWRECEIPT with an unconditional `Uncommitted` fence. The receipt
observation placement is the separate coordinator observer, not the proposed
provider, and the provider implementation is not mislabeled as a package.
The launch/current view, provider proposal, RuntimeGraph, and signature are
hosted-reference provenance and integrity evidence only. They do not prove
authenticated membership, Governor admission/commit, capability or lease
authority, reservation, actual remote placement, dispatch, recovery, or
exactly-once effects. Mode 32 provides the native canonical/semantic receipt
comparison documented above, not native project execution, native Ed25519
verification, physical-hardware evidence, G1, or Workstream A acceptance. G1
remains defined and unpassed.

ProjectExec-A adds a separate, opt-in hosted executor for one resolved
`Explicit` or `Default` alternative. ProjectExec-B extends it to serial ordered
`Fallback` and `AnySuccess` alternatives:

```bash
./scripts/smoke-project-hgraph-exec.sh
O_PROJECT_EXECUTOR=hgraph olangc tests/fixtures/project_hgraph_exec \
    --target script --project-trace-out project-attempt.json
```

In this mode, the validated Project HGraph governs isolated workspace
materialization, typed prerequisite ordering, route execution, and policy
selection. `ReadySchedule` gives only
`SelectRoute(fallback|any_success)` an `OrderedFirstSuccess` input policy.
`Fallback` follows resolved priority order; `AnySuccess` follows declaration
order. Both retain the attempted result prefix and stop before later branch
materialization after the first success. When the terminal alternative settles
unsuccessfully, the next alternative starts only if every route child was
guard-skipped or every route that executed in the branch, including successful
prerequisites, declares
`failure_continuation = "declared_idempotent"`. The field defaults to
`unproven`, so execution fails closed before the next branch. A failed
prerequisite remains a hard stop because this slice does not synthesize a
branch-terminal result from it. A settled nonzero route still publishes its
result and conservative
`HostWorld` successor, but not its success-completion token; infrastructure
abort publishes no route result and stops the policy. Guard skips continue to
the next alternative when no route child executed. The unsigned trace v5 binds
the canonical `LogicalHGraphV1` schema/digest and distinguishes
`SettledSuccess`, `SettledFailure`, `Skipped`, and `Aborted` and binds each run
to stable source/graph digests plus a fresh execution-attempt identifier. It
also records the assessed route prefix, proposed next route,
`no_execution`/`declared_idempotent`/`unproven_effects` evidence, and the
allow/deny decision. On this ordinary opt-in path, trace v5 additionally binds
the canonical hosted-unbound `DeploymentPlanV1` schema/digest before execution;
plan-aware replay recomputes it and rejects substitution of that exact artifact.
This ordinary trace does not bind or execute a snapshot-derived plan, attach
World identity, or turn the fresh diagnostic execution-attempt identifier into
a World `TaskIdentity`. A denied
decision is persisted before the command reports
that no route succeeded. Structural replay checks lifecycle shape only;
plan-aware replay against the trusted HGraph verifies all bindings, recomputes
the evidence and exact next branch, requires complete causally ordered
lifecycle coverage for every transitive route prerequisite, and rejects missing
decisions or execution after denial. Every complete coordinator trace passes
that semantic replay.
The compatibility runtime remains the default when `O_PROJECT_EXECUTOR` is
unset.

Materialization and route commands remain fallible `HostWorld` operations even
when a manifest says `pure=true`. Race, aggregate, equivalence, and benchmark
policies fail closed. This bounded hosted gate does not establish parallel
race/cancellation, retry, authenticated or actual remote placement, Governor
authority or commit, capability/lease enforcement, reservation, recovery,
exactly-once effects, native project execution, native Ed25519 verification,
physical hardware, G1, Workstream A acceptance, or G0--G13 passage.
`declared_idempotent` is a bundle-bound author declaration, not verified
idempotency, sandboxing, effect journaling, fencing, compensation, or an
exactly-once guarantee.

### Docker

The Dockerfile builds the hosted `O`, `olangc`, and `o-link` binaries and
packages Python 3 with the core shims. The native O-core compiler and
`o-unlink` remain part of the direct Cargo build:

```bash
docker build -t o-lang .

docker run --rm -v "$PWD:/work" o-lang examples/hello.O
docker run --rm -it o-lang --repl
# Bare directory mode literal-links and runs immediately.
docker run --rm -v "$PWD:/work" --entrypoint o-link \
    o-lang src/ -o app.O
# Use --project when only an inert route-preserving bundle is wanted.
docker run --rm -v "$PWD:/work" --entrypoint o-link \
    o-lang --project src/ -o project.O
```

The O-core QEMU proof is intended to run directly on the host because it
needs QEMU and the local Rust linker toolchain.

### What gets built

| Binary | Location | What it does |
|--------|----------|--------------|
| `O` | `target/release/O` | Runs `.O` documents and provides the interactive REPL. |
| `o` | `scripts/o-cli.sh` through an installed wrapper | Runs repository commands such as `o plan` and `o kernel`; unknown command forms retain lowercase evaluator compatibility. |
| `olangc` | `target/release/olangc` | Produces native hosted binaries, WASI modules, script execution, OIR dumps, or Graphviz DOT hypergraph export. |
| `ocorec` | `target/release/ocorec` | Compiles `.oc` modules through AST, typed HIR, and SSA MIR to freestanding ELF64 objects for the primary x86_64 and bounded AArch64 targets. |
| `o-link` | `target/release/o-link` | Recursively literal-links and runs a bare single directory; `--project` creates an inert route-preserving bundle. |
| `o-unlink` | `target/release/o-unlink` | Restores safe project bundles and legacy literal link sections. |
| `o-live-host` | `target/release/o-live-host` | Runs the hosted package-store, activation, service-supervision, and cross-world semantic oracle. |
| `o-notebook` | feature-gated Cargo binary | Runs the local notebook server when built with `--features notebook`. |
| `ostadix-mcp` | `mcp/ostadix_lang_mcp_server/target/release/ostadix-mcp` | Exposes the local agent tools above through MCP stdio; normal setup also installs `~/.local/bin/ostadix-mcp`. |
| `O` | `c_cpp/O` | Runs `.O` through the standalone C17 edition. |
| `olangc` | `c_cpp/olangc` | Produces a hosted native executable through the C17 edition. |

### Build artifacts and source-only checkout

The repository tracks source, specifications, tests, examples, the Ostadix-lang
logo, and the intentional mascot assets. It does not track compiled programs,
object files, Python bytecode, fuzz crashes, coverage output, virtual
environments, or compiler caches.

Cargo places Rust products under `target/`. The C17 edition writes `c_cpp/O`,
`c_cpp/olangc`, and `c_cpp/src/*.o`. O-core kernel objects and the linked kernel
also live under `target/ocore-kernel`. The commands in this README place direct
`olangc` and `ocorec` output under `target/` for the same reason. All of these
locations are ignored by Git.

The ignore rules cover:

- Cargo, cargo-fuzz, C, C++, CMake, linker, profiler, and WebAssembly output.
- Root-level generated Ostadix-lang command binaries and the C17 binaries.
- Python `__pycache__`, bytecode, virtual environments, test caches, type-check
  caches, lint caches, and coverage output.
- Default `o-link` output, generated extraction directories, editor state, and
  operating-system metadata.

Uppercase `.O` files are Ostadix-lang source and remain trackable. Lowercase `.o`
files are native objects. On case-folding macOS filesystems Git can treat those
patterns as equivalent, so object rules are scoped to real build directories
instead of using a global `*.o` rule.

To remove local build products without touching source:

```bash
cargo clean
make -C c_cpp clean
rm -rf fuzz/target fuzz/artifacts fuzz/coverage
```

To audit what Git is excluding:

```bash
git status --short --ignored
git check-ignore -v target/release/O c_cpp/O fuzz/artifacts/parser/crash
```

### Verifying the installation

```bash
# Rust unit and binary-target tests
cargo test --all-targets --all-features

# Release CLI contract, including olangc and ocorec object emission
cargo build --release
bash tests/test_cli.sh

# Hosted example suite
bash test_o_lang_examples.sh

# C17 edition
make -C c_cpp test

# Python reference
python3 -m tests.test_parser
python3 -m tests.test_evaluator

# Native boot proof
./ocore/kernel/smoke-qemu.sh
```

---

## Table of Contents

1. [What is new here?](#what-is-new-here)
2. [Related work and how Ostadix-lang differs](#related-work-and-how-ostadix-lang-differs)
3. [Gentle introduction](#gentle-introduction)
4. [Quickstart](#quickstart)
5. [Hosted language tour](#hosted-language-tour)
6. [OValue and the runtime boundary](#ovalue-and-the-runtime-boundary)
7. [Hosted backends](#hosted-backends)
8. [Compiler and composition tools](#compiler-and-composition-tools)
9. [Architecture](#architecture)
10. [O-core native systems language](#o-core-native-systems-language)
11. [Running the tests](#running-the-tests)
12. [Status](#status)
13. [Citation and authorship](#citation-and-authorship)

---

## What is new here?

Most languages make one or all of these assumptions:

* A program is written in one language.
* When you call another language you use an FFI, a bridge bolted on the side.
* The language a piece of code belongs to is determined by the file it sits
  in, or by a special import or escape mechanism.
* Native systems code and orchestration code must share one intermediate
  representation even though they have different semantics.

Ostadix-lang breaks all four assumptions. Here are the five ideas that make it
different.

### 1. Typed parentheses: the language is in the syntax

In every ordinary language, parentheses are anonymous. `(x + y)` is grouping;
nothing about the parentheses tells you what evaluator will handle the
contents.

Ostadix-lang gives parentheses a *type*: the identifier before `^(` names the
evaluator, and the matching `)_IDENT` closes it.

```O
python^( 6 * 7 )_python
html^( <b>hello</b> )_html
markdown^( **bold** )_markdown
nix^( builtins.nixVersion )_nix
sql^( SELECT 40 + 2 AS answer; )_sql
```

These are not escape sequences inside another language. They are first-class
expressions, and they nest freely:

```O
html^(
  <p>Count: python^( sum(range(10)) )_python</p>
)_html
```

The Python expression is evaluated first, its result is converted to
something HTML can embed, and then the HTML expression completes. **No
pairwise FFI. No template bridge. The nesting is the interface.**

### 2. OValue: the universal exchange type

When Python produces `42` and HTML needs to embed it, something has to cross
the boundary. In Ostadix-lang that something is always an `OValue`, a tagged union
that every backend speaks.

```text
ONull | OBool | ONumber | OText | OChar | OHtml
OList | OMap | OSeq | OObject | OEntriesMap | OSet
OSymbol | OKeyword | OScope | OBlob | OBytes | OGraph | OExpr
ONixExpr | ODerivation | OStorePath | OSystem | ONative
ORequest | OThunk | OGroup | OError | OCapability | OSnapshot
```

The critical insight is that **the receiving language decides how to render a
foreign value, not the sending language**. This is the `render_child`
operation: each backend knows how to turn OValue into its own source syntax.

```text
HTML.render_child(OBlob(png, "image/png"))
  -> <img src="data:image/png;base64,...">

HTML.render_child(OList([OText("a"), OText("b")]))
  -> <ul><li>a</li><li>b</li></ul>

Python.render_child(ONumber::Int(42))
  -> 42
```

With N languages and this single protocol, interoperability costs O(N) code,
one renderer per language, instead of O(N squared) bridges between every
pair. The canonical exchange form is explicit and inspectable rather than
hidden in a compiler pass.

### 3. Explicit persistent environments

Bare hosted expressions are ephemeral. They receive a fresh environment for
that expression and are cleaned up afterward:

```O
python^( x = 10 )_python
python^( x )_python             # x is not retained
```

Persistence is explicit through the environment index:

```O
python[0]^( x = 10 )_python[0]
python[1]^( x = 99 )_python[1]
python[0]^( x * x  )_python[0]  # 100
```

The number in brackets is an environment index. State, imports, functions,
and backend-owned resources survive for the life of the evaluator for every
expression that names the same `(language, index)` pair. Different indices
are isolated from one another.

This gives Ostadix-lang notebook-like state without making the notebook's one global
namespace an invisible part of the language.

### 4. Homoiconicity across languages

Lisp is famous for homoiconicity: code and data have the same shape, so a
program can inspect another program, transform it, and evaluate it. Ostadix-lang
generalizes that idea across multiple languages.

The `quote^` backend captures an O expression as `OExpr` without evaluating
it:

```O
let q = quote^( python^( 6 * 7 )_python )_quote
```

Python can receive `q` as a live `OExprValue` and evaluate it through the
current O evaluator:

```O
python[0]^(
result = O.eval(q)
)_python[0]
```

Python can also construct O source and parse it back into an expression:

```O
python[0]^(
src = "python^(2 ** 10)_python"
result = O.eval(O.quote(src))
)_python[0]
```

The language boundary is not a barrier to metaprogramming. An O expression
can be constructed in one language, moved as data through another, and
evaluated by its named backend later.

`O.eval` uses a lexical snapshot of the O scope visible at the backend call
site. A quoted fragment can read caller `let` bindings, including bindings
created earlier inside the current typed expression. New `let` bindings inside
the fragment remain local to that callback and do not mutate the caller.

Scope capture can also be explicit. `scope()` returns a detached OScope value,
and the two-argument form chooses it instead of the callback-site scope:

```O
let answer = python[2]^(41)_python[2]
let captured = scope()
let answer = python[2]^(99)_python[2]
let q = quote^(python[1]^($answer + 1)_python[1])_quote
python[0]^(O.eval($q, $captured))_python[0]  # 42
```

Python can also call `O.scope()` to capture the current O bindings or
`O.scope({"name": value})` to construct a restricted scope explicitly.

### 5. Orchestration and machine computation have different IRs

Ostadix-lang does not force every kind of computation into the same abstraction.

Hosted `.O` programs lower into OIR. OIR names text, loads, stores, builtin
calls, backend execution, structural dependencies, sequencing dependencies,
and data dependencies. It is the correct representation for scheduling
polyglot work.

Native `.oc` programs lower into typed HIR and then SSA MIR. MIR has typed
values, mutable places, basic blocks, phi nodes, calls, branches, memory
operations, intrinsics, and assembly. It is the correct representation for
machine code.

```text
.O  -> ONode -> OIR -> ExecutionPlan -> hosted evaluators
.oc -> AST -> typed HIR -> SSA MIR -> target ELF64 object
```

This is the point where Ostadix-lang becomes both a polyglot meta-language and a
systems programming language without pretending those are the same problem.

---

## Related work and how Ostadix-lang differs

Ostadix-lang sits at the intersection of language-oriented programming, polyglot
execution, metaprogramming, workflow systems, and native systems languages.
The one-sentence thesis is: **the evaluator is named by the delimiter shape,
so language choice becomes a structural property of an expression at any
nesting depth, and distinct runtimes exchange values through OValue while
native computation remains in a separate typed compiler pipeline.**

**Racket `#lang` and language-oriented programming.** Racket pioneered the
idea that a program declares its own language and that defining new languages
should be cheap. The differences are granularity and substrate. `#lang` is a
module-level declaration, and its languages ultimately run through Racket.
Ostadix-lang places the language tag at expression level and dispatches to separate,
real runtimes such as CPython, Nix, Node.js, Rust, SQLite, Racket, and others.
Racket unifies languages through a common host. Ostadix-lang keeps the evaluators
distinct and unifies the values that cross between them.

**Polyglot notebooks and literate programming.** Jupyter, .NET Interactive,
and Org-mode Babel let top-level cells use different languages. Ostadix-lang makes a
language block an expression inside the AST. A Python expression can occur
inside HTML which occurs inside another Python expression. The boundary is a
composable node, not only a cell delimiter. The local O notebook is one UI
over this evaluator, not the definition of the language.

**Staged metaprogramming.** Lisp quotation, MetaOCaml, Template Haskell, and
Terra all make code available as data. Ostadix-lang's generalization is across
backend languages. `OExpr` carries O syntax, `quote^` captures it, and
`O.eval` re-enters the active evaluator.

**String-embedded DSLs.** Heredocs, JSX, tagged templates, and SQL strings
usually leave the embedded language opaque to the host. Ostadix-lang parses the
typed-expression boundary into its AST, evaluates the named backend, and
returns an OValue that the surrounding expression consumes as a first-class
atom.

**Workflow engines.** Deferred requests, content fingerprints, execution
plans, groups, and autonomous scheduling make Ostadix-lang capable of expressing
workflow topology. The difference is that these control values live in the
same value system as the language results. `batch`, `all`, `any`, and `race`
are not external scheduler configuration; they are O expressions.

**The claim, stated exactly.** Individual prior systems provide pieces of
this design. Eco composes languages through arbitrarily nested language boxes,
but it is primarily a language-composition editor. PyHyp provides executable
fine-grained nesting for one specifically engineered language pair. GraalVM
standardizes interoperability across languages implemented within or exposed
through its polyglot substrate. Ostadix-lang's novelty is a *conjunction* of
properties rather than any single one:

1. **Expression-granular evaluator selection.** The evaluator is chosen at the
   level of an individual expression, not a file, module, or cell.
2. **Recursive nestability.** Evaluator expressions may contain other
   evaluator expressions to arbitrary depth.
3. **A registry-extensible evaluator family.** The set of evaluators is a
   compile-time-extensible registry, not a fixed pair. Adding an evaluator
   means adding a registry entry and rebuilding; registration is static, not
   a runtime plugin system.
4. **Independent runtimes.** Evaluators are independently implemented real
   runtimes such as CPython, Node.js, Nix, SQLite, Racket, and rustc, not
   reimplementations on one shared substrate.
5. **A common value domain.** Results cross boundaries through OValue, a
   single language-neutral value type, instead of pairwise foreign-function
   interfaces.
6. **Global lowering.** The full heterogeneous computation lowers into one
   execution representation (OIR and its value-flow graph) for whole-program
   scheduling and analysis.

To our knowledge, Ostadix-lang is the first general-purpose programming system
to combine expression-granular recursive evaluator composition across a
registry-extensible family of independently implemented runtimes with a
language-neutral value boundary and source-derived lowering into a unified
whole-program execution representation. The current Rust implementation
derives, validates, analyzes, and executes a directed state-complete HGraph.
Arbitrary hosted effects remain conservative: ordinary unknown operations are
serialized through `HostWorld`, persistent evaluator state is carried by
actor-state tokens, and strict-equivalent worker dispatch is restricted to
compiler-verified O-scope loads plus source-proven-preparable trees of four
trusted pure inline renderers (`html`, `markdown`, `text`, and `latex`). Direct
ephemeral members of an explicitly autonomous group may separately opt into
non-strict, unordered host effects. Evidence schema v3 binds those choices and
their dispatch semantics to stable preparation-adapter IDs before execution. A
fixed-size local pool is created only when a graph run contains admitted
`LocalWorker` operations, reuses its threads across readiness frontiers, and
reports completions individually; a coordinator-only graph creates no worker
pool. This does not admit arbitrary hosted code or make pool capacity an
evidence-backed CPU or memory reservation.
The serial OIR executor remains the differential oracle. Wherever a
prior system satisfies part of this
conjunction, the paragraphs above and below say so; corrections and closer
prior art are welcome as issues.

**Systems languages.** C, Rust, Zig, and freestanding subsets of other
languages already compile kernels. O-core's distinct point is its placement
inside Ostadix-lang's two-level model. Hosted O can generate, compose, build, boot,
and inspect native O-core while O-core itself stays free of the hosted runtime
and its dependencies.

Ostadix-lang is now an implemented toolchain rather than only an organizing idea.
The repository contains the parser, evaluator, OValue protocol, persistent
process registry, OIR and execution planner, scheduler and disk cache, real
hosted backends, native and WASI packaging, linker and unlinker, notebook,
static O-core front end, SSA lowering, primary x86_64 and bounded AArch64 object
generation, freestanding runtime, and bounded QEMU boot evidence. The current
boundaries are documented at the end of this README as concrete engineering
scope, not as placeholders for features that already exist.

---

## Gentle introduction

*This section is for readers who are new to programming languages as objects
of study. You do not need prior experience with compilers, interpreters, or
kernel development. You need only curiosity.*

### What is a programming language, really?

When you write `2 + 2` in Python and run it, something has to interpret those
characters and produce the number `4`. That something is an evaluator. Every
programming language is, at bottom, a pair of things:

1. **Syntax**, the rules about what text is a valid program.
2. **Semantics**, the rules about what a valid program does.

Most of the time, you pick one language and use its evaluator for the whole
file.

Ostadix-lang changes the unit at which that choice is made. The evaluator belongs to
the expression:

```O
python^(
1 + 1
)_python
```

Read this as: "evaluate this body in Python." The opener and closer are a
matched pair. Everything between them is Python source.

### Nested expressions

Now place a Python block inside HTML:

```O
html^(
  <h1>The answer is python^(
6 * 7
)_python!</h1>
)_html
```

The evaluator works inside-out, leaves before roots, like arithmetic. Python
produces `42`. The HTML backend receives the value, renders it as HTML-safe
content, and produces:

```html
<h1>The answer is 42!</h1>
```

No string interpolation library is needed. The nesting is the template.

### Naming values

```O
let answer = python^( 40 + 2 )_python

python^(
$answer + 1
)_python
```

The first expression binds an ONumber integer to `$answer`. The receiving
Python backend renders that number as the Python literal `42`, so the second
block evaluates `42 + 1`.

### Persistent state when you ask for it

```O
python[0]^(
import random
random.seed(42)
samples = [random.gauss(0, 1) for _ in range(500)]
)_python[0]

python[0]^(
round(sum(samples) / len(samples), 4)
)_python[0]
```

The `[0]` is what makes the Python process persistent. A bare Python block is
single-use. This distinction keeps state visible in the source.

### Native computation

The hosted language is about composing evaluators. O-core is what you use
when the computation itself must become freestanding machine code:

```ocore
module example;

struct Point {
    x: i64,
    y: i64,
}

fn sum(point: *const Point) -> i64 {
    unsafe {
        return (*point).x + (*point).y;
    }
}
```

The source is parsed, resolved, statically checked, lowered through typed HIR
and SSA MIR, and emitted as an ELF object. There is no backend interpreter in
the resulting target code.

---

## Quickstart

### Run a hosted O program

```bash
cargo build
cargo run -- examples/hello.O
```

### Use the REPL

```bash
cargo run -- --repl backends
```

The REPL keeps O-level `let` bindings and explicit backend environments alive
between entries. It supports multiline typed expressions, history, scope
inspection, reset, and terminal-aware output.

### Use the local notebook

```bash
cargo run --features notebook --bin o-notebook -- backends
```

The notebook listens on `127.0.0.1:8888`, opens a local browser, and keeps one
evaluator session across cells. It renders HTML and image OValues directly,
supports cell reordering and run-all, saves and loads notebook JSON, and can
restart the evaluator state.

### Compile hosted O

```bash
cargo run --bin olangc -- examples/hello.O -o target/hello
./target/hello

cargo run --bin olangc -- examples/hello.O --target script
cargo run --bin olangc -- examples/hello.O --target ir
cargo run --bin olangc -- examples/hello.O --target dot
cargo run --bin olangc -- examples/hello.O --target dot | dot -Tpng -o graph.png
```

### Compile O-core

```bash
cargo run --bin ocorec -- kernel.oc --emit hir -o -
cargo run --bin ocorec -- kernel.oc --emit mir -o -
cargo run --bin ocorec -- kernel.oc --emit obj --keep-asm -o target/kernel.o
```

### Link source into one O document

```bash
# Explicit files retain the sequential typed-block linker.
cargo run --bin o-link -- calc.py page.html app.O -o target/program.O
cargo run -- target/program.O

# Safe project lifting is explicit; the resulting bundle is inert.
cargo run --bin o-link -- --project src/ -o target/project.O
cargo run --bin o-link -- --list-routes target/project.O

cargo run --bin o-unlink -- target/project.O -o target/restored/
```

---

## Hosted language tour

### Typed expression syntax

```text
LANG^( body )_LANG
LANG[n]^( body )_LANG[n]
LANG{lazy}^( body )_LANG{lazy}
LANG[n]{defer}^( body )_LANG[n]{defer}
```

The opener and closer must match exactly as written. The language name must be
registered. An identifier that is not a registered language remains ordinary
text even when followed by `^(`, which prevents inner-language operators from
being mistaken for O syntax.

The parser recognizes backslash escapes for literal O openers, closers, and
splices. Inside a Bash block, write `\$PATH` when you want the backend to
receive the literal shell expression `$PATH` rather than an O-level splice.

#### Aliases

| Alias | Canonical language |
|-------|--------------------|
| `py` | `python` |
| `md` | `markdown` |
| `tex` | `latex` |
| `plain` | `text` |
| `o` | `O` |

Aliases retain their source spelling in the closer but resolve to the same
backend and environment namespace.

#### Shebang support

Executable O documents may begin with:

```text
#!/usr/bin/env o
```

The interpreter, compiler, linker, and unlinker handle the shebang as part of
the source-file workflow.

### `let` bindings and `$var` splicing

```O
let name = LANG^( ... )_LANG
```

The expression is evaluated and its OValue is stored in the O-level scope.
When `$name` appears inside another expression, the receiving backend renders
that OValue in its own syntax.

```O
let answer = python^( 40 + 2 )_python
html^( <p>The answer is $answer.</p> )_html
```

### Python result rules

A Python block chooses its result in this order:

1. The value assigned to `__oval_result__`.
2. The value of the final bare expression.
3. Captured stdout when neither of the first two produces a value.

```O
python^( 6 * 7 )_python
python^( print("hi") )_python
python^( __oval_result__ = 99 )_python
```

Python values are converted recursively into OValue, including booleans,
integers, floats, strings, lists, maps, bytes, HTML, store paths, expressions,
and image blobs.

### `O^(...)_O` sequencing

The `O` backend is the structural document host. It evaluates children from
left to right and returns the last non-null value:

```O
O^(
  python[0]^( x = 10 )_python[0]
  python[0]^( x * x  )_python[0]
)_O
```

Because `O` controls child evaluation directly, it is implemented as an
inline AST backend rather than as a subprocess shim.

### Environment lifetime

```O
python^( x = 40 )_python
python^( x + 2 )_python        # fresh environment, x is absent

python[0]^( x = 40 )_python[0]
python[0]^( x + 2 )_python[0] # 42
python[1]^( x )_python[1]     # isolated environment
```

The Rust runtime uses an internal ephemeral environment identifier for bare
blocks and destroys that backend process after the expression. An explicit
numeric identifier names a persistent `(language, environment)` process.

### Lazy and deferred blocks

`{lazy}` and `{defer}` capture backend evaluation as a first-class Request:

```O
let cached = html{lazy}^(<p>stable</p>)_html{lazy}
let effect = python{defer}^(import time; time.time())_python{defer}

let a = now($cached)
let b = now($effect)
```

- `{lazy}` is accepted only for backends marked pure. Its forced result is
  cached by the request fingerprint.
- `{defer}` is accepted for any backend. It is never result-cached and runs
  again each time it is forced.
- `lazy(expr)` evaluates its argument under the lazy policy.
- `now(value)` forces a Request or coordination Group.
- Splicing a `{lazy}` Request forces it before rendering. Splicing a `{defer}`
  Request is rejected because an implicit splice must not silently repeat an
  effect; use `now()` when that force is intentional.

Purity is centralized in the backend registry rather than inferred from the
language name at every call site.

### Backend authority is ambient by default

Hosted source runs as normal Ostadix-lang execution, so hosted backends receive every
grantable backend right by default: `fs_read`, `fs_write`, `network`, and
`process`. A plain block can use the host as directly as the same user could
from Python, Bash, Nix, or another supported language:

```O
python^(
import os
__oval_result__ = os.system("printf host-accessible")
)_python
```

The older `cap=...` and per-right block attributes are still parsed for
compatibility with existing source and for embedding-specific experiments, but
ordinary O programs do not need host-launched backend grants. `--backend-grant`
remains accepted by `O` and `olangc`; it is no longer the happy path for backend
access.

Persistent process identity includes the complete authority policy, so process
reuse cannot cross policies.

Some adapters must invoke a target runtime or compiler to implement the block
at all. Those required rights are part of the backend interface embedded in
OIR. Bash and shell require `process`; compiled-language adapters require
`fs_write` and `process`; Nix execution requires all four rights. These rights
are available through the default backend authority. `olangc --target ir` prints
the required authority set so it is inspectable before execution.
Unregistered shim interfaces default to all four required rights, and public
OIR execution rejects an embedded backend interface that weakens the registry
policy.

### `quote^` and `O.eval`

```O
let q = quote^(
  python^(6 * 7)_python
)_quote

python[0]^(
O.eval(q)
)_python[0]
```

`quote^` is a structural backend. It reconstructs the enclosed O source into
an OExpr without evaluating its children. The Python shim represents OExpr as
a live `OExprValue`; `O.eval` sends an evaluator callback over the same IPC
channel and receives the resulting OValue. `O.eval(q)` uses the call-site
snapshot. `O.eval(q, snapshot)` requires an OScope and uses that explicit
lexical root.

### The Nix lattice

Ostadix-lang models the Nix and NixOS path as a value chain:

```text
nix_expr^(...)_nix_expr -> ONixExpr
instantiate($expr)       -> ODerivation
realise($drv)            -> OStorePath
activate($path)           -> OSystem by real activation
dry_activate($path)       -> OSystem by dry activation
activate($cap, $path)     -> OSystem by real activation with an embedding guard
```

`nix^` remains the immediate evaluation form. `nix_expr^` captures Nix source
and its dependencies without evaluating it. `instantiate` uses `nix eval` to
obtain a derivation, `realise` uses `nix build`, and `activate` invokes the
closure's `switch-to-configuration switch` entry point.

`activate(path[, profile])` performs a real host switch using the same ambient
authority available to this process from a shell. `dry_activate(path[, profile])`
uses `switch-to-configuration dry-activate`. If a host passes a live
`system_activation` OCapability as the first argument, O treats it as an
embedding guard: the capability is bound to one profile, checked when the
request is built, checked again when it is forced, and can be revoked.

`current_system()` returns the current profile as a referential OSystem value.

### Autonomous scheduling

Inside `autonomous(...)`, schedulable Nix requests and dry activations are
buffered. At a force point, the scheduler constructs their dependency graph,
executes ready work concurrently up to its parallelism limit, and writes safe
results to memory and disk caches. Eval requests and real activation stay on
the evaluator thread because they require live process state or mutate the host
profile.

```O
let result = autonomous(
  batch(
    realise(instantiate($one)),
    realise(instantiate($two))
  )
)
```

Eval requests remain on the evaluator thread because the live process
registry is not Send. This preserves persistent backend state while still
parallelizing the Nix operations that can safely run on worker threads.

The main graph executor follows the same ownership boundary. It represents
unknown host effects with `HostWorld` state and persistent environments with
actor-state versions, but executes shim operations on the evaluator owner
thread. Its local pool accepts only compiler-verified O-scope loads through
`o-scope-load/v1` and source-closed `html`, `markdown`, `text`, and `latex`
renderers through `trusted-inline-renderer/v1`; `coordinator/v1` retains every
other operation. These IDs are evidence-bound adapter selections, not names
rediscovered by the scheduler at dispatch time.

### Coordination groups

Groups make execution topology part of the value model:

```O
let bundle = batch($a, $b, $c)
let results = now($bundle)

let required = all($a, $b)
let fallback = any($primary, $secondary)
let fastest = race($left, $right)
```

| Form | Meaning | Result |
|------|---------|--------|
| `batch(a, b, ...)` | Run every member for throughput. | OList containing every result; ordinary failures become OError values. |
| `all(a, b, ...)` | Require every member to succeed. | OList on success; the group fails on the first error. |
| `any(a, b, ...)` | Try members as fallbacks. | The first successful value; fails only when all members fail. |
| `race(a, b, ...)` | Take the first member to settle. | The first success or failure. |

Group construction is capture-oriented by definition. Under eager evaluation,
nested request chains are captured lazily inside the group instead of being
resolved before the topology is built. Inside `autonomous(...)`, the constructor
preserves Autonomous policy, so captured request chains are also buffered for
the scheduler. Member order is significant and is part of the group
fingerprint.

After an autonomous scheduler flush, group members resolve through strict cache
reads. A strict cache miss means the scheduler failed to materialize buffered
work and remains a hard error, even for `batch`; normal Fresh-mode member
failures are the ones represented as OError values.

### Builtin call reference

| Call | Input to output | Description |
|------|-----------------|-------------|
| `instantiate(expr)` | ONixExpr to ODerivation | Instantiates a Nix derivation. |
| `realise(drv)` | ODerivation to OStorePath | Builds the default derivation output. |
| `activate(path[, profile])` | OStorePath to OSystem | Performs a real host switch. |
| `dry_activate(path[, profile])` | OStorePath to OSystem | Runs `dry-activate` without switching. |
| `activate(capability, path[, profile])` | OCapability and OStorePath to OSystem | Performs a real switch after validating an embedding-specific profile guard. |
| `current_system()` | none to OSystem | Returns the current system profile reference. |
| `scope()` | current O bindings to OScope | Captures a detached lexical snapshot for explicit evaluation. |
| `lazy(expr)` | any to ORequest or value | Evaluates under the lazy policy. |
| `now(req)` | ORequest or OGroup to OValue | Forces deferred work. |
| `autonomous(expr)` | any to OValue | Buffers and schedules requests that do not require evaluator-local state. |
| `batch(...)` | values or Requests to OGroup | Captures throughput topology. |
| `all(...)` | values or Requests to OGroup | Captures an all-success barrier. |
| `any(...)` | values or Requests to OGroup | Captures ordered fallback topology. |
| `race(...)` | values or Requests to OGroup | Captures first-settlement topology. |

---

## OValue and the runtime boundary

OValue is both the inter-language exchange type and the boundary between pure
data, live references, and authority-bearing values.

| OValue | Meaning |
|--------|---------|
| ONull | Absence of a result. |
| OBool | Boolean true/false. |
| ONumber | Supports arbitrary-precision integers, exact rationals, and binary floats; the legacy OInt alias is retained for wire compatibility. |
| OText | Text with explicit encoding metadata. |
| OChar | A single Unicode scalar value. |
| OHtml | Trusted HTML fragment, kept distinct from escaped text. |
| OList, OMap | Recursive heterogeneous containers. Map keys are strings. |
| OSeq, OObject, OEntriesMap, OSet | Richer structural collections used by the canonical value model. |
| OSymbol, OKeyword | Interned symbolic identifiers and keyword values. |
| OScope | Detached O-level lexical bindings for `O.eval(expr, scope)`. |
| OBlob | Base64 wire data with a MIME type. |
| OBytes | Structural byte value. |
| OGraph | Value graph frame for values with shared identity or cycles. |
| OExpr | Unevaluated O source captured by `quote^`. |
| ONixExpr | Unevaluated Nix source plus dependencies and a fingerprint. |
| ODerivation | Instantiated Nix derivation and output metadata. |
| OStorePath | Realized Nix store path. |
| ORequest | Deferred computation with a compositional fingerprint. |
| OThunk | Captured backend body and dependencies for Eval requests. |
| OGroup | Explicit batch, all, any, or race topology. |
| OError | Captured failed outcome used by batch results. |
| OSystem | Live reference to a system profile. |
| OCapability | Authority-bearing reference to a resource. |
| OSnapshot | Inert captured world state suitable for persistence. |
| ONative | Same-backend native capsule with explicit rehydration policy. |

Legacy wire tags `int`, `float`, and `str` are still accepted for hosted IPC
compatibility, but they deserialize into `ONumber` and `OText`. New runtime code
emits the canonical variants.

The runtime classifies values into three groups:

- **Pure values** are serializable, replayable, cacheable when their contents
  are cache-safe, and suitable for persistence.
- **Referential values** name live world objects whose state can change.
  OSystem identity is the profile reference, not a frozen system state.
- **Effectful values** carry authority, scope, or orchestration semantics.
  Requests, groups, errors, scopes, and capabilities require explicit treatment by caches,
  schedulers, and persistence layers.

Every OValue has a tagged schema that can be serialized for hosted IPC. The
backend transport is length-prefixed canonical CBOR, not JSON text. That fact
does not make every OValue safe to replay. `is_cache_safe`, `is_replay_safe`,
and `is_boot_persistable` enforce the distinction in the Rust value layer.

Representative wire values are:

```json
{"t":"null"}
{"t":"int","v":42}
{"t":"str","v":"hello"}
{"t":"blob","v":"<base64>","mime":"image/png"}
{"t":"expr","src":"python^(6 * 7)_python"}
{"t":"scope","bindings":{"answer":{"t":"int","v":42}}}
{"t":"nix_expr","body":"...","deps":[],"fingerprint":"..."}
{"t":"request","kind":"instantiate","source":{"t":"nix_expr","body":"...","deps":[],"fingerprint":"..."},"fingerprint":"..."}
{"t":"group","mode":"batch","members":[],"fingerprint":"..."}
{"t":"capability","kind":"service","identity":"ocore-live:...","metadata":{}}
{"t":"snapshot","kind":"system","identity":"generation-42","state":{}}
{"t":"error","msg":"member failed"}
```

### OValue and the TCF terminal object

The TCF connection is now stated precisely. Fix a behavior space `Beh` and
form the representation category `Set/Beh`. An object is a carrier together
with a map into `Beh`. Its terminal object is `(Beh, id)`, because every
representation has exactly one behavior-preserving map into behavior itself.

OValue realizes that terminal object relative to the observation theory used
at an O boundary. For the supported fragment in which two closed, terminating
computations are equivalent exactly when they return the same OValue, take
`Beh_O = OValue`. Each backend's OValue lifting map is then its unique arrow to
the terminal carrier.

The terminal-object statement applies to backend-to-OValue lifting, not to
every `render_child` projection back into source. Rendering is deliberately
consumer-specific and some consumers only have a presentation or marker for a
value. The implemented matrix is:

| OValue family | Python | Nix | HTML | LaTeX | Markdown | Default |
|---------------|--------|-----|------|-------|----------|---------|
| Null, bool, number | T | T | P | P | P | S |
| Text | S | T | P | P | P | S |
| Char, bytes, symbol, keyword | S | S | P | P | P | S |
| HTML, store path, expr, derivation, system | T | S | P | P | P | O |
| Blob | S | S | P | P | P | O |
| NixExpr | T | T | P | P | P | O |
| List, map, seq, set, object | T | T | P | P | P | S |
| EntriesMap | S | S | P | P | P | S |
| Scope | T | O | O | O | O | O |
| Graph, native | T | S | O | O | O | O |
| Thunk | T | O | O | O | O | O |
| Error | T | O | P | P | P | O |
| Request, capability, snapshot, group | T | O | O | O | O | O |

`T` means the consumer syntax preserves the O-level type, `S` means the
payload or structure survives but its O tag does not, `P` means an intentional
human-facing presentation, and `O` means an opaque marker or summary. Container
fidelity is bounded by the least faithfully rendered child. The Rust
`RenderFidelity` match and its exhaustive matrix test cover every current
OValue variant and every renderer. Adding a value or renderer requires the
classification to be updated.

Python closes its non-native cells with `OOpaqueValue`, a lossless handle over
the complete tagged wire value. It can pass requests, capabilities, snapshots,
groups, and other O-specific values back across the boundary without reducing
them to display strings. The handle does not mint authority; a capability
identity still has to resolve in the evaluator's private live table.

This is deliberately not a claim that ordinary OValue equality is already
fully abstract for every observable fact about a program. OExpr preserves
source, OCapability preserves authority, OScope preserves a namespace, and an
ordinary returned value does not encode divergence, timing, or a complete
effect trace. Extending the result to full O semantics requires an observation
carrier that includes effects and divergence, followed by a proof that its
equality is exactly the intended behavioral equivalence. The OValue enum has a
finite set of registered variants, but its carrier is not finite because
strings, blobs, lists, maps, expressions, and scopes are unbounded.

OCapability is descriptive on the ordinary hosted wire. A serialized identity
does not become kernel authority by being parsed. The O-core capability bridge
requires that identity to already be bound inside a live authenticated kernel
session before it can resolve to a generation-tagged kernel handle.

---

## Hosted backends

The Rust runtime currently registers the following languages. Inline backends
run inside the evaluator. Hosted backends run as Rust backend processes through
length-prefixed canonical CBOR IPC and require their local runtime to be
installed. A few compatibility adapters still bridge to legacy Python code for
semantics that are not a plain external command, such as live Python `O.eval`.

| Tag | Runtime or handler | Behavior |
|-----|--------------------|----------|
| `O` | inline AST | Sequences child expressions from left to right. Alias: `o`. |
| `quote` | inline AST | Captures child source as OExpr without evaluating it. |
| `html` | inline value | Returns OHtml and renders image blobs as data URL images. |
| `markdown` | inline value | Returns spliced Markdown text. Alias: `md`. |
| `latex` | inline value | Returns spliced LaTeX text. Alias: `tex`. |
| `text` | inline value | Returns plain spliced text. Alias: `plain`. |
| `nix_expr` | inline value | Captures deferred Nix source and dependencies as ONixExpr. |
| `python` | Rust backend bridge to CPython | Executes Python, preserves explicit environments, converts native values, and supports `O.quote` and `O.eval`. Alias: `py`. |
| `nix` | Rust backend runner plus Nix CLI | Evaluates Nix expressions and converts JSON results to OValue. |
| `nix_store` | Rust backend runner plus Nix CLI | Realizes derivations and returns OStorePath. |
| `nixos_test` | Rust bridge to Nix test-driver adapter | Runs NixOS VM test expressions. |
| `bash` | Rust backend runner plus Bash | Executes Bash with scalar O bindings exported as environment variables. |
| `shell` | Rust backend runner plus POSIX `sh` | Executes portable shell source with scalar bindings. |
| `rust` | Rust backend runner plus `rustc` | Compiles a temporary Rust program, runs it, and returns stdout. |
| `racket` | Rust backend runner plus Racket | Executes a temporary Racket module and returns stdout. |
| `cpp` | Rust backend runner plus `g++` | Compiles C++17 source, runs it, and returns stdout. |
| `c` | Rust backend runner plus `cc` | Compiles C17 source, runs it, and returns stdout. |
| `csharp` | Rust backend runner plus .NET or Mono | Builds and runs C# with the locally available toolchain. |
| `haskell` | Rust backend runner plus `runghc` or `ghc` | Interprets or compiles Haskell and returns stdout. |
| `lisp` | Rust backend runner plus SBCL or CLISP | Executes Common Lisp source. |
| `common_lisp` | Rust backend runner plus SBCL or CLISP | Executes Common Lisp source. |
| `sql` | Rust backend runner plus SQLite CLI | Executes SQL against a persistent SQLite database per environment. |
| `ruby` | Rust backend runner plus Ruby | Executes Ruby with scalar O bindings rendered as local values. |
| `matlab` | Rust backend runner plus Octave or MATLAB | Executes MATLAB-compatible source and returns stdout. |
| `mathematica` | Rust backend runner plus WolframScript | Executes Wolfram Language source and returns stdout. |
| `webassembly` | WABT plus Wasmtime or Wasmer | Compiles WAT when needed and executes the resulting WebAssembly module. |
| `java` | Rust backend runner plus `javac` and `java` | Compiles and runs a Java class. |
| `javascript` | Rust backend runner plus Node.js | Executes JavaScript with O bindings injected as constants. |
| `ocaml` | Rust backend runner plus OCaml toolchain | Interprets or compiles OCaml and returns stdout. |

These are executing backends, not parse-only registrations. A missing target
runtime produces an explicit backend error. The default example suite
exercises Python, Bash, POSIX shell, JavaScript, SQL, HTML, Nix-independent
orchestration, and the structural backends. Backends requiring optional local
toolchains are available when those toolchains are installed.

Compatibility shim resolution for a language `<lang>` searches:

```text
<shim-dir>/<lang>_shim.py
<shim-dir>/<lang>_shim
<shim-dir>/<lang>.py
<shim-dir>/<lang>
```

Adding another hosted language requires a Rust backend adapter that handles
`exec` and `cleanup`, a backend registry entry describing purity and rendering,
and a registered parser tag. A language with structural evaluation semantics can
instead use an inline AST handler like `O` and `quote`.

---

## Compiler and composition tools

### `O`: interpreter and REPL

```bash
O program.O [backends_dir]
O --repl [backends_dir]
```

With a file, `O` strips an optional shebang, parses the document, evaluates it,
and prints the final OValue. With `--repl`, it keeps O-level scope and backend
processes alive across entries. With no arguments in an interactive terminal,
it enters the REPL automatically.

`--backend-grant NAME=LANG[:RIGHT,...]` may be repeated before the input path
for compatibility with older sources or embedding experiments. Ordinary backend
blocks do not need grants; the default evaluator gives hosted backends full
grantable host authority.

### `olangc`: hosted AOT, WASI, script, OIR, and DOT graph

`olangc` shares the parser, evaluator, OValue model, and OIR implementation
with `O`.

| Target | Command | Result |
|--------|---------|--------|
| `binary` | `olangc app.O -o target/app` | Builds a native hosted executable containing the program and Rust O runtime. |
| `wasm` | `olangc app.O --target wasm -o target/app.wasm` | Builds for `wasm32-wasip1`; suited to programs that do not require unavailable WASI subprocess runtimes. |
| `script` | `olangc app.O --target script` | Parses and executes directly inside the `olangc` process. |
| `ir` | `olangc app.O --target ir` | Prints lowered OIR, its ExecutionPlan, and its directed executable HGraph; for a directory or lifted project, prints the deterministic ProjectExecutionPlan and project HGraph. Nothing executes. |
| `dot` | `olangc app.O --target dot` | Emits Graphviz DOT for an ordinary OIR HGraph or a directory/lifted-project HGraph. Pipe to `dot -Tpng` for a rendered graph. Nothing executes. |

Native hosted binaries contain the `.O` source, runtime modules, lockfile
dependency versions, and bundled core shims. Python, Nix, and other language
runtimes remain explicit host dependencies. `--shim-dir` overlays or adds
shim files before packaging. `--keep-build-dir` retains the generated Cargo
project for inspection. `--backend-grant` may be repeated for script mode and
native hosted binaries as a compatibility hook. Compiled binaries mint fresh
process-local default backend authority at startup instead of embedding
serialized authority.

### `o-link`: default literal execution and explicit safe projects

`o-link` treats a bare single directory as a **sequence of scripts**: it
recursively literal-links every selected UTF-8 file, writes `combined.O`, and
immediately executes that combined program. This is intentionally an unsafe
default because setup programs, migrations, test harnesses, installers, and
obsolete bootstraps can run merely because a directory walk discovered them:

```bash
o-link src/                         # writes combined.O and runs it now
o-link src/ -o sequential.O        # writes sequential.O and runs it now

# Explicit --literal retains the same linker but suppresses the inferred run:
o-link src/ --literal -o sequential.O
```

Use explicit `--project` whenever the directory must be captured without
executing arbitrary files:

```bash
o-link src/ --project -o project.O
o-link --list-routes project.O
o-link project.O --run --route py-main

# When discovery or a manifest establishes one unambiguous default route:
o-link src/ --project --run
```

Explicit project mode captures the selected tree as one lossless project bundle,
discovers ecosystem and manifest routes, and embeds the bundle as inert text.
Neither linking the directory nor evaluating the generated document executes a
source file or route:

```bash
O project.O
# Ostadix project bundle loaded safely. No project route was executed.
```

Project execution is an explicit operation through `--project --run`. An
already-lifted project `.O` file remains self-identifying and is detected
automatically.

Project planning is a separate nonexecuting inspection path. Select a route or
route set with `--route`; an optional checked `--routes-policy` override accepts
`explicit`/`explicit:ROUTE`, `default`, `fallback`, `any_success`,
`race_success`, `race_settle`, `all`, `verify_equivalent`, or
`benchmark_and_select`:

```bash
olangc src/ --target ir --route main
olangc project.O --target dot --route main > project.dot
./scripts/o-cli.sh plan src/ --route main
```

Directory and losslessly lifted inputs produce the same logical plan for the
same bundle and selection. Planning validates project references and exact
bundle/policy provenance but deliberately does not run a guard, prerequisite,
or command.

`olangc --target ir` is the direct project planner interface.
`scripts/o-cli.sh` is the repository-owned lowercase dispatcher: `setup.sh`
installs an `o` wrapper that delegates to it, so `o plan` reaches `olangc` while
all other arguments retain the historical lowercase evaluator behavior. Keep
`~/.local/bin` (or `~/.cargo/bin`) before `target/release` in `PATH`: on a
case-insensitive filesystem the raw release binary named `O` is also reachable
as lowercase `o` and would otherwise shadow the dispatcher.

Project bundles preserve binary assets, empty and extensionless files,
executable bits, Unix modes, and in-root symlinks. They respect `.gitignore`
and `.olinkignore`, skip `.git`, `target`, `node_modules`, `__pycache__`, and
prior generated `o-link` documents, and are deterministic across repeated
runs. Because this mode is deliberately lossless for non-ignored content,
secrets should be listed in `.gitignore` or `.olinkignore` before a bundle is
distributed.

Explicit files remain link-only unless `--run` is supplied:

```bash
o-link calc.py page.html app.O -o program.O
o-link notes.txt --lang txt=markdown --stdout
o-link calc.py --run
```

For a single directory, explicit `--literal` (also exposed as `--execute-all`)
is the literal **link-only** spelling and therefore disables the inferred run:

```bash
o-link src/ --literal -o sequential.O
# Equivalent spelling:
o-link src/ --execute-all -o sequential.O
```

Add `--run` when using the explicit spelling to execute immediately. Running
the generated `sequential.O` later executes every selected executable backend
block in dependency order. Multiple or mixed directory inputs still require
`--literal`. Options that configure per-file wrapping -- including `--lang`,
`--verbose-skips`, `--no-validate`, `--shim-dir`, and `--backend-grant` -- are
rejected under `--project` rather than being silently ignored.

Literal directory wrapping does not infer that unrelated source files are safe
to reorder. Ordinary wrapped files retain dependency/source order. Explicit
`.O` inputs keep authored `autonomous(batch(...))` regions intact, so those
regions still reach evidence admission and the graph worker scheduler.

Literal mode retains these correctness properties:

- Recursive directory walks are deterministic.
- Source markers are relative to one common root computed across every input,
  so absolute invocation paths do not leak into the linked document.
- `.gitignore` and `.olinkignore` rules, including negation, are honored at
  each walked directory.
- Every readable UTF-8 file is selected. Known extensions use their registered
  backend; unknown and extensionless files use inert `text`.
- Hidden paths, `target`, `node_modules`, `__pycache__`, `.git`, ignored paths,
  generated outputs, unreadable entries, binary data, duplicates, symlink
  aliases, and the output file itself are skipped. `--verbose-skips` reports
  each exclusion; the default groups warnings by reason.
- O openers, matching closers, and `$name` sequences inside source files are
  escaped and round-trip literally.
- Every section records its exact byte length, so embedded marker-like text and
  final-newline differences cannot be mistaken for section boundaries.
- Static imports are dependency ordered for Python, JavaScript, Rust, C and
  C++, Java, Haskell, Ruby, OCaml, Racket and Lisp, shell, Nix, C#, MATLAB,
  and Wolfram Language. Unrecognized dependencies retain stable walk order.
- Every wrapped file receives an isolated explicit environment number, and the
  combined source is parsed again before writing unless `--no-validate` is
  requested.

The built-in extension map includes Python, shell, HTML, LaTeX, Markdown,
Rust, Racket, Nix, text, C and C++, C#, Haskell, Scheme, Common Lisp, SQL,
Ruby, MATLAB, Wolfram Language, WAT, Java, JavaScript, and OCaml.

### `o-unlink`: restore either representation

`o-unlink` recognizes both formats produced by `o-link`:

```bash
o-unlink project.O -o restored/
o-unlink sequential.O -o restored-literal/
o-unlink project.O --dry-run
```

For a lifted project, it extracts the embedded bundle and restores binary
bytes, empty files, executable modes, and safe in-root symlinks. For a literal
link document, it reconstructs each escaped typed-block section. All stored
paths are confined to the selected output directory -- absolute paths,
parent-directory traversal, and writes through escaping symlinks are rejected.

### `o-notebook`: local interactive documents

The optional notebook feature embeds its HTML, CSS, and JavaScript UI in the
Rust binary. It exposes only a local evaluator endpoint and reset endpoint,
keeps one O scope per server process, and renders text, trusted HTML, and image
blobs as distinct output forms.

```bash
cargo run --features notebook --bin o-notebook -- backends
```

---

## Architecture

Ostadix-lang has two compiler and execution pipelines with a deliberate boundary
between them.

```text
Hosted orchestration
====================
.O source
    -> ONode parser tree
    -> OIR and ExecutionPlan
    -> Evaluator
    -> inline handlers or persistent backend processes
    -> OValue

Native computation
==================
.oc modules
    -> AST
    -> resolved and typed HIR
    -> SSA MIR
    -> x86_64 assembly
    -> ELF relocatable object
```

### Repository layout

```text
Ostadix-lang/
├── src/
│   ├── main.rs                 # O interpreter and REPL
│   ├── parser.rs               # hosted typed-parenthesis parser
│   ├── value.rs                # OValue and hosted wire protocol
│   ├── capability.rs           # live bearer identity generation
│   ├── ir.rs                   # OIR, ExecutionPlan, backend registry
│   ├── eval.rs                 # evaluator and rendering semantics
│   ├── process.rs              # persistent backend IPC
│   ├── backend.rs              # Rust hosted backend runner
│   ├── scheduler.rs            # dependency scheduling and caches
│   ├── nix_ops.rs              # instantiate and realise
│   ├── nixos_ops.rs            # activation and system references
│   ├── live_system/            # hosted package, CAS, protocol, and supervisor oracle
│   ├── ocore/                  # native front end, IRs, codegen, capability bridge
│   └── bin/                    # compilers, bundle tools, notebook, hosted live CLI
├── backends/                   # compatibility hosted-language adapters
├── ocore/                      # freestanding runtime and kernel proof
├── c_cpp/                      # standalone C17 hosted implementation
├── o_lang/                     # Python reference implementation
├── examples/                   # runnable hosted examples
├── .gitignore                  # source-only checkout and artifact boundaries
├── docs/OCORE.md               # O-core language and ABI contract
├── SPEC.md                     # hosted language specification
└── ARCHITECTURE.md             # implementation architecture
```

### Hosted evaluation

The hosted evaluator runs five conceptual stages:

1. Parse source into typed expression nodes.
2. Evaluate child expressions before their receiving parent unless a
   structural backend takes control.
3. Render each child OValue into the parent language's source syntax.
4. Dispatch the completed source to an inline handler or Rust backend process.
5. Cache only values and requests whose runtime-boundary classification
   permits reuse.

Backend processes communicate with the Rust runtime through length-prefixed
canonical CBOR frames. The frame body carries the same tagged command/response
schema:

```text
Runtime -> backend: u32be_len || cbor({"cmd":"exec","code":"...","bindings":{...}})
Backend -> runtime: u32be_len || cbor({"status":"ok","value":{"t":"int","v":42}})
Backend -> runtime: u32be_len || cbor({"status":"eval_request","src":"...","scope":{...}})
Runtime -> backend: u32be_len || cbor({"cmd":"eval_result","value":{...}})
Runtime -> backend: u32be_len || cbor({"cmd":"cleanup"})  # reset; keep serving
Runtime -> backend: u32be_len || cbor({"cmd":"shutdown"}) # final acknowledgement; exit
```

The callback forms are what allow Python's `O.eval` to re-enter the O
evaluator without starting a second unrelated document process. Each callback
receives a snapshot of the O bindings visible at the call site. The snapshot is
used as the callback's lexical root, so reads see caller bindings while new
callback bindings do not leak into the caller. When the request carries an
explicit OScope, that value replaces the implicit call-site snapshot.

Ephemeral worker operations use a one-shot process owner. A successful result
is not reported to the coordinator until `shutdown` is acknowledged, the direct
backend leader exits and is reaped, and the response reader finishes. On Linux
and macOS, completion additionally requires proof that no active descendant
remains in the leader's inherited process group. A descendant that deliberately
creates a new session or process group is outside this v1 boundary; other
platforms do not receive the stronger group-quiescence claim.
`O_BACKEND_OPERATION_TIMEOUT_MS` and `O_BACKEND_SHUTDOWN_TIMEOUT_MS` set bounded
operation and shutdown deadlines; their defaults are 60,000 ms and 2,000 ms.
The same absolute operation deadline is inherited by recursive `O.eval`
callbacks. Native backends and the primary Python shim acknowledge `shutdown`
directly; the production compatibility proxy translates it into command-channel
EOF for older standalone shims.
`ProcessRegistry::shutdown_all` is the explicit, error-reporting persistent
backend shutdown path. Registry destructors never perform protocol I/O: each
remaining process only receives a bounded best-effort termination and
direct-leader reap attempt.

Set `O_LIFECYCLE_TRACE=/absolute/path/to/trace.log` for an append-only
diagnostic timeline of admission hashing, task preparation/submission, worker
callbacks, backend/proxy PIDs, shutdown acknowledgement, result settlement,
and worker-pool joins. The trace excludes source and OValue payloads and never
changes execution when it cannot be written.

### OIR and ExecutionPlan

OIR is a backend-neutral lowering of hosted syntax:

```text
RawText      -> Text
VarRef       -> Load
LetBinding   -> Store
Call         -> Invoke
TypedExpr    -> Exec
```

Every public hosted execution path lowers to OIR before it runs. `O`, the
REPL, notebook cells, `olangc --target script`, linked programs, and recursive
`O.eval` callbacks all enter the same OIR evaluator. There is no production
ONode interpreter beside it.

The ExecutionPlan adds three kinds of graph edge:

- Structural edges connect child expressions to the expressions receiving
  their values.
- Sequence edges preserve left-to-right document semantics.
- Data edges connect `$name` loads to the latest visible `let name` store.

BackendRegistry records aliases and shim resolution. BackendInterface freezes
the canonical name, purity, splice renderer, and execution mode into each OIR
`Exec` instruction. Before execution, the plan validates node identities, root
coverage, edge bounds, and acyclicity, then produces the stable
topological root schedule and direct-child schedules used by every `Store`,
`Invoke`, and `Exec`. The most recent runtime plan is available through
`Evaluator::last_execution_plan()`.

Evidence analysis independently verifies that each embedded backend interface
matches the registered language policy and that special invocation metadata has
canonical name/mode/arity before it can issue an admission bundle.

`Invoke` is also typed during lowering as eager, lazy, autonomous, or a
specific coordination-group mode. The evaluator does not rediscover special
form policy from an unrelated name table after planning.

The validated plan is projected into a directed HGraph before execution.
Ordinary results, successful completion, evaluator/host resource versions, and
persistent actor state are nodes. Operations are directed hyperedges whose
outputs include one ordinary value, one completion token, and successor state
versions. Evidence schema v3 admits each executable operation and binds its
dispatch adapter before the coordinator accepts the graph. The coordinator
marks an operation graph-ready exactly when every input node is materialized;
dispatch additionally respects its admitted adapter, local-pool capacity, and
the strict semantic settlement frontier. For admitted local-worker work, owned
`PreparedTask` envelopes enter a fixed-size pool whose
threads persist for one graph run; each accepted completion recomputes readiness
without waiting for an unrelated earlier ready set to drain. Fallible outcomes
may complete physically out of order but settle by serial topological ordinal.
A successful verified-pure, admitted-infallible result may provisionally expose
its outputs to other safe worker tasks; an earlier failure revokes those outputs
and discards the provisional work. Because `NodeFinished` denotes durable
settlement, a provisionally unlocked dependent may emit `NodeStarted` before
its producer's `NodeFinished`; `--explain-schedule` reports this rule. An error
returned by an admitted-infallible adapter is an infrastructure contract
violation, not `NodeFailed`. A semantic failure produces no completion or
successor state.

Unknown hosted code still reports reads and writes against `HostWorld` and
`EvaluatorState`; exact filesystem and network footprints are not inferred.
Ordinary hosted blocks therefore remain coordinator-owned and strict. A narrow
source-level opt-in is available for direct, attribute-free ephemeral members
of a coordination group under the effective `autonomous(...)` policy, for
example `autonomous(batch(python^(...)_python, python^(...)_python))`. Those
members use the `autonomous-ephemeral-shim/v1` adapter and evidence records
`explicit-autonomous-unordered` semantics. Explicit O dataflow, lexical scope,
live capability checks, bounded worker capacity, and deterministic result/error
settlement are preserved; hidden external effects from already-started members
may race and are not rolled back. Indexed persistent environments such as
`python[0]^(...)_python[0]` retain actor and host-state serialization.
`O_EXECUTOR=serial` selects the ordered reference executor, while `--workers N`
overrides only the graph local-worker pool capacity. It does not override graph
readiness, effect or actor ordering, or runtime availability, and it may exceed
either the execution host's reported parallelism or the admitted static
worker width. Without that override, the pool
size is
`min(available_parallelism, admitted_max_local_worker_wave_width).max(1)`; if
the platform cannot report `available_parallelism`, O conservatively
substitutes one. A graph with no admitted `LocalWorker` operations creates no
pool. The admitted width is the widest local-worker subset of a conservative
static Kahn wave; it is a sizing heuristic, not a CPU or memory reservation and
not a bound on the completion-driven dynamic frontier.

`olangc --target ir` prints the same executable program, plan, and textual
state-complete HGraph used by the runtime. `olangc --target dot` shows both
constraint hyperedges and the directed operation ports for ordinary, resource,
actor, and completion/control nodes.

`olangc --target ir --explain-schedule` additionally prints the v3 admission
digests, exact adapter IDs, provenance, blockers, and legal static waves without
dispatching. Its advisory marker has schema ID
`oexec.realizability/v1`, introduced by the line
`; ScheduleRealizability oexec.realizability/v1`. The marker fields mean:

| Field | Meaning |
|---|---|
| `status=inspection-only` | The values describe this `olangc` inspection, not a dispatch-time snapshot. |
| `execution-realizable=unknown` | The marker does not establish that the inspected execution can run now. |
| `dispatch=not-run`, `observed-overlap=not-run` | No operation was dispatched and no overlap was measured. |
| `scope=local-worker-static-wave` | The capacity comparison concerns only admitted `LocalWorker` operations in static Kahn waves. |
| `worker-count-covers-static-wave=yes\|no\|not-applicable` | `yes` means the selected count is at least the maximum local-worker static-wave width; `no` means it is smaller; `not-applicable` means that width is zero. |
| `runtime-readiness=unknown`, `placement-lease=none` | External-runtime availability was not established and no current placement lease was created. |
| `source=machine-default\|cli-override` | The count came from the default formula or the inspection-only `olangc --workers N` argument. |
| `available-parallelism=A` | The inspection host reported `A`; if the platform query fails, the displayed fallback is `1`. |
| `admitted-static-max-wave-width=T` | `T` is the largest total operation count in any admitted static wave, including coordinator-owned operations. It is not worker demand. |
| `admitted-max-local-worker-wave-width=W` | `W` is the largest `LocalWorker` subset in any admitted static wave and is the default sizing heuristic. It is not a dynamic-frontier bound. |
| `selected-workers=K` | `K` is the derived or explicitly supplied pool capacity; when `W=0`, the reported minimum remains one although execution creates no pool. |

The marker is live, advisory, and outside the admission digest. In particular,
`worker-count-covers-static-wave=yes` proves only the arithmetic comparison
`K >= W`; it does not prove simultaneous dispatch, CPU, memory, device or I/O
fit, backend readiness, placement, or observed speedup. Static waves describe
graph legality only; they are not runtime batches or completion order.

The same explanation also emits one machine-readable
`oexec.schedule-prediction/v1` record. It projects the exact admitted dependency
DAG onto shim-backed hosted execution: hosted operations have unit cost and all
other admitted operations have zero cost. Weighted longest-path depth defines
the emitted hosted-task layers; `predicted-width` is the largest layer and
`predicted-span` is the number of layers. The record includes its admission
SHA-256 and exact layer membership. It is derived after admission and remains
outside that digest; `admission-sha256` is a reference to the enclosing
admission rather than a circular self-binding. This is a static topology model,
not a duration estimate, resource-capacity proof, dispatch trace, or overlap
claim.

The reproducible four-shape hosted benchmark, expected outputs, methodology,
and single-core wait-overlap caveat are documented in
[`benchmarks/hgraph_hosted/README.md`](benchmarks/hgraph_hosted/README.md).

Project inputs take a direct, typed
`ProjectBundle -> ProjectExecutionPlan -> HGraph` inspection path rather than
synthesizing OIR. The project-specific validator reconstructs the exact plan
from the bundle and selected policy before checking its operation, dependency,
effect, and HGraph projection. Unlike ordinary OIR execution, this project
HGraph has its own opt-in coordinator: `O_PROJECT_EXECUTOR=hgraph` executes one
resolved `Explicit`/`Default` branch or serial ordered `Fallback`/`AnySuccess`
alternatives through graph-governed materialization, typed prerequisite
readiness, route settlement, and selection. Ordered first-success selectors
retain attempted results and stop before later branches start. The compatibility
hosted project runtime remains the default; parallel races and aggregate,
equivalence, and benchmark policies are not yet implemented by the Project
HGraph coordinator.

OIR is not SSA and does not model native pointer mutation. Those semantics
belong to O-core MIR.

---

## O-core native systems language

O-core is the statically typed, ahead-of-time systems member of Ostadix-lang. Its
first target is `x86_64-unknown-none`, using ELF64, the LP64 data model, and
the System V AMD64 calling convention.

### Modules, items, and control flow

Every source file declares a module. One `ocorec` invocation may compile
multiple modules as one unit:

```ocore
module kernel::serial;
use kernel::ports::write_byte;

const COM1: u16 = 0x3f8;
static mut BYTES_WRITTEN: u64 = 0;

pub unsafe fn write(data: *const u8, len: usize) -> void {
    let mut index: usize = 0;
    while index < len {
        write_byte(*(data + index));
        index += 1;
    }
}
```

Implemented items include functions, extern functions, structs, enums,
constants, and immutable or mutable statics. Implemented control flow includes
lexical blocks, `let`, assignment, `if` and `else`, `while`, `loop`, `break`,
`continue`, and `return`.

Name resolution covers locals, current-module items, explicit imports, and
predeclared hardware intrinsics. Cross-module functions receive deterministic
mangled symbols unless their attributes specify an exported symbol.

### Static types and aggregates

Primitive types are:

```text
bool
u8 u16 u32 u64 usize
i8 i16 i32 i64 isize
f32 f64
void never
```

Compound types include:

```text
[T; N]                 fixed-size array
*const T               immutable raw pointer
*mut T                 mutable raw pointer
struct Name { ... }    declaration-ordered product type
enum Name { ... }      tagged union
fn(T, U) -> R          function-pointer type
```

The type checker resolves all module items before checking bodies, computes
deterministic layouts, validates assignments and returns, checks direct-call
arguments, applies expected integer types to literals, validates casts, and
rejects unsafe operations outside an unsafe function or block.

Structs support construction, field access, locals, statics, and aggregate
copying. Arrays support literals, repeated initializers, indexing, locals,
statics, and pointer decay through explicit address operations. Enums support
unit and payload variants with a computed tag and payload layout.

### Layout and ABI

The x86_64 layout contract is fixed:

| Type | Size | Alignment |
|------|-----:|----------:|
| `bool`, `u8`, `i8` | 1 | 1 |
| `u16`, `i16` | 2 | 2 |
| `u32`, `i32`, `f32` | 4 | 4 |
| `u64`, `i64`, `usize`, `isize`, `f64`, pointers | 8 | 8 |
| `void`, `never` | 0 | 1 |

Struct fields retain declaration order and receive natural padding.
`@packed` removes inter-field padding and gives the struct alignment 1.
`@align(N)` can increase alignment to a power of two.

Enums use the smallest `u8`, `u16`, or `u32` tag capable of representing all
variants. The payload is aligned after the tag, and the final enum size is
rounded to its maximum required alignment.

System V scalar arguments use RDI, RSI, RDX, RCX, R8, and R9, with additional
arguments on the stack. Scalar results use RAX. The stack is 16-byte aligned
at calls. Interrupt functions use the compiler's `@interrupt` convention and
return with `iretq`.

### Explicit unsafe

O-core makes operations that can violate memory or machine invariants visible
in the source:

```ocore
unsafe {
    let status: u32 = volatile_load(status_register);
    volatile_store(device_register, command);
    outb(0x3f8, byte);
    invalidate_page(address);
}
```

Raw dereference, raw pointer arithmetic, pointer and integer casts, mutable
static access, inline assembly, port I/O, interrupt control, page invalidation,
halt, syscall instructions, volatile memory, and atomic memory operations are
checked as unsafe.

### Volatile and atomic operations

The compiler recognizes:

```text
volatile_load
volatile_store
atomic_load
atomic_store
atomic_exchange
atomic_compare_exchange
atomic_fetch_add
```

Atomic orders are `relaxed`, `acquire`, `release`, `acq_rel`, and `seq_cst`.
The type checker rejects invalid load-release and store-acquire combinations,
requires pointer and value widths to agree, and requires mutable pointers for
mutating atomic operations. Volatile operations currently require scalar
pointees. The x86_64 backend emits the corresponding locked or ordered
instructions and independently checks the atomic pointee, value, result, and
ordering types before selecting an instruction width.

### Hardware intrinsics and assembly

O-core directly supports:

```text
inb inw inl
outb outw outl
enable_interrupts disable_interrupts halt
invalidate_page
syscall0 through syscall6
asm!
```

Inline assembly uses Intel syntax with explicit register operands and options
such as `nomem`, `readonly`, and `nostack`. Register constraints are checked
against the backend's safe calling convention assumptions. Operands are
limited to non-floating scalar values because the current register interface
names general-purpose registers only.

### Linkage and sections

The implemented item attributes are:

| Attribute | Meaning |
|-----------|---------|
| `@export` | Make the symbol externally visible. |
| `@no_mangle` | Use the source identifier as the symbol name. |
| `@link_section("name")` | Emit an item into a named ELF section. |
| `@align(N)` | Increase item or type alignment. |
| `@used` | Retain a static item. |
| `@packed` | Use packed struct layout. |
| `@interrupt` | Generate an x86_64 interrupt entry and `iretq` return. |
| `@naked` | Restrict the body to assembly without an ordinary frame. |

### Compiler pipeline

`ocorec` exposes every major stage:

```bash
ocorec a.oc b.oc --emit ast -o -
ocorec a.oc b.oc --emit hir -o -
ocorec a.oc b.oc --emit mir -o -
ocorec a.oc b.oc --emit asm -o target/program.s
ocorec a.oc b.oc --emit obj -o target/program.o
```

The front end creates source spans and diagnostics, parses modules and items,
resolves types and imports, computes aggregate layouts, and emits typed HIR.
MIR lowering creates explicit basic blocks, SSA values, phi nodes, places,
loads, stores, aggregate copies, branches, calls, intrinsics, assembly, and
terminators.

The x86_64 backend emits GNU Intel-syntax assembly and uses local Clang only as
the hosted assembler for object production. The resulting file is an ELF64
x86_64 relocatable object suitable for a freestanding link. The target object
contains no O interpreter, Python runtime, JSON protocol, filesystem runtime,
libc, or Rust standard library.

### Freestanding kernel proof

The included kernel gates prove the native path in dependency order:

```text
Multiboot2 or Xen PVH entry
    -> 32-bit bootstrap
    -> bounded bootstrap page tables with kernel W^X
    -> long mode
    -> O-core kernel_main
    -> COM1 serial initialization
    -> physical page allocation
    -> generation-tagged domain, process, address-space, and CSpace registries
    -> user/supervisor page split and 64-bit TSS
    -> IDT, PIC, PIT, and SYSCALL MSR setup
    -> guarded copy-fault recovery and normalized trap frames
    -> M0 linked native[0] CPL3 task and fault/memory gates
    -> personality-routed capability syscall
    -> user-range and authority denial probes
    -> IRQ0 ring transition and iretq
    -> M1 independent CR3/process teardown gates
    -> M2 preemptive and blocking four-TCB scheduler gate
    -> M3 public CPL3 endpoint IPC, transfer, death cleanup, and containment gate
    -> M4 static ELF loader, immutable OVFS, and service namespace gate
    -> M5 four-service native image, serial activation, and bounded restart gates
    -> M6A packaged scalar personality RPC and supervisor-directed lifecycle gate
```

The bootstrap assembly builds the initial P4, P3, P2, and 4 KiB leaf tables;
enables PAE, NX, supervisor write protection, and long mode; loads a 64-bit GDT
and TSS; and calls the O-core `kernel_main`. Kernel metadata and read-only data
are R/NX, text is RX, and mutable state is RW/NX. Mappings stop after the
required 20 MiB bootstrap window. The linker places one user image at 16 MiB
and an adjacent writable NX stack with a non-present lower guard page. Separate
ELF `PT_LOAD` entries reserve and zero-fill the image and mapped stack pages;
the bootstrap page allocator stops below them.

The runtime modules provide:

- COM1 initialization and polled serial writes.
- A reclaiming registry for the admitted supervisor-only physical-frame pool.
  Legacy direct/PVH gates use the fixed 3,072-frame 4..16 MiB range;
  Multiboot2/UEFI selects a firmware-covered aligned subwindow of at least
  4 MiB inside it, with typed generation handles, reference counts,
  zero-before-reuse, quotas, and checked rollback.
- A packed 256-entry IDT and IDTR with normalized assembly stubs for vectors
  0 through 31.
- 8259 PIC remapping and IRQ masks.
- 8253/8254 PIT programming.
- A compiler-generated interrupt handler that atomically increments ticks,
  acknowledges the PIC, and returns with `iretq`.
- Generation-tagged domain, process, address-space, mapping, CSpace, and TCB
  registries. Dynamic process roots combine shared RX text and supervisor-only
  kernel mappings with private RW/NX data and guarded stacks.
- Object-typed process CSpaces with live, reserved, closing, empty, and retired
  slot states, exact owner identities, and type-aware drain.
- An architectural `SYSCALL` entry that uses `SWAPGS` CPU-local state,
  immediately leaves the user stack, preserves tested GPRs, validates return
  state, and routes through the current PCB's personality.
- Guarded kernel and user stacks, a dedicated double-fault IST, and exact
  page-fault fixups for `copy_from_user` and `copy_to_user`.
- A 256-byte kernel bounce buffer for capability-gated debug output.
- Checked `debug_write`, `cap_close`, capability-returning `page_alloc`,
  cooperative `yield`, lifecycle-gated `exit`, scheduler-gated `sleep`, and a
  diagnostic tick counter.
- Canonical 22-word thread frames, FIFO runnable and blocked queues, timer
  preemption, sleep deadlines, wake reasons, bounded priority quanta,
  accounting, and a ring-0 idle path for the single-CPU M2 gate.
- Public generation-safe CPL3 endpoint create/send/receive/cancel, bounded FIFO
  backpressure, real TCB block/wake epochs, request correlation, lifecycle
  cancellation, exact attenuated capability transfer, and one optional
  capability-authorized fixed RW/NX shared mapping per dynamic address space.
- A fixed-capacity immutable OVFS importer, strict static x86_64 ELF loader,
  BSS and minimal SysV-stack materialization, exact loaded W^X mappings,
  domain-relative mount/process namespaces, and capability-returning service
  registration.
- A fixed-capacity immutable package-root and health-gated activation registry,
  plus capability-checked single-byte serial input and bounded control-command
  submission for the loaded CPL3 REPL.

### Capabilities and syscall ABI

Kernel authority is represented by a 64-bit handle:

```text
handle = (generation << 32) | slot
```

Each process cspace stores object identifiers, object types, rights,
generations, and occupancy in kernel-owned arrays. Validation selects the
cspace from the current PCB, then checks slot bounds, occupied bit, generation,
object type, and required rights. Closing clears the slot and increments its
generation. A slot is retired instead of wrapping its generation, so an old
handle cannot silently regain authority after reuse. Kernel pointers never
become capability handles.

The initial syscall number contract is:

| Number | Operation |
|-------:|-----------|
| 0 | `debug_write(cap, ptr, len)` |
| 1 | `cap_close(cap)` |
| 2 | `cap_copy(source_cap, destination_endpoint_cap, rights)`; prepares an attenuation-only transfer ticket |
| 3 | `page_alloc(page_pool_cap, kind)`; returns a generated memory capability |
| 4 | `yield()` |
| 5 | `ticks()` |
| 6 | `exit(status)`; enabled only by a trusted lifecycle harness |
| 7 | `sleep(delta_ticks)`; enabled only while the scheduler is active |
| 8 | `endpoint_create()`; returns a generated endpoint capability |
| 9 | `endpoint_send(endpoint_cap, word0, correlation, transfer_ticket)` |
| 10 | `endpoint_receive(endpoint_cap, message_ptr, 32)` |
| 11 | `endpoint_cancel(endpoint_cap, correlation)` |
| 12 | `serial_read(control_cap, byte_ptr, 1)`; nonblocking and mode-gated |
| 13 | `control_submit(control_cap, command_ptr, len)`; bounded to 192 bytes |
| 14 | `personality_call(call_cap, operation, scalar, timeout_ticks)`; M6A scalar route |
| 15 | `personality_reply(reply_cap, request, status, scalar)`; M6A daemon completion |
| 16 | `personality_supervise(supervise_cap, action, generation, subject)`; M6A policy action |

The exported `kernel_syscall_dispatch` implements checked debug output,
capability close, anonymous/shared page-object allocation, yield, diagnostic
ticks, lifecycle-gated exit, scheduler-gated sleep, endpoint IPC, the mode-gated
native control path, and the mode-18 scalar personality route. The generic entry
reads the current PCB's personality rather than accepting one from the caller.
Native `debug_write` validates slot bounds, occupancy, object type, generation,
`RIGHT_DEBUG_WRITE`, and one concrete readable address-space region before an
exact-fixup copy into the bounded kernel buffer. The serial driver never
receives the raw user pointer. `page_alloc` validates a typed page-pool
capability, enforces a per-CSpace quota, and returns a kernel-selected CSpace
capability rather than a frame address. Executable allocation is loader-only,
device memory is rejected from the RAM pool, and `cap_copy` can only prepare a
ticket bound to the exact creating process generation and the receiver CSpace
derived from a validated endpoint. The endpoint object itself is not recorded
in the ticket.
`yield` records a request in every mode and performs an actual scheduler
transition when a scheduler gate is active. `exit` abandons a user frame only
after a trusted lifecycle continuation is configured. `sleep` is available only
to an active scheduler. Endpoint send/receive use fault-aware bounded message
copy, generation-safe endpoint capabilities, exact attenuation, and scheduler
retry after real blocking; user code never selects a destination CSpace slot.
The M5 serial and control operations additionally require the exact typed
control capability held by the REPL CSpace. They copy one byte or one bounded
command across the checked user boundary before the driver or control parser
sees it. The M6A call, reply, and supervision operations require distinct typed
capabilities and current process/generation ownership. Only the scalar call ABI
is enabled; the test personality cannot invoke pointer-bearing endpoint calls.

On the hosted side, `CapabilityBroker<T>` binds a 256-bit per-session bearer
identity from the operating system CSPRNG to a kernel-issued handle,
capability kind, and rights. Its operation-specific methods fix the syscall,
kind, required rights, and argument layout before invoking a
`KernelSyscallTransport`. Callers cannot ask a generic authorization method to
understate policy. A guessed, deserialized, forged, forgotten, or cross-session
identity never becomes a kernel handle. A kernel close removes its hosted
bearer only after confirmed success. Serialized metadata is descriptive and
cannot add rights or choose a kernel slot.

The threat boundary is explicit. The broker prevents identity guessing,
metadata-based escalation, stale token use, revocation bypass, and
cross-session replay. It does not protect against theft of a still-live bearer
inside the same broker session, compromise of the broker process, or
compromise of the authenticated kernel transport. Possession of a live bearer
is delegation, so callers must keep the token inside the intended trust domain
and revoke it when that delegation ends.

This is the bridge between OValue's authority-bearing hosted form and the
kernel's actual capability table. The transport can be implemented by a
native syscall, VM socket, shared memory channel, or monitor connection
without changing the authority rule.

### The freestanding boundary

Hosted O may use Python, Rust, Nix, JSON, subprocesses, files, and QEMU to
construct and test a system. Freestanding O-core may not assume any of them.
The build tools run on the host. The emitted object and kernel image depend
only on their target ABI and explicitly linked runtime symbols.

That distinction lets this remain valid:

```O
python^(
# Generate or inspect O-core source here.
)_python
```

without making Python part of this:

```ocore
unsafe fn kernel_main(info: usize) -> never {
    loop { halt(); }
}
```

The complete normative language, layout, ABI, unsafe, intrinsic, section, and
capability contract is in [docs/OCORE.md](docs/OCORE.md).

---

## Included examples

[`examples/manifest.json`](examples/manifest.json) is the authoritative,
complete classification for this tree. For every `.O` example it declares the
supported editions, unit/integration/manual class, backend and host/authority
requirements, timeout, and edition-specific result or output oracle. Rust,
Python, and C17 test entrypoints consume that file; an unknown backend returning
literal text is therefore not counted as successful execution, and a sweep with
no executed cases fails rather than reporting an all-skipped success. The
manifest's `authorities` list records ambient host requirements (`fs_read`,
`fs_write`, `network`, `process`, `elevated`, or `virtualization`); it is not a
serialized capability grant. Rust and C17 evidence uses required output
patterns, while the Python semantic runner can additionally compare exact
OValue JSON. The table below is a descriptive index, not a claim that every
edition supports every file.

| File | What it demonstrates |
|------|----------------------|
| `examples/hello.O` | Smallest Python-backed O program. |
| `examples/bindings.O` | `let` and `$var` splicing. |
| `examples/nested_splice.O` | Nested Python expressions. |
| `examples/trailing_expr.O` | Python trailing-expression result rule. |
| `examples/html_basic.O` | HTML with an embedded computation. |
| `examples/html_python_html.O` | HTML receiving an OHtml fragment produced through Python. |
| `examples/html_escape.O` | Escaped strings versus trusted OHtml. |
| `examples/html_raw_roundtrip.O` | OText round-trip through HTML. |
| `examples/python_html_python.O` | Three nested language levels. |
| `examples/computed_plot.O` | Matplotlib image blob rendered as an HTML image. |
| `examples/literate_report.O` | Markdown report with persistent Python state. |
| `examples/persist.O` | Explicit persistent environment. |
| `examples/env_split.O` | Independent environment indices. |
| `examples/ephemeral.O` | Fresh state for bare blocks. |
| `examples/meta_eval.O` | `quote^`, OExpr, `O.quote`, and `O.eval`. |
| `examples/script.O` | Executable O document with shebang. |
| `examples/bash_hello.O` | Executing Bash backend. |
| `examples/bash_binding.O` | Passing O bindings into Bash. |
| `examples/bash_exit_code.O` | Bash success-path exit code. |
| `examples/bash_multiline.O` | Multi-statement Bash block. |
| `examples/shell_hello.O` | Executing POSIX shell backend. |
| `examples/js_hello.O` | Executing JavaScript backend. |
| `examples/js_binding.O` | Passing O bindings into JavaScript. |
| `examples/js_json.O` | JavaScript returning a JSON object. |
| `examples/js_multiline.O` | Multi-function JavaScript block. |
| `examples/sql_create_insert_select.O` | Persistent in-memory SQLite state. |
| `examples/sql_select.O` | Simple SQL query. |
| `examples/sql_aggregation.O` | SQL aggregate over persistent table. |
| `examples/sql_python_sql.O` | SQL to Python to SQL value flow. |
| `examples/nix_basic.O` | Immediate Nix evaluation. |
| `examples/nix_python_html.O` | Nix to Python to HTML. |
| `examples/nix_storepath.O` | `nix_store^` returning an OStorePath. |
| `examples/nix_storepath_python.O` | Store path passed into Python. |
| `examples/instantiate_realise_basic.O` | ONixExpr to derivation to store path. |
| `examples/lazy_defer_attrs_basic.O` | Lazy and deferred Eval requests. |
| `examples/lazy_request_basic.O` | Lazy-wrapped Nix request chain. |
| `examples/coordination_groups.O` | Batch, all, any, and race values. |
| `examples/group_pipeline/main.O` | O-Git semantic receipt group pipeline. |
| `examples/os_as_participant_basic.O` | OSystem and activation boundary. |
| `examples/nixos_test.O` | Single-machine NixOS VM test. |
| `examples/nixos_test_two_machine.O` | Two-machine NixOS VM test. |

---

## Running the tests

The primary verification command is:

```bash
cargo test --all-targets --all-features
```

The release CLI suite checks interpreter errors, successful execution,
`olangc` native output, `ocorec` ELF object output, and linker help contracts:

```bash
cargo build --release
bash tests/test_cli.sh
```

The example suite executes every `.O` example with an explicit expected
output. Nix examples are skipped when Nix is not part of the local test
environment:

```bash
bash test_o_lang_examples.sh
```

The Milestones 0.1 through 0.3 bootstrap gate compiles every O-core runtime
module, links and boots the kernel, then asserts CPL3 entry, native personality
routing, capability and range denials, user-segment zero-fill, hostile-RFLAGS
sanitization, ordered timer return, the reclaiming typed frame/object lifecycle,
and a later CPL3 heartbeat. It also fails if any payload containing `LEAKED`
reaches serial:

```bash
./ocore/kernel/smoke-qemu.sh
```

The required portable gates are declared in `evidence/gates.toml`; the
aggregate validates that manifest, executes its ordered script projection, and
requires every declared marker exactly once in each gate's live transcript.
Each gate remains separately runnable for focused diagnosis:

```bash
python3 scripts/release_evidence.py validate
./boot-and-test.sh smoke

# KernelWorld first AMD SVM/NPT vCPU execution (mode 21; nested SVM + /dev/kvm)
./ocore/kernel/smoke-kernel-world-execution-qemu.sh
```

Additional implementation checks are:

```bash
make -C c_cpp test
python3 -m tests.test_parser
python3 -m tests.test_evaluator
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
bash scripts/check_declared_bins.sh
bash scripts/smoke-hosted-live-reference.sh
./ocore/kernel/build-m4-artifacts.sh
./ocore/kernel/check-m5-control.sh
./ocore/kernel/build-m5-artifacts.sh
./ocore/kernel/build-m6-artifacts.sh
python3 scripts/release_evidence.py validate
./boot-and-test.sh smoke
# Hardware-only; requires an AMD host with nested SVM and writable /dev/kvm.
./ocore/kernel/smoke-kernel-world-execution-qemu.sh
cargo test --test kernel_world_contract --no-default-features
bash scripts/check_release_claims.sh
python3 -m unittest -v tests.test_source_release

# Parser properties in the ordinary test suite
cargo test --test parser_proptest

# Continuous raw-byte parser fuzzing
cargo install cargo-fuzz
rustup toolchain install nightly
cargo +nightly fuzz run parser
```

The per-commit CI workflow runs all Cargo targets, names the deterministic
parser properties as their own gate, checks that the libFuzzer harness builds,
and runs a named reproducibility test. That test compiles the same O-core module
from two different source directories and asserts that the emitted x86_64 ELF
object bytes are identical.

The separate `Parser fuzz campaign` workflow runs the seeded libFuzzer target
for five minutes every Monday and whenever it is manually dispatched. This
keeps fast, deterministic property coverage on every change while making the
sanitizer-instrumented adversarial campaign an executing CI job rather than
type-checked scaffolding.

---

## Status

**v0.2.0**, with the Rust hosted runtime authoritative, the C17 edition as the
standalone native port, the Python edition as the semantic reference, and
O-core as the freestanding systems language.

<!-- BEGIN GENERATED: REQUIRED_QEMU_EVIDENCE -->
The 26 required portable QEMU release gates and 1
supplemental hardware-dependent gate are defined once in
[`evidence/gates.toml`](evidence/gates.toml). The aggregate reads that manifest
at runtime, selects only `required = true`, streams each gate's output, and
requires every declared marker exactly once in that live transcript. This table
is a checked projection.

| Gate | Required | Milestone | Evidence | Establishes | Explicit non-claims |
|------|----------|-----------|----------|-------------|---------------------|
| `ocore-bootstrap` | yes | M0.1-M0.3 | [ocore/kernel/smoke-qemu.sh](ocore/kernel/smoke-qemu.sh) (`portable_tcg`) | CPL3 entry, SYSCALL return, IRQ0 return, and a later heartbeat execute in QEMU<br>W^X pages, frame reclamation, typed memory objects, and capability denials pass the bounded bootstrap corpus | This one-process bootstrap is not multi-process isolation, IPC, or a foreign ABI<br>It is not evidence of Linux, Plan 9, or a foreign-kernel boot |
| `ostadix-x86_64-boot-info` | yes | OSTADIX Alpha x86_64 BootInfo / Mode 33 | [ocore/kernel/smoke-x86_64-boot-info-qemu.sh](ocore/kernel/smoke-x86_64-boot-info-qemu.sh) (`portable_tcg`) | A challenged x86_64 UEFI/Multiboot2 handoff is strictly normalized into bounded kernel-owned BootInfo facts and causally selects one page-aligned allocator subwindow from the firmware memory map<br>The temporary firmware inspection aperture is closed before the W^X check, and the same challenged mode-0 image reaches CPL3 entry, timer return, and a later heartbeat<br>The transcript grammar accepts the exact challenge and source commit in causal order and rejects a wrong challenge | This QEMU TCG and OVMF gate is not physical-machine, KVM, Secure Boot, measured-boot, or hardware-trust evidence<br>The bounded Multiboot2 parser and ACPI status validation are not a general ACPI consumer, initrd loader, firmware service, or general physical-memory allocator<br>It provides no SMP, PCI/device assignment, DMA isolation, IOMMU isolation, interrupt remapping, or hardware-reset evidence |
| `ostadix-x86_64-smp4` | yes | OSTADIX Alpha x86_64 bounded SMP / Mode 34 | [ocore/kernel/smoke-x86_64-smp-qemu.sh](ocore/kernel/smoke-x86_64-smp-qemu.sh) (`portable_tcg`) | One challenged QEMU q35/TCG and OVMF boot admits an exact four-CPU ACPI/MADT topology, validates PIT progress before x2APIC INIT/SIPI, and brings three APs into kernel RX text on distinct stacks<br>The low trampoline follows an RW/NX copy, R/X execution, erased-and-unmapped retirement sequence without a writable-and-executable mapping<br>Four unique APIC identities cross one atomic BSP/AP release-and-progress barrier and reach a later PIT transition and heartbeat; the same image under one vCPU rejects before startup success markers | This is exactly a four-vCPU QEMU TCG and OVMF proof using bounded type-0 8-bit APIC identities; it is not physical-machine, KVM, or arbitrary-topology evidence<br>APs park after one barrier; this is not a general SMP scheduler, IPI service, interrupt balancer, per-CPU allocator, concurrent syscall path, or SMP-safe version of every existing O-core subsystem<br>It provides no Secure Boot, measured boot, PCI/device assignment, DMA isolation, IOMMU isolation, interrupt remapping, or hardware-reset evidence |
| `world-g2-aarch64-native` | yes | World G2 / AArch64 native compiler | [ocore/kernel/smoke-aarch64-g2-qemu.sh](ocore/kernel/smoke-aarch64-g2-qemu.sh) (`qemu_tcg_aarch64`) | One O-core kernel compiled for AArch64 retains EL2, enters host EL1, completes one domain-separated HVC return with register and stack integrity, and in one live QEMU TCG run executes native EL0 process, IPC, capability, lifecycle, stale-generation, reclamation, and bounded post-lifecycle counter-progress checks | This single-vCPU QEMU TCG gate is not physical AArch64, KVM/SVM, SMP, or G3 evidence<br>It does not boot Linux or Plan 9 and does not establish a general foreign ABI<br>It provides no PCI or physical-device assignment, DMA isolation, or IOMMU/SMMU evidence |
| `world-identity-v1` | yes | World identity PR2 / Mode 27 | [ocore/kernel/smoke-world-identity-qemu.sh](ocore/kernel/smoke-world-identity-qemu.sh) (`portable_tcg`) | All 20 constitutional World identity atoms have shared typed Rust and O-core definitions with strict nonzero generation, version, term, and index rules<br>A bounded OWIDENT v1 identity-only corpus converges byte-for-byte between the Rust oracle and native O-core under QEMU; strict decode rejects malformed or zero-valued records, and hierarchical current/reference comparison rejects stale generations and same-generation logical mismatches | Serialized capability IDs are descriptive non-authority; this gate creates no bearer, CSpace handle, delegation, or authenticated authority<br>OWIDENT v1 remains the identity-only nested format and does not itself provide OWPROTO framing, transport, schema negotiation, an OValue envelope, a receipt codec, a Governor, or consensus<br>This repository-conformance slice does not pass G0 or any G0-G13 gate, and QEMU TCG is not physical or hardware-isolation evidence |
| `world-protocol-v1` | yes | World protocol PR3 / Mode 28 | [ocore/kernel/smoke-world-protocol-qemu.sh](ocore/kernel/smoke-world-protocol-qemu.sh) (`portable_tcg`) | The architecture-independent OWPROTO v1 record codec uses deterministic big-endian framing, four fixed record kinds, a 16 KiB hard maximum, caller/negotiated record bounds, and strict exact-length, reserved-field, kind, schema, and nested-identity validation<br>A fixed 20-record, 1254-byte corpus containing two offers, one canonical v1 selection, one disjoint rejection, and all 16 OWIDENT v1 conformance records converges byte-for-byte between the Rust oracle and native O-core under QEMU; version negotiation deterministically selects the highest common version and smaller record limit or an exact contextual rejection | OWPROTO v1 is a record codec with an offline bounded negotiation function, not a stream or network transport, live peer handshake, authenticated session, encryption, replay protection, membership protocol, or multiplexing layer<br>Identity and capability descriptions remain inert metadata; decoding or negotiating a record grants no bearer, CSpace handle, delegation, authenticated authority, or ambient process identity<br>This PR3 slice does not implement PR4 OValue or extension envelopes, PR5 receipts, a Governor, consensus, WorldFS, or Workstream A acceptance, and it passes no G0-G13 gate; QEMU TCG is not physical or hardware-isolation evidence |
| `world-value-v1` | yes | World OValue PR4 / Mode 29 | [ocore/kernel/smoke-world-value-qemu.sh](ocore/kernel/smoke-world-value-qemu.sh) (`portable_tcg`) | The separate self-framed OWVALUE v1 format freezes an explicit portable allowlist with a 4096-byte record maximum, depth-16 and 128-node limits, deterministic architecture-independent framing, strictly ordered records and scalar-key maps, and a root-only inert versioned extension envelope whose payload must itself be portable<br>The fixed 19-record, 928-byte corpus (1856 lowercase hex digits; concatenated-corpus SHA-256 264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc) converges byte-for-byte between the Rust oracle and native O-core under QEMU, with matching SHA-256 over each complete record; canonical encode/decode/reencode is stable, strict decoding rejects malformed or noncanonical values, and hosted projection rejects authority-bearing, capsule, and effectful values | OWVALUE v1 is inert portable data and admits no capability bearer, CSpace handle, delegation or session token, native capsule, live process, system, or device reference, executable request, or ambient identity; code and object references remain descriptive only<br>Versioned extension envelopes do not auto-dispatch code, load schemas, rehydrate capsules, resolve authority, or authenticate peers<br>Mode 29 is an offline codec and hash oracle, not a transport, live M9 crossing, PR5 receipt or signature implementation, execution or grounding convergence result, Governor, consensus, WorldFS, or Workstream A acceptance; it passes no G0-G13 gate, and QEMU TCG is not physical or hardware-isolation evidence<br>This gate does not make the full hosted src/value.rs OValue enum portable or replace the hosted canonical-CBOR shim wire format |
| `world-receipt-v1` | yes | World receipt PR5 / Mode 30 | [ocore/kernel/smoke-world-receipt-qemu.sh](ocore/kernel/smoke-world-receipt-qemu.sh) (`portable_tcg`) | The separate self-framed OWRECEIPT v1 format deterministically binds one bounded canonical execution receipt to exact World identities and generations, SHA-256 content references, descriptive capability rights, terminal and commit fields, evidence-gate identity, and an algorithm-tagged signature envelope<br>The fixed two-record, 3239-byte conformance corpus (6478 lowercase hex digits; concatenated-corpus SHA-256 1edd90bf881cd42d08e2031482baae4e7c9a95bd78cfa65f0cbe14147c0a2604) converges byte-for-byte between the Rust oracle and native O-core under QEMU, including its 1575-byte current and 1546-byte stale canonical signing preimages; hosted Ed25519 signs and verifies the exact domain-separated preimage and rejects tampering or a wrong key, while native O-core strictly validates the bounded receipt and signature-envelope structure | OWRECEIPT v1 carries signed descriptive evidence; receipt capability identities and rights are not bearers, CSpace handles, delegation certificates, session tokens, or grants of authority, and signature validity does not establish authorization or current World state<br>The pinned conformance key is public test material; this slice provides no production key generation, secure storage, hardware binding, enrollment, certificate chain, rotation, revocation, recovery, peer authentication, or trusted-signer policy<br>Mode 30 proves canonical receipt bytes and signing-preimage convergence under QEMU plus hosted Ed25519 sign/verify; native O-core structurally validates the signature envelope but does not implement or prove a general freestanding Ed25519 verifier<br>The Mode 30 corpus is constructed offline and does not itself prove live emission; the separate World-project hosted-reference path emits a caller-signed uncommitted OWRECEIPT and Mode 32 consumes it for bounded canonical/semantic comparison, while live-system, KernelWorld, object, capability, and evidence components remain outside that path and the existing O-Git semantic receipt remains a separate unsigned JSON demo<br>Local generation and commit-field checks are not an authoritative Governor snapshot, replay or commit-fencing service, transport, consensus, WorldFS, or Workstream A acceptance<br>This gate is not the typed OSTADIX Alpha attestation schema, does not admit evidence into evidence/world_alpha_gates.toml, passes no G0-G13 gate, and QEMU TCG is not physical, hardware-virtualization, or hardware-isolation evidence<br>OWRECEIPT remains separate from frozen four-kind OWPROTO v1 and OWVALUE v1 and does not change the hosted canonical-CBOR shim wire format |
| `world-project-runtime-mode32` | yes | World project runtime / Mode 32 | [ocore/kernel/smoke-world-project-runtime-qemu.sh](ocore/kernel/smoke-world-project-runtime-qemu.sh) (`portable_tcg`) | One focused hosted test enters the coordinator through an exact caller-supplied current-view World launch before workspace or child creation, observes a terminal RuntimeGraph, and emits a caller-signed OWRECEIPT with an unconditional Uncommitted fence<br>Native Mode 32 fully decodes and exactly re-encodes that live generated receipt, reconstructs its validated signing preimage, independently matches the signer-independent semantic SHA-256, rejects a malformed envelope, clears reused success-only validation tags, and reaches a later timer | The launch/current view and coordinator observer are caller-supplied descriptive identities, not authenticated Governor membership, admission, authority, reservation, provider placement, dispatch, recovery, or commit<br>Mode 32 compares canonical receipt structure and semantic content only; it neither executes the project natively nor verifies Ed25519 in freestanding O-core<br>The hosted execution retains residual HostWorld effects and provides no exactly-once guarantee, remote execution, physical-hardware evidence, G1 passage, Workstream A acceptance, or passage of any G0-G13 gate |
| `m02-fault-recovery` | yes | M0.2 | [ocore/kernel/smoke-faults-qemu.sh](ocore/kernel/smoke-faults-qemu.sh) (`portable_tcg`) | Eight fresh boots contain the bounded fatal CPL3 fault corpus<br>A ninth boot recovers a bounded user-copy fault and reaches a later heartbeat | The one-process fault corpus is not the current kernel ceiling<br>It does not establish arbitrary fault recovery or multi-process scheduling |
| `m1-process-isolation` | yes | M1 | [ocore/kernel/smoke-processes-qemu.sh](ocore/kernel/smoke-processes-qemu.sh) (`portable_tcg`) | Two bounded native processes use separate CR3s and same-VA physical isolation<br>Exit and fault teardown reject stale identities, reclaim frames, and preserve the sibling | The gate is single-CPU and does not establish SMP isolation<br>It is not a general scheduler, IPC, or foreign-process proof |
| `m2-scheduler` | yes | M2 | [ocore/kernel/smoke-scheduler-qemu.sh](ocore/kernel/smoke-scheduler-qemu.sh) (`portable_tcg`) | Four TCBs across two processes exercise bounded single-CPU yield, sleep, wake-once, and timer preemption<br>One million forced identity transactions and lifecycle reclamation pass | The gate does not establish SMP safety or an unbounded production scheduler<br>The million-transaction phase does not itself enter CPL3 |
| `m3-ipc-foundation` | yes | M3 foundation | [ocore/kernel/smoke-ipc-foundation-qemu.sh](ocore/kernel/smoke-ipc-foundation-qemu.sh) (`portable_tcg`) | Kernel-side shared mapping, bounded FIFO/cancel, waiter cleanup, endpoint generation, and attenuating-transfer mechanisms pass<br>The bounded foundation reclaims its resources and reaches a later timer | This foundation gate is not the public CPL3 IPC gate<br>It does not establish a general foreign personality or distributed IPC |
| `m3-public-ipc` | yes | M3 | [ocore/kernel/smoke-ipc-qemu.sh](ocore/kernel/smoke-ipc-qemu.sh) (`portable_tcg`) | Four CPL3 processes exercise public endpoint calls, bounded blocking, request/reply, and exact attenuated capability transfer<br>Personality failure is contained while an unrelated world survives and all bounded resources are reclaimed | The fixed-capacity single-CPU gate is not unbounded or SMP IPC<br>It does not implement a Linux or other foreign ABI |
| `m4-native-loader` | yes | M4 | [ocore/kernel/smoke-loader-qemu.sh](ocore/kernel/smoke-loader-qemu.sh) (`portable_tcg`) | A deterministic OVFS image imports two separately linked native ELF personalities as data<br>Malformed and W+X corpus entries are rejected before isolated loads, service lookup, teardown, and frame reclamation | The bounded read-only OVFS is not a general filesystem or dynamic linker<br>The native test personalities are not Linux, Plan 9, or another foreign OS |
| `m5-native-live` | yes | M5 | [ocore/kernel/smoke-live-qemu.sh](ocore/kernel/smoke-live-qemu.sh) (`portable_tcg`) | Four packaged native ELFs are hash-verified, loaded into isolated CSpaces, and health-gated before publication<br>A package-daemon fault withdraws generation 1, preserves unrelated services, and republishes a healthy generation 2 before reclamation | The bounded control plane is not a general package manager or unbounded retry policy<br>It does not boot or supervise a foreign operating system |
| `m5-supervisor-semantics` | yes | M5 semantics | [ocore/kernel/smoke-live-semantics-qemu.sh](ocore/kernel/smoke-live-semantics-qemu.sh) (`portable_tcg`) | The bounded native state corpus covers immutable roots, overgrant denial, failed-health nonpublication, rollback, stale references, and strict parsing<br>Crash and restart preserve explicitly unrelated state and reach a post-test tick | The self-test corpus is not the separate interactive M5 service-process gate<br>It does not establish a general backoff, durability, or foreign-service policy |
| `m6a-scalar-personality` | yes | M6A | [ocore/kernel/smoke-personality-qemu.sh](ocore/kernel/smoke-personality-qemu.sh) (`portable_tcg`) | Four packaged CPL3 ELFs exercise health-gated scalar personality RPC, deterministic terminal arbitration, and one supervised restart<br>Generation-1 authority stays stale after generation-2 rebind while an unrelated observer survives and resources return to baseline | Pointer-bearing calls and request-scoped foreign memory views are disabled<br>The native test personality is not a Linux or other foreign operating-system ABI |
| `m6b-bounded-copy` | yes | M6B mechanism | [ocore/kernel/smoke-m6b-qemu.sh](ocore/kernel/smoke-m6b-qemu.sh) (`portable_tcg`) | Generation-tagged bounded-copy request views enforce snapshot input, written-prefix output, typed rights, quotas, and revoke-before-terminal ordering<br>Five delegated lease classes support transactional create-bind rollback and request-wide revocation while unrelated scope survives | The mechanism is not integrated with the live M6A CPL3 RPC path<br>It does not establish pinned windows, streaming, signals, a Linux oracle, or concrete delegated services |
| `m6b-live-bounded-personality` | yes | M6B Mode 24 live | [ocore/kernel/smoke-live-bounded-personality-qemu.sh](ocore/kernel/smoke-live-bounded-personality-qemu.sh) (`portable_tcg`) | Four digest-pinned CPL3 ELFs exercise one-shot four-byte INOUT bounded personality RPC across health-gated publication, one contained daemon fault, and a generation-2 rebind<br>The live terminal corpus covers cancellation, timeout, service death, and supervisor-triggered pre-terminal unmap, request-revoke, delegated-resource-revoke, and caller-exit dispositions with stale and duplicate denial plus bounded cleanup | Mode 24 is a native test personality, not a Linux or Plan 9 boot, general foreign ABI, or general guest-agent path<br>The generation-2 lifecycle operations are supervisor-triggered pre-terminal dispositions; the gate does not mutate a mapping, observe an external resource event, or cover the post-reply/pre-consume process-exit or unmap race<br>The delegated device resource is one internal typed lease; this is not KVM, PCI or physical-device assignment, DMA, IOMMU, interrupt-remapping, or physical-device evidence |
| `m6-linux-minimal-live` | yes | M6 Linux Mode 25 live | [ocore/kernel/smoke-live-linux-personality-qemu.sh](ocore/kernel/smoke-live-linux-personality-qemu.sh) (`portable_tcg`) | One exact digest-pinned static Linux x86-64 ELF and three native service principals load from immutable OVFS data into isolated CPL3 address spaces<br>Bounded fd 1/fd 2 writes, exact -ENOSYS, and exit_group(42) survive one contained daemon fault, health-gated generation-2 replacement, stale generation-1 denial, and complete authority/resource reclamation | The pinned four-call success path, with a fifth failure-only exit site, is not Linux or Plan 9 boot, a distribution, root filesystem, dynamic linker, general foreign ABI, or arbitrary Linux binary compatibility<br>QEMU TCG CPL3 execution is not KVM/SVM or physical-hardware evidence<br>The gate has no PCI or physical-device assignment, DMA mapping/isolation, IOMMU isolation, interrupt remapping, or hardware reset |
| `m7-linux-plan9-9p2000-live` | yes | M7 Linux/Plan 9 Mode 26 live | [ocore/kernel/smoke-live-linux-plan9-qemu.sh](ocore/kernel/smoke-live-linux-plan9-qemu.sh) (`portable_tcg`) | One exact digest-pinned static Linux x86-64 ELF, an unprivileged native Linux 9P2000 server, a native supervisor, and a Plan-9-style native 9P2000 client load from immutable OVFS data into four isolated CPL3 address spaces<br>The Linux ELF's bounded stdout/stderr results are read through exact 9P2000 version, attach, walk, open, read, and clunk exchanges at /srv/linux/status across namespace withdrawal, one contained server fault, generation-2 replacement, stale generation-1 denial, and complete resource reclamation | Mode 26 executes the same bounded Linux-ABI ELF and a native O-core Plan-9-style client; it does not boot Linux or Plan 9, run a Plan 9 binary, provide a distribution, root filesystem, or dynamic linker, or establish a general foreign ABI<br>The exact 128-byte 9P2000 corpus exposes only the generation-bound /srv/linux/status path; it is not a general 9P server, Plan 9 namespace or mount environment, network transport, persistent filesystem, or guest-agent framework<br>Generation 2 is the same server implementation serving a later, different snapshot after generation 1 completed; this is not two-provider routing for one immutable object, requester-local fallback, fresh provider-B session/fid reconstruction, causal multi-attempt tracing, or live OWRECEIPT emission<br>QEMU TCG CPL3 execution is not KVM/SVM or physical-hardware evidence<br>The gate has no PCI or physical-device assignment, DMA mapping or isolation, IOMMU isolation, interrupt remapping, or hardware reset |
| `m7b-logical-read-fallback-live` | yes | M7B-1 native LogicalRead Mode 31 | [ocore/kernel/smoke-m7b-logical-read-qemu.sh](ocore/kernel/smoke-m7b-logical-read-qemu.sh) (`portable_tcg`) | One deterministic provider ELF is instantiated as two generation-distinct isolated CPL3 provider principals; distinct A/B service bindings, endpoints, and client call capabilities are admitted before one requester-local LogicalRead for an exact immutable 20-byte object<br>Provider A returns a valid terminal 9P Rerror, faults, and has its local route and call authority withdrawn; the client proves A stale before staged provider-B activation, then completes a fresh B-local version/attach/walk/open/read/clunk sequence with different fids, verifies the pinned SHA-256, and reaches separate cleanup, full reclamation, witness-survival, and post-timer evidence | This is the bounded M7B-1 local mechanism, not complete M7B: requester and router are one principal, both providers instantiate one implementation artifact, and the route set is fixed local configuration rather than a general route registry<br>The kernel causal state and serial transcript are non-persisted unsigned diagnostics, not a live OWRECEIPT, attestation, Governor commitment, lease protocol, or distributed consensus evidence<br>The exact read-only 9P2000 corpus is not general 9P, WorldFS, a writable filesystem, fid migration, exactly-once effects, network transport, persistence, Linux or Plan 9 boot, or a foreign KernelWorld<br>Forced QEMU TCG CPL3 execution is not KVM/SVM, physical-hardware, G7/G8, PCI/device assignment, DMA/IOMMU isolation, interrupt-remapping, or hardware-reset evidence |
| `kernel-world-mode20-objects` | yes | KernelWorld Mode 20 | [ocore/kernel/smoke-kernel-world-qemu.sh](ocore/kernel/smoke-kernel-world-qemu.sh) (`portable_tcg`) | The exact hash-pinned V2 record is parsed under default-deny package, manifest, request, export, and typed-rights binding<br>Generation-bound nonexecuting VM, vCPU, and guest-page objects enforce quota, stale denial, exact-world reclaim, and unrelated-VM survival | Mode 20 does not enter a guest, execute firmware, or publish a provider export<br>It does not establish device assignment, DMA mapping, or IOMMU isolation |
| `kernel-world-mode21-svm-kvm` | no | KernelWorld Mode 21 | [ocore/kernel/smoke-kernel-world-execution-qemu.sh](ocore/kernel/smoke-kernel-world-execution-qemu.sh) (`hardware_kvm`) | On an AMD host with nested SVM/NPT and writable /dev/kvm, KVM enters a bounded synthetic guest and observes controlled hypercall and interrupt exits<br>An unmapped guest-physical access fails closed before exact NPT teardown, vCPU restart, and unrelated-VM survival | Mode 21 is supplemental hardware-dependent evidence and is not part of the portable release aggregate<br>It does not boot Linux, Plan 9, firmware, or a supplied image<br>It has no provider lifecycle, guest agent, service export, virtual device, PCI assignment, DMA mapping, or IOMMU-isolation proof |
| `kernel-world-mode22-live` | yes | KernelWorld Mode 22 | [ocore/kernel/smoke-kernel-world-live-qemu.sh](ocore/kernel/smoke-kernel-world-live-qemu.sh) (`portable_tcg`) | The TCG-compatible native administrative gate health-publishes exact typed exports and dispatches bounded reset intent<br>Failure withdraws clients before exact VM-graph revoke; policy restarts generation 2 while unrelated service survives and generation 1 stays stale | Mode 22 does not enter a guest or enforce the manifest health timeout<br>It has no Linux or Plan 9 boot, guest agent, shared queue, device assignment, DMA/IOMMU isolation, or hardware reset |
| `kernel-world-mode23-execution-device` | yes | KernelWorld Mode 23 | [ocore/kernel/smoke-kernel-world-execution-device-qemu.sh](ocore/kernel/smoke-kernel-world-execution-device-qemu.sh) (`portable_tcg`) | QEMU TCG emulates SVM/NPT guest entry, VMMCALL-derived health, one validated virtual PIO request/reply, and an NPF VMEXIT<br>Quiesce releases execution pins before endpoint and supervisor teardown; generation 2 rebinds while stale generation 1 and cross-world authority fail closed | Mode 23 does not boot Linux, Plan 9, firmware, or a supplied user image<br>It is not KVM, physical-hardware, PCI assignment, DMA/IOMMU, interrupt-remapping, or hardware-reset evidence<br>It has no general guest agent, shared queue or ring, asynchronous guarantee, or SMP guarantee |

Validate the schema, scripts, runtime transcript checks, projections, CI wiring,
claim-guard wiring, and aggregate byte identity with:

```bash
python3 scripts/release_evidence.py validate
./boot-and-test.sh smoke
```
<!-- END GENERATED: REQUIRED_QEMU_EVIDENCE -->

### Implemented

- Typed-parenthesis parsing with exact openers, closers, aliases, environment
  indices, block attributes, and literal escapes.
- Applicative-order nested evaluation with inline structural backends and
  receiving-language rendering.
- Ephemeral bare blocks and explicit persistent backend environments.
- O-level `let` bindings and `$var` splicing.
- The complete current OValue sum type, canonical CBOR backend wire protocol,
  content identity, runtime-boundary classification, and persistence checks.
- `quote^`, OExpr, `O.quote`, and callback-based `O.eval`.
- Lexical scope snapshots for `O.eval`, including caller binding visibility
  without callback writes leaking into the caller.
- First-class OScope values, `scope()`, `O.scope()`, and explicit
  `O.eval(expr, scope_snapshot)` evaluation.
- Lazy and deferred Eval requests with purity validation and caching rules.
- The ONixExpr, ODerivation, OStorePath, and OSystem lattice.
- Autonomous dependency scheduling with memory and disk caches.
- Batch, all, any, and race coordination groups with distinct failure
  semantics and nested groups.
- OIR as the hosted execution engine, with embedded backend interfaces,
  typed invocation policy, validated ExecutionPlan graphs, planned root and
  child scheduling, structural regions, and runtime plan inspection.
- Real hosted shims for the registered backend table.
- Interpreter, REPL, local notebook, native and WASI `olangc` targets,
  script execution, OIR dumps, `o-link`, and `o-unlink`.
- C17 interpreter and C17 hosted AOT compiler.
- O-core modules, functions, control flow, static checking, arrays, pointers,
  structs, enums, unsafe, volatile and atomic operations, assembly, hardware
  intrinsics, ABI layout, linker attributes, typed HIR, SSA MIR, x86_64
  assembly, and ELF relocatable objects.
- Freestanding Multiboot2 and Xen PVH kernel image with long-mode bootstrap,
  serial output, physical page allocation, IDT, PIC, PIT, timer interrupt,
  atomic tick, and `iretq`.
- Generation-tagged kernel capabilities, rights validation, checked syscall
  dispatch, 256-bit live bearer identities, and hosted OCapability broker
  binding.
- Complete bounded O-core Milestones 0.1 through 0.3 gates for architectural
  CPL3 entry/return, hardened faults and user copy, and the reclaiming typed
  frame and memory-object lifecycle in the fixed QEMU window.
- Complete bounded Milestone 1 gate for two independent native address spaces,
  same-VA isolation, atomic full-identity switching, split process/CSpace
  teardown, stale-handle denial, sibling survival, and frame reclamation.
- Complete bounded Milestone 2 gate for four TCBs on one CPU, with timer
  preemption, cooperative yield, cross-thread hostile-RFLAGS sanitization,
  blocking sleep, wake-once timers, priority and accounting checks, idle
  execution, hostile saved-RSP TCB containment, exit containment, and one
  million forced identity transactions.
- A bounded Milestone 3 gate for public CPL3 endpoint IPC, real FIFO
  block/wake, cross-domain request/reply, attenuation-only capability transfer,
  automatic dead-sender cleanup, exception-driven personality crash
  containment, unrelated-world progress, reclamation, and timer survival.
- A bounded Milestone 4 gate for deterministic read-only OVFS artifacts, strict
  static x86_64 ELF validation and rejection corpus, two independently loaded
  native personalities, exact W^X/BSS/stack mappings, capability service
  lookup, transactional namespace teardown, and reclamation.
- A bounded Milestone 5 gate for four separately linked service ELFs, isolated
  CSpaces, a real capability-gated CPL3 serial loop, immutable-digest install,
  exact health-gated activation, one contained package-daemon fault and
  health-gated fresh-generation restart, final control-plane deactivation,
  control revocation, namespace/process teardown, reclamation, and
  post-lifecycle timer survival. Its canonical embedded OVFS image is 62,056
  bytes with SHA-256
  `388b9253ce6f92bef1e1f986b46aabbeb728604cc73589d12105031f5f6b780a`,
  checked independently by the host and kernel before import.
- A bounded M6A gate for four package-loaded CPL3 principals: a test client,
  native personality daemon, native supervisor daemon, and unrelated observer.
  It proves health-before-publication, scalar endpoint-backed personality RPC,
  supervisor cancellation, deterministic timeout and service-death results,
  exactly one terminal wake, one contained daemon fault, generation-2 restart
  and call-capability rebind, stale/late/duplicate reply denial, supervisor-owned
  cooperative stop, complete reclamation, and post-lifecycle timer survival.
- A separate bounded M6B mechanism gate for generation-tagged request views,
  kernel-owned snapshot/output staging, direction-attenuated nontransferable
  view capabilities, written-prefix-only commit, and capability-close-before-
  terminal-before-wake ordering across reply, cancel, timeout, service-death,
  process-exit, unmap, and delegated-revocation hooks. Its five typed lease
  classes carry exact request identities; request-wide revoke has no ambient
  fallback and leaves an unrelated request alive. Post-reply process-exit/unmap
  cleanup publishes no second terminal or wake. The lifecycle/wake hooks are
  directly exercised rather than wired to live teardown/scheduling. The slice
  is not yet integrated into the CPL3 personality daemon or public
  pointer-bearing RPC and does not include pinned windows or concrete delegated
  services.
- A bounded Mode 24 live M6B composition gate for four digest-pinned CPL3
  principals and one exact four-byte `INOUT` request shape. It exercises the
  public bounded call/view/reply syscalls, one contained daemon fault and
  generation-2 rebind, plus supervisor-triggered pre-terminal unmap,
  request-revoke, delegated-device-resource-revoke, and caller-exit
  dispositions. It is not a general foreign ABI, a mapping-mutation or external
  resource-event gate, a post-reply lifecycle-race gate, or physical-device
  evidence.
- A bounded Mode 25 Linux-personality gate for one exact 8,520-byte static
  x86-64 ELF plus three packaged native service principals. The foreign ELF
  executes at CPL3, completes exact fd 1/fd 2 writes through request-scoped
  bounded `IN` views, receives Linux `-ENOSYS`, and exits with status 42. One
  daemon fault is contained before health-gated generation-2 replacement,
  stale generation-1 denial, full authority/resource reclamation, and a later
  timer. This is an exact four-call success path with a fifth failure-only exit
  site, not Linux or Plan 9 boot, a
  distribution, general foreign ABI, KVM/hardware evidence, PCI assignment,
  DMA, IOMMU, or physical-device isolation.
- A bounded Mode 26 Linux-to-9P2000 service gate for the exact Mode 25 Linux
  ELF, an unprivileged native 9P server, a native supervisor, and an
  independently linked native Plan-9-style client. The Linux stdout/stderr
  results are read from `/srv/linux/status` through exact bounded 9P2000 wire
  exchanges across one contained server fault, namespace withdrawal,
  health-gated replacement, stale generation-1 denial, complete reclamation,
  and a later timer. This is not Linux or Plan 9 boot, a Plan 9 binary, general
  Linux ABI, general 9P or namespace environment, hardware virtualization, or
  physical-device isolation.
- A bounded Mode 27 shared-World-identity gate with all 20 constitutional
  identity atoms typed in Rust and `.oc`, plus an exact cross-language
  `OWIDENT` v1 identity-only byte oracle under QEMU TCG. Strict decode rejects
  malformed and zero-valued records; hierarchical current/reference checks
  reject stale generations and same-generation logical mismatches. Serialized
  capability IDs remain descriptive non-authority; this is not a general World wire
  protocol, Governor, consensus implementation, or G0--G13 passage.
- A bounded Mode 28 canonical World-protocol gate with deterministic `OWPROTO`
  v1 framing, strict bounded/canonical decoding, pure schema negotiation, and a
  byte-exact 20-record, 1254-byte Rust/`.oc` corpus under QEMU TCG. It is a
  record codec, not a transport, live handshake, authenticated authority path,
  OValue or receipt format, Governor, consensus system, or G0--G13 passage.
- A bounded Mode 29 canonical World-value gate with a separate self-framed
  `OWVALUE` v1 portable allowlist, strict 4096-byte/depth-16/128-node bounds,
  canonical records and scalar-key maps, root-only inert extensions, and a
  fixed 19-record, 928-byte Rust/`.oc` corpus with concatenated SHA-256
  `264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc` that
  converges by exact bytes and full-record SHA-256 under QEMU TCG. It rejects hosted authority, capsules, live references,
  requests, and other effectful forms. It is not transport, a live crossing,
  the full hosted `OValue`, PR5 receipts, or G0--G13 passage.
- A bounded Mode 30 canonical World-receipt gate with a separate self-framed
  `OWRECEIPT` v1 record, exact Rust/`.oc` receipt and signing-preimage
  convergence, strict structural rejection, and real hosted Ed25519 tests under
  a pinned public conformance key. Native Mode 30 is not a general Ed25519
  verifier, and the offline fixture is not live receipt emission, authority,
  trusted-signer policy, OSTADIX Alpha attestation, Acceptance A, or G0--G13
  passage.
- A bounded Mode 31 M7B-1 local LogicalRead gate. One deterministic provider
  ELF is instantiated as two distinct generation-bound CPL3 provider
  principals, and both A/B routes are admitted before the request for one exact
  immutable 20-byte object. A returns a valid 9P `Rerror`, faults, and has its
  local route/authority withdrawn; the requester-local client/router proves the
  A handle stale before staged B activation, then completes a fresh
  provider-local setup/read/clunk with different fids and verifies the pinned
  SHA-256. A bounded volatile causal state, separate A physical cleanup, B
  session cleanup, full resource reclamation, an unrelated witness, and a later
  timer pass under QEMU TCG. This is not complete M7B: both providers share one
  implementation artifact, routing is fixed and local, and the non-persisted
  causal state is not a live `OWRECEIPT`. It adds no general 9P/WorldFS, writes,
  network, Governor, foreign kernel, G7/G8, hardware virtualization,
  DMA/IOMMU isolation, or physical-hardware evidence.
- A bounded World-project hosted-reference evidence path and Mode 32 receipt
  comparison. Caller-supplied exact current-view World/Governor/provider,
  coordinator-observer, dedicated coordinator-attempt, and operation-attempt
  identities enter `ProjectCoordinator` before workspace or child creation; terminal
  `RuntimeGraphV1` observes normalized trace outcomes and residual `HostWorld`;
  and a caller-supplied signer emits canonical OWRECEIPT with an unconditional
  `Uncommitted` fence. Mode 32 fully decodes and exactly re-encodes that receipt,
  constructs its validated signing preimage, and compares a domain-separated
  unsigned-body semantic hash under QEMU TCG. This is not Governor
  admission/commit, capability or lease authority, reservation, remote dispatch,
  recovery, exactly-once execution, native project execution, native Ed25519
  verification, physical hardware, G1, or Workstream A acceptance.
- A separately gated hosted Live-World oracle with bounded strict manifests,
  immutable package CAS objects, default-deny activation policy, health-gated
  service generations, rollback, targeted restart, reconstruction, revocable
  private bearers, and cross-package OValue composition. It remains a separate
  hosted differential oracle, not evidence for the native QEMU gates.
- A strict execution-neutral `KernelWorld` manifest and bounded host-side
  lifecycle oracle shared by source-integrated and binary-contained provider
  designs, plus a deterministic verified native normal form. A separate mode-20
  gate verifies and parses that embedded record, applies exact-package and
  byte-exact kind/purpose default-deny supervisor admission, and locally seals
  generation-bound
  nonexecuting VM/vCPU/guest-page objects with exact-world reclaim while package
  admission remains `ADMITTED`. No foreign-kernel
  execution, provider publication, interrupt/device path, DMA, or IOMMU
  isolation is claimed.
- Ambient real NixOS activation through `activate(path[, profile])`, plus
  explicit `dry_activate` and optional profile-scoped embedding guards.
- Default full backend authority for shim execution, with legacy
  `--backend-grant` and `cap=...` syntax still accepted for compatibility.
- Policy-keyed hosted processes with Python audit enforcement and a macOS
  operating-system sandbox layer.
- Exhaustive producer-to-consumer rendering fidelity classification for every
  OValue variant and renderer.
- Byte-reproducible O-core object emission for identical modules across source
  directories, enforced by a named test and CI.
- Raw-byte and structured adversarial parser properties plus a cargo-fuzz
  target.
- Deterministic allowlisted source-release ZIP construction from an exact Git
  commit, with dirty-tree refusal, an embedded canonical manifest and
  checksums, self-verification, and debris/tamper regression tests.
- Source-only Git tracking with Rust, native, Python, fuzzing, coverage, and
  local compiler products excluded while `.O` source and intentional visual
  assets remain tracked.

### Current boundaries

These are the boundaries of the current implementation, not descriptions of
features that are already present:

- `O.eval` uses either the caller snapshot or an explicit OScope and reuses live
  backend environments. A callback cannot recursively execute the same persistent
  backend environment that is currently waiting for its result; use a
  different environment index for that nested block.
- Concurrent group dispatch currently applies to threadable Nix-family
  requests. Eval requests preserve the single evaluator thread. Race selects a
  winner but does not cancel already-running loser work.
- `olangc` bundles the core compatibility adapters by default. Rust-native
  backends do not need adjacent shim files; programs using compatibility
  adapters outside the bundled set can compile with `--shim-dir backends`.
- Hosted backend policy is intentionally permissive by default: Ostadix-lang gives
  backend code the host access available to the current process. Restricted
  policies still route through `sandbox-exec` on macOS, and the direct legacy
  Python bridge remains covered by audit-hook tests.
- Reproducibility is currently asserted for O-core assembly and ELF relocatable
  objects under the same compiler, assembler, and target contract. Hosted
  `olangc` executables are not claimed byte-identical across different host
  linkers or toolchain versions.
- The C17 and Python editions implement their documented subsets and are not
  feature-identical to the authoritative Rust runtime. The C17 native port
  keeps activation dry-only; ambient real activation is implemented in the Rust
  evaluator.
- O-core's broad compiler/kernel target remains x86_64; G2 adds a bounded,
  conservative AArch64 scalar stack-spill backend. Neither backend has
  optimization or general register allocation.
- O-core direct calls are implemented. Function-pointer types are represented,
  while indirect calls are not yet lowered.
- O-core aggregates support deterministic layout, construction, indexing,
  fields, locals, statics, and copying. Aggregate parameters and returns use
  pointers in the current ABI slice. Enum construction is implemented;
  pattern matching is not yet part of the surface language.
- The x86_64 backend rechecks MIR contracts for unary and binary operations,
  casts, calls, returns, branches, phi inputs, indexed places, atomics,
  volatile scalar access, and assembly operands. A malformed MIR program is
  rejected instead of being interpreted as an integer-shaped machine value.
- Floating-point types have specified x86_64 storage layouts. Float literals,
  arithmetic, comparisons, casts, and `sysv64` float parameters and returns are
  rejected until SSE lowering and the floating-point ABI are implemented.
- The reclaiming typed allocator covers only the fixed, supervisor-only 4..16
  MiB QEMU bootstrap window. It does not discover firmware RAM, reserve
  arbitrary boot modules, register MMIO/device ranges, provide demand paging,
  or claim concurrent SMP allocation.
- Milestone 1 is bounded to two native processes on one CPU. It does not provide
  copy-on-write, ASLR, arbitrary user mapping selection, fork/exec/wait,
  signals, SMP, or a general process service.
- Milestone 2 is bounded to four TCBs, two processes, and one CPU. It has no SMP
  locking, FPU/SIMD context, load balancing, or production fairness and
  denial-of-service claim.
- Milestone 3 is bounded to fixed-capacity, single-CPU endpoint scenarios. It
  does not claim SMP IPC, unbounded queues, every sender/receiver-death
  interleaving, or the request-scoped foreign-memory protocol. `cap_copy`
  uses an authorized endpoint to derive the receiver CSpace, then prepares an
  attenuation ticket bound to the exact creating process generation and that
  destination CSpace, not to the endpoint object. The gate exhausts all 16
  ticket slots, denies abort by a different process, lets the owner abort each
  ticket exactly once, denies a stale abort, and proves a fresh ticket can be
  created afterward. The legacy all-zero probe remains unavailable.
- The Milestone 4 loader accepts only bounded static x86_64 `ET_EXEC` images in
  one fixed user window. There is no dynamic linker, shared-library ABI, demand
  paging, writable filesystem, or general path service. The host checks
  deterministic repacking, and the kernel independently recomputes the exact
  SHA-256 before it imports or publishes the embedded OVFS image.
- Within the Milestone 5 gate, package and supervisor state machines remain
  privileged behind the REPL's typed control syscall. The other three loaded
  service ELFs are isolated startup/completion principals, not yet independently
  operating daemons over endpoint RPC. The gate covers one immutable package
  install and activation plus one exact package-daemon crash/restart cycle. It
  does not prove two-package dependency resolution, real-path rollback, general or
  unbounded retry/backoff, or replacement-fault recovery; a replacement that
  does not publish its exact health token remains withdrawn and fails closed.
  It also does not provide durable reboot reconstruction or compiler receipts.
- M6A moves the tested personality lifecycle policy into an unprivileged CPL3
  supervisor and the scalar operation corpus into an unprivileged personality
  daemon. Its package and registries are fixed-capacity; it proves exactly one
  crash/restart/rebind cycle, not general retry/backoff or durable reboot
  reconstruction. The test-personality syscall surface is scalar-only and
  explicitly denies pointer-bearing endpoint access. Mode 19 implements a
  separate bounded-copy request-view and delegated-lease revocation mechanism.
  Mode 24 integrates only one exact four-byte `INOUT` request shape with a
  separately packaged native test daemon/router path. Pinned windows, signals,
  the post-reply process/unmap race, actual mapping mutation and external
  resource events, concrete delegated filesystem/network/timer/device
  services, shared OValues, arbitrary foreign executable corpora, a general
  foreign ABI, and full Milestone 6 remain future work. Mode 25 separately
  admits one exact static Linux x86-64 ELF with only two writes, one unknown
  syscall, and `exit_group(42)`.
- Outside the bounded Mode 34 AP-startup/barrier probe, O-core's general
  scheduler, process, IPC, and syscall paths remain fixed-capacity and
  single-CPU; Mode 34 does not establish SMP locking or subsystem-wide SMP
  safety. O-core provides no general Linux ABI, foreign root filesystem,
  native compiler/self-hosting, framebuffer, or nested-kernel execution. The
  hosted capability bridge remains a tested transport boundary, not a live
  connection to this QEMU kernel.
- The exact per-mode `KernelWorld` evidence boundaries are projected in the
  generated status table above from `evidence/gates.toml`; detailed contracts
  remain in `docs/KERNEL_WORLD_CONTRACT.md`. Hardware-only Mode 21 is
  supplemental and stays outside the required portable set. No portable gate
  may be read beyond its manifest non-claims.

See [SPEC.md](SPEC.md) for the hosted language contract,
[ARCHITECTURE.md](ARCHITECTURE.md) for the repository architecture, and
[docs/OCORE.md](docs/OCORE.md) for the native language and ABI contract. The
native World constitution and G0--G13 convergence program are in
[docs/OSTADIX_WORLD.md](docs/OSTADIX_WORLD.md); the superseded hosted-first
design survives only as the explicitly non-qualifying
[hosted reference profile](docs/HOSTED_WORLD_REFERENCE_PROFILE.md). The
dependency-ordered path from `native[0]` to foreign personalities is tracked in
[docs/ODOMAIN_PLAN.md](docs/ODOMAIN_PLAN.md), with the native package/REPL
contract in [docs/LIVE_SYSTEM.md](docs/LIVE_SYSTEM.md) and the bounded foreign
memory protocol in
[docs/PERSONALITY_MEMORY_VIEW.md](docs/PERSONALITY_MEMORY_VIEW.md).
The shared source-integrated/binary-contained world contract and its native
dependency order are in
[docs/KERNEL_WORLD_CONTRACT.md](docs/KERNEL_WORLD_CONTRACT.md).
The normative future host-EL1-to-EL2 machine-resource ABI, including
resource-class-specific asynchronous revocation and the G7 no-guest-HVC
boundary, is in [docs/O_MACHINE_CONTRACT.md](docs/O_MACHINE_CONTRACT.md); it is
a design contract and not a claim that G7 or G8 is implemented.
The design proposal for containing and utilizing foreign kernels — Linux,
Android, XNU/Darwin, and Windows NT — as O-Domain personalities is in
[okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md](okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md).
It is an architecture note against the existing `ocore` runtime and
`ODOMAIN_PLAN` roadmap; it claims nothing the milestone gates have not proven.

---

## License

GNU Lesser General Public License v2.1 only (SPDX identifier
`LGPL-2.1-only`). See [LICENSE](LICENSE) for the full text.

Generated AOT build crates are `publish = false`. Their component-scoped Cargo
metadata identifies the embedded Ostadix runtime as LGPL-2.1-only while marking
embedded user or project inputs as retaining the licensing attached to their
source; it does not declare one license for the mixed generated package.

## Citation and authorship

Ostadix-lang / ^Ostadix_ was created by Lee Daghlar Ostadi.

### How to cite

Citation metadata lives in the root-level [`CITATION.cff`](CITATION.cff)
file, which GitHub surfaces through the repository's **"Cite this
repository"** button. For the paper and its supporting package, cite the
existing Zenodo preprint/package record:

    Lee Daghlar Ostadi. The Nesting Is the Interface: Recursive Evaluator
    Composition for Whole-Program Polyglot Execution. Draft v0.2. Zenodo.
    https://doi.org/10.5281/zenodo.21544345

DOI `10.5281/zenodo.21544345` identifies that existing preprint/package
record; it is not an archive of a tagged Ostadix-lang source release. When
citing the current source before a separate tagged source-release DOI exists,
identify the repository version and exact revision:

    Lee Daghlar Ostadi. Ostadix-lang: Recursive Evaluator Composition for
    Whole-Program Polyglot Execution. Version 0.2.0.
    Commit: `FULL_COMMIT_SHA_USED`.
    https://github.com/lostadi/Ostadix-lang

Once Zenodo archives a future tagged source release, cite that separate,
version-specific source-release DOI for the exact source snapshot used. The
future DOI belongs in the top-level `doi` field of `CITATION.cff`; the existing
preprint/package DOI remains under `preferred-citation`.

### Archival releases and DOI

The existing preprint/package DOI above and a future tagged source-release DOI
identify different archived objects. For each archival source release:

1. Choose the exact commit that represents the archival research release.
2. Give it a real Git tag and a published GitHub release whose version
   matches `CITATION.cff` and `Cargo.toml`.
3. With the repository connected to Zenodo's GitHub integration, publishing
   the release causes Zenodo to archive the repository state and mint a DOI
   for that release.
4. Put the resulting source-release DOI into the top-level `doi` field of
   `CITATION.cff` and into this section of the README. Do not replace the
   existing preprint/package DOI under `preferred-citation`.

Evaluation artifacts associated with any paper — benchmark scripts, inputs,
raw results, and reproduction instructions — should be deposited on Zenodo as
their own archived, DOI-bearing artifact alongside the software release.
Software Heritage archival can additionally be requested to obtain a
content-addressed persistent identifier for the source itself.

### Core contribution

    Typed expression boundaries: LANG^( body )_LANG

Canonical phrase:

    The nesting is the interface.
