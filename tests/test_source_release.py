"""Regression tests for the deterministic source-release builder."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest import mock
import zipfile


PROJECT_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = PROJECT_ROOT / "scripts" / "build_source_release.py"
SPEC = importlib.util.spec_from_file_location("ostadix_source_release", SCRIPT)
if SPEC is None or SPEC.loader is None:  # pragma: no cover - import failure is fatal
    raise RuntimeError(f"cannot import {SCRIPT}")
release = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = release
SPEC.loader.exec_module(release)

WORLD_NORMATIVE_BYTES = {
    path: (PROJECT_ROOT / path).read_bytes()
    for path in release.SEALED_WORLD_ALPHA_SHA256
}
WORLD_ATTESTATION_PATHS = (
    "evidence/world/g0-repository-conformance.toml",
    "evidence/world/g0-repository-conformance-2026-08-03.toml",
    "evidence/world/g0-repository-conformance-2026-08-03-v2.toml",
    "evidence/world/g0-ostadix-alpha-branding-2026-08-09.toml",
    "evidence/world/g0-independent-engine-2026-08-17.toml",
    "evidence/world/g2-aarch64-qemu.toml",
    "evidence/world/g2-aarch64-qemu-2026-08-03.toml",
)
WORLD_EVIDENCE_EVENT_PATHS = {
    "evidence/world/g0-derivation-rederive-2026-08-03.toml",
    "evidence/world/g0-machine-contract-supersession-2026-08-03.toml",
    "evidence/world/g0-ostadix-alpha-branding-supersession-2026-08-09.toml",
    "evidence/world/g0-independent-engine-supersession-2026-08-17.toml",
    "evidence/world/g0-schema-v3-supersession-2026-08-03.toml",
    "evidence/world/g2-derivation-rederive-2026-08-03.toml",
    "evidence/world/g2-counter-wording-supersession-2026-08-03.toml",
}
WORLD_EVIDENCE_RELEASE_PATHS = set(WORLD_ATTESTATION_PATHS) | WORLD_EVIDENCE_EVENT_PATHS
for _attestation_path in WORLD_ATTESTATION_PATHS:
    _attestation = tomllib.loads(
        (PROJECT_ROOT / _attestation_path).read_text(encoding="utf-8")
    )
    WORLD_EVIDENCE_RELEASE_PATHS.add(_attestation["transcript"])
    WORLD_EVIDENCE_RELEASE_PATHS.add(_attestation["command"][0][2:])
    WORLD_EVIDENCE_RELEASE_PATHS.update(
        release._released_path_for_historical_source(source["path"])
        for source in _attestation["source"]
    )
    WORLD_EVIDENCE_RELEASE_PATHS.update(
        artifact["path"]
        for artifact in _attestation["artifact"]
        if artifact["retained"]
    )

EVIDENCE_SCRIPT = PROJECT_ROOT / "scripts" / "release_evidence.py"
EVIDENCE_SPEC = importlib.util.spec_from_file_location(
    "ostadix_release_evidence", EVIDENCE_SCRIPT
)
if EVIDENCE_SPEC is None or EVIDENCE_SPEC.loader is None:  # pragma: no cover
    raise RuntimeError(f"cannot import {EVIDENCE_SCRIPT}")
evidence_tool = importlib.util.module_from_spec(EVIDENCE_SPEC)
sys.modules[EVIDENCE_SPEC.name] = evidence_tool
EVIDENCE_SPEC.loader.exec_module(evidence_tool)

FIXTURE_REQUIRED_EVIDENCE_GATE_COUNT = release.EXPECTED_REQUIRED_EVIDENCE_GATES
FIXTURE_EVIDENCE_GATE_COUNT = (
    FIXTURE_REQUIRED_EVIDENCE_GATE_COUNT
    + release.EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES
)
FIXTURE_CARGO = f"""\
[package]
name = "o-lang"
version = "0.1.0"
edition = "2021"
rust-version = "1.93.1"
repository = "{release.ROOT_REPOSITORY}"
authors = ["Lee Daghlar Ostadi"]
license = "{release.ROOT_LICENSE_SPDX}"

[workspace]
members = [".", "crates/ostadix-api"]
default-members = [".", "crates/ostadix-api"]
exclude = ["fuzz", "mcp/ostadix_lang_mcp_server"]
resolver = "2"

[features]
default = ["graph_executor"]
graph_executor = ["ostadix-api/graph_executor"]
notebook = ["ostadix-api/notebook"]

[dependencies]
ostadix-api = {{ path = "crates/ostadix-api", version = "=0.1.0", default-features = false }}
"""
FIXTURE_API_CARGO = f"""\
[package]
name = "ostadix-api"
version = "0.1.0"
edition = "2021"
rust-version = "1.93.1"
description = "Fixture independent runtime engine"
repository = "{release.ROOT_REPOSITORY}"
authors = ["Lee Daghlar Ostadi"]
license = "{release.ROOT_LICENSE_SPDX}"
readme = "README.md"
publish = true

[features]
default = ["graph_executor"]
graph_executor = []
notebook = []

[dependencies]
anyhow = "1"
"""
FIXTURE_PRIVATE_ENGINE_MODULES = {
    "backend_catalog",
    "canonical_cbor",
    "capability",
    "dispatch_model",
    "eval_core",
    "placement_protocol",
}
FIXTURE_PUBLIC_ENGINE_MODULES = (
    set(release.OSTADIX_API_ROOT_MODULE_PATHS) - FIXTURE_PRIVATE_ENGINE_MODULES
)
FIXTURE_ROOT_LIB_SOURCE = (
    "pub use ostadix_api::{\n    "
    + ", ".join(sorted(FIXTURE_PUBLIC_ENGINE_MODULES))
    + ",\n};\n"
)
FIXTURE_API_SOURCE = (
    "\n".join(
        f"pub mod {name};" for name in release.OSTADIX_API_ROOT_MODULE_PATHS
    )
    + "\npub use api::{Runtime, RuntimeError, RuntimeStage};\n"
)
FIXTURE_ENGINE_API_SOURCE = """\
pub mod aot_source;
use crate::ir::BackendRegistry;
pub use crate::eval::{Evaluator};
pub use crate::parser::Parser;
pub use num_bigint::BigInt;
pub enum RuntimeStage { Parse, Evaluate }
pub struct RuntimeError;
pub struct Runtime { evaluator: Evaluator }
impl Runtime {
    pub fn evaluate(&mut self, _source: &str) {}
}
"""
FIXTURE_AOT_SOURCE = """\
pub const RUNTIME_VALUE_RS: &str = include_str!("../value.rs");
"""
FIXTURE_API_TEST = """\
#[test]
fn complete_ovalue_payload_vocabulary_is_nameable_from_the_engine_root() {}
#[test]
fn runtime_owns_success_parse_failure_and_evaluate_failure_stages() {}
#[test]
fn engine_owns_the_full_runtime_without_a_compatibility_dependency() {}
"""
FIXTURE_CITATION = f"""\
cff-version: 1.2.0
message: "Cite this fixture."
title: "Release fixture"
type: software
version: "0.1.0"
license: {release.ROOT_LICENSE_SPDX}
repository-code: "{release.ROOT_REPOSITORY}"
authors:
  - family-names: Ostadi
    given-names: Lee Daghlar
preferred-citation:
  type: article
  title: "Fixture preprint"
  doi: "{release.EXISTING_PREPRINT_DOI}"
  url: "https://doi.org/{release.EXISTING_PREPRINT_DOI}"
"""
FIXTURE_LICENSE = """\
GNU LESSER GENERAL PUBLIC LICENSE
Version 2.1, February 1999
fixture copy of the license text
"""
FIXTURE_NOTICE = """\
Ostadix-lang fixture notice
Copyright fixture contributors
"""


def fixture_readme(extra: str = "") -> str:
    return f"""\
# Release fixture

committed [index](llms.txt)

## License

GNU Lesser General Public License v2.1 only (SPDX identifier
`{release.ROOT_LICENSE_SPDX}`). See [LICENSE](LICENSE) for the full text.

Generated AOT build crates are `publish = false`. Their component-scoped Cargo
metadata identifies the embedded Ostadix runtime as LGPL-2.1-only while marking
embedded user or project inputs as retaining the licensing attached to their
source; it does not declare one license for the mixed generated package.

## Citation

### How to cite

Use the existing Zenodo preprint/package record:
https://doi.org/{release.EXISTING_PREPRINT_DOI}

DOI `{release.EXISTING_PREPRINT_DOI}` identifies that existing preprint/package record;
it is not an archive of a tagged Ostadix-lang source release.

Source citation: Version 0.1.0. Commit: `FULL_COMMIT_SHA_USED`.
{release.ROOT_REPOSITORY}

Once Zenodo archives a future tagged source release, put its separate DOI in the
top-level `doi` field; the existing preprint/package DOI remains under
`preferred-citation`.

### Archival source releases

The two records remain distinct.

