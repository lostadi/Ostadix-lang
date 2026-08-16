# DOI-ready release checklist

This checklist covers the **Ostadix-lang** component source archive. The
umbrella system release is named **OSTADIX Alpha**; an Ostadix-lang tag or DOI
does not by itself qualify that system release. OSTADIX Alpha additionally
requires G13 and the evidence bound by the qualification registry below.

This checklist is grounded in the current repository state: `Cargo.toml` version
`0.2.0`, `CITATION.cff` version `0.2.0`, the README citation example, the CI
workflow in `.github/workflows/ci.yml`, and the active release-claim guard in
`scripts/check_release_claims.sh`. The source-release builder and its tests
also reject disagreement among the root LGPL-2.1-only license text, Cargo
package metadata, `CITATION.cff`, and the live README citation prose. The
`olangc` unit suite parses both generated host and project Cargo manifests,
requires component-scoped metadata to identify the embedded runtime as
LGPL-2.1-only, records that embedded input licensing is retained by its source,
and keeps the mixed generated crates `publish = false`.

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
bash scripts/smoke-project-hgraph.sh
bash scripts/smoke-project-hgraph-exec.sh
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
./ocore/kernel/smoke-world-project-runtime-qemu.sh
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
python3 scripts/local_ci_posture.py --profile baseline --format text
```

The posture command is a standard-library-only, read-only contract check. See
[`CI_POSTURE.md`](CI_POSTURE.md) for its machine-readable form, optional local
analyzers, remote read-only inspection, and exit-code contract.

`smoke-world-project-runtime-qemu.sh` is the required no-argument Mode 32 gate.
It runs the focused hosted World-project test to produce a fresh caller-signed
receipt and semantic digest, then delegates to the direct vector interface. For
an additional caller-selected vector, pass the emitted lowercase-hex file and
hosted domain-separated unsigned-body semantic digest directly:

```bash
./ocore/kernel/smoke-world-project-receipt-qemu.sh RECEIPT_HEX_FILE EXPECTED_SEMANTIC_SHA256
```

The two-argument gate is not a source of receipt fixtures or signing keys. It
must fully decode and exactly re-encode the canonical receipt, construct the
validated signing preimage, require `ReceiptCommitFenceV1::Uncommitted`, and
match the hosted unsigned-body semantic SHA-256. It also reuses validation
scratch after a successful record with a malformed envelope and requires the
prior terminal/commit tags to have been reset. It does not execute the project
or verify Ed25519 natively.

The hosted World ResourceKey smoke is the bounded PR6 repository-conformance
gate. It verifies typed governed vocabulary, underlying identity helpers'
caller-pair comparison, generic/device/accelerator HGraph chaining, alias-aware
grounding projection, source-forgery rejection, and residual `HostWorld` on a
real CLI projection. Grounding itself checks only the bound World
epoch/membership. This is not Mode 31, a ResourceKey wire ABI, production
governed lowering, native/QEMU/hardware evidence, Governor authority, device
assignment, DMA/IOMMU isolation, Acceptance A, or G0--G13 passage.

The composite Project HGraph smoke contains bounded hosted PR7 planning and
World PR8-1/PR8-2 project-profile phases. It constructs an exact-provenance plan
and real HGraph operations from a checked-in project, validates logical
alternatives and prerequisites, route policy and equivalence metadata,
malformed/substitution rejection, stable nonexecuting IR/DOT output, residual
`HostWorld`, and ordinary `.O` IR compatibility. The PR8-1 phase checks
canonical `LogicalHGraphV1`
encoding/digest, raw and scheduler-expanded effect resources, strict decoding,
forged-governed-resource rejection, and the hosted no-authority boundary. That
digest is exact-source-bound; it does not normalize source or manifest
formatting. The PR8-2 phase checks canonical hosted-unbound and
snapshot-derived `DeploymentPlanV1` records, exact logical/bundle binding,
bundle-scoped role/path compatibility, deterministic provider proposals,
World/task hierarchy rejection, and trusted substitution rejection. Supported
hosted policies use only `AmbientHost` and `HostedCoordinator`; unsupported
hosted policies remain `Unresolved`. Its logical alternative branches may
remain serialized and cross-coupled by shared conservative ambient/resource
state chains; the smoke claims neither parallel execution nor independent host
mediation. It also checks byte parity between direct `olangc` and
repository-owned `o plan`, then compiles a real project binary, checks its CLI,
and runs opt-in AnySuccess for immediate short-circuit plus nonzero-to-success
continuation in disposable workspaces.

The execution Project HGraph smoke is the bounded
ProjectExec-A/ProjectExec-B opt-in hosted gate for one resolved
`Explicit`/`Default` alternative plus serial ordered
`Fallback`/`AnySuccess`. It checks shared isolated workspace execution, typed
value/success prerequisite edges, first-success prefix readiness, attempted
result retention, guard-skip/nonzero continuation, conservative `HostWorld`
progression, infrastructure aborts, and unsigned deterministic lifecycle
events. Trace v5 binds those events to the canonical logical graph schema and
digest plus the exact canonical hosted-unbound `DeploymentPlanV1` schema and
digest. Trusted plan-aware replay reconstructs that deployment artifact and
rejects its substitution. A terminal alternative may continue only after no
route child executed or after every executed route declares
`failure_continuation = "declared_idempotent"`; a failed prerequisite remains a
hard stop. Bundle v2 carries that contract, and complete traces pass plan-aware
semantic replay against the trusted HGraph. It is not parallel
race/cancellation, retry, authenticated placement, Governor authority,
exactly-once effects, native execution, hardware evidence, G1, or G0--G13
passage. The separate explicit World-bound adapter described below does not
change those ordinary opt-in semantics.

The snapshot-derived deployment record is a deterministic descriptive proposal
from caller-supplied exact snapshot and task identities. It is not current or
authenticated inventory, Governor admission, authority, dispatch, reservation,
health, or execution. Active
compatibility checks cover the exact bundle plus bundle-scoped role/path
declarations, runtime classes, executable/evaluator facts, platform and
ambient-environment guards, authority absence, and residual `HostWorld`
admission. Architecture, package, and failure-domain fields are currently
unconstrained or empty schema vocabulary; `require_current_world` checks only
World identity/epoch. The ordinary executor does not consume this plan; the
explicit hosted-reference World entry point does. It re-derives exact
logical/deployment/snapshot bindings and fences caller-supplied current
World/Governor; dedicated coordinator observer
node/domain/optional-process; dedicated coordinator attempt; selected provider
node/domain/optional-process/service and implementation; and every operation
task attempt inside `ProjectCoordinator` before any workspace or child process.
The coordinator attempt is distinct from all operation attempts and identifies
the World-bound trace.

Terminal `RuntimeGraphV1` is built only after plan-aware causal replay against
the trusted HGraph and exact deployment. Its neutral `RouteSettlement` covers
successful, nonzero, and guard-skipped results, and terminal residual
`HostWorld` aggregates every observed started/terminal operation; never-started
operations do not contribute. A caller-supplied Ed25519 signer then emits
canonical OWRECEIPT with an unconditional `Uncommitted` fence. Receipt placement
names the coordinator observer and the receipt context uses the coordinator
attempt, not the proposed provider or a per-operation attempt. The receipt
subject leaves package absent rather than storing the provider implementation.
Only route success produces receipt success. Mode 32 performs the native
canonical/unsigned-semantic comparison above.

Release wording must retain the boundary: this is no Governor admission or
commit, capability/lease grant, reservation, remote dispatch, recovery, or
exactly-once protocol. It is no native project execution or native Ed25519
verification; QEMU TCG is not physical hardware. It passes neither G1 nor
Workstream A acceptance, and G1 remains defined and unpassed.

The OSTADIX Alpha registry in
[`world_alpha_gates.toml`](../evidence/world_alpha_gates.toml) is a
qualification schema, not an additional set of portable release commands. Its
checked-in ledger view must report 14 defined entries--G0 plus 13 integration
gates through G13--with only G0 and G2 passed and `G13 DEFINED`. Schema v4 keeps
the registry definition-only; the validator derives status from immutable
attestations, typed transcript observations, evidence-class and claim floors,
dependency status, and separate history events. Hosted reference and virtual
multinode classes can never be substituted for physical/native qualification.

When derivation policy or validator code changes, use this order:

1. finish the validator and rule changes;
2. compute the final derivation and validator hashes;
3. append every required `rederive` event, recording the exact claims lost and
   gained without editing the original attestation;
4. validate the complete ledger and deterministic source release; and
5. only then ask an external CI/reviewer to witness or countersign the final
   implementation hash.

Repository CI checks the checked-in ledger, but it is not by itself an
independent trust anchor. Never pin an intermediate validator and never repair
history by rewriting an attestation. A later reviewer must append a separate
witness record rather than edit the correction event; until trusted-key policy
and cryptographic verification exist, the repository reports that envelope as
`external_unverified` and does not use it for gate status. Run ledger
validation from a full Git checkout: historical content-addressed working-tree
attestations may resolve
their immutable source bytes from a later reachable commit, and shallow clones
cannot establish that provenance.

<!-- BEGIN GENERATED: REQUIRED_QEMU_EVIDENCE_CHECKLIST -->
The portable native release surface contains exactly **26** required
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
| 2 | `ostadix-x86_64-boot-info` | OSTADIX Alpha x86_64 BootInfo / Mode 33 | `portable_tcg` | `ocore/kernel/smoke-x86_64-boot-info-qemu.sh` |
| 3 | `ostadix-x86_64-smp4` | OSTADIX Alpha x86_64 bounded SMP / Mode 34 | `portable_tcg` | `ocore/kernel/smoke-x86_64-smp-qemu.sh` |
| 4 | `world-g2-aarch64-native` | World G2 / AArch64 native compiler | `qemu_tcg_aarch64` | `ocore/kernel/smoke-aarch64-g2-qemu.sh` |
| 5 | `world-identity-v1` | World identity PR2 / Mode 27 | `portable_tcg` | `ocore/kernel/smoke-world-identity-qemu.sh` |
| 6 | `world-protocol-v1` | World protocol PR3 / Mode 28 | `portable_tcg` | `ocore/kernel/smoke-world-protocol-qemu.sh` |
| 7 | `world-value-v1` | World OValue PR4 / Mode 29 | `portable_tcg` | `ocore/kernel/smoke-world-value-qemu.sh` |
| 8 | `world-receipt-v1` | World receipt PR5 / Mode 30 | `portable_tcg` | `ocore/kernel/smoke-world-receipt-qemu.sh` |
| 9 | `world-project-runtime-mode32` | World project runtime / Mode 32 | `portable_tcg` | `ocore/kernel/smoke-world-project-runtime-qemu.sh` |
| 10 | `m02-fault-recovery` | M0.2 | `portable_tcg` | `ocore/kernel/smoke-faults-qemu.sh` |
| 11 | `m1-process-isolation` | M1 | `portable_tcg` | `ocore/kernel/smoke-processes-qemu.sh` |
| 12 | `m2-scheduler` | M2 | `portable_tcg` | `ocore/kernel/smoke-scheduler-qemu.sh` |
| 13 | `m3-ipc-foundation` | M3 foundation | `portable_tcg` | `ocore/kernel/smoke-ipc-foundation-qemu.sh` |
| 14 | `m3-public-ipc` | M3 | `portable_tcg` | `ocore/kernel/smoke-ipc-qemu.sh` |
| 15 | `m4-native-loader` | M4 | `portable_tcg` | `ocore/kernel/smoke-loader-qemu.sh` |
| 16 | `m5-native-live` | M5 | `portable_tcg` | `ocore/kernel/smoke-live-qemu.sh` |
| 17 | `m5-supervisor-semantics` | M5 semantics | `portable_tcg` | `ocore/kernel/smoke-live-semantics-qemu.sh` |
| 18 | `m6a-scalar-personality` | M6A | `portable_tcg` | `ocore/kernel/smoke-personality-qemu.sh` |
| 19 | `m6b-bounded-copy` | M6B mechanism | `portable_tcg` | `ocore/kernel/smoke-m6b-qemu.sh` |
| 20 | `m6b-live-bounded-personality` | M6B Mode 24 live | `portable_tcg` | `ocore/kernel/smoke-live-bounded-personality-qemu.sh` |
| 21 | `m6-linux-minimal-live` | M6 Linux Mode 25 live | `portable_tcg` | `ocore/kernel/smoke-live-linux-personality-qemu.sh` |
| 22 | `m7-linux-plan9-9p2000-live` | M7 Linux/Plan 9 Mode 26 live | `portable_tcg` | `ocore/kernel/smoke-live-linux-plan9-qemu.sh` |
| 23 | `m7b-logical-read-fallback-live` | M7B-1 native LogicalRead Mode 31 | `portable_tcg` | `ocore/kernel/smoke-m7b-logical-read-qemu.sh` |
| 24 | `kernel-world-mode20-objects` | KernelWorld Mode 20 | `portable_tcg` | `ocore/kernel/smoke-kernel-world-qemu.sh` |
| 25 | `kernel-world-mode22-live` | KernelWorld Mode 22 | `portable_tcg` | `ocore/kernel/smoke-kernel-world-live-qemu.sh` |
| 26 | `kernel-world-mode23-execution-device` | KernelWorld Mode 23 | `portable_tcg` | `ocore/kernel/smoke-kernel-world-execution-device-qemu.sh` |

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
OSTADIX Alpha constitution version 3 it also verifies sealed bytes for the
native constitution, hosted reference profile, composed executable G0
contracts, and typed `evidence/world_alpha_gates.toml`; it inertly validates
exact-byte-sealed historical attestations and ledger events plus their retained
transcripts/artifacts, topology, markers, and bounded non-claims. Only a current
schema-v3 active attestation binds its source digests to current ZIP members.
Schema-v2 source reconstruction requires the coherent
`c25d38c00283f2873eed1aa84dd89b437777e356` Git tree and is not archive-only
proof.
The allowlisted surface includes the separate `mcp/ostadix_lang_mcp_server`
crate, the root LGPL-2.1-only license, `.mcp.json`, its direct stdio smoke client,
and the focused MCP/example regression tests. CI tests and lints that crate with
its own lockfile, builds its release binary separately from the root package,
and runs `scripts/smoke_ostadix_mcp.py` against the real transport.

## Version synchronization points

Before tagging, verify all public version references agree:

- `Cargo.toml` `[package].version`.
- `CITATION.cff` `version`.
- The Git tag name, for example `v0.2.0`.
- The README citation example in `README.md` under "How to cite".
- `Cargo.toml` and `CITATION.cff` both declare `LGPL-2.1-only`, matching the root
  `LICENSE`; the attribution-only `NOTICE` does not grant an alternate license.

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

The existing DOI `10.5281/zenodo.21544345` identifies the preprint/package
record. It is not an archive of a tagged Ostadix-lang source release and must
remain the `preferred-citation` DOI. The steps below mint a separate DOI for a
future tagged source snapshot:

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

1. Fill the top-level `doi` field in `CITATION.cff` with the separate tagged
   source-release DOI; do not replace the existing `preferred-citation` DOI.
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
