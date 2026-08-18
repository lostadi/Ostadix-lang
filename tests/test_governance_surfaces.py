from __future__ import annotations

import re
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PRIVATE_REPORT_URL = (
    "https://github.com/lostadi/Ostadix-lang/security/advisories/new"
)


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def string_constant(relative: str, name: str) -> str:
    source = read(relative)
    match = re.search(
        rf"\b{name}\b[^=]*=\s*\"([^\"]+)\"\s*;",
        source,
    )
    if match is None:
        raise AssertionError(f"missing string constant {name} in {relative}")
    return match.group(1)


def integer_constant(relative: str, name: str) -> int:
    source = read(relative)
    match = re.search(rf"\b{name}\b[^=]*=\s*(\d+)\s*;", source)
    if match is None:
        raise AssertionError(f"missing integer constant {name} in {relative}")
    return int(match.group(1))


class GovernanceSurfaceTests(unittest.TestCase):
    def test_required_surfaces_exist(self) -> None:
        required = (
            "CONTRIBUTING.md",
            "SECURITY.md",
            "CODE_OF_CONDUCT.md",
            "CHANGELOG.md",
            "docs/VERSIONING.md",
            ".github/CODEOWNERS",
            ".github/ISSUE_TEMPLATE/config.yml",
            ".github/ISSUE_TEMPLATE/bug_report.yml",
            ".github/ISSUE_TEMPLATE/feature_request.yml",
            ".github/pull_request_template.md",
        )
        missing = [relative for relative in required if not (ROOT / relative).is_file()]
        self.assertEqual(missing, [])

    def test_security_reports_stay_private(self) -> None:
        security = read("SECURITY.md")
        lowered = security.lower()
        self.assertIn(PRIVATE_REPORT_URL, security)
        self.assertIn("do not disclose", lowered)
        self.assertIn("public issues", lowered)
        self.assertNotIn("mailto:", lowered)
        self.assertIsNone(
            re.search(
                r"[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)+",
                security,
                re.IGNORECASE,
            )
        )

        config = read(".github/ISSUE_TEMPLATE/config.yml")
        self.assertIn("blank_issues_enabled: false", config)
        self.assertIn(PRIVATE_REPORT_URL, config)
        for template in (
            ".github/ISSUE_TEMPLATE/bug_report.yml",
            ".github/ISSUE_TEMPLATE/feature_request.yml",
        ):
            body = read(template)
            self.assertIn(PRIVATE_REPORT_URL, body)
            self.assertRegex(body.lower(), r"not (?:this|a) public|do not disclose")

    def test_codeowners_and_release_boundary_are_explicit(self) -> None:
        self.assertRegex(
            read(".github/CODEOWNERS"),
            r"(?m)^\*\s+@lostadi\s*$",
        )
        changelog = read("CHANGELOG.md")
        self.assertIn("## [Unreleased]", changelog)
        self.assertIn("does not itself assert a tag", changelog)
        citation = read("CITATION.cff")
        self.assertIn('version: "0.4.0"', citation)
        self.assertNotIn("date-released:", citation)

    def test_ci_builds_the_independent_engine_and_shell_publish_artifacts(self) -> None:
        workflow = read(".github/workflows/ci.yml")
        self.assertIn(
            "cargo +1.97.1 package --locked --package ostadix-api",
            workflow,
        )
        self.assertIn(
            "cargo +1.97.1 package --locked --no-verify --package "
            "ostadix-api --package o-lang",
            workflow,
        )

    def test_versioning_document_tracks_source_coordinates(self) -> None:
        versioning = read("docs/VERSIONING.md")
        cargo = tomllib.loads(read("Cargo.toml"))["package"]
        toolchain = tomllib.loads(read("rust-toolchain.toml"))["toolchain"]
        for value in (
            cargo["version"],
            cargo["rust-version"],
            toolchain["channel"],
        ):
            self.assertIn(f"`{value}`", versioning)

        constants = (
            ("crates/ostadix-api/src/evidence/intent.rs", "EXECUTION_INTENT_SCHEMA_V1"),
            ("crates/ostadix-api/src/evidence/fact.rs", "EVIDENCE_SCHEMA_V5"),
            ("crates/ostadix-api/src/evidence/fact.rs", "ADMISSION_SCHEMA_V5"),
            ("crates/ostadix-api/src/evidence/fact.rs", "EVIDENCE_SCHEMA_V6"),
            ("crates/ostadix-api/src/evidence/fact.rs", "ADMISSION_SCHEMA_V6"),
            ("crates/ostadix-api/src/evidence/fact.rs", "ANALYZER_ID_V6"),
            ("crates/ostadix-api/src/evidence/admit.rs", "SCHEDULE_EXPLANATION_SCHEMA_V1"),
            ("crates/ostadix-api/src/evidence/admit.rs", "SCHEDULE_EXPLANATION_SCHEMA_V2"),
            ("crates/ostadix-api/src/evidence/admit.rs", "SCHEDULE_WHY_SCHEMA_V1"),
            ("crates/ostadix-api/src/evidence/admit.rs", "SCHEDULE_WHY_SCHEMA_V2"),
            ("crates/ostadix-api/src/evidence/admit.rs", "PLACEMENT_ADMISSION_DIGEST_DOMAIN_V1"),
            ("crates/ostadix-api/src/evidence/admit.rs", "PLACEMENT_ADMISSION_DIGEST_DOMAIN_V2"),
            (
                "crates/ostadix-api/src/hosted_remote/v2/store.rs",
                "HOSTED_STATE_AUTHORITY_SCHEMA_V1",
            ),
            ("crates/ostadix-api/src/hosted_remote/protocol.rs", "HOSTED_PROTOCOL_V1"),
            ("crates/ostadix-api/src/hosted_remote/v2/protocol.rs", "HOSTED_PROTOCOL_V2"),
            ("crates/ostadix-api/src/information/mod.rs", "INFORMATION_SCHEMA_V1"),
            ("crates/ostadix-api/src/information/model.rs", "INFORMATION_ATOM_SCHEMA_V1"),
            ("crates/ostadix-api/src/information/model.rs", "ENTITY_DESCRIPTOR_SCHEMA_V1"),
            ("crates/ostadix-api/src/information/root.rs", "INFORMATION_SNAPSHOT_SCHEMA_V1"),
            ("crates/ostadix-api/src/information/root.rs", "INFORMATION_REVISION_SCHEMA_V1"),
            ("crates/ostadix-api/src/information/delta.rs", "INFORMATION_DELTA_SCHEMA_V1"),
            ("crates/ostadix-api/src/information/projection.rs", "PROJECTION_RECEIPT_SCHEMA_V1"),
            ("crates/ostadix-api/src/information/exchange.rs", "INFORMATION_DELTA_PACK_SCHEMA_V1"),
            (
                "crates/ostadix-api/src/information/exchange.rs",
                "SIGNED_INFORMATION_DELTA_PACK_SCHEMA_V1",
            ),
            ("crates/ostadix-api/src/information/decision.rs", "DECISION_RECEIPT_SCHEMA_V1"),
            ("crates/ostadix-api/src/information/decision.rs", "OBSERVATION_RECORD_SCHEMA_V1"),
            ("crates/ostadix-api/src/backend_morphism.rs", "BACKEND_MORPHISM_SCHEMA_V1"),
        )
        for relative, name in constants:
            with self.subTest(name=name):
                self.assertIn(f"`{name}`", versioning)
                self.assertIn(f"`{relative}`", versioning)
                self.assertIn(string_constant(relative, name), versioning)

        world_constants = (
            ("crates/ostadix-api/src/world/protocol.rs", "WORLD_SCHEMA_V1"),
            ("crates/ostadix-api/src/world/codec.rs", "WORLD_WIRE_CODEC_VERSION"),
            ("crates/ostadix-api/src/world/identity_wire.rs", "IDENTITY_WIRE_VERSION"),
            ("crates/ostadix-api/src/world/value_codec.rs", "OVALUE_WIRE_SCHEMA_V1"),
            ("crates/ostadix-api/src/world/receipt_codec.rs", "WORLD_RECEIPT_SCHEMA_V1"),
        )
        for relative, name in world_constants:
            with self.subTest(name=name):
                self.assertIn(f"`{name}`", versioning)
                self.assertIn(f"`{relative}`", versioning)
                self.assertIn(f"coordinate `{integer_constant(relative, name)}`", versioning)

        catalog = re.search(
            r'current_schema:\s*"([^"]+)"',
            read("crates/ostadix-api/src/backend_catalog.inc.rs"),
        )
        self.assertIsNotNone(catalog)
        assert catalog is not None
        self.assertIn(catalog.group(1), versioning)
        self.assertIn("Hosted Placement V6", versioning)
        self.assertIn("not a source-level schema constant", versioning)
        self.assertIn("not execution authority", versioning)


if __name__ == "__main__":
    unittest.main()
