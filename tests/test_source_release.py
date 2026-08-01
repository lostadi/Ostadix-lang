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
import unittest
import zipfile


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "build_source_release.py"
SPEC = importlib.util.spec_from_file_location("ostadix_source_release", SCRIPT)
if SPEC is None or SPEC.loader is None:  # pragma: no cover - import failure is fatal
    raise RuntimeError(f"cannot import {SCRIPT}")
release = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = release
SPEC.loader.exec_module(release)

EVIDENCE_SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "release_evidence.py"
EVIDENCE_SPEC = importlib.util.spec_from_file_location(
    "ostadix_release_evidence", EVIDENCE_SCRIPT
)
if EVIDENCE_SPEC is None or EVIDENCE_SPEC.loader is None:  # pragma: no cover
    raise RuntimeError(f"cannot import {EVIDENCE_SCRIPT}")
evidence_tool = importlib.util.module_from_spec(EVIDENCE_SPEC)
sys.modules[EVIDENCE_SPEC.name] = evidence_tool
EVIDENCE_SPEC.loader.exec_module(evidence_tool)


def fixture_evidence_manifest() -> str:
    lines = [
        "schema_version = 1",
        "required_gate_count = 17",
        "supplemental_gate_count = 1",
        'portable_command = "./boot-and-test.sh smoke"',
        "",
    ]
    for index in range(18):
        required = index < 17
        evidence_class = "portable_tcg" if required else "hardware_kvm"
        lines.extend(
            [
                "[[gate]]",
                f'id = "fixture-gate-{index:02}"',
                f"required = {'true' if required else 'false'}",
                'milestone = "fixture"',
                f'script = "ocore/kernel/fixture-evidence-{index:02}.sh"',
                f'evidence_class = "{evidence_class}"',
                'required_tools = ["bash"]',
                f'positive_claims = ["fixture claim {index:02}"]',
                f'nonclaims = ["fixture nonclaim {index:02}"]',
                f'expected_markers = ["FIXTURE {index:02} START", "FIXTURE {index:02} PASS"]',
                "",
            ]
        )
    return "\n".join(lines)


