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
