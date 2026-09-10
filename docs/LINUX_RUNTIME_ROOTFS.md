# Linux runtime rootfs collection

`scripts/collect_runtime_rootfs.py` constructs a runtime image on Linux using
the Python standard library and the host's `readelf` (binutils). It inspects ELF
metadata; it does not run the supplied executables or invoke `ldd`.

```json
{
  "schema": "ostadix.runtime-rootfs-closure/v1",
  "commands": {"bash": "/bin/bash", "python3": "/usr/bin/python3"},
  "paths": ["/usr/lib/python3.12", "/usr/share/terminfo"],
  "runner": "/absolute/path/to/olangc"
}
```

Use the paths and Python version actually installed on the collection host.
`commands` names the executable aliases exposed under the image's real `bin/`
directory. `paths` supplies runtime data, standard libraries, configurations,
and modules loaded dynamically. Optional `runner` names a Linux O/olangc ELF
with the same ABI as the runtime commands; only its loader/dependencies are
included for generated-program reentry. The compiler binary itself is not copied;
the launcher installs the actual generated binary at `/.ostadix/runner`.
The collector checks ELF class/machine compatibility,
not cross-host CPU features or kernel compatibility.

```bash
python3 scripts/collect_runtime_rootfs.py closure.json --output ./runtime-rootfs
python3 scripts/collect_runtime_rootfs.py closure.json --output ./runtime-rootfs \
  --report ./runtime-rootfs.collection.json
```

The output must be absent or empty. The report defaults to an adjacent
`DIR.collection.json` and must remain outside the image; an existing report is
not overwritten. Failure can leave an incomplete output directory. A successful
run writes `runtime.json` with `execution: linux-rootfs-v1`, plus the copied
payloads. The separate report records hashes, modes, links, ELF metadata, and
resolved dependencies.

Files retain their absolute coordinates inside the image. The collector follows
`PT_INTERP` and `DT_NEEDED` recursively, including ELF extensions found in declared
data directories. It uses `RPATH` (including inherited paths), direct `RUNPATH`,
the host's `ldconfig -p` cache, and default library directories, checking library
ELF class/machine. Cache-based resolution also carries `/etc/ld.so.cache`, records
its hash, and collects compatible candidates for each needed library, including
hardware capability variants. `DF_1_NODEFLIB` retains nondefault cache lookup
while excluding default library directories. `$ORIGIN` is expanded at the canonical executable/library
location. Missing libraries, cwd-relative search paths, and unsupported dynamic
search tokens fail explicitly. Ambient `LD_LIBRARY_PATH` is not an input.

File symlinks become relative links to copied targets. Directory symlinks are
materialized as directories, preserving file aliases to canonical locations and
refusing cycles. This supports hosts where `/bin` is a symlink without making
the image's `bin/` a directory symlink. Empty directories and ordinary Unix
permission bits are retained. The launcher-owned roots `.ostadix`, `.old-root`,
`proc`, `dev`, `tmp`, `work`, and `run` cannot be supplied as runtime data.
Scripts collect their absolute shebang interpreter; `/usr/bin/env` shebangs
require the selected command to be listed in `commands`.

Static ELF dependencies do not describe every runtime behavior. Supply assets
loaded by `dlopen`, language imports, plugins, or configuration explicitly in
`paths`; then qualify the resulting image under the intended clean environment.
The collector does not synthesize daemons, external services, licenses, device
access, or a universal runtime closure. Its unit tests use synthetic ELF metadata
and filesystems; real Linux execution is a separate qualification step.
