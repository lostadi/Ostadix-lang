# Minimal Linux write/exit personality

Status: the deterministic corpus is pinned, the isolated kernel-admin fd-table
and classifier semantics remain covered by Mode 19, and Mode 25 is the live
CPL3 acceptance slice. Mode 25 loads the exact foreign ELF from an immutable
four-file OVFS image and routes only the operations below through the bounded
personality path. This document does not claim a Linux kernel boot, a Linux
distribution, or general Linux binary compatibility.

## Exact bounded surface

The first foreign ABI corpus is one static x86-64 `ET_EXEC` file. Its only
success-path operations are:

| Linux x86-64 operation | Result |
|---|---|
| `write(1, pointer, length)` | bytes are read only through one live Mode 25 bounded `IN` view; exact length is returned |
| `write(2, pointer, length)` | same, routed to the bounded stderr sink |
| unknown syscall number | Linux `-ENOSYS` (`-38`) |
| `exit_group(42)` | classified for direct O-core process exit and cleanup; it does not enter personality RPC |

Linux fd integers remain foreign names. `linux_fd_table.oc` maps fd 1 and fd 2
to separate kernel-internal objects tagged with their own slot generation,
caller-process identity, exact personality service process, and exact service
generation. No raw O-core capability or object handle is returned to the
foreign program.

These fd objects are trusted internal table tokens, not
`runtime::capability` entries, and they are never installed in a CSpace. The
isolated oracle validates token generations and exact stored identity values;
it does not resolve its `u64` process identities against live
`runtime::process` generations. The Mode 25 adapter therefore supplies only
identities already validated by the process and bounded-RPC layers; the foreign
program cannot select those identities.

The Mode 19 kernel-admin proof calls
`kernel::linux_personality_semantics::self_test` and requires the exact marker
`M6B minimal Linux fd/classification kernel-admin semantics: PASS`. It covers
syscall classification, generation-tagged fd lookup/authorization, scoped
service-generation revocation, stale denial, unrelated-service survival,
process cleanup, and zero remaining fd objects. It deliberately remains
separate from Mode 25, which calls `linux_personality::complete_write`, executes
the pinned ELF at CPL3, and exercises the live bounded bridge.

## Live bridge contract

`linux_personality.oc` assumes the Mode 25 composition has already correlated
the caller, thread, address space, request frame, service process, service
generation, pointer, length, and direction. The daemon must obtain the view
capability with
exactly one `personality_bounded_rpc::lookup_view(request)` call. Linux write
completion then retrieves the immutable request frame, bounded length, and
direction through `personality_bounded_rpc::request_metadata(request, view)`.
The frame binds the exact `write` operation, Linux fd, and count; daemon-supplied
fd/count arguments must match it exactly before any fd lookup or view read.
Linux write then uses only:

- `personality_bounded_rpc::read_byte(capability, offset, out)` for snapshot
  bytes; and
- `personality_bounded_rpc::reply(request, capability, 0, result)` to revoke
  the view, terminalize the request, and publish the real scheduler wake.

The adapter copies into a 128-byte private scratch buffer. It commits its
bounded stdout/stderr transcript only after the generation-bound bridge reply
succeeds. A stale request therefore cannot publish new output. The bridge owns
saved-`RAX` completion; the foreign binary does not retry the syscall.

At the generation-1 daemon crash, the successful stdout reply is already
terminal and its daemon-side view capability is closed. The lifecycle path
withdraws that service and revokes the exact `(service process, service
generation)` fd objects without discarding the committed terminal result. Its
20-byte charge and bounded record remain until the client resumes after
generation-2 publication and consumes it. The replacement provisions fresh fd
objects under generation 2. Old internal handles remain stale, while objects
owned by an unrelated service identity survive even if its numeric generation
is also 1. `exit_group` invokes process-scoped fd cleanup before the ordinary
process reap completes.

## Pinned artifact and oracle

Build and validate the isolated slice with:

```bash
ocore/kernel/check-linux-minimal-slice.sh
```

The current deterministic ELF is 8,520 bytes with SHA-256:

```text
06240b6a840ed4262835aceff64a94f6ebd77838666f05eb7415d9a0d1b5868d
```

`verify_linux_minimal_corpus.py` enforces the exact source hashes, static
`ET_EXEC` header, x86-64 machine, entry point, two canonical non-W+X `PT_LOAD`
segments, non-executable GNU stack, absence of interpreter/dynamic/symbol
sections, fixed loader window, text/rodata hashes, syscall-site count, output
bytes, strict duplicate-free JSON, and oracle schema. Its negative suite
mutates the digest, machine, entry, program-header type, W+X flags, load
geometry, load window, section type, rodata, and oracle expectations, rejects
duplicate oracle keys, and requires every mutant to fail closed.

This Darwin arm64 host cannot provide an authoritative native x86-64 Linux
replay. The oracle records that confirmation as pending. A future native
x86-64 Linux CI job must replay these exact ELF bytes and record kernel/tool
metadata before that observation is claimed.

## Live acceptance gate

Run the live slice from the repository root with:

```bash
./ocore/kernel/smoke-live-linux-personality-qemu.sh
```

