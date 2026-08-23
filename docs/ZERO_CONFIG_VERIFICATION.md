# Zero-configuration LAN verification record

## Reciprocal passcode-pairing validation — August 23, 2026

The reciprocal-pairing change was validated from a detached worktree rooted at
`f2ef9efd99f57ac9f5dcc93b3b95c64ba9c618bf`, with the exact working-tree Rust
and lockfile patch applied there. No Cargo command ran in the live development
checkout.

The isolated Rust gates completed successfully:

```bash
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

The full workspace test command passed, including the pairing protocol's
correct-code, wrong-code, transcript-tampering, secret-redaction, and passcode-
shape tests; paired-store conflict, replacement, rollback, permission, legacy-
downgrade, and symlink-boundary tests; the real emitted-Cargo fixture; and all
other workspace targets. The changed Rust files also passed direct
`rustfmt --check`. Repository-wide `cargo fmt --all -- --check` remains blocked
by pre-existing formatting drift in `crates/ostadix-api/src/hosted_remote/v2/auth.rs`
and `tests/hosted_remote_cli.rs`, neither changed by this pairing work.

The local non-Cargo gates completed successfully:

```bash
bash -n scripts/o-cli.sh scripts/smoke-zero-config-lan-netns.sh o-node-quickstart.sh
python3 -m unittest -v tests.test_o_cli_dispatch tests.test_contract_surfaces
```

Those 24 Python tests verify exact dispatcher forwarding, the no-positional-
passcode boundary, the explicit `--replace` and routed-pairing flags, and the
required Linux smoke markers.

A two-process acceptance run then used separate HOME/XDG configuration and
state roots for two node identities. It proved initial direct pairing over
`--address`, bidirectional TLS 1.3 mutual-X.509 profiles with discovery
disabled, route-only `--node ... --address ...` reuse of stored identity, a
restart using remembered state, and explicit `--replace` recovery after one
side's peer directory was moved aside. Both directions succeeded after that
one-sided recovery. This is process and loopback evidence on macOS, not a
physical two-host or router/NAT claim.

The Linux non-loopback gate remains
`scripts/smoke-zero-config-lan-netns.sh`. It now checks wrong-code/no-state,
directly routed pairing on a veth address, reciprocal public material with
distinct locally retained private keys, one-use listener consumption, closed
legacy bootstrap ports in both directions, bidirectional restart reconnect,
explicit one-sided `--replace` recovery with key rotation, and a severed-link
failure. The script passed shell syntax validation but was
not executed on this macOS host: Docker was not running and was not restarted,
and the designated Multipass VM did not complete startup. Linux CI remains the
execution substrate for that gate.

## Historical zero-configuration integration record

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

The required `rust-hosted` CI lane also runs
`scripts/smoke-zero-config-lan-netns.sh`. That gate gives a multi-homed client
a silent decoy network as its default and multicast route while the real node
is reachable only through a non-default `192.0.2.0/30` veth. It exercises the
passcode-paired default, direct veth pairing, reciprocal stored identities,
closed plaintext bootstrap ports, bidirectional reconnect after restart, and
a severed-link failure. This is Linux veth evidence; it does not replace the
physical two-host acceptance path for Wi-Fi, router, firewall, macOS, IPv6,
NAT, or cross-subnet behavior.

Acceptance requires that ordinary client commands request no address, port,
hostname, CA, certificate, key, receipt key, capability, lease, operation ID,
task digest, or attempt generation.
