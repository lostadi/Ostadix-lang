from __future__ import annotations

import unittest
from unittest.mock import Mock, patch
import subprocess

from scripts.check_attribution import (
    CommitMetadata,
    LEGACY_POLICY_BASELINE,
    cargo_author_values,
    commit_violations,
    contains_forbidden_identity,
    policy_boundary,
    project_metadata_violations,
    revision_spec,
    select_scan_base,
)


def commit(**overrides: str) -> CommitMetadata:
    values = {
        "oid": "a" * 40,
        "author_name": "Lee Daghlar Ostadi",
        "author_email": "ostadi.lee@gmail.com",
        "committer_name": "Lee Daghlar Ostadi",
        "committer_email": "ostadi.lee@gmail.com",
        "message": "Document Codex as a supported development tool",
    }
    values.update(overrides)
    return CommitMetadata(**values)


class AttributionPolicyTests(unittest.TestCase):
    def test_tooling_mentions_are_not_attribution(self) -> None:
        self.assertEqual(commit_violations(commit()), [])

    def test_ai_author_and_committer_identities_are_rejected(self) -> None:
        violations = commit_violations(
            commit(
                author_name="Claude Sonnet",
                committer_email="codex-validation@localhost",
            )
        )
        self.assertEqual(
            {(item.field, item.value) for item in violations},
            {
                ("author name", "Claude Sonnet"),
                ("committer email", "codex-validation@localhost"),
            },
        )

    def test_ai_coauthor_and_contributor_trailers_are_rejected(self) -> None:
        violations = commit_violations(
            commit(
                message=(
                    "Implement feature\n\n"
                    "Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>\n"
                    "Contributor: OpenAI Codex <codex@example.invalid>\n"
                )
            )
        )
        self.assertEqual(len(violations), 2)

    def test_project_author_metadata_is_clean(self) -> None:
        self.assertEqual(project_metadata_violations(), [])

    def test_revision_spec_uses_policy_boundary_for_missing_or_zero_base(self) -> None:
        self.assertEqual(
            revision_spec(None, "HEAD", fallback_base=LEGACY_POLICY_BASELINE),
            f"{LEGACY_POLICY_BASELINE}..HEAD",
        )
        self.assertEqual(
            revision_spec(
                "0" * 40,
                "abc123",
                fallback_base=LEGACY_POLICY_BASELINE,
            ),
            f"{LEGACY_POLICY_BASELINE}..abc123",
        )
        self.assertEqual(
            revision_spec("base", "head", fallback_base="fallback"),
            "base..head",
        )

    @patch("scripts.check_attribution.subprocess.run")
    def test_policy_boundary_tracks_rewritten_introduction(
        self, run: Mock
    ) -> None:
        introduction = "1" * 40
        parent = "2" * 40
        run.side_effect = (
            subprocess.CompletedProcess([], 0, f"{introduction}\n", ""),
            subprocess.CompletedProcess([], 0, f"{parent}\n", ""),
        )
        self.assertEqual(policy_boundary("rewritten-head"), (introduction, parent))

    def test_missing_or_unrelated_push_base_uses_policy_boundary(self) -> None:
        fallback = "f" * 40
        self.assertEqual(
            select_scan_base(None, fallback, base_is_ancestor=False), fallback
        )
        self.assertEqual(
            select_scan_base("0" * 40, fallback, base_is_ancestor=False), fallback
        )
        self.assertEqual(
            select_scan_base("old-lineage", fallback, base_is_ancestor=False),
            fallback,
        )
        self.assertEqual(
            select_scan_base("current-base", fallback, base_is_ancestor=True),
            "current-base",
        )

    def test_workspace_inherited_author_identity_is_inspected(self) -> None:
        manifest = {
            "package": {"authors": {"workspace": True}},
            "workspace": {"package": {"authors": ["OpenAI Codex"]}},
        }
        self.assertEqual(
            cargo_author_values(manifest),
            [("workspace.package.authors", "OpenAI Codex")],
        )

    def test_identity_matching_is_narrow(self) -> None:
        self.assertTrue(contains_forbidden_identity("OpenAI Codex"))
        self.assertTrue(contains_forbidden_identity("claude-code@example.invalid"))
        self.assertTrue(contains_forbidden_identity("Assistant <noreply@anthropic.com>"))
        self.assertFalse(
            contains_forbidden_identity("Claude Shannon <claude@example.invalid>")
        )
        self.assertFalse(contains_forbidden_identity("Alice <alice@openai.com>"))
        self.assertFalse(contains_forbidden_identity("codec validation"))


if __name__ == "__main__":
    unittest.main()
