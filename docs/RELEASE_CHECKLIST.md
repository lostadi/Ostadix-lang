# DOI-ready release checklist

This checklist is grounded in the current repository state: `Cargo.toml` version
`0.2.0`, `CITATION.cff` version `0.2.0`, the README citation example, the CI
workflow in `.github/workflows/ci.yml`, and the active release-claim guard in
`scripts/check_release_claims.sh`.

## Pre-tag validation

Run these commands from the repository root before creating an archival tag.
They are the release gate copied from CI plus the local release-claim guard:

The native artifact and QEMU gates require Clang, LLD, `llvm-objdump`, `nm`
(provided by binutils in CI), and `qemu-system-x86_64`. CI installs each
dependency explicitly; local runs must make them discoverable on `PATH` (or use
the documented linker override).

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --all-targets --all-features --verbose
bash scripts/check_declared_bins.sh
cargo test --all-targets --all-features --verbose
bash scripts/smoke-hosted-live-reference.sh
bash scripts/smoke-world-resource-keys.sh
cargo test --test parser_proptest
cargo test --lib ocore::driver::tests::ocore_object_is_byte_reproducible_across_source_directories -- --exact
cargo check --manifest-path fuzz/Cargo.toml
cargo build --release --locked --bin O --bin olangc
cargo test --locked --manifest-path mcp/ostadix_lang_mcp_server/Cargo.toml
cargo clippy --locked --manifest-path mcp/ostadix_lang_mcp_server/Cargo.toml -- -D warnings
cargo build --release --locked --manifest-path mcp/ostadix_lang_mcp_server/Cargo.toml
python3 scripts/smoke_ostadix_mcp.py
python3 scripts/release_evidence.py validate
cargo test --test world_identity_wire
./ocore/kernel/smoke-world-identity-qemu.sh
./ocore/kernel/smoke-world-value-qemu.sh
cargo test --test world_receipt
./ocore/kernel/smoke-world-receipt-qemu.sh
python3 scripts/world_alpha_evidence.py
python3 -m unittest -v tests.test_world_alpha_evidence
./boot-and-test.sh smoke
python3 -m tests.test_parser
python3 tests/example_manifest.py validate
python3 -m tests.test_evaluator
python3 -m compileall -q o_lang backends tests
make -C c_cpp clean && make -C c_cpp && make -C c_cpp test && make -C c_cpp olangc-test
make -C c_cpp warnings-as-errors
cmake -S c_cpp -B /tmp/olang-cmake-build -DCMAKE_BUILD_TYPE=Release && cmake --build /tmp/olang-cmake-build --parallel && ctest --test-dir /tmp/olang-cmake-build --output-on-failure
bash scripts/check_release_claims.sh
python3 -m unittest -v tests.test_source_release
```

The hosted World ResourceKey smoke is the bounded PR6 repository-conformance
gate. It verifies typed governed vocabulary, underlying identity helpers'
caller-pair comparison, generic/device/accelerator HGraph chaining, alias-aware
grounding partitioning, source-forgery rejection, and residual `HostWorld` on a
real CLI projection. Grounding itself checks only the bound World
epoch/membership. This is not Mode 31, a ResourceKey wire ABI, production
governed lowering, native/QEMU/hardware evidence, Governor authority, device
assignment, DMA/IOMMU isolation, Acceptance A, or G0--G13 passage.

The World Alpha registry in
[`world_alpha_gates.toml`](../evidence/world_alpha_gates.toml) is a
qualification schema, not an additional set of portable release commands. Its
checked-in baseline must report 14 defined entries--G0 plus 13 integration
gates through G13--zero passed gates, and `G13 DEFINED`. Schema v1 rejects every
`passed` status and every nonempty evidence list. The first passage requires a
new typed attestation schema binding the exact gate, source commit, commands,
transcripts, artifact digests,
hardware/topology inventory, and required signatures. Hosted reference and
virtual multinode classes can never be substituted for physical/native
qualification.

<!-- BEGIN GENERATED: REQUIRED_QEMU_EVIDENCE_CHECKLIST -->
The portable native release surface contains exactly **21** required
QEMU gates. `evidence/gates.toml` is authoritative; the aggregate, CI, this
checklist, and the README status table are validated projections. After each
successful gate, the aggregate requires every manifest marker exactly once in
the captured live stdout/stderr transcript.

```bash
python3 scripts/release_evidence.py validate
./boot-and-test.sh smoke
```

| Order | Gate | Milestone | Class | Script |
|------:|------|-----------|-------|--------|
| 1 | `ocore-bootstrap` | M0.1-M0.3 | `portable_tcg` | `ocore/kernel/smoke-qemu.sh` |
| 2 | `world-identity-v1` | World identity PR2 / Mode 27 | `portable_tcg` | `ocore/kernel/smoke-world-identity-qemu.sh` |
| 3 | `world-protocol-v1` | World protocol PR3 / Mode 28 | `portable_tcg` | `ocore/kernel/smoke-world-protocol-qemu.sh` |
| 4 | `world-value-v1` | World OValue PR4 / Mode 29 | `portable_tcg` | `ocore/kernel/smoke-world-value-qemu.sh` |
| 5 | `world-receipt-v1` | World receipt PR5 / Mode 30 | `portable_tcg` | `ocore/kernel/smoke-world-receipt-qemu.sh` |
| 6 | `m02-fault-recovery` | M0.2 | `portable_tcg` | `ocore/kernel/smoke-faults-qemu.sh` |
| 7 | `m1-process-isolation` | M1 | `portable_tcg` | `ocore/kernel/smoke-processes-qemu.sh` |
| 8 | `m2-scheduler` | M2 | `portable_tcg` | `ocore/kernel/smoke-scheduler-qemu.sh` |
| 9 | `m3-ipc-foundation` | M3 foundation | `portable_tcg` | `ocore/kernel/smoke-ipc-foundation-qemu.sh` |
| 10 | `m3-public-ipc` | M3 | `portable_tcg` | `ocore/kernel/smoke-ipc-qemu.sh` |
| 11 | `m4-native-loader` | M4 | `portable_tcg` | `ocore/kernel/smoke-loader-qemu.sh` |
| 12 | `m5-native-live` | M5 | `portable_tcg` | `ocore/kernel/smoke-live-qemu.sh` |
| 13 | `m5-supervisor-semantics` | M5 semantics | `portable_tcg` | `ocore/kernel/smoke-live-semantics-qemu.sh` |
| 14 | `m6a-scalar-personality` | M6A | `portable_tcg` | `ocore/kernel/smoke-personality-qemu.sh` |
| 15 | `m6b-bounded-copy` | M6B mechanism | `portable_tcg` | `ocore/kernel/smoke-m6b-qemu.sh` |
| 16 | `m6b-live-bounded-personality` | M6B Mode 24 live | `portable_tcg` | `ocore/kernel/smoke-live-bounded-personality-qemu.sh` |
| 17 | `m6-linux-minimal-live` | M6 Linux Mode 25 live | `portable_tcg` | `ocore/kernel/smoke-live-linux-personality-qemu.sh` |
| 18 | `m7-linux-plan9-9p2000-live` | M7 Linux/Plan 9 Mode 26 live | `portable_tcg` | `ocore/kernel/smoke-live-linux-plan9-qemu.sh` |
| 19 | `kernel-world-mode20-objects` | KernelWorld Mode 20 | `portable_tcg` | `ocore/kernel/smoke-kernel-world-qemu.sh` |
| 20 | `kernel-world-mode22-live` | KernelWorld Mode 22 | `portable_tcg` | `ocore/kernel/smoke-kernel-world-live-qemu.sh` |
| 21 | `kernel-world-mode23-execution-device` | KernelWorld Mode 23 | `portable_tcg` | `ocore/kernel/smoke-kernel-world-execution-device-qemu.sh` |

Supplemental hardware evidence is validated by the same manifest but is not
executed by the portable aggregate:

| Gate | Milestone | Class | Script |
|------|-----------|-------|--------|
| `kernel-world-mode21-svm-kvm` | KernelWorld Mode 21 | `hardware_kvm` | `ocore/kernel/smoke-kernel-world-execution-qemu.sh` |

Explicit supplemental non-claims:
- Mode 21 is supplemental hardware-dependent evidence and is not part of the portable release aggregate
- It does not boot Linux, Plan 9, firmware, or a supplied image
- It has no provider lifecycle, guest agent, service export, virtual device, PCI assignment, DMA mapping, or IOMMU-isolation proof

<!-- END GENERATED: REQUIRED_QEMU_EVIDENCE_CHECKLIST -->

The manifest also records each gate's positive claims, explicit non-claims,
required tools, and expected transcript markers. Static validation rejects a
missing or non-executable gate, projection drift, CI bypass, claim-guard bypass,
or loss of byte identity between the two aggregate entrypoints. During the
aggregate run, each gate's combined output remains visible and is also captured;
the gate counts as passed only when it exits successfully and every marker
declared for that script occurs exactly once in that live transcript. A marker
left in a source comment or dead string therefore cannot satisfy the release
gate. A directory scan or an "all present" result is not release evidence.

Build the public source ZIP from the exact commit or annotated tag that passed
the gate. The command rejects a dirty worktree by default and reads payload
bytes from the resolved Git commit, not from local build products:

```bash
python3 scripts/build_source_release.py \
  --ref v0.2.0 \
  --output dist/Ostadix-lang-source-v0.2.0.zip
