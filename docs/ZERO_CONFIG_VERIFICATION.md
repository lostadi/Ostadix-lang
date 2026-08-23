# Zero-configuration LAN verification record

This record applies to the two source snapshots used during this repair:

- `bca0667d724c12b4c36ea8919608ceeba1e1b097`, the snapshot on which the
  zero-configuration implementation was first integrated and compiled by the
  user;
- `24dd07fbd44581379cb913ee274f3e5b485ea06b`, the newly uploaded source
  archive used for the clean corrected integration.

A complete source comparison established that the hosted-node Rust sources and
all patched pre-existing files are byte-identical between these baselines. The
`24dd07f` archive lacks `o-node-quickstart.sh`; the corrected integration adds
it as the ordinary zero-configuration entry point and includes it in the
deterministic source-release closure.

## Compiler evidence from the user host

On macOS with the pinned Rust toolchain available, `./setup.sh --minimal`
compiled the dependency graph and reached the `o-lang` binaries. The compiler
reported two `E0382` ownership errors in `src/bin/octl.rs`: each expression moved
`resolved.address` and then tried to borrow the partially moved `resolved` value
to construct the TLS identity.

Both faults are corrected here by deriving `tls_identity` before moving the
owned address into the client constructor. This preserves the same transport,
identity, key, and timeout values and changes only evaluation order and
ownership lifetime.

On August 23, 2026, the corrected tree completed a clean
`./setup.sh -y --minimal` release build and
`cargo check --workspace --all-targets` on the user's macOS host. The
post-build checks confirmed that the rebuilt `o-node`, `octl`, repository
dispatcher, and installed `o` wrapper exposed the zero-configuration runtime
surfaces.

## Executed successfully after the correction

- `bash -n scripts/o-cli.sh`
- `bash -n o-node-quickstart.sh`
- `bash -n setup.sh`
- `python3 -m unittest tests.test_o_cli_dispatch -v` -- 10 tests passed
- `python3 -m unittest tests.test_setup -v` -- 16 tests passed
- `python3 -m unittest tests.test_source_release -v` -- 75 tests passed
- exact full-patch application against fresh `bca0667d` and `24dd07f`
  extractions -- passed
- exact minimal hotfix application against the earlier patched tree -- passed
- post-application source-tree comparisons -- exact
- `git diff --check` on every generated patch -- passed

The 101 focused Python tests cover ordinary and manual node command routing,
quickstart intent projection, setup behavior, and the deterministic public
source-release closure.

## Required Rust gate

Run from the corrected source directory:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Then run the two-host acceptance path:

```bash
# Capacity host
./setup.sh -y --minimal
source "$HOME/.config/ostadix/env.sh"
o node start
o node status

# Client on the same LAN
./setup.sh -y --minimal
source "$HOME/.config/ostadix/env.sh"
o node list
o node profile
o node doctor
o node run examples/hello.O
o node session run examples/hello.O
```

Acceptance requires that ordinary client commands request no address, port,
hostname, CA, certificate, key, receipt key, capability, lease, operation ID,
task digest, or attempt generation.