The gate independently pins the complete four-file OVFS image and the embedded
8,520-byte `/bin/linux-minimal.elf` payload. It rejects user-principal symbols
linked into the kernel, then requires real static-ELF load and CPL3 execution,
the exact `o-core linux stdout\n` and `o-core linux stderr\n` streams exactly
once and in program order, unknown-syscall `-ENOSYS`, and direct
`exit_group(42)`. Generation 1 completes stdout and then faults deliberately;
the crash withdraws generation-1 service/fd authority but preserves the
already-committed terminal for later client consumption. The replacement
starts privately: generation 2 first proves generation-1 lookup authority
stale, then answers health and is published. Only then does the
client consume the preserved stdout terminal and proceed to stderr. The gate
also requires unrelated-observer progress and zero remaining requests, views,
fd objects, capabilities, processes, address spaces, and frames, plus a later
timer and a one-second post-completion survival window.

Mode 25 also emits `M25D:` lifecycle diagnostics. `S` markers identify bounded
progress through fault containment, generation replacement, terminal teardown,
and the post-cleanup timer arm. `R0`, `R0S`, and `R1` through `R3` identify the
post-S2 supervisor's generation-one monitor-arm observation, crash-monitor
observation, and restart request. `R0B` is fatal because a current generation
that reached S2 must retain its earlier monitor-arm milestone. `C`, `D`, and
`F` markers identify a specific crash-continuation, Linux dispatch/stream, or
finalization failure and are acceptance-fatal. Most immediately precede a
fail-closed halt; `C10` also identifies a typed restart rejection returned to
the supervisor. These markers are diagnostic only: none is accepted in place
of any canonical marker required by the live acceptance gate.

The marker bytes, post-S2 latch, and marker dispatcher are build-selected in
`kernel::m6_mode25_diagnostics` only for Mode 25. Mode 26 links the same module
surface as a no-data, no-op stub. The shared `kernel::m6` implementation still
carries the monotonic monitor-arm rule and its lifecycle assertions in both
modes; the selection removes diagnostics from Mode 26 without weakening that
semantic repair or increasing fixed kernel-image headroom.

The monitor-arm fact is generation-scoped and monotonic. The current generation
reports it after the initial arm while `ACTIVE`, `CRASHED`,
`RESTART_REQUESTED`, or `STOP_REQUESTED`; a stale handle is rejected, and a
new `REPLACEMENT_STARTING` generation reports it only after its own monitor arm.
This prevents a supervisor that observes the initial arm after the service
crash from waiting forever on an already-crossed state transition.

The pinned CPL3 personality and supervisor binaries retain their existing
pause-on-protocol-mismatch behavior and are not assigned `M25D:` codes, because
changing those binaries would change the canonical corpus and OVFS identities.
For those user-principal stalls, QMP register state and the last serial-marker
arrival provide generic diagnostic context; they do not claim exact source-site
localization.

When QEMU remains alive without satisfying the gate, the harness first freezes
the failed lifecycle classification, then uses a private QMP socket for a
bounded, best-effort capture of pre-stop status and vCPU state. It asks QEMU to
stop, confirms the resulting status, and captures post-stop vCPU, register,
PIC, and IRQ state before terminating the process. It also resolves the
just-built kernel's data-symbol addresses with `nm` and reads bounded physical
memory ranges for thread states and queues, current/prepared threads, run and
switch counts, the supervisor's saved 22-word frame, scheduler state, M6
process/thread/physical identities and fault latch, and supervision states and
counters. The post-stop snapshot is a stable QEMU safe point, not the exact
timeout instant. An early QEMU exit and any QMP connection, stop, symbol, or
command failure are reported separately; QMP output never changes a failed gate
into a pass. Passing runs do not request QMP state.

For a non-admissible repeat check with fresh build directories and no silent
retry, run:

```bash
OCORE_M6_LINUX_STRESS_RUNS=2 \
  ./ocore/kernel/stress-live-linux-personality-qemu.sh
```

Every requested invocation must independently pass. This wrapper is a CI
lifecycle-stress diagnostic and is intentionally not listed as an evidence
gate in `evidence/gates.toml`. Local runs default to no synthetic host load.
Set `OCORE_M6_LINUX_STRESS_PRESSURE_WORKERS` to an integer from `0` through `8`
to run that exact number of tracked `yes` workers during the repeated smokes;
the wrapper terminates and waits for only those recorded PIDs. CI uses two
workers to exercise the scheduler interleaving that exposed the generation-1
monitor-observation race. That two-run pressure check is intentionally blocking
in Required CI as a diagnostic-quality requirement; it does not become
admissible execution evidence or substitute for any canonical gate marker.

The complete Mode 25 OVFS image is 60,104 bytes with SHA-256:

```text
b380e5cbbe50403bd58bdafb11c54f2201f0cc742fc898487fa08ba26e2886e8
```

This is the smallest honest bounded-copy Linux-personality gate. It does not by
itself close the broader pinned-window, streaming, partial-effect, signal, SMP,
or general concurrent mapping-race acceptance matrix described in
`PERSONALITY_MEMORY_VIEW.md`; it therefore is not evidence that the full M7
roadmap or a general Linux process environment is complete.

## Nonclaims

This slice provides no `read`, `brk`, `mmap`, `clock_gettime`, signals, threads,
dynamic linker, shared libraries, root filesystem, arbitrary executable corpus,
streaming I/O, or general Linux ABI. A pre-mapped arena would not establish
general Linux virtual-memory behavior and is not part of this surface.