python3 scripts/build_source_release.py \
  --verify dist/Ostadix-lang-source-v0.2.0.zip
```

The ZIP is deterministic for one commit and prefix. It contains a canonical
`SOURCE-MANIFEST.json` plus `SHA256SUMS`, and the command prints the digest of
the complete archive. Rebuilding the same ref must produce identical bytes.
Archive verification also checks relative Markdown-link closure: every local
target referenced outside code or comments by included documentation must be
present in the release. Git symlinks are refused rather than encoded as archive
members, and verification requires the writer's canonical ZIP metadata and
layout. It also parses, without importing or executing released code,
`.mcp.json`, the MCP crate metadata/license, `examples/manifest.json`, and
`evidence/gates.toml`, then proves their required archive-local references. For
World Alpha version 1 it also verifies sealed bytes for the native
constitution, hosted reference profile, and definition-only
`evidence/world_alpha_gates.toml`, and inertly validates the registry structure.
The allowlisted surface includes the separate `mcp/ostadix_lang_mcp_server`
crate, the root LGPL-2.1 license, `.mcp.json`, its direct stdio smoke client, and
the focused MCP/example regression tests. CI tests and lints that crate with its
own lockfile, builds its release binary separately from the root package, and
runs `scripts/smoke_ostadix_mcp.py` against the real transport.

## Version synchronization points

Before tagging, verify all public version references agree:

- `Cargo.toml` `[package].version`.
- `CITATION.cff` `version`.
- The Git tag name, for example `v0.2.0`.
- The README citation example in `README.md` under "How to cite".

For the current release candidate these all point at `0.2.0`; do not tag while
any one of them disagrees.

## Git tag and GitHub release

1. Choose the exact commit that passed the pre-tag validation above.
2. Create an annotated version tag, for example `git tag -a v0.2.0 -m "Ostadix-lang v0.2.0"`.
3. Push the tag only after verifying it points to the intended commit.
4. Draft a GitHub release for that tag.
5. Build and verify the allowlisted source ZIP from that tag. Attach that ZIP,
   not a recursive worktree archive, as the canonical source-release asset.
6. Record the printed whole-archive SHA-256 in the release notes.
7. Publish the GitHub release when the release notes and metadata are final.

## Zenodo DOI minting

1. Enable the repository in Zenodo's GitHub integration.
2. Confirm Zenodo sees `lostadi/Ostadix-lang` and is authorized to archive releases.
3. Publish the GitHub release for the tag.
4. Wait for Zenodo to archive that exact repository state.
5. Record the DOI minted by Zenodo for the versioned release, not just the
   concept DOI unless the citation intentionally targets the project as a whole.

## ORCID and DataCite metadata

- Verify `CITATION.cff` keeps the author ORCID
  `https://orcid.org/0009-0001-6380-9558`.