ZERO_GATE_EVIDENCE_MANIFEST = """\
schema_version = 1
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


class SourceReleaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self._git("init", "-q")
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

    def _commit(self, files: dict[str, str | bytes] | None = None) -> str:
        contents = {
            ".mcp.json": (
                '{"mcpServers":{"ostadix":{"command":"ostadix-mcp","args":[]}}}\n'
            ),
            "Cargo.toml": "[package]\nname = \"release-fixture\"\nversion = \"0.1.0\"\n",
            "LICENSE": "GNU Lesser General Public License version 2.1\n",
            "README.md": "committed [index](llms.txt)\n",
            "boot-and-test.sh": "#!/bin/sh\nexit 0\n",
            "evidence/gates.toml": fixture_evidence_manifest(),
            "examples/manifest.json": '{"schema_version": 1, "examples": []}\n',
            "llms.txt": "release index\n",
            "mcp/ostadix_lang_mcp_server/Cargo.lock": "# fixture lock\n",
            "mcp/ostadix_lang_mcp_server/Cargo.toml": (
                "[package]\n"
                'name = "ostadix-mcp-server"\n'
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
            "scripts/smoke_ostadix_mcp.py": "#!/usr/bin/env python3\n",
            "scripts/release_evidence.py": "#!/usr/bin/env python3\n",
            "tests/example_manifest.py": "# fixture example manifest consumer\n",
            "tests/test_example_manifest.py": "# fixture example manifest tests\n",
            "tests/test_mcp_smoke.py": "# fixture MCP smoke tests\n",
        }
        if files:
            contents.update(files)
        for index in range(18):
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
                    "okernel-multikernel/boot-and-test.sh",
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
                            }
                        ],
                    },
                    sort_keys=True,
                )
                + "\n",
                "examples/generated.html": "<p>generated</p>\n",
                "fuzz/fuzz_targets/parser.rs": "fn main() {}\n",
                "ocore/kernel/main.oc": "module kernel::main;\n",
                "one-off.patch": "diff --git a/a b/a\n",
                "okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md": "published proposal\n",
                "scratch.txt": "not an allowlisted top-level surface\n",
                "scripts/tool.sh": "#!/bin/sh\nexit 0\n",
                "src/lib.rs": "pub fn fixture() {}\n",
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
                ".mcp.json",
                "Cargo.toml",
                "LICENSE",
                "README.md",
                "boot-and-test.sh",
                "evidence/gates.toml",
                "examples/manifest.json",
                "llms.txt",
                "mcp/ostadix_lang_mcp_server/Cargo.lock",
                "mcp/ostadix_lang_mcp_server/Cargo.toml",
                "mcp/ostadix_lang_mcp_server/README.md",
                "mcp/ostadix_lang_mcp_server/src/main.rs",
                "okernel-multikernel/boot-and-test.sh",
                "okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md",
                "scripts/smoke_ostadix_mcp.py",
                "scripts/release_evidence.py",
                "tests/example_manifest.py",
                "tests/test_example_manifest.py",
                "tests/test_mcp_smoke.py",
                ".github/workflows/ci.yml",
                "assets/logo.bin",
                "backends/shim.py",
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
                for index in range(18)
            )
            excluded = {
                ".DS_Store",
                ".ocore-repair-backups/run/typeck.rs",
                "backends/__pycache__/shim.pyc",
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
            self.assertEqual(
                modes["okernel-multikernel/boot-and-test.sh"], "100755"
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
                "README.md": "[missing guide](docs/missing.md)\n",
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
                    "[guide](docs/guide.md#usage) [docs](docs/) "
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
                    "[missing guide][guide]\n"
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
                    data = b"[missing](docs/absent-after-packaging.md)\n"
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

    def test_root_license_is_a_required_release_member(self) -> None:
        self._commit()
        self._git("rm", "LICENSE")
        self._git("commit", "-q", "-m", "remove release license")

        with self.assertRaisesRegex(
            release.ReleaseError, r"missing required path\(s\): LICENSE"
        ):
            self._build("missing-license.zip")

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

    def test_zero_gate_evidence_manifest_is_rejected_before_packaging(self) -> None:
        self._commit({"evidence/gates.toml": ZERO_GATE_EVIDENCE_MANIFEST})
        with self.assertRaisesRegex(
            release.ReleaseError, r"required_gate_count must be 17"
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
            release.ReleaseError, r"required_gate_count must be 17"
        ):
            release.verify_archive(tampered)

    def test_same_commit_produces_byte_identical_zip_across_ref_aliases(self) -> None:
        commit = self._commit(
            {
                "assets/data.bin": b"\x00\x01\x02",
                "scripts/release-helper.sh": "#!/bin/sh\nexit 0\n",
                "src/lib.rs": "pub const ANSWER: u8 = 42;\n",
            }
        )
        first = self._build("first.zip", ref="HEAD")
        second = self._build("second.zip", ref=commit)
        self.assertEqual(first.prefix, second.prefix)
        self.assertEqual(first.archive_sha256, second.archive_sha256)
        self.assertEqual(first.output.read_bytes(), second.output.read_bytes())

    def test_dirty_tree_requires_override_and_override_uses_commit_bytes(self) -> None:
        self._commit({"src/lib.rs": "pub fn clean() {}\n"})
        self._write("README.md", "uncommitted readme\n")
        self._write("untracked.txt", "untracked\n")

        with self.assertRaisesRegex(release.ReleaseError, "working tree is dirty"):
            self._build("refused.zip")

        result = self._build("allowed.zip", allow_dirty=True)
        manifest = release.verify_archive(result.output)
        with zipfile.ZipFile(result.output) as archive:
            payload = archive.read(f"{manifest['prefix']}/README.md")
            self.assertEqual(payload, b"committed [index](llms.txt)\n")
            self.assertNotIn(
                f"{manifest['prefix']}/untracked.txt", set(archive.namelist())
            )

    def test_verifier_rejects_payload_tampering(self) -> None:
        self._commit({"src/lib.rs": "pub fn intact() {}\n"})
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