{extra}"""


def fixture_evidence_manifest() -> str:
    lines = [
        "schema_version = 2",
        f"required_gate_count = {FIXTURE_REQUIRED_EVIDENCE_GATE_COUNT}",
        "supplemental_gate_count = 1",
        'portable_command = "./boot-and-test.sh smoke"',
        "",
    ]
    for index in range(FIXTURE_EVIDENCE_GATE_COUNT):
        required = index < FIXTURE_REQUIRED_EVIDENCE_GATE_COUNT
        is_g2 = index == 1
        if is_g2:
            gate_id = release.G2_AARCH64_GATE_ID
            script = release.G2_AARCH64_SCRIPT
            evidence_class = "qemu_tcg_aarch64"
            required_tools = sorted(release.G2_AARCH64_REQUIRED_TOOLS)
            positive_claims = list(release.G2_AARCH64_POSITIVE_CLAIMS)
            nonclaims = list(release.G2_AARCH64_NONCLAIMS)
            expected_markers = list(release.G2_AARCH64_EXPECTED_MARKERS)
        else:
            gate_id = f"fixture-gate-{index:02}"
            script = f"ocore/kernel/fixture-evidence-{index:02}.sh"
            evidence_class = "portable_tcg" if required else "hardware_kvm"
            required_tools = sorted(
                release.EVIDENCE_COMMON_REQUIRED_TOOLS
                | release.EVIDENCE_CLASS_REQUIRED_TOOLS[evidence_class]
            )
            positive_claims = [f"fixture claim {index:02}"]
            nonclaims = [f"fixture nonclaim {index:02}"]
            expected_markers = [
                f"FIXTURE {index:02} START",
                f"FIXTURE {index:02} PASS",
            ]
        lines.extend(
            [
                "[[gate]]",
                f'id = {json.dumps(gate_id)}',
                f"required = {'true' if required else 'false'}",
                'milestone = "fixture"',
                f'script = {json.dumps(script)}',
                f'evidence_class = "{evidence_class}"',
                f"required_tools = {json.dumps(required_tools)}",
                f"positive_claims = {json.dumps(positive_claims)}",
                f"nonclaims = {json.dumps(nonclaims)}",
                f"expected_markers = {json.dumps(expected_markers)}",
                "",
            ]
        )
    return "\n".join(lines)


ZERO_GATE_EVIDENCE_MANIFEST = """\
schema_version = 2
required_gate_count = 0
supplemental_gate_count = 0
portable_command = "./boot-and-test.sh smoke"
gate = []
"""


class ReleaseEvidenceTranscriptTests(unittest.TestCase):
    def setUp(self) -> None:
        self.gate = {
            "id": "fixture-live-gate",
            "script": "ocore/kernel/fixture-live-gate.sh",
            "expected_markers": ["FIXTURE START", "FIXTURE PASS"],
        }

    def test_exact_runtime_markers_pass(self) -> None:
        gate_id, marker_count = evidence_tool.verify_transcript(
            [self.gate],
            "./ocore/kernel/fixture-live-gate.sh",
            b"diagnostic\nFIXTURE START\nFIXTURE PASS\n",
        )
        self.assertEqual(gate_id, "fixture-live-gate")
        self.assertEqual(marker_count, 2)

    def test_source_comment_cannot_replace_runtime_marker(self) -> None:
        source = b"#!/bin/sh\n# FIXTURE START\n# FIXTURE PASS\nexit 0\n"
        self.assertIn(b"FIXTURE PASS", source)
        with self.assertRaisesRegex(
            evidence_tool.EvidenceError, r"FIXTURE START.*: 0.*FIXTURE PASS.*: 0"
        ):
            evidence_tool.verify_transcript([self.gate], self.gate["script"], b"")

    def test_duplicate_runtime_marker_fails(self) -> None:
        with self.assertRaisesRegex(evidence_tool.EvidenceError, r"FIXTURE PASS.*: 2"):
            evidence_tool.verify_transcript(
                [self.gate],
                self.gate["script"],
                b"FIXTURE START\nFIXTURE PASS\nFIXTURE PASS\n",
            )


class RootReleaseMetadataValidationTests(unittest.TestCase):
    @staticmethod
    def fixture_files() -> dict[str, bytes]:
        return {
            "Cargo.toml": FIXTURE_CARGO.encode(),
            "CITATION.cff": FIXTURE_CITATION.encode(),
            "LICENSE": FIXTURE_LICENSE.encode(),
            "README.md": fixture_readme().encode(),
        }

    def test_live_repository_metadata_is_consistent(self) -> None:
        files = {
            path: (PROJECT_ROOT / path).read_bytes()
            for path in ("Cargo.toml", "CITATION.cff", "LICENSE", "README.md")
        }
        release._validate_root_release_metadata(files)

    def test_license_version_doi_and_prose_drift_fail_closed(self) -> None:
        mutations = {
            "cargo license": (
                "Cargo.toml",
                FIXTURE_CARGO.replace(release.ROOT_LICENSE_SPDX, "Apache-2.0"),
                r"Cargo\.toml package\.license must be 'LGPL-2\.1-only'",
            ),
            "citation license": (
                "CITATION.cff",
                FIXTURE_CITATION.replace(
                    f"license: {release.ROOT_LICENSE_SPDX}", "license: Apache-2.0"
                ),
                r"CITATION\.cff license must match Cargo\.toml",
            ),
            "citation version": (
                "CITATION.cff",
                FIXTURE_CITATION.replace('version: "0.1.0"', 'version: "9.9.9"'),
                r"CITATION\.cff version must match Cargo\.toml",
            ),
            "cargo tool author": (
                "Cargo.toml",
                FIXTURE_CARGO.replace("Lee Daghlar Ostadi", "OpenAI Codex"),
                r"package\.authors must not attribute Claude or Codex",
            ),
            "citation tool author": (
                "CITATION.cff",
                FIXTURE_CITATION.replace("Lee Daghlar", "Claude Sonnet 5"),
                r"authors line .* must not attribute Claude or Codex",
            ),
            "license text": (
                "LICENSE",
                "Apache License\nVersion 2.0\n",
                r"LICENSE does not contain the expected LGPL-2\.1 text",
            ),
            "preprint DOI": (
                "CITATION.cff",
                FIXTURE_CITATION.replace(
                    release.EXISTING_PREPRINT_DOI, "10.5281/zenodo.99999999"
                ),
                r"preferred-citation DOI must remain the existing preprint/package DOI",
            ),
            "relocated preferred DOI": (
                "CITATION.cff",
                FIXTURE_CITATION.replace(
                    f'  doi: "{release.EXISTING_PREPRINT_DOI}"\n', ""
                ).replace(
                    f'  url: "https://doi.org/{release.EXISTING_PREPRINT_DOI}"\n',
                    "",
                )
                + (
                    "unrelated-metadata:\n"
                    f'  doi: "{release.EXISTING_PREPRINT_DOI}"\n'
                    f'  url: "https://doi.org/{release.EXISTING_PREPRINT_DOI}"\n'
                ),
                r"CITATION\.cff is missing\s+doi:",
            ),
            "source DOI collision": (
                "CITATION.cff",
                FIXTURE_CITATION
                + f'doi: "{release.EXISTING_PREPRINT_DOI}"\n',
                r"top-level source-release DOI must differ.*preprint/package DOI",
            ),
            "README version": (
                "README.md",
                fixture_readme().replace("Version 0.1.0.", "Version 9.9.9."),
                r"README\.md How to cite must contain 'Version 0\.1\.0\.'",
            ),
            "citation prose": (
                "README.md",
                fixture_readme().replace(
                    "it is not an archive of a tagged Ostadix-lang source release.",
                    "it is the source release.",
                ),
                r"README\.md How to cite must contain.*not an archive",
            ),
            "generated-runtime license policy": (
                "README.md",
                fixture_readme().replace(
                    "embedded Ostadix runtime as LGPL-2.1-only",
                    "embedded runtime has no fixed license",
                ),
                r"README\.md generated-runtime license policy must contain.*embedded Ostadix",
            ),
        }
        for label, (path, replacement, message) in mutations.items():
            with self.subTest(label=label):
                files = self.fixture_files()
                files[path] = replacement.encode()
                with self.assertRaisesRegex(release.ReleaseError, message):
                    release._validate_root_release_metadata(files)


class WorkspaceFacadeReleaseValidationTests(unittest.TestCase):
    @staticmethod
    def fixture_files() -> dict[str, bytes]:
        files = {path: b"fixture\n" for path in release.OSTADIX_API_RELEASE_PATHS}
        files.update(
            {path: b"// fixture engine module\n" for path in release.OSTADIX_API_ROOT_MODULE_PATHS.values()}
        )
        files.update({
            "Cargo.toml": FIXTURE_CARGO.encode(),
            "LICENSE": FIXTURE_LICENSE.encode(),
            "NOTICE": FIXTURE_NOTICE.encode(),
            "src/lib.rs": FIXTURE_ROOT_LIB_SOURCE.encode(),
            "crates/ostadix-api/LICENSE": FIXTURE_LICENSE.encode(),
            "crates/ostadix-api/NOTICE": FIXTURE_NOTICE.encode(),
            "crates/ostadix-api/src/api.rs": FIXTURE_ENGINE_API_SOURCE.encode(),
            "crates/ostadix-api/src/api/aot_source.rs": FIXTURE_AOT_SOURCE.encode(),
            "crates/ostadix-api/Cargo.toml": FIXTURE_API_CARGO.encode(),
            "crates/ostadix-api/src/lib.rs": FIXTURE_API_SOURCE.encode(),
            "crates/ostadix-api/tests/public_surface.rs": FIXTURE_API_TEST.encode(),
        })
        return files

    def test_live_workspace_engine_is_structurally_closed(self) -> None:
        files = {
            path: (PROJECT_ROOT / path).read_bytes()
            for path in {
                "Cargo.toml",
                "LICENSE",
                "NOTICE",
                "src/lib.rs",
                *release.OSTADIX_API_RELEASE_PATHS,
                *release.OSTADIX_API_ROOT_MODULE_PATHS.values(),
            }
        }
        release._validate_workspace_facade_release_surface(files)

    def test_tool_authors_cannot_be_made_consistent_across_packages(self) -> None:
        files = self.fixture_files()
        files["Cargo.toml"] = FIXTURE_CARGO.replace(
            "Lee Daghlar Ostadi", "OpenAI Codex"
        ).encode()
        files["crates/ostadix-api/Cargo.toml"] = FIXTURE_API_CARGO.replace(
            "Lee Daghlar Ostadi", "OpenAI Codex"
        ).encode()
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"package\.authors must not attribute Claude or Codex",
        ):
            release._validate_workspace_facade_release_surface(files)

    def test_workspace_dependency_and_surface_drift_fail_closed(self) -> None:
        mutations = {
            "members": (
                "Cargo.toml",
                FIXTURE_CARGO.replace(
                    'members = [".", "crates/ostadix-api"]',
                    'members = ["."]',
                    1,
                ),
                r"workspace\.members",
            ),
            "default members": (
                "Cargo.toml",
                FIXTURE_CARGO.replace(
                    'default-members = [".", "crates/ostadix-api"]',
                    'default-members = ["."]',
                    1,
                ),
                r"workspace\.default-members",
            ),
            "excludes": (
                "Cargo.toml",
                FIXTURE_CARGO.replace(
                    'exclude = ["fuzz", "mcp/ostadix_lang_mcp_server"]',
                    'exclude = ["fuzz"]',
                    1,
                ),
                r"workspace\.exclude",
            ),
            "resolver": (
                "Cargo.toml",
                FIXTURE_CARGO.replace('resolver = "2"', 'resolver = "1"', 1),
                r"workspace\.resolver",
            ),
            "duplicate shell dependency": (
                "Cargo.toml",
                FIXTURE_CARGO
                + '\n[dev-dependencies]\nostadix-api = { path = "crates/ostadix-api" }\n',
                r"depend on ostadix-api exactly once",
            ),
            "engine version": (
                "crates/ostadix-api/Cargo.toml",
                FIXTURE_API_CARGO.replace('version = "0.1.0"', 'version = "9.9.9"', 1),
                r"package\.version",
            ),
            "shell dependency coordinate": (
                "Cargo.toml",
                FIXTURE_CARGO.replace('version = "=0.1.0"', 'version = "0.1"'),
                r"depend on ostadix-api exactly once",
            ),
            "engine reverse dependency": (
                "crates/ostadix-api/Cargo.toml",
                FIXTURE_API_CARGO
                + '\n[dev-dependencies]\no-lang = { path = "../.." }\n',
                r"must not depend on the o-lang compatibility shell",
            ),
            "not publishable": (
                "crates/ostadix-api/Cargo.toml",
                FIXTURE_API_CARGO.replace("publish = true", "publish = false"),
                r"package\.publish",
            ),
            "engine README binding": (
                "crates/ostadix-api/Cargo.toml",
                FIXTURE_API_CARGO.replace('readme = "README.md"', 'readme = "../README.md"'),
                r"package\.readme",
            ),
            "engine license bytes": (
                "crates/ostadix-api/LICENSE",
                FIXTURE_LICENSE + "engine-only drift\n",
                r"LICENSE must be byte-identical to LICENSE",
            ),
            "engine notice bytes": (
                "crates/ostadix-api/NOTICE",
                FIXTURE_NOTICE + "engine-only drift\n",
                r"NOTICE must be byte-identical to NOTICE",
            ),
            "shell compiles a module": (
                "src/lib.rs",
                FIXTURE_ROOT_LIB_SOURCE + "\npub mod eval;\n",
                r"must not compile runtime modules",
            ),
            "shell module closure": (
                "src/lib.rs",
                FIXTURE_ROOT_LIB_SOURCE.replace("backend, ", ""),
                r"compatibility module closure differs",
            ),
            "engine module closure": (
                "crates/ostadix-api/src/lib.rs",
                FIXTURE_API_SOURCE.replace("pub mod backend;\n", ""),
                r"module closure differs",
            ),
            "API reverse source path": (
                "crates/ostadix-api/src/api.rs",
                FIXTURE_ENGINE_API_SOURCE + "\nuse o_lang::value::OValue;\n",
                r"must not depend on o-lang source paths",
            ),
            "registry seam": (
                "crates/ostadix-api/src/api.rs",
                FIXTURE_ENGINE_API_SOURCE.replace(
                    "use crate::ir::BackendRegistry;\n", ""
                ),
                r"must retain engine seam",
            ),
            "AOT inventory": (
                "crates/ostadix-api/src/api/aot_source.rs",
                "pub const EMPTY: &str = \"\";\n",
                r"must own generated-runtime source bytes",
            ),
        }
        for label, (path, replacement, message) in mutations.items():
            with self.subTest(label=label):
                files = self.fixture_files()
                files[path] = replacement.encode()
                with self.assertRaisesRegex(release.ReleaseError, message):
                    release._validate_workspace_facade_release_surface(files)

    def test_engine_legal_metadata_is_required(self) -> None:
        for path in ("crates/ostadix-api/LICENSE", "crates/ostadix-api/NOTICE"):
            with self.subTest(path=path):
                files = self.fixture_files()
                del files[path]
                with self.assertRaisesRegex(
                    release.ReleaseError,
                    rf"workspace engine release surface is incomplete; missing: {path}",
                ):
                    release._validate_workspace_facade_release_surface(files)

    def test_engine_package_readme_is_required(self) -> None:
        path = "crates/ostadix-api/README.md"
        files = self.fixture_files()
        del files[path]
        with self.assertRaisesRegex(
            release.ReleaseError,
            rf"workspace engine release surface is incomplete; missing: {path}",
        ):
            release._validate_workspace_facade_release_surface(files)

    def test_hosted_tls_test_identity_is_required(self) -> None:
        for path in release.HOSTED_TLS_TEST_IDENTITY_PATHS:
            with self.subTest(path=path):
                files = self.fixture_files()
                del files[path]
                with self.assertRaisesRegex(
                    release.ReleaseError,
                    rf"workspace engine release surface is incomplete; missing: {path}",
                ):
                    release._validate_workspace_facade_release_surface(files)

    def test_engine_release_allowlist_is_scoped_not_crates_wide(self) -> None:
        for path in release.OSTADIX_API_RELEASE_PATHS:
            self.assertTrue(release.is_allowed_release_path(path))
        self.assertTrue(
            release.is_allowed_release_path("crates/ostadix-api/src/new_module.rs")
        )
        self.assertFalse(release.is_allowed_release_path("crates/private/Cargo.toml"))
        self.assertFalse(release.is_allowed_release_path("crates/ostadix-api/notes.txt"))
        self.assertTrue(release.is_allowed_release_path("src/lib.rs"))
        self.assertTrue(release.is_allowed_release_path("src/bin/o-cli.rs"))
        self.assertTrue(release.is_allowed_release_path("src/bin/olangc.rs"))
        self.assertFalse(release.is_allowed_release_path("src/eval.rs"))
        self.assertFalse(release.is_allowed_release_path("src/world/identity.rs"))


class SourceReleaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self._git("init", "-q")
        # These disposable one-commit repositories never benefit from Git's
        # detached maintenance. A background auto-GC can otherwise recreate
        # `.git/info/*` while TemporaryDirectory is removing the fixture.
        self._git("config", "gc.auto", "0")
        self._git("config", "maintenance.auto", "false")
        self._git("config", "user.name", "Source Release Test")
        self._git("config", "user.email", "source-release@example.invalid")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _git(self, *arguments: str) -> str:
        environment = os.environ.copy()
        environment.update(
            {
                "GIT_AUTHOR_DATE": "2001-02-03T04:05:06+0000",
                "GIT_COMMITTER_DATE": "2001-02-03T04:05:06+0000",
            }
        )
        result = subprocess.run(
            ["git", "-C", os.fspath(self.repo), *arguments],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
        )
        return result.stdout.strip()

    def _write(self, relative: str, data: str | bytes, *, executable: bool = False) -> None:
        destination = self.repo / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(data, bytes):
            destination.write_bytes(data)
        else:
            destination.write_text(data, encoding="utf-8")
        if executable:
            destination.chmod(0o755)

    def _assert_missing_required_paths(
        self, archive: str, paths: tuple[str, ...]
    ) -> None:
        with self.assertRaises(release.ReleaseError) as raised:
            self._build(archive)
        message = str(raised.exception)
        self.assertIn("missing required path(s)", message)
        for path in paths:
            self.assertIn(path, message)

    def _commit(self, files: dict[str, str | bytes] | None = None) -> str:
        contents = {
            ".dockerignore": "target\nOstadix-lang\n",
            ".github/dependabot.yml": "version: 2\nupdates: []\n",
            ".github/CODEOWNERS": "* @fixture\n",
            ".github/ISSUE_TEMPLATE/bug_report.yml": "name: Bug\ndescription: Fixture\nbody: []\n",
            ".github/ISSUE_TEMPLATE/config.yml": "blank_issues_enabled: false\n",
            ".github/ISSUE_TEMPLATE/feature_request.yml": "name: Feature\ndescription: Fixture\nbody: []\n",
            ".github/pull_request_template.md": "## Fixture\n",
            ".github/workflows/ci.yml": "name: fixture\n",
            ".github/workflows/fuzz.yml": "name: fuzz fixture\n",
            ".gitignore": "target/\nOstadix-lang/\n",
            ".mcp.json": (
                '{"mcpServers":{"ostadix":{"command":"ostadix-mcp","args":[]}}}\n'
            ),
            "CITATION.cff": FIXTURE_CITATION,
            "CHANGELOG.md": "# Changelog\n",
            "CODE_OF_CONDUCT.md": "# Code of Conduct\n",
            "CONTRIBUTING.md": "# Contributing\n",
            "Cargo.lock": "# fixture root lock\n",
            "Cargo.toml": FIXTURE_CARGO,
            "crates/ostadix-api/Cargo.toml": FIXTURE_API_CARGO,
            "crates/ostadix-api/LICENSE": FIXTURE_LICENSE,
            "crates/ostadix-api/NOTICE": FIXTURE_NOTICE,
            "crates/ostadix-api/README.md": "# Fixture independent engine\n",
            "crates/ostadix-api/src/api.rs": FIXTURE_ENGINE_API_SOURCE,
            "crates/ostadix-api/src/api/aot_source.rs": FIXTURE_AOT_SOURCE,
            "crates/ostadix-api/src/lib.rs": FIXTURE_API_SOURCE,
            "crates/ostadix-api/tests/public_surface.rs": FIXTURE_API_TEST,
            "Dockerfile": "FROM scratch\n",
            "LICENSE": FIXTURE_LICENSE,
            "NOTICE": FIXTURE_NOTICE,
            "README.md": fixture_readme(),
            "SECURITY.md": "# Security\n",
            "boot-and-test.sh": "#!/bin/sh\nexit 0\n",
            "ci/architecture-roots.toml": (
                PROJECT_ROOT / "ci/architecture-roots.toml"
            ).read_text(encoding="utf-8"),
            "ci/required-jobs.toml": (
                'schema = "ostadix.ci-required-jobs/v1"\nrequired_jobs = ["contracts"]\n'
            ),
            "ci/test-suites.toml": (
                'schema = "ostadix.ci-test-suites/v1"\n'
                '[suites.contracts]\nrequired_executables = ["python3"]\n'
            ),
            "rust-toolchain.toml": (
                '[toolchain]\nchannel = "1.97.1"\nprofile = "minimal"\n'
            ),
            "setup.sh": "#!/bin/sh\nexit 0\n",
            "docs/CLAIMS.md": "fixture claims\n",
            "docs/CI_POSTURE.md": "fixture local CI posture contract\n",
            "docs/IMAGE_ADMISSION.md": "fixture image-admission contract\n",
            "docs/INFORMATION_KERNEL_V1.md": "fixture information kernel contract\n",
            "docs/releases/v0.3.0.md": "# Ostadix-lang v0.3.0 fixture\n",
            "docs/HOSTED_LIVE_REFERENCE.md": "fixture hosted reference\n",
            "docs/HOSTED_PLACEMENT_V6.md": "fixture hosted placement contract\n",
            "docs/PROJECT_MESH_V1.md": "fixture project mesh contract\n",
            "docs/UNIFIED_INTENT_FRONT_DOOR_V1.md": "fixture unified intent front door contract\n",
            "docs/OIR_EXECUTION_FABRIC_V1.md": "fixture OIR execution-fabric contract\n",
            "docs/KERNEL_WORLD_CONTRACT.md": "fixture kernel World contract\n",
            "docs/HOSTED_WORLD_REFERENCE_PROFILE.md": WORLD_NORMATIVE_BYTES[
                "docs/HOSTED_WORLD_REFERENCE_PROFILE.md"
            ],
            "docs/O_MACHINE_CONTRACT.md": WORLD_NORMATIVE_BYTES[
                "docs/O_MACHINE_CONTRACT.md"
            ],
            "docs/OSTADIX_BOOT.md": "# Fixture OSTADIX boot contract\n",
            "docs/OSTADIX_WORLD.md": WORLD_NORMATIVE_BYTES[
                "docs/OSTADIX_WORLD.md"
            ],
            "docs/SEMANTIC_CUSTODY.md": "fixture semantic custody boundary\n",
            "docs/VERSIONING.md": "fixture version axes\n",
            "evidence/gates.toml": fixture_evidence_manifest(),
            "evidence/world_alpha_gates.toml": WORLD_NORMATIVE_BYTES[
                "evidence/world_alpha_gates.toml"
            ],
            "evidence/world_contract_v1.toml": WORLD_NORMATIVE_BYTES[
                "evidence/world_contract_v1.toml"
            ],
            "evidence/world_contract_v2.toml": WORLD_NORMATIVE_BYTES[
                "evidence/world_contract_v2.toml"
            ],
            "evidence/o_machine_contract_v1.toml": WORLD_NORMATIVE_BYTES[
                "evidence/o_machine_contract_v1.toml"
            ],
            "examples/manifest.json": json.dumps(
                {
                    "schema_version": 1,
                    "examples": [
                        {
                            "path": "semantic_custody.O",
                            "editions": ["rust"],
                            "classification": "integration",
                            "requirements": {
                                "backends": ["python", "text"],
                                "programs": ["python3"],
                                "authorities": ["process"],
                            },
                            "expected": {
                                "rust": {
                                    "patterns": ["semantic-custody answer=42"]
                                }
                            },
                        }
                    ],
                },
                sort_keys=True,
            )
            + "\n",
            "examples/docker_literal/main.py": "__oval_result__ = 42\n",
            "examples/semantic_custody.O": "text^(fixture)_text\n",
            "llms.txt": "release index\n",
            "mcp/ostadix_lang_mcp_server/Cargo.lock": (
                "# This file is automatically @generated by Cargo.\n"
                "version = 4\n\n"
                "[[package]]\n"
                'name = "ostadix-mcp-server"\n'
                'version = "0.1.0"\n'
            ),
            "mcp/ostadix_lang_mcp_server/Cargo.toml": (
                "[package]\n"
                'name = "ostadix-mcp-server"\n'
                'version = "0.1.0"\n'
                'license = "LGPL-2.1-only"\n'
                "publish = false\n\n"
                "[[bin]]\n"
                'name = "ostadix-mcp"\n'
                'path = "src/main.rs"\n'
            ),
            "mcp/ostadix_lang_mcp_server/README.md": "fixture MCP\n",
            "mcp/ostadix_lang_mcp_server/src/main.rs": "fn main() {}\n",
            "okernel-multikernel/boot-and-test.sh": "#!/bin/sh\nexit 0\n",
            "okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md": "proposal\n",
            "ocore/kernel/boot.S": ".section .text\n",
            "ocore/kernel/aarch64/boot.S": ".section .text\n",
            "ocore/kernel/aarch64/linker.ld": "ENTRY(_start)\n",
            "ocore/kernel/aarch64/vectors.S": ".section .text\n",
            "ocore/kernel/build-aarch64-g2.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/build-x86_64-uefi-media.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/build.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/main.oc": "module kernel::main;\n",
            "ocore/kernel/m6_mode25_diagnostics.oc": (
                "module kernel::m6_mode25_diagnostics;\n"
            ),
            "ocore/kernel/m6_mode25_diagnostics_stub.oc": (
                "module kernel::m6_mode25_diagnostics;\n"
            ),
            "ocore/kernel/resolve-x86_64-ovmf-code.sh": "# resolver fixture\n",
            "ocore/kernel/smp_probe.oc": "module kernel::smp_probe;\n",
            "ocore/kernel/smp_probe_stub.oc": "module kernel::smp_probe;\n",
            "ocore/kernel/run-x86_64-uefi-media-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-x86_64-boot-info-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-x86_64-smp-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-x86_64-uefi-media-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-world-project-receipt-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-world-project-runtime-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-world-receipt-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-world-value-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-world-protocol-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-world-identity-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-aarch64-g2-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/stress-live-linux-personality-qemu.sh": (
                "#!/bin/bash\nexit 0\n"
            ),
            "ocore/kernel/world_value_semantics.oc": (
                "module kernel::world_value_semantics;\n"
            ),
            "ocore/kernel/world_value_semantics_stub.oc": (
                "module kernel::world_value_semantics;\n"
            ),
            "ocore/kernel/world_protocol_semantics.oc": (
                "module kernel::world_protocol_semantics;\n"
            ),
            "ocore/kernel/world_protocol_semantics_stub.oc": (
                "module kernel::world_protocol_semantics;\n"
            ),
            "ocore/kernel/world_identity_semantics.oc": (
                "module kernel::world_identity_semantics;\n"
            ),
            "ocore/kernel/world_identity_semantics_stub.oc": (
                "module kernel::world_identity_semantics;\n"
            ),
            "ocore/kernel/world_project_receipt_semantics.oc": (
                "module kernel::world_project_receipt_semantics;\n"
            ),
            "ocore/kernel/world_project_receipt_semantics_stub.oc": (
                "module kernel::world_project_receipt_semantics;\n"
            ),
            "ocore/kernel/world_receipt_semantics.oc": (
                "module kernel::world_receipt_semantics;\n"
            ),
            "ocore/kernel/world_receipt_semantics_stub.oc": (
                "module kernel::world_receipt_semantics;\n"
            ),
            "ocore/kernel/x86_64/grub.cfg": "multiboot2 /boot/okernel.elf\n",
            "ocore/kernel/x86_64/boot_info.oc": "module kernel::boot_info;\n",
            "ocore/kernel/x86_64/boot_info_stub.oc": "module kernel::boot_info;\n",
            "ocore/runtime/x86_64/trap.oc": "module runtime::trap;\n",
            "ocore/runtime/aarch64/g2_kernel.oc": "module runtime::g2_kernel;\n",
            "ocore/runtime/aarch64/g2_user_a.oc": "module runtime::g2_user_a;\n",
            "ocore/runtime/aarch64/g2_user_b.oc": "module runtime::g2_user_b;\n",
            "ocore/world/codec.oc": "module world::codec;\n",
            "ocore/world/identity.oc": "module world::identity;\n",
            "ocore/world/protocol.oc": "module world::protocol;\n",
            "ocore/world/receipt.oc": "module world::receipt;\n",
            "ocore/world/receipt_codec.oc": "module world::receipt_codec;\n",
            "ocore/world/sha256.oc": "module world::sha256;\n",
            "ocore/world/value.oc": "module world::value;\n",
            "ocore/world/value_codec.oc": "module world::value_codec;\n",
            "scripts/smoke_ostadix_mcp.py": "#!/usr/bin/env python3\n",
            "scripts/smoke-docker.sh": "#!/usr/bin/env bash\n",
            "scripts/smoke-zero-config-lan-netns.sh": "#!/usr/bin/env bash\n",
            "scripts/semantic_custody_demo.sh": "#!/usr/bin/env bash\n",
            "scripts/contract_surfaces.py": "#!/usr/bin/env python3\n",
            "scripts/check_attribution.py": "#!/usr/bin/env python3\n",
            "scripts/check_architecture_boundaries.py": "#!/usr/bin/env python3\n",
            "scripts/local_ci_posture.py": "#!/usr/bin/env python3\n",
            "scripts/install-o-cli-wrapper.sh": "#!/usr/bin/env bash\n",
            "scripts/o-cli.sh": "#!/usr/bin/env bash\nexec true\n",
            "scripts/o-kernel.sh": "#!/usr/bin/env bash\nexec true\n",
            "scripts/ostadix_boot_media.py": "#!/usr/bin/env python3\n",
            "scripts/ostadix_boot_info_qemu.py": "#!/usr/bin/env python3\n",
            "scripts/ostadix_media_writer.py": "#!/usr/bin/env python3\n",
            "scripts/ostadix_physical_evidence.py": "#!/usr/bin/env python3\n",
            "scripts/smoke-project-hgraph-exec.sh": "#!/usr/bin/env bash\n",
            "scripts/smoke-project-hgraph.sh": "#!/usr/bin/env bash\n",
            "scripts/smoke-world-resource-keys.sh": "#!/usr/bin/env bash\n",
            "scripts/smoke-world-g0-conformance.sh": "#!/usr/bin/env bash\n",
            "scripts/release_evidence.py": "#!/usr/bin/env python3\n",
            "scripts/world_alpha_evidence.py": "#!/usr/bin/env python3\n",
            "crates/ostadix-api/src/backend_catalog.rs": "// fixture canonical backend catalog implementation\n",
            "crates/ostadix-api/src/backend_catalog.inc.rs": "// fixture canonical backend catalog data\n",
            "crates/ostadix-api/src/backend.rs": "// fixture backend runtime\n",
            "crates/ostadix-api/src/backend_morphism.rs": "// fixture shadow backend morphism kernel\n",
            "crates/ostadix-api/src/backend_state.rs": "// fixture versioned backend state protocol\n",
            "crates/ostadix-api/src/canonical_cbor.rs": "// fixture canonical CBOR codec\n",
            "crates/ostadix-api/src/dispatch_model.rs": "// fixture pure dispatch contract\n",
            "crates/ostadix-api/src/evidence/admit.rs": "// fixture evidence admission compiler\n",
            "crates/ostadix-api/src/evidence/analyze.rs": "// fixture pre-execution evidence analyzer\n",
            "crates/ostadix-api/src/evidence/fact.rs": "// fixture evidence contract vocabulary\n",
            "crates/ostadix-api/src/evidence/intent.rs": "// fixture stable execution-intent projection\n",
            "crates/ostadix-api/src/evidence/mod.rs": "pub mod admit;\n",
            "crates/ostadix-api/src/evidence/profile.rs": "// fixture non-authoritative cost profiles\n",
            "crates/ostadix-api/src/effects.rs": "// fixture governed effect vocabulary\n",
            "crates/ostadix-api/src/eval.rs": "// fixture evaluator state bridge\n",
            "crates/ostadix-api/src/eval_core.rs": "// fixture evaluator-independent graph contract\n",
            "crates/ostadix-api/src/execution_contract.rs": "// fixture canonical execution contract\n",
            "crates/ostadix-api/src/intent/mod.rs": "// fixture unified intent API\n",
            "crates/ostadix-api/src/intent/record.rs": "// fixture private run-record schema\n",
            "crates/ostadix-api/src/intent/store.rs": "// fixture private run-record store\n",
            "crates/ostadix-api/src/runtime_exec.rs": "// fixture direct-launch executable authority\n",
            "crates/ostadix-api/src/syntax_dialect.rs": "// fixture parser syntax dialect\n",
            "src/bin/o-cli.rs": "// fixture compiled intent front door\n",
            "src/bin/o-info.rs": "// fixture local information CLI\n",
            "src/bin/o-node.rs": "// fixture direct hosted-node CLI\n",
            "src/bin/o-registry.rs": "// fixture signed local registry CLI\n",
            "src/bin/octl.rs": "// fixture direct hosted-node client CLI\n",
            "src/bin/olink.rs": "// fixture project linker CLI\n",
            "src/bin/olangc.rs": "use o_lang::api::aot_source::*;\n",
            "src/bin/ocorec.rs": "// fixture O-core compiler CLI\n",
            "crates/ostadix-api/src/ocore/codegen.rs": "// fixture x86_64 O-core code generator\n",
            "crates/ostadix-api/src/ocore/codegen_aarch64.rs": "// fixture AArch64 O-core code generator\n",
            "crates/ostadix-api/src/ocore/boot_info.rs": "// fixture BootInfoV1 contract\n",
            "crates/ostadix-api/src/ocore/driver.rs": "// fixture O-core target driver\n",
            "crates/ostadix-api/src/ocore/mod.rs": "// fixture O-core module exports\n",
            "crates/ostadix-api/src/executor/mod.rs": "// fixture public executor effects surface\n",
            "crates/ostadix-api/src/executor/driver.rs": "// fixture attempt-driver boundary\n",
            "crates/ostadix-api/src/executor/pool.rs": "// fixture persistent local-worker pool\n",
            "crates/ostadix-api/src/executor/task.rs": "// fixture prepared-task contract\n",
            "crates/ostadix-api/src/execution_fabric/codec.rs": "// fixture execution-fabric codec\n",
            "crates/ostadix-api/src/execution_fabric/protocol.rs": "// fixture execution-fabric protocol\n",
            "crates/ostadix-api/src/hgraph/graph.rs": "// fixture HGraph validation\n",
            "crates/ostadix-api/src/hgraph/kinds.rs": "// fixture HGraph operation vocabulary\n",
            "crates/ostadix-api/src/hgraph/from_oir.rs": "// fixture HGraph effect lowering\n",
            "crates/ostadix-api/src/hgraph/solve.rs": "// fixture HGraph fidelity transfer\n",
            "crates/ostadix-api/src/hosted_remote/client.rs": "// fixture hosted client\n",
            "crates/ostadix-api/src/hosted_remote/mod.rs": "pub mod protocol;\n",
            "crates/ostadix-api/src/hosted_remote/mesh.rs": "// fixture hosted mesh data plane\n",
            "crates/ostadix-api/src/hosted_remote/node.rs": "// fixture hosted node runtime\n",
            "crates/ostadix-api/src/hosted_remote/paths.rs": "// fixture hosted paths\n",
            "crates/ostadix-api/src/hosted_remote/project_mesh.rs": "// fixture project mesh scheduler\n",
            "crates/ostadix-api/src/hosted_remote/protocol.rs": "// fixture hosted V1 protocol\n",
            "crates/ostadix-api/src/hosted_remote/tls.rs": "// fixture hosted TLS\n",
            "crates/ostadix-api/src/hosted_remote/v2/auth.rs": "// fixture hosted V2 authority adapter\n",
            "crates/ostadix-api/src/hosted_remote/v2/client.rs": "// fixture hosted V2 client\n",
            "crates/ostadix-api/src/hosted_remote/v2/crypto.rs": "// fixture hosted V2 signatures\n",
            "crates/ostadix-api/src/hosted_remote/v2/dev.rs": "// fixture hosted V2 development authority\n",
            "crates/ostadix-api/src/hosted_remote/v2/mod.rs": "pub mod protocol;\n",
            "crates/ostadix-api/src/hosted_remote/v2/protocol.rs": "// fixture hosted V2 wire protocol\n",
            "crates/ostadix-api/src/hosted_remote/v2/runtime.rs": "// fixture hosted V2 state machine\n",
            "crates/ostadix-api/src/hosted_remote/v2/server.rs": "// fixture hosted V2 server\n",
            "crates/ostadix-api/src/hosted_remote/v2/store.rs": "// fixture hosted V2 durable store\n",
            "crates/ostadix-api/src/information/acquisition.rs": "// fixture bounded information acquisition\n",
            "crates/ostadix-api/src/information/decision.rs": "// fixture information decisions\n",
            "crates/ostadix-api/src/information/delta.rs": "// fixture information deltas\n",
            "crates/ostadix-api/src/information/exchange.rs": "// fixture signed offline information packs\n",
            "crates/ostadix-api/src/information/id.rs": "// fixture information identities\n",
            "crates/ostadix-api/src/information/invalidation.rs": "// fixture information invalidation\n",
            "crates/ostadix-api/src/information/loss.rs": "// fixture projection loss contract\n",
            "crates/ostadix-api/src/information/mod.rs": "// fixture information module\n",
            "crates/ostadix-api/src/information/model.rs": "// fixture information atoms\n",
            "crates/ostadix-api/src/information/projection.rs": "// fixture information projections\n",
            "crates/ostadix-api/src/information/provenance.rs": "// fixture V2 provenance witness vocabulary\n",
            "crates/ostadix-api/src/information/root.rs": "// fixture information roots\n",
            "crates/ostadix-api/src/information/store.rs": "// fixture local information store\n",
            "crates/ostadix-api/src/information_bridge/mod.rs": "// fixture read-only information bridge\n",
            "crates/ostadix-api/src/information_provenance/mod.rs": "// fixture V2 provenance sidecar admission\n",
            "crates/ostadix-api/src/ir.rs": "// fixture canonical backend catalog projection\n",
            "src/lib.rs": FIXTURE_ROOT_LIB_SOURCE,
            "src/main.rs": "fn main() {}\n",
            "crates/ostadix-api/src/placement/mod.rs": "pub mod protocol;\n",
            "crates/ostadix-api/src/placement/projection.rs": "// fixture OIR requirement projection\n",
            "crates/ostadix-api/src/placement/protocol/candidate.rs": "// fixture candidate validation\n",
            "crates/ostadix-api/src/placement/protocol/catalog.rs": "// fixture catalog authority trait\n",
            "crates/ostadix-api/src/placement/protocol/digest.rs": "// fixture semantic digest\n",
            "crates/ostadix-api/src/placement/protocol/error.rs": "// fixture placement validation errors\n",
            "crates/ostadix-api/src/placement/protocol/mod.rs": "pub mod records;\n",
            "crates/ostadix-api/src/placement/protocol/records.rs": "// fixture placement validity records\n",
            "crates/ostadix-api/src/placement/protocol/requirement.rs": "// fixture placement requirements\n",
            "crates/ostadix-api/src/placement/protocol/state.rs": "// fixture state and quota protocol\n",
            "crates/ostadix-api/src/placement/protocol/target.rs": "// fixture placement target descriptors\n",
            "crates/ostadix-api/src/placement/protocol/warrant.rs": "// fixture placement warrants\n",
            "crates/ostadix-api/src/process.rs": "// fixture persistent backend lifecycle\n",
            "crates/ostadix-api/src/version.rs": "// fixture machine-readable version report\n",
            "crates/ostadix-api/src/project/executor.rs": "// fixture project HGraph executor\n",
            "crates/ostadix-api/src/project/deployment.rs": "// fixture canonical project deployment plan\n",
            "crates/ostadix-api/src/project/launch.rs": "// fixture World-bound project launch\n",
            "crates/ostadix-api/src/project/logical.rs": "// fixture canonical project logical HGraph\n",
            "crates/ostadix-api/src/project/mod.rs": "pub mod plan;\n",
            "crates/ostadix-api/src/project/model.rs": "// fixture project model\n",
            "crates/ostadix-api/src/project/plan.rs": "// fixture project HGraph planner\n",
            "crates/ostadix-api/src/project/runtime.rs": "// fixture shared project selection\n",
            "crates/ostadix-api/src/project/runtime_graph.rs": "// fixture observed project RuntimeGraph\n",
            "crates/ostadix-api/src/project/trace.rs": "// fixture project HGraph trace\n",
            "crates/ostadix-api/src/project/world_execution.rs": (
                "// fixture bounded World-project execution and receipt emission\n"
            ),
            "crates/ostadix-api/src/registry/store.rs": "// fixture transactional signed registry store\n",
            "crates/ostadix-api/src/registry/bundle/mod.rs": "// fixture canonical backend bundle\n",
            "crates/ostadix-api/src/registry/placement_compat.rs": "// fixture registry placement compatibility\n",
            "crates/ostadix-api/src/world/grounding.rs": "// fixture World grounding projection\n",
            "crates/ostadix-api/src/world/identity.rs": "// fixture World identities\n",
            "crates/ostadix-api/src/world/identity_wire.rs": "// fixture World identity wire oracle\n",
            "crates/ostadix-api/src/world/codec.rs": "// fixture World protocol codec oracle\n",
            "crates/ostadix-api/src/world/mod.rs": "pub mod identity;\n",
            "crates/ostadix-api/src/world/protocol.rs": "// fixture World protocol vocabulary\n",
            "crates/ostadix-api/src/world/receipt.rs": "// fixture canonical World receipt vocabulary\n",
            "crates/ostadix-api/src/world/receipt_codec.rs": "// fixture canonical World receipt codec\n",
            "crates/ostadix-api/src/world/value.rs": "// fixture portable World value vocabulary\n",
            "crates/ostadix-api/src/world/value_codec.rs": "// fixture portable World value codec\n",
            "tests/example_manifest.py": "# fixture example manifest consumer\n",
            "tests/hosted_remote_v2.rs": "// fixture hosted V2 integration tests\n",
            "tests/fixtures/world_identity_v1.hex": "4f574944454e5431\n",
            "tests/fixtures/project_hgraph/input.txt": "fixture input\n",
            "tests/fixtures/project_hgraph/olang.project.toml": "[project]\nname = \"fixture\"\n",
            "tests/fixtures/project_hgraph_exec/input.txt": "fixture exec input\n",
            "tests/fixtures/project_hgraph_exec/olang.project.toml": "[project]\nname = \"fixture-exec\"\n",
            "tests/fixtures/project_hgraph_tools/sh": "#!/bin/sh\nexec /bin/sh \"$@\"\n",
            "tests/fixtures/world_protocol_v1.hex": "4f5750524f544f31\n",
            "tests/fixtures/world_receipt_v1.hex": "4f57524543454950\n",
            "tests/fixtures/world_value_v1.hex": "4f5756414c554531\n",
            "tests/test_example_manifest.py": "# fixture example manifest tests\n",
            "tests/test_attribution_policy.py": "# fixture attribution policy tests\n",
            "tests/test_contract_surfaces.py": "# fixture contract projection tests\n",
            "tests/test_architecture_boundaries.py": "# fixture architecture boundary tests\n",
            "tests/test_governance_surfaces.py": "# fixture governance surface tests\n",
            "tests/test_local_ci_posture.py": "# fixture local CI posture tests\n",
            "tests/test_backend_state_protocol.py": "# fixture backend state protocol tests\n",
            "tests/test_mcp_smoke.py": "# fixture MCP smoke tests\n",
            "tests/test_ostadix_boot_media.py": "# fixture boot-media tests\n",
            "tests/test_ostadix_boot_info_qemu.py": "# fixture boot-info QEMU tests\n",
            "tests/test_ostadix_media_writer.py": "# fixture media-writer tests\n",
            "tests/test_ostadix_physical_evidence.py": "# fixture physical-evidence tests\n",
            "tests/test_o_cli_dispatch.py": "# fixture lowercase CLI dispatch tests\n",
            "tests/unified_intent_acceptance.rs": "#[test] fn unified_intent_acceptance_fixture() {}\n",
            "tests/o_cli_intent_blackbox.rs": "#[test] fn o_cli_intent_blackbox_fixture() {}\n",
            "tests/unified_plan_boundaries.rs": "#[test] fn unified_plan_boundaries_fixture() {}\n",
            "tests/test_release_evidence.py": "# fixture release evidence tests\n",
            "tests/test_setup.py": "# fixture setup tests\n",
            "tests/test_bundled_shim_protocol.py": "# fixture bundled shim protocol tests\n",
            "tests/backend_morphism_v1.rs": "#[test] fn backend_morphism_fixture() {}\n",
            "tests/test_world_alpha_evidence.py": "# fixture World evidence tests\n",
            "tests/hosted_remote_cli.rs": "#[test] fn hosted_remote_cli_fixture() {}\n",
            "tests/project_mesh_cli.rs": "#[test] fn project_mesh_cli_fixture() {}\n",
            "tests/support/mod.rs": "// fixture shared integration-test support\n",
            "tests/o_info_cli.rs": "#[test] fn o_info_cli_fixture() {}\n",
            "tests/information_bridge_v1.rs": "#[test] fn information_bridge_fixture() {}\n",
            "tests/placement_v6.rs": "#[test] fn placement_v6_fixture() {}\n",
            "tests/registry_v1.rs": "#[test] fn registry_v1_fixture() {}\n",
            "backends/o_shim_common.py": "# fixture common shim protocol loop\n",
            "tests/project_hgraph.rs": "#[test] fn project_hgraph_fixture() {}\n",
            "tests/project_hgraph_exec.rs": "#[test] fn project_hgraph_exec_fixture() {}\n",
            "tests/project_deployment_plan.rs": "#[test] fn project_deployment_plan_fixture() {}\n",
            "tests/project_logical_hgraph.rs": "#[test] fn project_logical_hgraph_fixture() {}\n",
            "tests/project_world_runtime.rs": "#[test] fn project_world_runtime_fixture() {}\n",
            "tests/world_resource_keys.rs": "#[test] fn resource_key_fixture() {}\n",
            "tests/world_identity.rs": "#[test] fn identity_fixture() {}\n",
            "tests/world_identity_wire.rs": "#[test] fn wire_fixture() {}\n",
            "tests/world_protocol.rs": "#[test] fn protocol_fixture() {}\n",
            "tests/world_receipt.rs": "#[test] fn receipt_fixture() {}\n",
            "tests/world_value.rs": "#[test] fn value_fixture() {}\n",
        }
        for path in WORLD_EVIDENCE_RELEASE_PATHS:
            contents[path] = (PROJECT_ROOT / path).read_bytes()
        for path in release.HOSTED_HGRAPH_BENCHMARK_RELEASE_PATHS:
            contents[path] = (PROJECT_ROOT / path).read_bytes()
        for path in release.OSTADIX_API_ROOT_MODULE_PATHS.values():
            contents.setdefault(path, "// fixture engine root module\n")
        for path in release.OSTADIX_API_RUNTIME_ASSET_PATHS:
            contents.setdefault(path, b"fixture runtime asset\n")
        for path in release.HOSTED_TLS_TEST_IDENTITY_PATHS:
            contents.setdefault(path, b"fixture hosted TLS test identity\n")
        if files:
            contents.update(files)
        for index in range(FIXTURE_EVIDENCE_GATE_COUNT):
            contents[f"ocore/kernel/fixture-evidence-{index:02}.sh"] = (
                "#!/bin/sh\n"
                f"printf 'FIXTURE {index:02} START\\nFIXTURE {index:02} PASS\\n'\n"
            )
        for path, data in contents.items():
            self._write(
                path,
                data,
                executable=path
                in {
                    "boot-and-test.sh",
                    "setup.sh",
                    "okernel-multikernel/boot-and-test.sh",
                    "ocore/kernel/build.sh",
                    "ocore/kernel/build-aarch64-g2.sh",
                    "ocore/kernel/build-x86_64-uefi-media.sh",
                    "ocore/kernel/run-x86_64-uefi-media-qemu.sh",
                    "ocore/kernel/smoke-x86_64-boot-info-qemu.sh",
                    "ocore/kernel/smoke-x86_64-smp-qemu.sh",
                    "ocore/kernel/smoke-x86_64-uefi-media-qemu.sh",
                    "ocore/kernel/smoke-world-project-receipt-qemu.sh",
                    "ocore/kernel/smoke-world-project-runtime-qemu.sh",
                    "ocore/kernel/smoke-world-receipt-qemu.sh",
                    "ocore/kernel/smoke-world-value-qemu.sh",
                    "ocore/kernel/smoke-world-protocol-qemu.sh",
                    "ocore/kernel/smoke-world-identity-qemu.sh",
                    "ocore/kernel/smoke-aarch64-g2-qemu.sh",
                    "ocore/kernel/stress-live-linux-personality-qemu.sh",
                    "scripts/o-cli.sh",
                    "scripts/o-kernel.sh",
                    "scripts/ostadix_boot_media.py",
                    "scripts/ostadix_boot_info_qemu.py",
                    "scripts/ostadix_media_writer.py",
                    "scripts/ostadix_physical_evidence.py",
                    "scripts/benchmark_hgraph_hosted.sh",
                    "scripts/install-o-cli-wrapper.sh",
                    "scripts/smoke-zero-config-lan-netns.sh",
                    "scripts/smoke-project-hgraph-exec.sh",
                    "scripts/smoke-project-hgraph.sh",
                    "scripts/smoke-world-resource-keys.sh",
                    "scripts/smoke-world-g0-conformance.sh",
                    "tests/fixtures/project_hgraph_tools/sh",
                }
                or path.startswith("ocore/kernel/fixture-evidence-"),
            )
        self._git("add", "-f", "--all")
        self._git("commit", "-q", "-m", "fixture")
        return self._git("rev-parse", "HEAD")

    def _build(self, name: str, **options: object):
        return release.build_release(
            self.repo,
            str(options.pop("ref", "HEAD")),
            self.root / name,
            **options,
        )

    def _rewrite_self_consistent(self, source: Path, name: str, transform):
        manifest = release.verify_archive(source)
        prefix = manifest["prefix"]
        entries: list[release.SourceEntry] = []
        with zipfile.ZipFile(source) as archive:
            for item in manifest["files"]:
                entry = release.SourceEntry(
                    path=item["path"],
                    mode=item["mode"],
                    data=archive.read(f"{prefix}/{item['path']}"),
                )
                entries.append(transform(entry))
        destination = self.root / name
        manifest_bytes = release._manifest_bytes(manifest["commit"], prefix, entries)
        release._write_archive(
            destination,
            prefix,
            entries,
            manifest_bytes,
            release._checksums_bytes(entries, manifest_bytes),
        )
        return destination

    def test_allowlist_includes_sources_and_excludes_release_debris(self) -> None:
        commit = self._commit(
            {
                ".github/workflows/ci.yml": "name: fixture\n",
                ".DS_Store": b"finder",
                ".ocore-repair-backups/run/typeck.rs": "backup\n",
                "assets/logo.bin": b"intentional asset",
                "backends/__pycache__/shim.pyc": b"bytecode",
                "backends/shim.py": "print('source')\n",
                "benchmarks/scratch.txt": "not an exact benchmark release path\n",
                "c_cpp/O": b"native executable",
                "c_cpp/src/eval.c": "int eval(void) { return 0; }\n",
                "c_cpp/src/eval.o": b"object file",
                "codebase_tape.md": "development transcript\n",
                "cvelist-our": "generated list\n",
                "docs/design.md": "design\n",
                "examples/demo.O": "text^(demo)_text\n",
                "examples/manifest.json": json.dumps(
                    {
                        "schema_version": 1,
                        "examples": [
                            {
                                "path": "demo.O",
                                "editions": ["rust"],
                                "classification": "unit",
                                "requirements": {
                                    "backends": ["text"],
                                    "programs": [],
                                    "authorities": [],
                                },
                                "expected": {"rust": {"patterns": ["demo"]}},
                            },
                            {
                                "path": "semantic_custody.O",
                                "editions": ["rust"],
                                "classification": "integration",
                                "requirements": {
                                    "backends": ["python", "text"],
                                    "programs": ["python3"],
                                    "authorities": ["process"],
                                },
                                "expected": {
                                    "rust": {
                                        "patterns": ["semantic-custody answer=42"]
                                    }
                                },
                            }
                        ],
                    },
                    sort_keys=True,
                )
                + "\n",
                "examples/generated.html": "<p>generated</p>\n",
                "fuzz/fuzz_targets/parser.rs": "fn main() {}\n",
                "ocore/kernel/main.oc": "module kernel::main;\n",
                "o-node-quickstart.sh": "#!/bin/sh\nexit 0\n",
                "one-off.patch": "diff --git a/a b/a\n",
                "okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md": "published proposal\n",
                "scratch.txt": "not an allowlisted top-level surface\n",
                "scripts/tool.sh": "#!/bin/sh\nexit 0\n",
                "src/lib.rs": FIXTURE_ROOT_LIB_SOURCE + "// fixture marker\n",
                "tests/fixture.rs": "#[test] fn fixture() {}\n",
            }
        )
        result = self._build("source.zip")
        self.assertEqual(result.commit, commit)

        manifest = release.verify_archive(result.output)
        prefix = manifest["prefix"]
        with zipfile.ZipFile(result.output) as archive:
            names = set(archive.namelist())
            included = {
                ".dockerignore",
                ".github/CODEOWNERS",
                ".github/dependabot.yml",
                ".github/ISSUE_TEMPLATE/bug_report.yml",
                ".github/ISSUE_TEMPLATE/config.yml",
                ".github/ISSUE_TEMPLATE/feature_request.yml",
                ".github/pull_request_template.md",
                ".github/workflows/fuzz.yml",
                ".gitignore",
                ".mcp.json",
                "CITATION.cff",
                "CHANGELOG.md",
                "CODE_OF_CONDUCT.md",
                "CONTRIBUTING.md",
                "Cargo.lock",
                "Cargo.toml",
                "crates/ostadix-api/Cargo.toml",
                "crates/ostadix-api/src/lib.rs",
                "crates/ostadix-api/tests/public_surface.rs",
                "Dockerfile",
                "LICENSE",
                "NOTICE",
                "o-node-quickstart.sh",
                "README.md",
                "SECURITY.md",
                "boot-and-test.sh",
                "ci/architecture-roots.toml",
                "ci/required-jobs.toml",
                "ci/test-suites.toml",
                "rust-toolchain.toml",
                "setup.sh",
                "docs/CLAIMS.md",
                "docs/CI_POSTURE.md",
                "docs/IMAGE_ADMISSION.md",
                "docs/INFORMATION_KERNEL_V1.md",
                "docs/releases/v0.3.0.md",
                "docs/HOSTED_LIVE_REFERENCE.md",
                "docs/HOSTED_PLACEMENT_V6.md",
                "docs/PROJECT_MESH_V1.md",
                "docs/UNIFIED_INTENT_FRONT_DOOR_V1.md",
                "docs/OIR_EXECUTION_FABRIC_V1.md",
                "docs/HOSTED_WORLD_REFERENCE_PROFILE.md",
                "docs/KERNEL_WORLD_CONTRACT.md",
                "docs/O_MACHINE_CONTRACT.md",
                "docs/OSTADIX_BOOT.md",
                "docs/OSTADIX_WORLD.md",
                "docs/SEMANTIC_CUSTODY.md",
                "docs/VERSIONING.md",
                "evidence/gates.toml",
                "evidence/o_machine_contract_v1.toml",
                "evidence/world_alpha_gates.toml",
                "evidence/world_contract_v1.toml",
                "evidence/world_contract_v2.toml",
                "evidence/world/g0-derivation-rederive-2026-08-03.toml",
                "evidence/world/g0-machine-contract-supersession-2026-08-03.toml",
                "evidence/world/g0-ostadix-alpha-branding-2026-08-09.toml",
                "evidence/world/g0-ostadix-alpha-branding-supersession-2026-08-09.toml",
                "evidence/world/g0-independent-engine-2026-08-17.toml",
                "evidence/world/g0-independent-engine-supersession-2026-08-17.toml",
                "evidence/world/g0-repository-conformance.toml",
                "evidence/world/g0-repository-conformance-2026-08-03.toml",
                "evidence/world/g0-repository-conformance-2026-08-03-v2.toml",
                "evidence/world/g0-schema-v3-supersession-2026-08-03.toml",
                "evidence/world/g2-aarch64-qemu.toml",
                "evidence/world/g2-aarch64-qemu-2026-08-03.toml",
                "evidence/world/g2-counter-wording-supersession-2026-08-03.toml",
                "evidence/world/g2-derivation-rederive-2026-08-03.toml",
                "evidence/world/transcripts/g0-repository-conformance.log",
                "evidence/world/transcripts/g0-repository-conformance-2026-08-03.log",
                "evidence/world/transcripts/g0-repository-conformance-2026-08-03-v2.log",
                "evidence/world/transcripts/g0-ostadix-alpha-branding-2026-08-09.log",
                "evidence/world/transcripts/g0-independent-engine-2026-08-17.log",
                "evidence/world/transcripts/g2-aarch64-qemu.log",
                "evidence/world/transcripts/g2-aarch64-qemu-2026-08-03.log",
                "examples/manifest.json",
                "examples/docker_literal/main.py",
                "examples/semantic_custody.O",
                "llms.txt",
                "mcp/ostadix_lang_mcp_server/Cargo.lock",
                "mcp/ostadix_lang_mcp_server/Cargo.toml",
                "mcp/ostadix_lang_mcp_server/README.md",
                "mcp/ostadix_lang_mcp_server/src/main.rs",
                "okernel-multikernel/boot-and-test.sh",
                "okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md",
                "ocore/kernel/boot.S",
                "ocore/kernel/aarch64/boot.S",
                "ocore/kernel/aarch64/linker.ld",
                "ocore/kernel/aarch64/vectors.S",
                "ocore/kernel/build-aarch64-g2.sh",
                "ocore/kernel/build-x86_64-uefi-media.sh",
                "ocore/kernel/build.sh",
                "ocore/kernel/main.oc",
                "ocore/kernel/m6_mode25_diagnostics.oc",
                "ocore/kernel/m6_mode25_diagnostics_stub.oc",
                "ocore/kernel/resolve-x86_64-ovmf-code.sh",
                "ocore/kernel/smp_probe.oc",
                "ocore/kernel/smp_probe_stub.oc",
                "ocore/kernel/run-x86_64-uefi-media-qemu.sh",
                "ocore/kernel/smoke-x86_64-boot-info-qemu.sh",
                "ocore/kernel/smoke-x86_64-smp-qemu.sh",
                "ocore/kernel/smoke-x86_64-uefi-media-qemu.sh",
                "ocore/kernel/smoke-world-project-receipt-qemu.sh",
                "ocore/kernel/smoke-world-project-runtime-qemu.sh",
                "ocore/kernel/smoke-world-receipt-qemu.sh",
                "ocore/kernel/smoke-world-value-qemu.sh",
                "ocore/kernel/smoke-world-protocol-qemu.sh",
                "ocore/kernel/smoke-world-identity-qemu.sh",
                "ocore/kernel/smoke-aarch64-g2-qemu.sh",
                "ocore/kernel/stress-live-linux-personality-qemu.sh",
                "ocore/kernel/world_protocol_semantics.oc",
                "ocore/kernel/world_protocol_semantics_stub.oc",
                "ocore/kernel/world_value_semantics.oc",
                "ocore/kernel/world_value_semantics_stub.oc",
                "ocore/kernel/world_identity_semantics.oc",
                "ocore/kernel/world_identity_semantics_stub.oc",
                "ocore/kernel/world_project_receipt_semantics.oc",
                "ocore/kernel/world_project_receipt_semantics_stub.oc",
                "ocore/kernel/world_receipt_semantics.oc",
                "ocore/kernel/world_receipt_semantics_stub.oc",
                "ocore/kernel/x86_64/grub.cfg",
                "ocore/kernel/x86_64/boot_info.oc",
                "ocore/kernel/x86_64/boot_info_stub.oc",
                "ocore/runtime/x86_64/trap.oc",
                "ocore/runtime/aarch64/g2_kernel.oc",
                "ocore/runtime/aarch64/g2_user_a.oc",
                "ocore/runtime/aarch64/g2_user_b.oc",
                "ocore/world/codec.oc",
                "ocore/world/identity.oc",
                "ocore/world/protocol.oc",
                "ocore/world/receipt.oc",
                "ocore/world/receipt_codec.oc",
                "ocore/world/sha256.oc",
                "ocore/world/value.oc",
                "ocore/world/value_codec.oc",
                "scripts/smoke_ostadix_mcp.py",
                "scripts/smoke-docker.sh",
                "scripts/smoke-zero-config-lan-netns.sh",
                "scripts/semantic_custody_demo.sh",
                "scripts/contract_surfaces.py",
                "scripts/check_attribution.py",
                "scripts/check_architecture_boundaries.py",
                "scripts/local_ci_posture.py",
                "scripts/install-o-cli-wrapper.sh",
                "scripts/o-cli.sh",
                "scripts/o-kernel.sh",
                "scripts/ostadix_boot_media.py",
                "scripts/ostadix_boot_info_qemu.py",
                "scripts/ostadix_media_writer.py",
                "scripts/ostadix_physical_evidence.py",
                "scripts/smoke-project-hgraph-exec.sh",
                "scripts/smoke-project-hgraph.sh",
                "scripts/smoke-world-resource-keys.sh",
                "scripts/smoke-world-g0-conformance.sh",
                "scripts/release_evidence.py",
                "scripts/world_alpha_evidence.py",
                "crates/ostadix-api/src/backend.rs",
                "crates/ostadix-api/src/backend_morphism.rs",
                "crates/ostadix-api/src/api.rs",
                "crates/ostadix-api/src/backend_catalog.rs",
                "crates/ostadix-api/src/backend_catalog.inc.rs",
                "crates/ostadix-api/src/backend_state.rs",
                "crates/ostadix-api/src/canonical_cbor.rs",
                "crates/ostadix-api/src/dispatch_model.rs",
                "crates/ostadix-api/src/evidence/admit.rs",
                "crates/ostadix-api/src/evidence/analyze.rs",
                "crates/ostadix-api/src/evidence/fact.rs",
                "crates/ostadix-api/src/evidence/intent.rs",
                "crates/ostadix-api/src/evidence/mod.rs",
                "crates/ostadix-api/src/evidence/profile.rs",
                "crates/ostadix-api/src/effects.rs",
                "crates/ostadix-api/src/eval.rs",
                "crates/ostadix-api/src/eval_core.rs",
                "crates/ostadix-api/src/execution_contract.rs",
                "crates/ostadix-api/src/intent/mod.rs",
                "crates/ostadix-api/src/intent/record.rs",
                "crates/ostadix-api/src/intent/store.rs",
                "crates/ostadix-api/src/runtime_exec.rs",
                "crates/ostadix-api/src/syntax_dialect.rs",
                "src/bin/o-cli.rs",
                "src/bin/o-info.rs",
                "src/bin/o-node.rs",
                "src/bin/o-registry.rs",
                "src/bin/octl.rs",
                "src/bin/olink.rs",
                "src/bin/olangc.rs",
                "src/bin/ocorec.rs",
                "crates/ostadix-api/src/ocore/codegen.rs",
                "crates/ostadix-api/src/ocore/codegen_aarch64.rs",
                "crates/ostadix-api/src/ocore/boot_info.rs",
                "crates/ostadix-api/src/ocore/driver.rs",
                "crates/ostadix-api/src/ocore/mod.rs",
                "crates/ostadix-api/src/executor/mod.rs",
                "crates/ostadix-api/src/executor/driver.rs",
                "crates/ostadix-api/src/executor/pool.rs",
                "crates/ostadix-api/src/executor/task.rs",
                "crates/ostadix-api/src/execution_fabric/codec.rs",
                "crates/ostadix-api/src/execution_fabric/protocol.rs",
                "crates/ostadix-api/src/hgraph/graph.rs",
                "crates/ostadix-api/src/hgraph/kinds.rs",
                "crates/ostadix-api/src/hgraph/from_oir.rs",
                "crates/ostadix-api/src/hgraph/solve.rs",
                "crates/ostadix-api/src/hosted_remote/client.rs",
                "crates/ostadix-api/src/hosted_remote/mod.rs",
                "crates/ostadix-api/src/hosted_remote/mesh.rs",
                "crates/ostadix-api/src/hosted_remote/node.rs",
                "crates/ostadix-api/src/hosted_remote/paths.rs",
                "crates/ostadix-api/src/hosted_remote/project_mesh.rs",
                "crates/ostadix-api/src/hosted_remote/protocol.rs",
                "crates/ostadix-api/src/hosted_remote/tls.rs",
                "crates/ostadix-api/src/hosted_remote/v2/auth.rs",
                "crates/ostadix-api/src/hosted_remote/v2/client.rs",
                "crates/ostadix-api/src/hosted_remote/v2/crypto.rs",
                "crates/ostadix-api/src/hosted_remote/v2/dev.rs",
                "crates/ostadix-api/src/hosted_remote/v2/mod.rs",
                "crates/ostadix-api/src/hosted_remote/v2/protocol.rs",
                "crates/ostadix-api/src/hosted_remote/v2/runtime.rs",
                "crates/ostadix-api/src/hosted_remote/v2/server.rs",
                "crates/ostadix-api/src/hosted_remote/v2/store.rs",
                "crates/ostadix-api/src/information/acquisition.rs",
                "crates/ostadix-api/src/information/decision.rs",
                "crates/ostadix-api/src/information/delta.rs",
                "crates/ostadix-api/src/information/exchange.rs",
                "crates/ostadix-api/src/information/id.rs",
                "crates/ostadix-api/src/information/invalidation.rs",
                "crates/ostadix-api/src/information/loss.rs",
                "crates/ostadix-api/src/information/mod.rs",
                "crates/ostadix-api/src/information/model.rs",
                "crates/ostadix-api/src/information/projection.rs",
                "crates/ostadix-api/src/information/provenance.rs",
                "crates/ostadix-api/src/information/root.rs",
                "crates/ostadix-api/src/information/store.rs",
                "crates/ostadix-api/src/information_bridge/mod.rs",
                "crates/ostadix-api/src/information_provenance/mod.rs",
                "crates/ostadix-api/src/ir.rs",
                "src/lib.rs",
                "src/main.rs",
                "crates/ostadix-api/src/placement/mod.rs",
                "crates/ostadix-api/src/placement/projection.rs",
                "crates/ostadix-api/src/placement/protocol/candidate.rs",
                "crates/ostadix-api/src/placement/protocol/catalog.rs",
                "crates/ostadix-api/src/placement/protocol/digest.rs",
                "crates/ostadix-api/src/placement/protocol/error.rs",
                "crates/ostadix-api/src/placement/protocol/mod.rs",
                "crates/ostadix-api/src/placement/protocol/records.rs",
                "crates/ostadix-api/src/placement/protocol/requirement.rs",
                "crates/ostadix-api/src/placement/protocol/state.rs",
                "crates/ostadix-api/src/placement/protocol/target.rs",
                "crates/ostadix-api/src/placement/protocol/warrant.rs",
                "crates/ostadix-api/src/process.rs",
                "crates/ostadix-api/src/version.rs",
                "crates/ostadix-api/src/project/executor.rs",
                "crates/ostadix-api/src/project/deployment.rs",
                "crates/ostadix-api/src/project/launch.rs",
                "crates/ostadix-api/src/project/logical.rs",
                "crates/ostadix-api/src/project/mod.rs",
                "crates/ostadix-api/src/project/model.rs",
                "crates/ostadix-api/src/project/plan.rs",
                "crates/ostadix-api/src/project/runtime.rs",
                "crates/ostadix-api/src/project/runtime_graph.rs",
                "crates/ostadix-api/src/project/trace.rs",
                "crates/ostadix-api/src/project/world_execution.rs",
                "crates/ostadix-api/src/registry/bundle/mod.rs",
                "crates/ostadix-api/src/registry/placement_compat.rs",
                "crates/ostadix-api/src/registry/store.rs",
                "crates/ostadix-api/src/world/grounding.rs",
                "crates/ostadix-api/src/world/identity.rs",
                "crates/ostadix-api/src/world/identity_wire.rs",
                "crates/ostadix-api/src/world/codec.rs",
                "crates/ostadix-api/src/world/mod.rs",
                "crates/ostadix-api/src/world/protocol.rs",
                "crates/ostadix-api/src/world/receipt.rs",
                "crates/ostadix-api/src/world/receipt_codec.rs",
                "crates/ostadix-api/src/world/value.rs",
                "crates/ostadix-api/src/world/value_codec.rs",
                "tests/example_manifest.py",
                "tests/fixtures/world_identity_v1.hex",
                "tests/fixtures/project_hgraph/input.txt",
                "tests/fixtures/project_hgraph/olang.project.toml",
                "tests/fixtures/project_hgraph_exec/input.txt",
                "tests/fixtures/project_hgraph_exec/olang.project.toml",
                "tests/fixtures/project_hgraph_tools/sh",
                "tests/fixtures/world_protocol_v1.hex",
                "tests/fixtures/world_receipt_v1.hex",
                "tests/fixtures/world_value_v1.hex",
                "tests/test_example_manifest.py",
                "tests/test_attribution_policy.py",
                "tests/test_contract_surfaces.py",
                "tests/test_architecture_boundaries.py",
                "tests/test_governance_surfaces.py",
                "tests/test_local_ci_posture.py",
                "tests/test_backend_state_protocol.py",
                "tests/test_mcp_smoke.py",
                "tests/test_ostadix_boot_media.py",
                "tests/test_ostadix_boot_info_qemu.py",
                "tests/test_ostadix_media_writer.py",
                "tests/test_ostadix_physical_evidence.py",
                "tests/test_o_cli_dispatch.py",
                "tests/unified_intent_acceptance.rs",
                "tests/o_cli_intent_blackbox.rs",
                "tests/unified_plan_boundaries.rs",
                "tests/test_release_evidence.py",
                "tests/test_setup.py",
                "tests/test_bundled_shim_protocol.py",
                "tests/backend_morphism_v1.rs",
                "tests/test_world_alpha_evidence.py",
                "tests/hosted_remote_cli.rs",
                "tests/project_mesh_cli.rs",
                "tests/hosted_remote_v2.rs",
                "tests/support/mod.rs",
                "tests/o_info_cli.rs",
                "tests/information_bridge_v1.rs",
                "tests/placement_v6.rs",
                "tests/registry_v1.rs",
                "tests/project_hgraph.rs",
                "tests/project_hgraph_exec.rs",
                "tests/project_deployment_plan.rs",
                "tests/project_logical_hgraph.rs",
                "tests/project_world_runtime.rs",
                "tests/world_resource_keys.rs",
                "tests/world_identity.rs",
                "tests/world_identity_wire.rs",
                "tests/world_protocol.rs",
                "tests/world_receipt.rs",
                "tests/world_value.rs",
                ".github/workflows/ci.yml",
                "assets/logo.bin",
                "backends/shim.py",
                "backends/o_shim_common.py",
                "c_cpp/src/eval.c",
                "docs/design.md",
                "examples/demo.O",
                "fuzz/fuzz_targets/parser.rs",
                "ocore/kernel/main.oc",
                "scripts/tool.sh",
                "src/lib.rs",
                "tests/fixture.rs",
            }
            included.update(
                f"ocore/kernel/fixture-evidence-{index:02}.sh"
                for index in range(FIXTURE_EVIDENCE_GATE_COUNT)
            )
            included.update(release.HOSTED_HGRAPH_BENCHMARK_RELEASE_PATHS)
            included.update(release.OSTADIX_API_RELEASE_PATHS)
            included.update(release.OSTADIX_API_ROOT_MODULE_PATHS.values())
            excluded = {
                ".DS_Store",
                ".ocore-repair-backups/run/typeck.rs",
                "backends/__pycache__/shim.pyc",
                "benchmarks/scratch.txt",
                "c_cpp/O",
                "c_cpp/src/eval.o",
                "codebase_tape.md",
                "cvelist-our",
                "examples/generated.html",
                "one-off.patch",
                "scratch.txt",
            }
            for path in included:
                self.assertIn(f"{prefix}/{path}", names)
            for path in excluded:
                self.assertNotIn(f"{prefix}/{path}", names)

            manifest_bytes = archive.read(f"{prefix}/{release.MANIFEST_NAME}")
            embedded = json.loads(manifest_bytes)
            self.assertEqual(embedded["file_count"], len(included))
            modes = {entry["path"]: entry["mode"] for entry in embedded["files"]}
            self.assertEqual(modes["boot-and-test.sh"], "100755")
            self.assertEqual(modes["setup.sh"], "100755")
            self.assertEqual(
                modes["okernel-multikernel/boot-and-test.sh"], "100755"
            )
            self.assertEqual(
                modes["ocore/kernel/smoke-world-project-receipt-qemu.sh"],
                "100755",
            )
            self.assertEqual(
                modes["ocore/kernel/smoke-world-project-runtime-qemu.sh"],
                "100755",
            )
            for executable_path in (
                "ocore/kernel/build-x86_64-uefi-media.sh",
                "ocore/kernel/run-x86_64-uefi-media-qemu.sh",
                "ocore/kernel/smoke-x86_64-boot-info-qemu.sh",
                "ocore/kernel/smoke-x86_64-smp-qemu.sh",
                "ocore/kernel/smoke-x86_64-uefi-media-qemu.sh",
                "ocore/kernel/stress-live-linux-personality-qemu.sh",
                "scripts/ostadix_boot_media.py",
                "scripts/ostadix_boot_info_qemu.py",
                "scripts/ostadix_media_writer.py",
                "scripts/ostadix_physical_evidence.py",
            ):
                self.assertEqual(modes[executable_path], "100755")
            self.assertEqual(modes["ocore/kernel/x86_64/grub.cfg"], "100644")
            self.assertEqual(modes["ocore/kernel/x86_64/boot_info.oc"], "100644")
            self.assertEqual(modes["ocore/kernel/x86_64/boot_info_stub.oc"], "100644")
            self.assertEqual(modes["crates/ostadix-api/src/ocore/boot_info.rs"], "100644")
            self.assertEqual(modes["tests/fixtures/project_hgraph_tools/sh"], "100755")
            self.assertEqual(
                modes["scripts/benchmark_hgraph_hosted.sh"], "100755"
            )
            checksums = archive.read(f"{prefix}/{release.CHECKSUMS_NAME}").decode()
            cargo_digest = hashlib.sha256(
                archive.read(f"{prefix}/Cargo.toml")
            ).hexdigest()
            self.assertIn(f"{cargo_digest}  Cargo.toml\n", checksums)
            self.assertIn(
                f"{hashlib.sha256(manifest_bytes).hexdigest()}  {release.MANIFEST_NAME}\n",
                checksums,
            )

    def test_relative_document_links_must_resolve_inside_release(self) -> None:
        self._commit(
            {
                "README.md": fixture_readme("[missing guide](docs/missing.md)\n"),
            }
        )

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing relative link target.*README\.md -> docs/missing\.md",
        ):
            self._build("broken-links.zip")

    def test_document_link_check_accepts_fragments_directories_and_external_urls(self) -> None:
        self._commit(
            {
                "README.md": (
                    fixture_readme()
                    + "[guide](docs/guide.md#usage) [docs](docs/) "
                    "[web](https://example.invalid/) [root](./)\n"
                    "[balanced](docs/target(foo).md) "
                    "[escaped](docs/escaped\\(name\\).md) "
                    "[space ref][space]\n"
                    "[proposal]: okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md\n"
                    "[paren]: docs/target(foo).md\n"
                    "[space]: <docs/guide with spaces.md> \"title\"\n"
                    "`[inline code](missing-inline.md)`\n"
                    "``[long code span](missing-long-inline.md)``\n"
                    "    [indented code](missing-indented.md)\n"
                    "<!-- [comment](missing-comment.md)\n"
                    "[multiline comment](missing-comment-two.md) -->\n"
                    "\\[escaped syntax](missing-escaped-syntax.md)\n"
                    "```markdown\n[excluded example](not-in-release.md)\n```\n"
                ),
                "docs/guide.md": "# Usage\n\n[home](../README.md) [root](../)\n",
                "docs/target(foo).md": "# Balanced destination\n",
                "docs/escaped(name).md": "# Escaped destination\n",
                "docs/guide with spaces.md": "# Angle destination\n",
            }
        )

        result = self._build("valid-links.zip")
        manifest = release.verify_archive(result.output)
        self.assertGreater(manifest["file_count"], 0)

    def test_reference_document_links_must_resolve_inside_release(self) -> None:
        self._commit(
            {
                "README.md": (
                    fixture_readme()
                    + "[missing guide][guide]\n"
                    "[guide]: docs/missing(reference).md\n"
                )
            }
        )

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing relative link target.*docs/missing\(reference\)\.md",
        ):
            self._build("broken-reference-links.zip")

    def test_archive_verifier_rejects_a_self_consistent_broken_document_link(self) -> None:
        result = self._build("valid-before-link-tamper.zip", ref=self._commit())
        manifest = release.verify_archive(result.output)
        prefix = manifest["prefix"]
        entries: list[release.SourceEntry] = []
        with zipfile.ZipFile(result.output) as archive:
            for item in manifest["files"]:
                path = item["path"]
                data = archive.read(f"{prefix}/{path}")
                if path == "README.md":
                    data = fixture_readme(
                        "[missing](docs/absent-after-packaging.md)\n"
                    ).encode()
                entries.append(
                    release.SourceEntry(path=path, mode=item["mode"], data=data)
                )

        broken = self.root / "self-consistent-broken-link.zip"
        manifest_bytes = release._manifest_bytes(manifest["commit"], prefix, entries)
        release._write_archive(
            broken,
            prefix,
            entries,
            manifest_bytes,
            release._checksums_bytes(entries, manifest_bytes),
        )
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing relative link target.*absent-after-packaging\.md",
        ):
            release.verify_archive(broken)

    def test_allowlisted_git_symlink_is_rejected_before_packaging(self) -> None:
        self._commit()
        link = self.repo / "docs/escape-link"
        link.parent.mkdir(parents=True, exist_ok=True)
        link.symlink_to("../../outside")
        self._git("add", "docs/escape-link")
        self._git("commit", "-q", "-m", "add escaping symlink")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"unsupported Git mode 120000 for docs/escape-link",
        ):
            self._build("symlink.zip")

    def test_unconfigured_gitlink_is_rejected_even_outside_allowlist(self) -> None:
        parent = self._commit()
        self._git(
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            parent,
            "Avesta",
        )
        self._git("commit", "-q", "-m", "add malformed gitlink")
        (self.repo / "Avesta").mkdir()

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"self-contained source releases forbid Git gitlinks: Avesta",
        ):
            self._build("gitlink.zip")

    def test_archive_manifest_cannot_reintroduce_symlink_mode(self) -> None:
        result = self._build("valid-before-mode-tamper.zip", ref=self._commit())

        def symlink_mode(entry):
            if entry.path == "README.md":
                return release.SourceEntry(entry.path, "120000", entry.data)
            return entry

        tampered = self._rewrite_self_consistent(
            result.output, "self-consistent-symlink-mode.zip", symlink_mode
        )
        with self.assertRaisesRegex(
            release.ReleaseError, r"manifest contains an invalid mode for README\.md"
        ):
            release.verify_archive(tampered)

    def test_verifier_rejects_noncanonical_internal_zip_attributes(self) -> None:
        result = self._build("valid-before-metadata-tamper.zip", ref=self._commit())
        tampered = self.root / "noncanonical-internal-attr.zip"
        with zipfile.ZipFile(result.output, "r") as source, zipfile.ZipFile(
            tampered,
            "w",
            compression=zipfile.ZIP_DEFLATED,
            compresslevel=9,
        ) as destination:
            for original in source.infolist():
                mode = f"{(original.external_attr >> 16) & 0xFFFF:06o}"
                info = release._zip_info(original.filename, mode)
                if original.filename.endswith("/README.md"):
                    info.internal_attr = 1
                destination.writestr(
                    info,
                    source.read(original),
                    compress_type=zipfile.ZIP_DEFLATED,
                    compresslevel=9,
                )

        with self.assertRaisesRegex(
            release.ReleaseError, r"non-canonical ZIP internal_attr.*README\.md"
        ):
            release.verify_archive(tampered)

    def test_archive_verifier_revalidates_mcp_config_structure(self) -> None:
        result = self._build("valid-before-config-tamper.zip", ref=self._commit())

        def remove_server(entry):
            if entry.path == ".mcp.json":
                return release.SourceEntry(
                    entry.path, entry.mode, b'{"mcpServers":{}}\n'
                )
            return entry

        tampered = self._rewrite_self_consistent(
            result.output, "self-consistent-invalid-mcp.zip", remove_server
        )
        with self.assertRaisesRegex(
            release.ReleaseError, r"mcpServers must contain exactly ostadix"
        ):
            release.verify_archive(tampered)

    def test_aggregate_smoke_wrappers_are_required_release_paths(self) -> None:
        self._commit()
        self._git("rm", "boot-and-test.sh")
        self._git("commit", "-q", "-m", "remove aggregate launcher")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): boot-and-test\.sh",
        ):
            self._build("missing-launcher.zip")

    def test_hosted_hgraph_benchmark_is_a_required_release_closure(self) -> None:
        self._commit()
        self._git(
            "rm",
            "benchmarks/hgraph_hosted/RESULTS-2026-08-08-be68dfef.md",
            "benchmarks/hgraph_hosted/heterogeneous.O",
            "scripts/benchmark_hgraph_hosted.sh",
            "tests/test_benchmark_hgraph_hosted.py",
        )
        self._git("commit", "-q", "-m", "remove hosted benchmark closure")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): .*RESULTS-2026-08-08-be68dfef\.md.*"
            r"heterogeneous\.O.*"
            r"benchmark_hgraph_hosted\.sh.*test_benchmark_hgraph_hosted\.py",
        ):
            self._build("missing-hosted-benchmark.zip")

    def test_analyzer_bound_hosted_benchmark_evidence_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "benchmarks/hgraph_hosted/RESULTS-2026-08-08-f216771.md",
            "benchmarks/hgraph_hosted/TRANSCRIPT-2026-08-08-f216771.log",
        )
        self._git("commit", "-q", "-m", "remove analyzer-bound benchmark evidence")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): .*RESULTS-2026-08-08-f216771\.md.*"
            r"TRANSCRIPT-2026-08-08-f216771\.log",
        ):
            self._build("missing-analyzer-bound-benchmark-evidence.zip")

    def test_root_license_is_a_required_release_member(self) -> None:
        self._commit()
        self._git("rm", "LICENSE")
        self._git("commit", "-q", "-m", "remove release license")

        with self.assertRaisesRegex(
            release.ReleaseError, r"missing required path\(s\): LICENSE"
        ):
            self._build("missing-license.zip")

    def test_engine_legal_metadata_is_required_release_closure(self) -> None:
        self._commit()
        required = ("crates/ostadix-api/LICENSE", "crates/ostadix-api/NOTICE")
        self._git("rm", *required)
        self._git("commit", "-q", "-m", "remove engine legal metadata")

        self._assert_missing_required_paths(
            "missing-engine-legal-metadata.zip", required
        )

    def test_engine_package_readme_is_required_release_member(self) -> None:
        self._commit()
        path = "crates/ostadix-api/README.md"
        self._git("rm", path)
        self._git("commit", "-q", "-m", "remove engine package README")

        self._assert_missing_required_paths("missing-engine-readme.zip", (path,))

    def test_root_citation_is_a_required_release_member(self) -> None:
        self._commit()
        self._git("rm", "CITATION.cff")
        self._git("commit", "-q", "-m", "remove release citation")

        with self.assertRaisesRegex(
            release.ReleaseError, r"missing required path\(s\): CITATION\.cff"
        ):
            self._build("missing-citation.zip")

    def test_archive_verifier_revalidates_root_release_metadata(self) -> None:
        result = self._build("valid-before-citation-tamper.zip", ref=self._commit())

        def replace_citation_license(entry):
            if entry.path == "CITATION.cff":
                return release.SourceEntry(
                    entry.path,
                    entry.mode,
                    entry.data.replace(
                        f"license: {release.ROOT_LICENSE_SPDX}".encode(),
                        b"license: Apache-2.0",
                    ),
                )
            return entry

        tampered = self._rewrite_self_consistent(
            result.output,
            "self-consistent-invalid-root-license.zip",
            replace_citation_license,
        )
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"CITATION\.cff license must match Cargo\.toml package\.license",
        ):
            release.verify_archive(tampered)

    def test_mcp_crate_config_and_transport_smoke_are_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            ".mcp.json",
            "mcp/ostadix_lang_mcp_server/Cargo.toml",
            "scripts/smoke_ostadix_mcp.py",
            "tests/test_mcp_smoke.py",
        )
        self._git("commit", "-q", "-m", "remove MCP release surface")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): \.mcp\.json.*Cargo\.toml.*"
            r"smoke_ostadix_mcp\.py.*test_mcp_smoke\.py",
        ):
            self._build("missing-mcp.zip")

    def test_toolchain_and_ci_contract_projection_are_required(self) -> None:
        required = (
            ".github/dependabot.yml",
            ".github/workflows/ci.yml",
            ".github/workflows/fuzz.yml",
            "Cargo.lock",
            "ci/architecture-roots.toml",
            "ci/required-jobs.toml",
            "ci/test-suites.toml",
            "rust-toolchain.toml",
            "docs/CI_POSTURE.md",
            "scripts/check_attribution.py",
            "scripts/contract_surfaces.py",
            "scripts/check_architecture_boundaries.py",
            "scripts/local_ci_posture.py",
            "tests/test_architecture_boundaries.py",
            "tests/test_attribution_policy.py",
            "tests/test_contract_surfaces.py",
            "tests/test_local_ci_posture.py",
        )
        self._commit()
        self._git("rm", *required)
        self._git("commit", "-q", "-m", "remove toolchain and CI contracts")

        with self.assertRaises(release.ReleaseError) as raised:
            self._build("missing-ci-contracts.zip")
        message = str(raised.exception)
        self.assertIn("missing required path(s)", message)
        for required_path in required:
            self.assertIn(required_path, message)

    def test_information_kernel_and_local_cli_surface_are_required(self) -> None:
        required = (
            "docs/IMAGE_ADMISSION.md",
            "docs/INFORMATION_KERNEL_V1.md",
            "crates/ostadix-api/src/canonical_cbor.rs",
            "src/bin/o-info.rs",
            "crates/ostadix-api/src/information/acquisition.rs",
            "crates/ostadix-api/src/information/decision.rs",
            "crates/ostadix-api/src/information/delta.rs",
            "crates/ostadix-api/src/information/exchange.rs",
            "crates/ostadix-api/src/information/id.rs",
            "crates/ostadix-api/src/information/invalidation.rs",
            "crates/ostadix-api/src/information/loss.rs",
            "crates/ostadix-api/src/information/mod.rs",
            "crates/ostadix-api/src/information/model.rs",
            "crates/ostadix-api/src/information/projection.rs",
            "crates/ostadix-api/src/information/provenance.rs",
            "crates/ostadix-api/src/information/root.rs",
            "crates/ostadix-api/src/information/store.rs",
            "crates/ostadix-api/src/information_bridge/mod.rs",
            "crates/ostadix-api/src/information_provenance/mod.rs",
            "tests/o_info_cli.rs",
            "tests/information_bridge_v1.rs",
        )
        self._commit()
        self._git("rm", *required)
        self._git("commit", "-q", "-m", "remove information-kernel surface")

        with self.assertRaises(release.ReleaseError) as raised:
            self._build("missing-information-kernel.zip")
        message = str(raised.exception)
        self.assertIn("missing required path(s)", message)
        for required_path in required:
            self.assertIn(required_path, message)

    def test_governance_surfaces_are_required(self) -> None:
        required = (
            ".github/CODEOWNERS",
            ".github/ISSUE_TEMPLATE/bug_report.yml",
            ".github/ISSUE_TEMPLATE/config.yml",
            ".github/ISSUE_TEMPLATE/feature_request.yml",
            ".github/pull_request_template.md",
            "CHANGELOG.md",
            "CODE_OF_CONDUCT.md",
            "CONTRIBUTING.md",
            "docs/releases/v0.3.0.md",
            "SECURITY.md",
            "tests/test_governance_surfaces.py",
        )
        self._commit()
        self._git("rm", *required)
        self._git("commit", "-q", "-m", "remove governance surface")

        with self.assertRaises(release.ReleaseError) as raised:
            self._build("missing-governance.zip")
        message = str(raised.exception)
        self.assertIn("missing required path(s)", message)
        for required_path in required:
            self.assertIn(required_path, message)

    def test_hosted_placement_v6_commands_and_contract_are_required(self) -> None:
        required = (
            "backends/o_shim_common.py",
            "crates/ostadix-api/src/backend.rs",
            "crates/ostadix-api/src/backend_morphism.rs",
            "crates/ostadix-api/src/backend_state.rs",
            "crates/ostadix-api/src/backend_catalog.rs",
            "crates/ostadix-api/src/eval.rs",
            "crates/ostadix-api/src/eval_core.rs",
            "crates/ostadix-api/src/execution_contract.rs",
            "docs/HOSTED_PLACEMENT_V6.md",
            "docs/PROJECT_MESH_V1.md",
            "setup.sh",
            "src/bin/o-node.rs",
            "src/bin/o-registry.rs",
            "src/bin/octl.rs",
            "crates/ostadix-api/src/hosted_remote/client.rs",
            "crates/ostadix-api/src/hosted_remote/mod.rs",
            "crates/ostadix-api/src/hosted_remote/mesh.rs",
            "crates/ostadix-api/src/hosted_remote/node.rs",
            "crates/ostadix-api/src/hosted_remote/paths.rs",
            "crates/ostadix-api/src/hosted_remote/project_mesh.rs",
            "crates/ostadix-api/src/hosted_remote/protocol.rs",
            "crates/ostadix-api/src/hosted_remote/tls.rs",
            "crates/ostadix-api/src/hosted_remote/v2/auth.rs",
            "crates/ostadix-api/src/hosted_remote/v2/client.rs",
            "crates/ostadix-api/src/hosted_remote/v2/crypto.rs",
            "crates/ostadix-api/src/hosted_remote/v2/dev.rs",
            "crates/ostadix-api/src/hosted_remote/v2/mod.rs",
            "crates/ostadix-api/src/hosted_remote/v2/protocol.rs",
            "crates/ostadix-api/src/hosted_remote/v2/runtime.rs",
            "crates/ostadix-api/src/hosted_remote/v2/server.rs",
            "crates/ostadix-api/src/hosted_remote/v2/store.rs",
            "crates/ostadix-api/src/hgraph/solve.rs",
            "crates/ostadix-api/src/ir.rs",
            "crates/ostadix-api/src/registry/placement_compat.rs",
            "crates/ostadix-api/src/placement/mod.rs",
            "crates/ostadix-api/src/placement/projection.rs",
            "crates/ostadix-api/src/placement/protocol/candidate.rs",
            "crates/ostadix-api/src/placement/protocol/catalog.rs",
            "crates/ostadix-api/src/placement/protocol/digest.rs",
            "crates/ostadix-api/src/placement/protocol/error.rs",
            "crates/ostadix-api/src/placement/protocol/mod.rs",
            "crates/ostadix-api/src/placement/protocol/records.rs",
            "crates/ostadix-api/src/placement/protocol/requirement.rs",
            "crates/ostadix-api/src/placement/protocol/state.rs",
            "crates/ostadix-api/src/placement/protocol/target.rs",
            "crates/ostadix-api/src/placement/protocol/warrant.rs",
            "crates/ostadix-api/src/process.rs",
            "crates/ostadix-api/src/registry/bundle/mod.rs",
            "crates/ostadix-api/src/registry/store.rs",
            "tests/backend_morphism_v1.rs",
            "tests/hosted_remote_cli.rs",
            "tests/project_mesh_cli.rs",
            "tests/hosted_remote_v2.rs",
            "tests/support/mod.rs",
            "tests/placement_v6.rs",
            "tests/registry_v1.rs",
            "tests/test_bundled_shim_protocol.py",
            "tests/test_backend_state_protocol.py",
            "tests/test_o_cli_dispatch.py",
        )
        self._commit()
        self._git("rm", *required)
        self._git("commit", "-q", "-m", "remove hosted placement V6 surface")

        with self.assertRaises(release.ReleaseError) as raised:
            self._build("missing-hosted-placement-v6.zip")
        message = str(raised.exception)
        self.assertIn("missing required path(s)", message)
        for required_path in required:
            self.assertIn(required_path, message)

    def test_mcp_config_and_crate_license_are_structurally_validated(self) -> None:
        self._commit({".mcp.json": '{"mcpServers":{}}\n'})
        with self.assertRaisesRegex(
            release.ReleaseError, r"mcpServers must contain exactly ostadix"
        ):
            self._build("invalid-mcp-config.zip")

        self._write(
            ".mcp.json",
            '{"mcpServers":{"ostadix":{"command":"ostadix-mcp","args":[]}}}\n',
        )
        cargo = (self.repo / "mcp/ostadix_lang_mcp_server/Cargo.toml").read_text(
            encoding="utf-8"
        )
        self._write(
            "mcp/ostadix_lang_mcp_server/Cargo.toml",
            cargo.replace("LGPL-2.1-only", "MIT"),
        )
        self._git("add", ".mcp.json", "mcp/ostadix_lang_mcp_server/Cargo.toml")
        self._git("commit", "-q", "-m", "replace invalid MCP fixture")
        with self.assertRaisesRegex(
            release.ReleaseError, r"Cargo\.toml license must be 'LGPL-2\.1-only'"
        ):
            self._build("invalid-mcp-license.zip")

    def test_mcp_lock_root_version_must_match_its_manifest(self) -> None:
        self._commit()
        lock_path = "mcp/ostadix_lang_mcp_server/Cargo.lock"
        lock = (self.repo / lock_path).read_text(encoding="utf-8")
        self._write(
            lock_path,
            lock.replace(
                'name = "ostadix-mcp-server"\nversion = "0.1.0"',
                'name = "ostadix-mcp-server"\nversion = "9.9.9"',
                1,
            ),
        )
        self._git("add", lock_path)
        self._git("commit", "-q", "-m", "drift isolated MCP lock version")
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"Cargo\.lock ostadix-mcp-server version must match .*Cargo\.toml",
        ):
            self._build("invalid-mcp-lock-version.zip")

    def test_evidence_manifest_and_projector_are_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "evidence/gates.toml",
            "scripts/release_evidence.py",
            "tests/test_example_manifest.py",
        )
        self._git("commit", "-q", "-m", "remove release evidence source")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): evidence/gates\.toml.*"
            r"release_evidence\.py.*test_example_manifest\.py",
        ):
            self._build("missing-evidence.zip")

    def test_boot_media_and_bootinfo_surface_is_required(self) -> None:
        required = (
            "docs/OSTADIX_BOOT.md",
            "ocore/kernel/build-x86_64-uefi-media.sh",
            "ocore/kernel/resolve-x86_64-ovmf-code.sh",
            "ocore/kernel/run-x86_64-uefi-media-qemu.sh",
            "ocore/kernel/smoke-x86_64-boot-info-qemu.sh",
            "ocore/kernel/smoke-x86_64-smp-qemu.sh",
            "ocore/kernel/smoke-x86_64-uefi-media-qemu.sh",
            "ocore/kernel/smp_probe.oc",
            "ocore/kernel/smp_probe_stub.oc",
            "ocore/kernel/x86_64/grub.cfg",
            "ocore/kernel/x86_64/boot_info.oc",
            "ocore/kernel/x86_64/boot_info_stub.oc",
            "scripts/ostadix_boot_media.py",
            "scripts/ostadix_boot_info_qemu.py",
            "scripts/ostadix_media_writer.py",
            "scripts/ostadix_physical_evidence.py",
            "crates/ostadix-api/src/ocore/boot_info.rs",
            "tests/test_ostadix_boot_media.py",
            "tests/test_ostadix_boot_info_qemu.py",
            "tests/test_ostadix_media_writer.py",
            "tests/test_ostadix_physical_evidence.py",
            "tests/test_setup.py",
        )
        self._commit()
        self._git("rm", *required)
        self._git("commit", "-q", "-m", "remove boot-media release surface")

        with self.assertRaises(release.ReleaseError) as raised:
            self._build("missing-boot-media.zip")
        message = str(raised.exception)
        self.assertIn("missing required path(s)", message)
        for required_path in required:
            self.assertIn(required_path, message)

    def test_g2_aarch64_compiler_boot_and_gate_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "ocore/kernel/aarch64/boot.S",
            "ocore/kernel/aarch64/linker.ld",
            "ocore/kernel/aarch64/vectors.S",
            "ocore/kernel/build-aarch64-g2.sh",
            "ocore/kernel/smoke-aarch64-g2-qemu.sh",
            "ocore/runtime/aarch64/g2_kernel.oc",
            "ocore/runtime/aarch64/g2_user_a.oc",
            "ocore/runtime/aarch64/g2_user_b.oc",
            "src/bin/ocorec.rs",
            "crates/ostadix-api/src/ocore/codegen_aarch64.rs",
            "crates/ostadix-api/src/ocore/driver.rs",
            "tests/test_release_evidence.py",
            "evidence/world/g2-aarch64-qemu.toml",
            "evidence/world/transcripts/g2-aarch64-qemu.log",
        )
        self._git("commit", "-q", "-m", "remove G2 AArch64 release surface")

        with self.assertRaises(release.ReleaseError) as raised:
            self._build("missing-g2-aarch64.zip")
        message = str(raised.exception)
        self.assertIn("missing required path(s)", message)
        for path in (
            "evidence/world/g2-aarch64-qemu.toml",
            "ocore/kernel/aarch64/boot.S",
            "ocore/kernel/build-aarch64-g2.sh",
            "ocore/kernel/smoke-aarch64-g2-qemu.sh",
            "ocore/runtime/aarch64/g2_kernel.oc",
            "crates/ostadix-api/src/ocore/codegen_aarch64.rs",
            "tests/test_release_evidence.py",
        ):
            self.assertIn(path, message)

    def test_world_constitution_registry_and_validator_are_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "docs/HOSTED_WORLD_REFERENCE_PROFILE.md",
            "docs/OSTADIX_WORLD.md",
            "evidence/world_alpha_gates.toml",
            "evidence/world_contract_v1.toml",
            "evidence/world/g0-repository-conformance.toml",
            "evidence/world/transcripts/g0-repository-conformance.log",
            "scripts/smoke-world-g0-conformance.sh",
            "scripts/world_alpha_evidence.py",
            "tests/test_world_alpha_evidence.py",
        )
        self._git("commit", "-q", "-m", "remove World constitution surfaces")

        with self.assertRaises(release.ReleaseError) as raised:
            self._build("missing-world-constitution.zip")
        message = str(raised.exception)
        self.assertIn("missing required path(s)", message)
        for path in (
            "docs/HOSTED_WORLD_REFERENCE_PROFILE.md",
            "docs/OSTADIX_WORLD.md",
            "evidence/world_alpha_gates.toml",
            "evidence/world_contract_v1.toml",
            "evidence/world/g0-repository-conformance.toml",
            "scripts/world_alpha_evidence.py",
            "tests/test_world_alpha_evidence.py",
        ):
            self.assertIn(path, message)

    def test_world_identity_cross_language_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "ocore/kernel/smoke-world-identity-qemu.sh",
            "ocore/world/identity.oc",
            "crates/ostadix-api/src/world/identity.rs",
            "crates/ostadix-api/src/world/identity_wire.rs",
            "tests/fixtures/world_identity_v1.hex",
            "tests/world_identity_wire.rs",
        )
        self._git("commit", "-q", "-m", "remove World identity surface")

        self._assert_missing_required_paths(
            "missing-world-identity.zip",
            (
                "ocore/kernel/smoke-world-identity-qemu.sh",
                "ocore/world/identity.oc",
                "crates/ostadix-api/src/world/identity.rs",
                "crates/ostadix-api/src/world/identity_wire.rs",
                "tests/fixtures/world_identity_v1.hex",
                "tests/world_identity_wire.rs",
            ),
        )

    def test_world_protocol_cross_language_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "ocore/kernel/smoke-world-protocol-qemu.sh",
            "ocore/kernel/world_protocol_semantics.oc",
            "ocore/kernel/world_protocol_semantics_stub.oc",
            "ocore/world/codec.oc",
            "ocore/world/protocol.oc",
            "crates/ostadix-api/src/world/codec.rs",
            "crates/ostadix-api/src/world/protocol.rs",
            "tests/fixtures/world_protocol_v1.hex",
            "tests/world_protocol.rs",
        )
        self._git("commit", "-q", "-m", "remove World protocol surface")

        self._assert_missing_required_paths(
            "missing-world-protocol.zip",
            (
                "ocore/kernel/smoke-world-protocol-qemu.sh",
                "ocore/kernel/world_protocol_semantics.oc",
                "ocore/kernel/world_protocol_semantics_stub.oc",
                "ocore/world/codec.oc",
                "ocore/world/protocol.oc",
                "crates/ostadix-api/src/world/codec.rs",
                "crates/ostadix-api/src/world/protocol.rs",
                "tests/fixtures/world_protocol_v1.hex",
                "tests/world_protocol.rs",
            ),
        )

    def test_world_value_cross_language_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "ocore/kernel/smoke-world-value-qemu.sh",
            "ocore/kernel/world_value_semantics.oc",
            "ocore/kernel/world_value_semantics_stub.oc",
            "ocore/world/sha256.oc",
            "ocore/world/value.oc",
            "ocore/world/value_codec.oc",
            "crates/ostadix-api/src/world/value.rs",
            "crates/ostadix-api/src/world/value_codec.rs",
            "tests/fixtures/world_value_v1.hex",
            "tests/world_value.rs",
        )
        self._git("commit", "-q", "-m", "remove World value surface")

        self._assert_missing_required_paths(
            "missing-world-value.zip",
            (
                "ocore/kernel/smoke-world-value-qemu.sh",
                "ocore/kernel/world_value_semantics.oc",
                "ocore/kernel/world_value_semantics_stub.oc",
                "ocore/world/sha256.oc",
                "ocore/world/value.oc",
                "ocore/world/value_codec.oc",
                "crates/ostadix-api/src/world/value.rs",
                "crates/ostadix-api/src/world/value_codec.rs",
                "tests/fixtures/world_value_v1.hex",
                "tests/world_value.rs",
            ),
        )

    def test_world_receipt_cross_language_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "ocore/kernel/smoke-world-receipt-qemu.sh",
            "ocore/kernel/world_receipt_semantics.oc",
            "ocore/kernel/world_receipt_semantics_stub.oc",
            "ocore/world/receipt.oc",
            "ocore/world/receipt_codec.oc",
            "crates/ostadix-api/src/world/receipt.rs",
            "crates/ostadix-api/src/world/receipt_codec.rs",
            "tests/fixtures/world_receipt_v1.hex",
            "tests/world_receipt.rs",
        )
        self._git("commit", "-q", "-m", "remove World receipt surface")

        self._assert_missing_required_paths(
            "missing-world-receipt.zip",
            (
                "ocore/kernel/smoke-world-receipt-qemu.sh",
                "ocore/kernel/world_receipt_semantics.oc",
                "ocore/kernel/world_receipt_semantics_stub.oc",
                "ocore/world/receipt.oc",
                "ocore/world/receipt_codec.oc",
                "crates/ostadix-api/src/world/receipt.rs",
                "crates/ostadix-api/src/world/receipt_codec.rs",
                "tests/fixtures/world_receipt_v1.hex",
                "tests/world_receipt.rs",
            ),
        )

    def test_world_resource_key_hosted_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "scripts/smoke-world-resource-keys.sh",
            "crates/ostadix-api/src/effects.rs",
            "crates/ostadix-api/src/executor/mod.rs",
            "crates/ostadix-api/src/hgraph/from_oir.rs",
            "crates/ostadix-api/src/world/grounding.rs",
            "tests/world_resource_keys.rs",
        )
        self._git("commit", "-q", "-m", "remove World ResourceKey surface")

        self._assert_missing_required_paths(
            "missing-world-resource-keys.zip",
            (
                "scripts/smoke-world-resource-keys.sh",
                "crates/ostadix-api/src/effects.rs",
                "crates/ostadix-api/src/executor/mod.rs",
                "crates/ostadix-api/src/hgraph/from_oir.rs",
                "crates/ostadix-api/src/world/grounding.rs",
                "tests/world_resource_keys.rs",
            ),
        )

    def test_evidence_bound_admission_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "crates/ostadix-api/src/evidence/admit.rs",
            "crates/ostadix-api/src/evidence/analyze.rs",
            "crates/ostadix-api/src/evidence/fact.rs",
            "crates/ostadix-api/src/evidence/intent.rs",
            "crates/ostadix-api/src/evidence/mod.rs",
            "crates/ostadix-api/src/evidence/profile.rs",
            "crates/ostadix-api/src/backend_catalog.rs",
            "crates/ostadix-api/src/backend_catalog.inc.rs",
            "crates/ostadix-api/src/execution_contract.rs",
            "crates/ostadix-api/src/runtime_exec.rs",
        )
        self._git("commit", "-q", "-m", "remove evidence-bound admission surface")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): .*crates/ostadix-api/src/backend_catalog\.inc\.rs.*"
            r"crates/ostadix-api/src/backend_catalog\.rs.*"
            r"crates/ostadix-api/src/evidence/admit\.rs.*"
            r"crates/ostadix-api/src/evidence/analyze\.rs.*crates/ostadix-api/src/evidence/fact\.rs.*"
            r"crates/ostadix-api/src/evidence/intent\.rs.*crates/ostadix-api/src/evidence/mod\.rs.*crates/ostadix-api/src/evidence/profile\.rs.*"
            r"crates/ostadix-api/src/execution_contract\.rs.*"
            r"crates/ostadix-api/src/runtime_exec\.rs",
        ):
            self._build("missing-evidence-bound-admission.zip")

    def test_olangc_embedded_runtime_source_closure_is_derived_from_compiler(self) -> None:
        self._commit(
            {
                "crates/ostadix-api/src/api/aot_source.rs": (
                    'pub const RUNTIME_VALUE_RS: &str = include_str!("../value.rs");\n'
                    'pub const AOT_ONLY: &str = include_str!("../aot_only.rs");\n'
                ),
                "crates/ostadix-api/src/aot_only.rs": (
                    "// generated-runtime fixture input\n"
                ),
            }
        )
        self._git("rm", "crates/ostadix-api/src/aot_only.rs")
        self._git("commit", "-q", "-m", "remove generated runtime input")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"generated-runtime source closure path\(s\): crates/ostadix-api/src/aot_only\.rs",
        ):
            self._build("missing-generated-runtime-input.zip")

    def test_archive_verifier_revalidates_olangc_embedded_runtime_closure(self) -> None:
        result = self._build(
            "valid-before-runtime-closure-tamper.zip",
            ref=self._commit(
                {
                    "crates/ostadix-api/src/value.rs": "// generated-runtime fixture value model\n",
                }
            ),
        )

        def redirect_runtime_include(entry):
            if entry.path == "crates/ostadix-api/src/api/aot_source.rs":
                return release.SourceEntry(
                    entry.path,
                    entry.mode,
                    entry.data.replace(b"../value.rs", b"../missing-runtime.rs"),
                )
            return entry

        tampered = self._rewrite_self_consistent(
            result.output,
            "self-consistent-missing-runtime-closure.zip",
            redirect_runtime_include,
        )
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"generated-runtime source closure path\(s\): crates/ostadix-api/src/missing-runtime\.rs",
        ):
            release.verify_archive(tampered)

    def test_prepared_task_driver_pool_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "crates/ostadix-api/src/executor/driver.rs",
            "crates/ostadix-api/src/executor/pool.rs",
            "crates/ostadix-api/src/executor/task.rs",
        )
        self._git("commit", "-q", "-m", "remove prepared-task pool surface")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): .*crates/ostadix-api/src/executor/driver\.rs.*"
            r"crates/ostadix-api/src/executor/pool\.rs.*"
            r"crates/ostadix-api/src/executor/task\.rs",
        ):
            self._build("missing-prepared-task-pool.zip")

    def test_execution_fabric_profile_and_codec_surface_are_required(self) -> None:
        required = (
            "docs/OIR_EXECUTION_FABRIC_V1.md",
            "crates/ostadix-api/src/execution_fabric/codec.rs",
            "crates/ostadix-api/src/execution_fabric/protocol.rs",
        )
        self._commit()
        self._git("rm", *required)
        self._git("commit", "-q", "-m", "remove execution-fabric release closure")

        self._assert_missing_required_paths(
            "missing-execution-fabric-profile.zip",
            required,
        )

    def test_unified_intent_front_door_contract_and_acceptance_are_required(
        self,
    ) -> None:
        required = (
            "docs/UNIFIED_INTENT_FRONT_DOOR_V1.md",
            "tests/unified_intent_acceptance.rs",
            "tests/o_cli_intent_blackbox.rs",
            "tests/unified_plan_boundaries.rs",
        )
        self._commit()
        self._git("rm", *required)
        self._git("commit", "-q", "-m", "remove unified intent release closure")

        self._assert_missing_required_paths(
            "missing-unified-intent-front-door.zip",
            required,
        )

    def test_project_hgraph_hosted_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "scripts/o-cli.sh",
            "scripts/o-kernel.sh",
            "scripts/install-o-cli-wrapper.sh",
            "scripts/smoke-project-hgraph-exec.sh",
            "scripts/smoke-project-hgraph.sh",
            ".github/workflows/ci.yml",
            "src/bin/o-cli.rs",
            "src/bin/olangc.rs",
            "src/bin/olink.rs",
            "crates/ostadix-api/src/hgraph/graph.rs",
            "crates/ostadix-api/src/hgraph/kinds.rs",
            "crates/ostadix-api/src/project/executor.rs",
            "crates/ostadix-api/src/project/deployment.rs",
            "crates/ostadix-api/src/project/logical.rs",
            "crates/ostadix-api/src/project/mod.rs",
            "crates/ostadix-api/src/project/model.rs",
            "crates/ostadix-api/src/project/plan.rs",
            "crates/ostadix-api/src/project/runtime.rs",
            "crates/ostadix-api/src/project/trace.rs",
            "tests/fixtures/project_hgraph/input.txt",
            "tests/fixtures/project_hgraph/olang.project.toml",
            "tests/fixtures/project_hgraph_exec/input.txt",
            "tests/fixtures/project_hgraph_exec/olang.project.toml",
            "tests/fixtures/project_hgraph_tools/sh",
            "tests/project_hgraph.rs",
            "tests/project_hgraph_exec.rs",
            "tests/project_deployment_plan.rs",
            "tests/project_logical_hgraph.rs",
        )
        self._git("commit", "-q", "-m", "remove hosted project HGraph surface")

        self._assert_missing_required_paths(
            "missing-project-hgraph.zip",
            (
                ".github/workflows/ci.yml",
                "scripts/o-cli.sh",
                "scripts/o-kernel.sh",
                "scripts/install-o-cli-wrapper.sh",
                "scripts/smoke-project-hgraph-exec.sh",
                "scripts/smoke-project-hgraph.sh",
                "src/bin/o-cli.rs",
                "src/bin/olangc.rs",
                "src/bin/olink.rs",
                "crates/ostadix-api/src/hgraph/graph.rs",
                "crates/ostadix-api/src/hgraph/kinds.rs",
                "crates/ostadix-api/src/project/executor.rs",
                "crates/ostadix-api/src/project/deployment.rs",
                "crates/ostadix-api/src/project/logical.rs",
                "crates/ostadix-api/src/project/mod.rs",
                "crates/ostadix-api/src/project/model.rs",
                "crates/ostadix-api/src/project/plan.rs",
                "crates/ostadix-api/src/project/runtime.rs",
                "crates/ostadix-api/src/project/trace.rs",
                "tests/fixtures/project_hgraph/input.txt",
                "tests/fixtures/project_hgraph/olang.project.toml",
                "tests/fixtures/project_hgraph_exec/input.txt",
                "tests/fixtures/project_hgraph_exec/olang.project.toml",
                "tests/fixtures/project_hgraph_tools/sh",
                "tests/project_hgraph.rs",
                "tests/project_hgraph_exec.rs",
                "tests/project_deployment_plan.rs",
                "tests/project_logical_hgraph.rs",
            ),
        )

    def test_project_world_runtime_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "ocore/kernel/smoke-world-project-receipt-qemu.sh",
            "ocore/kernel/smoke-world-project-runtime-qemu.sh",
            "ocore/kernel/world_project_receipt_semantics.oc",
            "ocore/kernel/world_project_receipt_semantics_stub.oc",
            "crates/ostadix-api/src/project/launch.rs",
            "crates/ostadix-api/src/project/runtime_graph.rs",
            "crates/ostadix-api/src/project/world_execution.rs",
            "tests/project_world_runtime.rs",
        )
        self._git("commit", "-q", "-m", "remove bounded World-project runtime")

        self._assert_missing_required_paths(
            "missing-project-world-runtime.zip",
            (
                "ocore/kernel/smoke-world-project-receipt-qemu.sh",
                "ocore/kernel/smoke-world-project-runtime-qemu.sh",
                "ocore/kernel/world_project_receipt_semantics.oc",
                "ocore/kernel/world_project_receipt_semantics_stub.oc",
                "crates/ostadix-api/src/project/launch.rs",
                "crates/ostadix-api/src/project/runtime_graph.rs",
                "crates/ostadix-api/src/project/world_execution.rs",
                "tests/project_world_runtime.rs",
            ),
        )

    def test_world_normative_bytes_are_sealed_before_packaging(self) -> None:
        for path, data in WORLD_NORMATIVE_BYTES.items():
            with self.subTest(path=path):
                self._commit({path: data + b"\n"})
                with self.assertRaises(release.ReleaseError) as raised:
                    self._build(f"tampered-world-{Path(path).name}.zip")
                message = str(raised.exception)
                self.assertIn(path, message)
                self.assertIn(
                    "SHA-256 differs from sealed OSTADIX Alpha constitutional bytes",
                    message,
                )

    def test_archive_verifier_rejects_self_consistent_world_byte_tamper(self) -> None:
        result = self._build("valid-before-world-tamper.zip", ref=self._commit())
        for index, path in enumerate(WORLD_NORMATIVE_BYTES):
            with self.subTest(path=path):

                def append_newline(entry, target=path):
                    if entry.path == target:
                        return release.SourceEntry(
                            entry.path, entry.mode, entry.data + b"\n"
                        )
                    return entry

                tampered = self._rewrite_self_consistent(
                    result.output,
                    f"self-consistent-world-tamper-{index}.zip",
                    append_newline,
                )
                with self.assertRaises(release.ReleaseError) as raised:
                    release.verify_archive(tampered)
                message = str(raised.exception)
                self.assertIn(path, message)
                self.assertIn(
                    "SHA-256 differs from sealed OSTADIX Alpha constitutional bytes",
                    message,
                )

    def test_world_attestation_rejects_transcript_tamper(self) -> None:
        path = "evidence/world/transcripts/g2-aarch64-qemu.log"
        self._commit({path: (PROJECT_ROOT / path).read_bytes() + b"tamper\n"})
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"g2-aarch64-qemu\.toml\.transcript digest does not match",
        ):
            self._build("tampered-g2-transcript.zip")

    def test_historical_world_attestation_does_not_claim_current_archive_sources(self) -> None:
        path = "crates/ostadix-api/src/ocore/codegen_aarch64.rs"
        commit = self._commit({path: (PROJECT_ROOT / path).read_bytes() + b"\n"})
        result = self._build("historical-g2-source.zip", ref=commit)
        self.assertTrue(result.output.is_file())

    def test_world_rederive_event_rejects_payload_tamper(self) -> None:
        path = "evidence/world/g2-derivation-rederive-2026-08-03.toml"
        tampered = (PROJECT_ROOT / path).read_text(encoding="utf-8").replace(
            "The claim set is unchanged", "The claim set changed"
        )
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"payload_sha256 does not bind the rederive event",
        ):
            release._validate_world_rederive_event_release_surface(
                {path: tampered.encode("utf-8")}, {path: "100644"}, path
            )
        self._commit({path: tampered})
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"bytes differ from the immutable evidence-event seal",
        ):
            self._build("tampered-g2-rederive.zip")

    def test_release_ledger_rejects_an_unenumerated_world_toml(self) -> None:
        files = {"evidence/world/ignored.toml": b'schema_version = 1\n'}
        modes = {"evidence/world/ignored.toml": "100644"}
        with mock.patch.object(
            release, "WORLD_HISTORICAL_ATTESTATION_SHA256", {}
        ), mock.patch.object(
            release, "WORLD_CURRENT_ATTESTATION_SHA256", {}
        ), mock.patch.object(
            release, "WORLD_EVIDENCE_EVENT_SHA256", {}
        ):
            with self.assertRaisesRegex(
                release.ReleaseError,
                r"exhaustive release set.*ignored\.toml",
            ):
                release._validate_world_evidence_ledger_release_surface(
                    files, modes, {"G0"}, {"repository_conformance"}
                )

    def test_release_ledger_exact_byte_seals_every_event(self) -> None:
        path = "evidence/world/event.toml"
        files = {path: b'event = "retract"\n'}
        modes = {path: "100644"}
        with mock.patch.object(
            release, "WORLD_HISTORICAL_ATTESTATION_SHA256", {}
        ), mock.patch.object(
            release, "WORLD_CURRENT_ATTESTATION_SHA256", {}
        ), mock.patch.object(
            release, "WORLD_EVIDENCE_EVENT_SHA256", {path: "0" * 64}
        ):
            with self.assertRaisesRegex(
                release.ReleaseError,
                r"immutable evidence-event seal",
            ):
                release._validate_world_evidence_ledger_release_surface(
                    files, modes, {"G0"}, {"repository_conformance"}
                )

    def test_release_ledger_exact_byte_seals_current_attestations(self) -> None:
        path = "evidence/world/current.toml"
        files = {path: b'schema_version = 3\n'}
        modes = {path: "100644"}
        with mock.patch.object(
            release, "WORLD_HISTORICAL_ATTESTATION_SHA256", {}
        ), mock.patch.object(
            release, "WORLD_CURRENT_ATTESTATION_SHA256", {path: "0" * 64}
        ), mock.patch.object(
            release, "WORLD_EVIDENCE_EVENT_SHA256", {}
        ):
            with self.assertRaisesRegex(
                release.ReleaseError,
                r"immutable current-attestation seal",
            ):
                release._validate_world_evidence_ledger_release_surface(
                    files, modes, {"G0"}, {"repository_conformance"}
                )

    def test_release_external_unverified_witness_is_status_inert(self) -> None:
        witness_path = "evidence/world/witness.toml"
        payload = "2" * 64
        witness_record = {
            "schema_version": 1,
            "id": "witness-a",
            "event": "witness",
            "subject": "rederive-a",
            "subject_record_sha256": payload,
            "algorithm": "ed25519",
            "key_id": "external-key-1",
            "public_key": "3" * 64,
            "run_identity": "external-run-1",
            "source_commit": "5" * 40,
            "verification": "external_unverified",
        }
        witness_payload_sha256 = release._world_witness_payload_sha256(
            witness_record
        )
        witness_text = "\n".join(
            (
                "schema_version = 1",
                'id = "witness-a"',
                'event = "witness"',
                'subject = "rederive-a"',
                f'subject_record_sha256 = "{payload}"',
                f'witness_payload_sha256 = "{witness_payload_sha256}"',
                'algorithm = "ed25519"',
                'key_id = "external-key-1"',
                f'public_key = "{"3" * 64}"',
                f'signature = "{"4" * 128}"',
                'run_identity = "external-run-1"',
                f'source_commit = "{"5" * 40}"',
                'verification = "external_unverified"',
                "",
            )
        ).encode("utf-8")
        witness = release._validate_world_witness_event_release_surface(
            {witness_path: witness_text}, {witness_path: "100644"}, witness_path
        )
        zero_key_text = witness_text.replace(
            f'public_key = "{"3" * 64}"'.encode("ascii"),
            f'public_key = "{"0" * 64}"'.encode("ascii"),
        )
        with self.assertRaisesRegex(
            release.ReleaseError, "public_key must not be all zero"
        ):
            release._validate_world_witness_event_release_surface(
                {witness_path: zero_key_text},
                {witness_path: "100644"},
                witness_path,
            )
        prior = "sha256:" + "1" * 64
        attestation = {
            "id": "g2-aarch64-qemu-tcg-2026-08-03",
            "path": "evidence/world/g2.toml",
            "gate": "G2",
            "schema_version": 2,
            "recorded_claims": {"claim"},
            "current_derived_claims": {"claim"},
            "derivation_hash": prior,
        }
        rederive = {
            "id": "rederive-a",
            "path": "evidence/world/rederive.toml",
            "event": "rederive",
            "subject": attestation["id"],
            "prior_derivation": prior,
            "current_derivation": release.WORLD_DERIVATION_HASH,
            "claims_lost": set(),
            "claims_gained": set(),
            "payload_sha256": payload,
            "record_sha256": payload,
        }
        active = release._validate_release_rederive_ledger(
            [attestation], [rederive, witness]
        )
        self.assertEqual([item["id"] for item in active], [attestation["id"]])

    def test_release_witness_can_bind_a_supersession_without_changing_status(self) -> None:
        old = {
            "id": "old",
            "path": "evidence/world/old.toml",
            "gate": "G0",
            "schema_version": 3,
            "recorded_claims": {"claim"},
            "current_derived_claims": {"claim"},
            "derivation_hash": release.WORLD_DERIVATION_HASH,
        }
        replacement = {
            **old,
            "id": "replacement",
            "path": "evidence/world/replacement.toml",
        }
        lifecycle = {
            "id": "supersede-old",
            "path": "evidence/world/supersede.toml",
            "event": "supersede",
            "subject": "old",
            "replacement": "replacement",
            "record_sha256": "6" * 64,
        }
        witness = {
            "id": "witness-supersede-old",
            "path": "evidence/world/witness.toml",
            "event": "witness",
            "subject": "supersede-old",
            "subject_record_sha256": "6" * 64,
            "verification": "external_unverified",
        }
        active = release._validate_release_rederive_ledger(
            [old, replacement], [lifecycle, witness]
        )
        self.assertEqual([item["id"] for item in active], ["replacement"])

    def test_release_schema_v1_attestation_cannot_be_active(self) -> None:
        attestation = {
            "id": "legacy",
            "path": "evidence/world/legacy.toml",
            "gate": "G0",
            "schema_version": 1,
            "recorded_claims": set(),
            "current_derived_claims": set(),
            "derivation_hash": None,
        }
        with self.assertRaisesRegex(
            release.ReleaseError,
            "schema-v1 attestation legacy cannot be an active ledger head",
        ):
            release._validate_release_rederive_ledger([attestation], [])

    def test_release_unpinned_schema_v2_attestation_cannot_be_active(self) -> None:
        attestation = {
            "id": "unlisted-schema-v2",
            "path": "evidence/world/legacy.toml",
            "gate": "G0",
            "schema_version": 2,
            "recorded_claims": {"claim"},
            "current_derived_claims": {"claim"},
            "derivation_hash": release.WORLD_DERIVATION_HASH,
        }
        with self.assertRaisesRegex(
            release.ReleaseError,
            "active attestation unlisted-schema-v2 must use schema v3",
        ):
            release._validate_release_rederive_ledger([attestation], [])

    def test_fresh_attestation_cannot_couple_replace_the_trusted_validator(self) -> None:
        path = "evidence/world/g0-independent-engine-2026-08-17.toml"
        source_path = PROJECT_ROOT / path
        if not source_path.is_file():
            self.skipTest("fresh schema-v3 G0 attestation has not been minted yet")
        attestation = tomllib.loads(source_path.read_text(encoding="utf-8"))
        referenced = {
            path,
            attestation["transcript"],
            attestation["command"][0][2:],
            *(
                release._released_path_for_historical_source(item["path"])
                for item in attestation["source"]
            ),
            *(
                item["path"]
                for item in attestation["artifact"]
                if item["retained"]
            ),
        }
        files = {item: (PROJECT_ROOT / item).read_bytes() for item in referenced}
        modes = {item: "100644" for item in referenced}
        modes[attestation["command"][0][2:]] = "100755"
        validator_path = "scripts/world_alpha_evidence.py"
        old_digest = attestation["validator_sha256"]
        files[validator_path] += b"\n"
        replacement_digest = hashlib.sha256(files[validator_path]).hexdigest()
        record = files[path].decode("utf-8")
        self.assertGreaterEqual(record.count(old_digest), 2)
        files[path] = record.replace(old_digest, replacement_digest).encode("utf-8")
        with self.assertRaisesRegex(
            release.ReleaseError,
            "validator_sha256 differs from the trusted validator bytes",
        ):
            release._validate_world_attestation_release_surface(
                files, modes, path, "G0", "repository_conformance"
            )

    def test_fresh_attestation_seal_rejects_coupled_transcript_rewrite(self) -> None:
        path = "evidence/world/g0-independent-engine-2026-08-17.toml"
        source_path = PROJECT_ROOT / path
        if not source_path.is_file():
            self.skipTest("fresh schema-v3 G0 attestation has not been minted yet")
        attestation = tomllib.loads(source_path.read_text(encoding="utf-8"))
        transcript_path = attestation["transcript"]
        transcript = (PROJECT_ROOT / transcript_path).read_bytes() + b"tamper\n"
        old_digest = attestation["transcript_sha256"]
        replacement_digest = hashlib.sha256(transcript).hexdigest()
        record = source_path.read_text(encoding="utf-8").replace(
            old_digest, replacement_digest
        ).encode("utf-8")
        files = {path: record, transcript_path: transcript}
        modes = {path: "100644", transcript_path: "100644"}
        original_seal = hashlib.sha256(source_path.read_bytes()).hexdigest()
        with mock.patch.object(
            release, "WORLD_HISTORICAL_ATTESTATION_SHA256", {}
        ), mock.patch.object(
            release, "WORLD_CURRENT_ATTESTATION_SHA256", {path: original_seal}
        ), mock.patch.object(
            release, "WORLD_EVIDENCE_EVENT_SHA256", {}
        ):
            with self.assertRaisesRegex(
                release.ReleaseError,
                "immutable current-attestation seal",
            ):
                release._validate_world_evidence_ledger_release_surface(
                    files, modes, {"G0"}, {"repository_conformance"}
                )

    def test_world_registry_structure_is_checked_beneath_byte_seal(self) -> None:
        path = "evidence/world_alpha_gates.toml"
        malformed = b"schema_version = 3\n"
        files = {
            candidate: (PROJECT_ROOT / candidate).read_bytes()
            for candidate in WORLD_EVIDENCE_RELEASE_PATHS
            | set(WORLD_NORMATIVE_BYTES)
        }
        files[path] = malformed
        modes = {
            candidate: (
                "100755"
                if candidate
                in {
                    "scripts/smoke-world-g0-conformance.sh",
                    "ocore/kernel/smoke-aarch64-g2-qemu.sh",
                }
                else "100644"
            )
            for candidate in files
        }
        with mock.patch.dict(
            release.SEALED_WORLD_ALPHA_SHA256,
            {path: hashlib.sha256(malformed).hexdigest()},
        ):
            with self.assertRaisesRegex(
                release.ReleaseError,
                r"world_alpha_gates\.toml root keys differ from schema",
            ):
                release._validate_world_alpha_release_surface(files, modes)

    def test_example_manifest_references_must_resolve(self) -> None:
        missing_example = {
            "schema_version": 1,
            "examples": [
                {
                    "path": "absent.O",
                    "editions": ["rust"],
                    "classification": "unit",
                    "requirements": {
                        "backends": ["text"],
                        "programs": [],
                        "authorities": [],
                    },
                    "expected": {"rust": {"patterns": ["absent"]}},
                }
            ],
        }
        self._commit(
            {"examples/manifest.json": json.dumps(missing_example, sort_keys=True)}
        )
        with self.assertRaisesRegex(
            release.ReleaseError, r"path references absent examples/absent\.O"
        ):
            self._build("invalid-example-reference.zip")

    def test_evidence_manifest_gate_scripts_must_resolve(self) -> None:
        evidence = fixture_evidence_manifest().replace(
            'script = "ocore/kernel/fixture-evidence-00.sh"',
            'script = "ocore/kernel/missing-gate.sh"',
            1,
        )
        self._commit({"evidence/gates.toml": evidence})
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"script references absent ocore/kernel/missing-gate\.sh",
        ):
            self._build("invalid-evidence-reference.zip")

    def test_g2_aarch64_false_physical_claim_is_rejected_before_packaging(self) -> None:
        evidence = fixture_evidence_manifest().replace(
            json.dumps(list(release.G2_AARCH64_POSITIVE_CLAIMS)),
            json.dumps(["Physical AArch64 and SMMU isolation are proven"]),
            1,
        )
        self._commit({"evidence/gates.toml": evidence})
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"G2 AArch64 positive claims exceed the sealed boundary",
        ):
            self._build("false-g2-physical-claim.zip")

    def test_g2_aarch64_linux_boot_claim_is_rejected_before_packaging(self) -> None:
        evidence = fixture_evidence_manifest().replace(
            json.dumps(list(release.G2_AARCH64_NONCLAIMS)),
            json.dumps(["This AArch64 gate boots Linux and Plan 9"]),
            1,
        )
        self._commit({"evidence/gates.toml": evidence})
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"G2 AArch64 nonclaims differ from the sealed boundary",
        ):
            self._build("false-g2-linux-claim.zip")

    def test_zero_gate_evidence_manifest_is_rejected_before_packaging(self) -> None:
        self._commit({"evidence/gates.toml": ZERO_GATE_EVIDENCE_MANIFEST})
        with self.assertRaisesRegex(
            release.ReleaseError,
            rf"required_gate_count must be {FIXTURE_REQUIRED_EVIDENCE_GATE_COUNT}",
        ):
            self._build("zero-gate-evidence.zip")

    def test_supplemental_gate_count_is_pinned_before_packaging(self) -> None:
        evidence = fixture_evidence_manifest().replace(
            "supplemental_gate_count = 1", "supplemental_gate_count = 2", 1
        )
        self._commit({"evidence/gates.toml": evidence})
        with self.assertRaisesRegex(
            release.ReleaseError, r"supplemental_gate_count must be 1"
        ):
            self._build("drifted-supplemental-count.zip")

    def test_archive_verifier_rejects_self_consistent_zero_gate_evidence(self) -> None:
        result = self._build("valid-before-evidence-tamper.zip", ref=self._commit())

        def remove_gates(entry):
            if entry.path == "evidence/gates.toml":
                return release.SourceEntry(
                    entry.path, entry.mode, ZERO_GATE_EVIDENCE_MANIFEST.encode("utf-8")
                )
            return entry

        tampered = self._rewrite_self_consistent(
            result.output, "self-consistent-zero-gate-evidence.zip", remove_gates
        )
        with self.assertRaisesRegex(
            release.ReleaseError,
            rf"required_gate_count must be {FIXTURE_REQUIRED_EVIDENCE_GATE_COUNT}",
        ):
            release.verify_archive(tampered)

    def test_same_commit_produces_byte_identical_zip_across_ref_aliases(self) -> None:
        commit = self._commit(
            {
                "assets/data.bin": b"\x00\x01\x02",
                "scripts/release-helper.sh": "#!/bin/sh\nexit 0\n",
                "src/lib.rs": FIXTURE_ROOT_LIB_SOURCE + "// answer=42\n",
            }
        )
        first = self._build("first.zip", ref="HEAD")
        second = self._build("second.zip", ref=commit)
        self.assertEqual(first.prefix, second.prefix)
        self.assertEqual(first.archive_sha256, second.archive_sha256)
        self.assertEqual(first.output.read_bytes(), second.output.read_bytes())

    def test_dirty_tree_requires_override_and_override_uses_commit_bytes(self) -> None:
        self._commit({"src/lib.rs": FIXTURE_ROOT_LIB_SOURCE + "// clean\n"})
        self._write("README.md", "uncommitted readme\n")
        self._write("untracked.txt", "untracked\n")

        with self.assertRaisesRegex(release.ReleaseError, "working tree is dirty"):
            self._build("refused.zip")

        result = self._build("allowed.zip", allow_dirty=True)
        manifest = release.verify_archive(result.output)
        with zipfile.ZipFile(result.output) as archive:
            payload = archive.read(f"{manifest['prefix']}/README.md")
            self.assertEqual(payload, fixture_readme().encode())
            self.assertNotIn(
                f"{manifest['prefix']}/untracked.txt", set(archive.namelist())
            )

    def test_verifier_rejects_payload_tampering(self) -> None:
        self._commit({"src/lib.rs": FIXTURE_ROOT_LIB_SOURCE + "// intact\n"})
        result = self._build("valid.zip")
        tampered = self.root / "tampered.zip"

        with zipfile.ZipFile(result.output, "r") as source:
            members = [
                (info.filename, f"{(info.external_attr >> 16) & 0xFFFF:06o}", source.read(info))
                for info in source.infolist()
            ]
        with zipfile.ZipFile(
            tampered,
            "w",
            compression=zipfile.ZIP_DEFLATED,
            compresslevel=9,
        ) as destination:
            for name, mode, data in members:
                if name.endswith("/README.md"):
                    data = b"tampered\n"
                destination.writestr(
                    release._zip_info(name, mode),
                    data,
                    compress_type=zipfile.ZIP_DEFLATED,
                    compresslevel=9,
                )

        with self.assertRaisesRegex(release.ReleaseError, "payload does not match manifest"):
            release.verify_archive(tampered)


if __name__ == "__main__":
    unittest.main()
