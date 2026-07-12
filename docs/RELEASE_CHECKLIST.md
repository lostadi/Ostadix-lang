# DOI-ready release checklist

This checklist is grounded in the current repository state: `Cargo.toml` version
`0.2.0`, `CITATION.cff` version `0.2.0`, the README citation example, the CI
workflow in `.github/workflows/ci.yml`, and the active release-claim guard in
`scripts/check_release_claims.sh`.

## Pre-tag validation

Run these commands from the repository root before creating an archival tag.
They are the release gate copied from CI plus the local release-claim guard:

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --all-targets --all-features --verbose
cargo test --all-targets --all-features --verbose
cargo test --test parser_proptest
cargo test --lib ocore::driver::tests::ocore_object_is_byte_reproducible_across_source_directories -- --exact
cargo check --manifest-path fuzz/Cargo.toml
python3 -m tests.test_parser
python3 -m tests.test_evaluator
python3 -m compileall -q o_lang backends tests
make -C c_cpp clean && make -C c_cpp && make -C c_cpp test && make -C c_cpp olangc-test
cmake -S c_cpp -B /tmp/olang-cmake-build -DCMAKE_BUILD_TYPE=Release && cmake --build /tmp/olang-cmake-build --parallel && ctest --test-dir /tmp/olang-cmake-build --output-on-failure
bash scripts/check_release_claims.sh
```

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
5. Use source archives produced by GitHub/Zenodo; do not attach generated build
   products as release assets.
6. Publish the GitHub release when the release notes and metadata are final.

## Zenodo DOI minting

1. Enable the repository in Zenodo's GitHub integration.
2. Confirm Zenodo sees `lostadi/O-lang` and is authorized to archive releases.
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

## Manual GitHub settings outside code

These release items cannot be completed by source-code changes alone and must be
checked in GitHub/Zenodo settings:

- Repository description.
- Repository topics and keywords.
- Default branch protection.
- Zenodo webhook / GitHub integration state.
- GitHub "Cite this repository" surfacing through `CITATION.cff`.
