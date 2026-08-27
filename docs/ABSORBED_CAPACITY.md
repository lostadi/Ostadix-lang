# Absorbed-capacity package manager

`scripts/ostadix_capacity.py` is the host-side package manager for exact OS,
kernel, userspace, firmware, and bundle artifacts. It turns verified artifact
bytes into installed capacity and commits exact package closures into a
revisioned active generation.

The manager is Python standard-library only. It does not invoke Cargo, QEMU,
GRUB, an ISO builder, a decompressor, a signature verifier, or a foreign guest.

## Meaning of absorbed capacity

The manager keeps three states separate:

| State | Meaning |
| --- | --- |
| Installed | Exact bytes and their immutable package record exist in the local stores. |
| Active | An exact dependency closure is named by the current revisioned generation. |
| Qualified | A separate exact-package observation established a named evidence contract. |

Version 1 installs and activates. It does not produce qualification records.
Every generated activation therefore has an empty `qualified_packages` set.
Foreign-kernel-lab observations and catalog integrity notes are not silently
promoted into package qualification.

An active package is descriptive capacity. It is not an O capability bearer,
an authority grant, a successful boot, or proof of O-core governance.

## Package kinds

The catalog schema admits exactly five kinds:

- `kernel`: a kernel image or kernel artifact set.
- `userspace`: an initramfs, root filesystem, or userspace artifact set.
- `firmware`: firmware bytes; firmware packages cannot depend on other kinds in v1.
- `os`: a boot-oriented system or an exact composition of kernel, userspace,
  firmware, and bundle packages.
- `bundle`: a metadata composition that can collect packages of any kind.

Every dependency states its expected kind. Catalog admission and activation
both reject kind mismatches, incompatible architectures, cycles, and an OS
loader that disagrees with its kernel dependency. Architectures are exact
except for the explicit `any` value.

Supported loader labels are `none`, `linux`, `multiboot2`, `uefi`, `bios`,
`plan9`, `redox`, and `chainload`. A loader label describes compatibility. It
does not assert that this manager has built or tested a boot path.

## Strict catalog contract

The catalog schema is `ostadix.absorbed-capacity-catalog/v1`. Unknown fields,
missing fields, duplicate identifiers, alias collisions, invalid tokens, and
unsupported enum values are rejected. Important v1 bounds include:

- catalog bytes: 1 MiB;
- packages per catalog: 512;
- artifacts per package: 64;
- dependencies per package: 64;
- aliases per package: 32;
- packages per activation closure: 1,024;
- bytes per artifact: 64 GiB;
- persisted record or plan bytes: 2 MiB.

Each artifact binds:

```toml
[[packages.artifacts]]
id = "media"
role = "install-media"
filename = "install79.iso"
source = "https://cdn.openbsd.org/pub/OpenBSD/7.9/amd64/install79.iso"
size_bytes = 798625792
sha256 = "7a4a92e953618035097c796a90b54424a0f3ae775552e1e7d102cf8a5130449f"
integrity = "Official published artifact identity."
```

`source` may be a local path, a local `file:` URL, or an HTTPS URL. Source
location and human-readable integrity text are provenance, not package
identity. Artifact role, filename, exact size, and SHA-256 are part of package
identity.

## Identities and immutable storage

Package, plan, and generation identities use separate domain strings and
canonical sorted JSON encodings. A package identity binds:

- schema, semantic name, version, kind, architecture, and loader;
- license and redistribution policy;
- exact artifact hashes, lengths, roles, and filenames;
- exact dependency package digests and expected kinds.

Aliases and download URLs are excluded. Changing an alias or mirror therefore
cannot change an already planned or active package.

The state root contains:

```text
absorbed-capacity/
├── capacity-blobs/sha256/<raw-sha256>
├── capacity-packages/sha256/<package-digest>.json
├── capacity-generations/sha256/<generation-digest>.json
├── aliases.json
├── head.json
└── state.lock
```

Local sources are opened as descriptor-pinned, non-symlink regular files.
HTTPS redirects must remain HTTPS. Data is read in bounded chunks while SHA-256
and byte length are checked. Candidates are staged in the destination
directory, fsynced, changed to mode `0444`, and published using an atomic
same-filesystem no-clobber hard link. Existing objects are accepted only after
full identity verification.

Package and generation records use the same no-clobber immutable publication
rule. Mutable aliases and the activation head use same-directory temporary
files, fsync, and atomic replacement.

## Aliases are not authority

An exact reference has the form:

```text
sha256:<64 lowercase hexadecimal digits>
```

Anything else is treated as an alias. Alias resolution occurs while creating a
plan, and the plan stores only exact package digests. `apply` never resolves an
alias again. Moving `kernel/current` after a plan is created cannot change that
plan's closure.

Package ids and catalog aliases are installed automatically. Repointing an
existing alias requires `install --replace-alias`. Alias records do not grant a
capability and are not activation records.

## Transaction model

`plan` performs these steps:

1. Resolve roots and any license acceptances to exact installed package digests.
2. Read the current activation revision.
3. Rebuild and validate the dependency DAG from immutable package records.
4. Produce a lexicographically sorted exact closure and a deterministic
   dependency-first activation order.
5. Bind the plan to `base_revision` with its own domain-separated digest.

`apply` revalidates the plan, closure, licenses, immutable package records, and
every artifact blob. It then creates an immutable generation and commits it
only if `head.revision == plan.base_revision`. A stale writer fails without
changing the head.

The commit increments the revision, makes the new generation current, and
retains the old current generation as previous. `rollback` atomically swaps
current and previous and increments the revision again. Repeated rollback can
therefore toggle between the two retained generations.

## License checks

Every package names a bounded license identifier and one redistribution policy:

- `permitted`;
- `restricted`;
- `user-supplied`.

