# Per-host offline AI build kit

The deterministic source ZIP is a source and evidence artifact. It pins Cargo
resolution but does not contain Cargo's registry sources or a native Rust
compiler. `build_offline_kit.py` can wrap that allowlisted source closure with
one caller-supplied Rust sysroot and one caller-supplied Cargo directory source
for a named POSIX host.

This format is intentionally per-host. Cargo and `rustc` are native programs,
and Rust compilation still uses platform facilities that are not safely or
legally portable inside one repository ZIP. In particular, the kit assumes a
compatible host linker and C development substrate; macOS builds require a
locally installed Apple SDK/toolchain. Python and any hosted languages used by
a `.O` program remain external runtimes. Recipient verification and extraction
require Python 3.10 or newer; constructing a new kit requires Python 3.11 or
newer for the standard-library TOML parser.

Required CI builds a native fixture and runs its Python 3.10 verifier and
idempotent extractor on `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, and
`aarch64-apple-darwin`. A distributable full kit must still be built separately
on, and for, each native host.

## Maintainer build

Create a single union vendor directory for the ordinary workspace, MCP,
Android-runtime, and fuzz lockfiles while online:

```sh
offline_vendor=$(mktemp -d "${TMPDIR:-/tmp}/ostadix-vendor.XXXXXX")
offline_vendor_config=$(mktemp "${TMPDIR:-/tmp}/ostadix-vendor-config.XXXXXX")
offline_kit="${TMPDIR:-/tmp}/Ostadix-lang-offline-$(rustc -vV | sed -n 's/^host: //p').zip"
cargo vendor --locked --versioned-dirs \
  --sync mcp/ostadix_lang_mcp_server/Cargo.toml \
  --sync apps/android-terminal/runtime/Cargo.toml \
  --sync fuzz/Cargo.toml \
  "$offline_vendor" >"$offline_vendor_config"

python3 scripts/build_offline_kit.py build \
  --repo . \
  --ref HEAD \
  --toolchain "$(rustc --print sysroot)" \
  --vendor "$offline_vendor" \
  --output "$offline_kit"

python3 scripts/build_offline_kit.py verify "$offline_kit"
```

The supplied sysroot must contain executable `cargo` and `rustc`, the
`wasm32-wasip1` standard library, Cargo's MIT and Apache-2.0 license texts, and
Rust's generated `COPYRIGHT.html`. The builder rejects symlinks and special
files in both payload trees. It normalizes tar ownership, timestamps, modes,
ordering, gzip metadata, ZIP metadata, and manifest JSON before self-verifying
the completed artifact. Byte-for-byte reproduction assumes the same Python and
zlib builder versions in addition to identical source, toolchain, and vendor
inputs; the manifest's unpacked-tree seals remain independent of compression
library output.

`cargo vendor` preserves registry package files and writes a
`.cargo-checksum.json` for each crate directory. Those checksums are useful for
Cargo source replacement but are not an authenticity root. The offline kit
therefore also hashes the complete vendor tar payload, every source member, the
toolchain payload, and the canonical manifest. Publish the outer ZIP SHA-256
through a separate authenticated release channel. The internal manifest and
`SHA256SUMS` detect inconsistency only after the archive is trusted; they cannot
authenticate an archive whose payload and embedded verifier were replaced
together. Preserve all license files from the Rust distribution and vendored
crates, and perform a dependency-license review before public redistribution.

## Recipient use

Before extracting or executing any archive member, obtain the expected outer
ZIP SHA-256 from the authenticated release channel and compare it to the ZIP.
The comparison is mandatory; substitute the published digest below:

```sh
kit_zip=Ostadix-lang-offline-EXACT-HOST.zip
expected_sha256='PUBLISHED_64_HEX_SHA256'
actual_sha256=$(shasum -a 256 "$kit_zip" | awk '{print $1}') # macOS
# Linux: actual_sha256=$(sha256sum "$kit_zip" | awk '{print $1}')
[ "$actual_sha256" = "$expected_sha256" ] || {
  printf '%s\n' 'offline kit authenticity check failed' >&2
  exit 1
}
```

Only after that succeeds, extract the ZIP with a tool that preserves POSIX
executable modes, enter its single top-level directory, and run one bounded
profile:

```sh
./bootstrap-offline.sh check
./bootstrap-offline.sh hosted-rust
./bootstrap-offline.sh mcp
./bootstrap-offline.sh all-supported
./bootstrap-offline.sh wasm-std-check
```

The bootstrap asks the included Python verifier to hash every declared file,
preflight both tar payloads, compare the current host to the manifest, and
refuse any unsealed `.offline` destination. A previously extracted copy is
reused only after its receipt, Cargo configuration, toolchain tree, and vendor
tree all reproduce this kit's cryptographic seals. The bootstrap then invokes
the unpacked Cargo, rustc, and rustdoc by absolute path, creates a kit-local
Cargo home whose crates.io source is replaced by the sealed vendor directory,
puts build artifacts under `.offline/target`, clears inherited compiler
wrappers and flags, disables incremental compilation, forces Cargo dependency
resolution/fetching offline, and invokes Cargo with `--frozen` from a fresh
environment containing only the kit's Cargo/Rust settings plus `HOME`, `PATH`,
`TMPDIR`, and `LANG` for host-tool discovery. Build scripts and other
subprocesses are not placed in an operating-system network sandbox.
Cargo runs from `/` with an absolute manifest path so configuration below the
recipient's extraction parent (including `~/.cargo/config.toml`) is not
discovered. The bootstrap fails closed if the filesystem root itself contains
an ambient Cargo configuration or if a legacy `$CARGO_HOME/config` appears
alongside the sealed `$CARGO_HOME/config.toml`.

The same extracted kit can therefore run `check` and then `all-supported`
without unpacking again. A different or modified kit cannot silently overwrite
an earlier toolchain or Cargo home.

## Exact scope

The manifest names `check`, `hosted-rust`, `mcp`, `all-supported`, and
`wasm-std-check`. The build profiles cover all root-package Rust binaries with
all Cargo features (including `o-notebook`) and the separately locked MCP
crate. The `check` profile also resolves metadata under `--frozen --locked` for
the Android-runtime and fuzz manifests, proving the sealed vendor directory
covers all four committed lockfiles. The profiles do not claim to build Android
APKs, the nightly fuzz target, O-core/QEMU media, the C17 edition, every test,
or every hosted language backend. The presence of `wasm32-wasip1` standard
library files is a target-availability check; it is not by itself evidence that
arbitrary `.O` programs execute in browsers.