- After Zenodo mints the DOI, inspect the DataCite metadata for title, author
  name, ORCID, version, release date, repository URL, license, and keywords.
- Confirm GitHub's "Cite this repository" view surfaces the expected
  `CITATION.cff` metadata.

## Post-DOI updates

After DOI minting:

1. Fill the `doi` field in `CITATION.cff`.
2. Set `date-released` to the actual release date.
3. Update the README citation section to cite the DOI-bearing archived release.
4. Re-tag, amend, or make a follow-up metadata release as appropriate for the
   repository policy; preserve a clear public trail from source tag to DOI.

## Generated artifacts must not be published

Never publish generated build products as source-release content or release
assets unless a future release explicitly defines binary distribution. Exclude
at least:

- Rust `target/`.
- C edition products such as `c_cpp/O`, `c_cpp/olangc`, and `c_cpp/src/*.o`.
- CMake build directories.
- Python `__pycache__/` and bytecode.
- Local fuzz, coverage, and compiler outputs.
- `.DS_Store` and editor metadata.
- `.ocore-repair-backups/` and one-off repair patches.
- Generated HTML and intermediate `cvelist*` reports.

The allowlist and forbidden-path rules live in
`scripts/build_source_release.py`; changing the public release surface requires
an explicit edit there plus a regression-test update. Do not replace this gate
with `zip -r` over a development checkout.

## Manual GitHub settings outside code

These release items cannot be completed by source-code changes alone and must be
checked in GitHub/Zenodo settings:

- Repository description.
- Repository topics and keywords.
- Default branch protection.
- Zenodo webhook / GitHub integration state.
- GitHub "Cite this repository" surfacing through `CITATION.cff`.