Restricted and user-supplied packages must set `requires_acceptance = true`.
They can be installed, but a plan containing them requires explicit acceptance
of the exact package reference:

```sh
python3 scripts/ostadix_capacity.py \
  --state /path/to/state \
  plan os/private \
  --accept-license sha256:EXACT_PACKAGE_DIGEST \
  --output /tmp/private-plan.json
```

Acceptance is transaction input. It is not qualification and does not grant
runtime authority.

## Commands

The central `o` dispatcher routes this tool. The direct Python entrypoint
remains useful when selecting an isolated test state:

```sh
CAPACITY_STATE="$PWD/.local-capacity"
CATALOG="$PWD/evidence/absorbed_capacity_catalog.toml"

python3 scripts/ostadix_capacity.py \
  --state "$CAPACITY_STATE" --catalog "$CATALOG" \
  inspect openbsd-7.9-amd64-install

# Equivalent default-state front door:
o capacity inspect openbsd-7.9-amd64-install
```

Install from the catalog's pinned HTTPS sources:

```sh
python3 scripts/ostadix_capacity.py \
  --state "$CAPACITY_STATE" --catalog "$CATALOG" \
  install openbsd-7.9-amd64-install
```

Install the same package from an already downloaded local file without changing
its package identity:

```sh
python3 scripts/ostadix_capacity.py \
  --state "$CAPACITY_STATE" --catalog "$CATALOG" \
  install openbsd-7.9-amd64-install \
  --source media=/path/to/install79.iso
```

For dependency closures with repeated artifact ids, qualify an override as
`PACKAGE/ARTIFACT=SOURCE`.

Create and apply a revision-bound plan:

```sh
python3 scripts/ostadix_capacity.py \
  --state "$CAPACITY_STATE" --catalog "$CATALOG" \
  plan os/openbsd-7.9-amd64 --output /tmp/openbsd-plan.json

python3 scripts/ostadix_capacity.py \
  --state "$CAPACITY_STATE" \
  apply /tmp/openbsd-plan.json
```

Inspect local state:

```sh
python3 scripts/ostadix_capacity.py --state "$CAPACITY_STATE" list
python3 scripts/ostadix_capacity.py --state "$CAPACITY_STATE" show os/openbsd-7.9-amd64
python3 scripts/ostadix_capacity.py --state "$CAPACITY_STATE" status
python3 scripts/ostadix_capacity.py --state "$CAPACITY_STATE" verify
python3 scripts/ostadix_capacity.py --state "$CAPACITY_STATE" rollback
python3 scripts/ostadix_capacity.py --state "$CAPACITY_STATE" gc --dry-run
```

All command results are JSON. Errors are written to stderr and return status 2.

`gc` is deliberately report-only in v1. It lists orphan blobs and generations
outside current/previous retention, but never deletes them. Every installed
package remains retained even when inactive.

## Initial catalog

`evidence/absorbed_capacity_catalog.toml` binds repository-pinned artifacts
already used by the foreign-kernel lab where a direct source is available:

- Alpine Linux 3.24.1 aarch64 kernel, initramfs, and their OS composition;
- Alpine Linux 3.24.1 x86_64 kernel plus matching virt modloop, initramfs,
  and their OS composition;
- FreeBSD 15.1-RELEASE aarch64 compressed boot-only installer;
- 9front build 11983 amd64 compressed qcow2;
- GNU Guix System 1.5.0 x86_64 ISO and detached-signature bytes;
- Redox OS 0.9.0 x86_64 compressed livedisk;
- OpenBSD 7.9 amd64 `install79.iso`;
- a metadata-only bundle of the x86_64 foreign systems.

The OpenBSD record uses the official release artifact:

```text
source: https://cdn.openbsd.org/pub/OpenBSD/7.9/amd64/install79.iso
size:   798625792 bytes
sha256: 7a4a92e953618035097c796a90b54424a0f3ae775552e1e7d102cf8a5130449f
```

Guix is described precisely: the system is Guile-defined and
Guile-orchestrated, while Linux-libre is its kernel. It is not described as a
Lisp-implemented kernel.

### Generated capacity-host initramfs

The capacity-host initramfs is derived output rather than an upstream immutable
release artifact. The committed catalog therefore does not bind a transient
build hash. V1 still supports installing it locally once the exact derived
bytes have been admitted:

1. Build the initramfs from the pinned base inputs and the QEMU package closure
   resolved from the configured Alpine v3.24 repositories. The resolved package
   list is retained inside the initramfs, but v1 does not pre-pin every APK in
   that closure.
2. Compute its exact byte length and SHA-256.
3. Create a private strict catalog entry with `kind = "userspace"`, those exact
   values, and a local source path.
4. Install it directly or use `--source ARTIFACT=/path/to/initramfs` to override
   only the source location of that exact record.

The source override never overrides size or digest. A rebuilt capacity-host
initramfs with different bytes is a new package identity and must receive a new
catalog record. This keeps derived local capacity usable without pretending
that today's build output is a reproducible release pin.

## Verification and nonclaims

Run the focused offline suite with:

```sh
python3 -m unittest -v tests.test_ostadix_capacity
```

The suite uses temporary state roots and local files. HTTPS behavior is tested
through a mocked byte stream, with no network or media download. It covers
tampering, immutable idempotence, bounded streaming, stale plans, rollback,
alias movement, cycle/type/architecture/loader rejection, license acceptance,
schema limits, and non-destructive GC.

This v1 does not:

- decompress compressed media;
- verify detached cryptographic signatures itself;
- build or modify a bootable ISO;
- boot any package under QEMU, O-core, or physical hardware;
- import foreign-kernel observations as qualification;
- mint capabilities or grant authority;
- delete objects during garbage collection;
- resolve versions or fetch a mutable remote package index.
