# Embedded foreign runtime bundles

`olangc --runtime-bundle DIR` embeds the regular files of an explicitly supplied
relocatable runtime tree in an ordinary native `.O` binary. The tree contains
`bin/` and this manifest:

```json
{
  "schema": "ostadix.embedded-runtime/v1",
  "environment": {
    "PYTHONHOME": "${BUNDLE}/python"
  }
}
```

The environment map is optional. Use `bin/python3`, `bin/node`, or the relevant
catalog command names. Include runtime libraries, standard libraries, helper
executables, and licenses in the tree. Internal file symlinks are retained so
invocation aliases and self-relative executable locations survive extraction.
Relative targets retain their spelling; absolute internal targets are rewritten
relative to the extracted link. Targets must remain inside the bundle. External
symlinks, directory symlinks, and special files are rejected. Empty directories,
UTF-8 paths, and Unix permissions are retained. Environment
entries support the listed Python, Node, Ruby, Java, .NET and dynamic-library
path keys; each value must name an existing path within `${BUNDLE}/`.

```bash
olangc program.O -o program --runtime-bundle ./runtime-tree
olangc program.O -o program --runtime-bundle ./runtime-tree \
  --materialize-only ./generated
```

The generated project includes `runtime-bundle-manifest.json` with path, byte
length, mode, and SHA-256 for each file, plus directory paths/modes and symlink
paths/targets. Startup checks the embedded digests,
extracts to a private randomly named temporary directory, and sets runtime
command lookup to its `bin/` exclusively before evidence/admission. Missing
commands therefore cannot be satisfied by ambient `PATH`. The extraction
remains alive during execution and is removed on normal scope exit, restoring
owner access to read-only directories during cleanup without following symlinks. Process
abort can leave its private directory behind.

The default `host` execution profile provides runtime payload embedding and closed command lookup. It does not
automatically discover or verify a complete dynamic library closure, replace
the host kernel, isolate arbitrary absolute file access or environment reads,
or embed external daemons/services. Clean-environment execution must qualify
each supplied closure. In particular, this feature alone cannot establish
hermetic embedding of every catalog runtime, including Mathematica, Multipass,
or Nix services. Native project binaries and WASI targets are not supported by
this option.

## Linux root filesystem execution

Add `"execution": "linux-rootfs-v1"` to the manifest to execute the generated
program inside its embedded filesystem image. The
[closure collector](LINUX_RUNTIME_ROOTFS.md) builds such an image from declared
runtime commands and data, recursively resolving static ELF dependencies.
Preserve absolute runtime locations inside the image; `bin/` contains command
aliases. The image must leave `.ostadix`, `.old-root`, `proc`, `dev`, `tmp`,
`work`, and `run` absent for the launcher to create.

Before evaluator or multicall dispatch, the launcher extracts the image and
reexecutes itself inside new Linux user, mount, PID, network, IPC, and UTS
namespaces. It detaches the previous root, makes the payload read-only, clears
ambient environment variables, drops process capabilities, sets `no_new_privs`,
and denies mount-changing syscalls. It closes inherited descriptors above
standard input/output/error. A private PID-namespace procfs is read-only;
`/tmp`, `/work`, `/run`, and `/dev/shm` are writable temporary filesystems.
The namespace exposes loopback and the four kernel devices `null`, `zero`,
`random`, and `urandom`. Ordinary threads, subprocess execution, temporary
compiler outputs, pipes, and local sockets remain available. The parent waits
for completion, removes its extracted image, and preserves exit codes/signals.
A dedicated namespace init reaps orphaned grandchildren, forwards termination
signals, and removes remaining descendants when the evaluator finishes. The
evaluator keeps its normal child-wait behavior. The init communicates the
evaluator's raw wait status to the outside parent, preserving fatal signals
despite Linux's special PID 1 signal rules.

The image identity binds the complete compiler inventory. Every multicall
entry checks that identity, the user namespace mapping, empty capability
sets, and `no_new_privs`, then installs the exact mount filter again. An
environment marker alone cannot enable execution. Namespace creation failure
stops before program evaluation; the launcher does not fall back to the host
profile or modify the host's namespace policy.

This profile currently supports Linux x86-64 and AArch64 with the requisite
namespace permissions and syscalls. The host kernel, the initial trusted
Linux executable loader, and the three standard streams remain its execution
substrate. This is filesystem/network isolation after entry, not a replacement
kernel or protection against a malicious host. Runtime services and libraries
loaded by computed names must be included explicitly. Mount-dependent runtimes
and external services need another qualified execution profile. No catalog-wide
hermeticity claim follows from one qualified Python/Bash image.

The ignored Linux integration test `tests/linux_runtime_rootfs.rs` compiles a
real one-binary Python/Bash program, removes the original image, and tests
runtime data, multicall callbacks, threads, subprocesses, loopback, absent host
paths/environment, immutable payloads, external route refusal, orphan reaping,
signal outcomes, and cleanup.
Run it explicitly on a suitable Linux host; `OSTADIX_ROOTFS_TEST_SUDO=1` allows
VM-local `sudo -n` for the generated program where unprivileged namespaces are
denied. It never changes a host sysctl or AppArmor policy.

```sh
cargo test --locked --all-features --test linux_runtime_rootfs -- --ignored
```

The launcher follows Linux's [user namespace mapping rules](https://man7.org/linux/man-pages/man7/user_namespaces.7.html),
[mount namespace semantics](https://man7.org/linux/man-pages/man7/mount_namespaces.7.html),
and [no_new_privs contract](https://cdn.kernel.org/doc/html/latest/userspace-api/no_new_privs.html).
