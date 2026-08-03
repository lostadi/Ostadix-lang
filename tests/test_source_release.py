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
    "evidence/world/g2-aarch64-qemu.toml",
    "evidence/world/g2-aarch64-qemu-2026-08-03.toml",
)
WORLD_EVIDENCE_EVENT_PATHS = {
    "evidence/world/g0-schema-v3-supersession-2026-08-03.toml",
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
        source["path"] for source in _attestation["source"]
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


def fixture_evidence_manifest() -> str:
    lines = [
        "schema_version = 2",
        "required_gate_count = 22",
        "supplemental_gate_count = 1",
        'portable_command = "./boot-and-test.sh smoke"',
        "",
    ]
    for index in range(23):
        required = index < 22
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
            "docs/CLAIMS.md": "fixture claims\n",
            "docs/HOSTED_LIVE_REFERENCE.md": "fixture hosted reference\n",
            "docs/HOSTED_WORLD_REFERENCE_PROFILE.md": WORLD_NORMATIVE_BYTES[
                "docs/HOSTED_WORLD_REFERENCE_PROFILE.md"
            ],
            "docs/OSTADIX_WORLD.md": WORLD_NORMATIVE_BYTES[
                "docs/OSTADIX_WORLD.md"
            ],
            "evidence/gates.toml": fixture_evidence_manifest(),
            "evidence/world_alpha_gates.toml": WORLD_NORMATIVE_BYTES[
                "evidence/world_alpha_gates.toml"
            ],
            "evidence/world_contract_v1.toml": WORLD_NORMATIVE_BYTES[
                "evidence/world_contract_v1.toml"
            ],
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
            "ocore/kernel/boot.S": ".section .text\n",
            "ocore/kernel/aarch64/boot.S": ".section .text\n",
            "ocore/kernel/aarch64/linker.ld": "ENTRY(_start)\n",
            "ocore/kernel/aarch64/vectors.S": ".section .text\n",
            "ocore/kernel/build-aarch64-g2.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/build.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/main.oc": "module kernel::main;\n",
            "ocore/kernel/smoke-world-receipt-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-world-value-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-world-protocol-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-world-identity-qemu.sh": "#!/bin/sh\nexit 0\n",
            "ocore/kernel/smoke-aarch64-g2-qemu.sh": "#!/bin/sh\nexit 0\n",
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
            "ocore/kernel/world_receipt_semantics.oc": (
                "module kernel::world_receipt_semantics;\n"
            ),
            "ocore/kernel/world_receipt_semantics_stub.oc": (
                "module kernel::world_receipt_semantics;\n"
            ),
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
            "scripts/install-o-cli-wrapper.sh": "#!/usr/bin/env bash\n",
            "scripts/o-cli.sh": "#!/usr/bin/env bash\nexec true\n",
            "scripts/smoke-project-hgraph.sh": "#!/usr/bin/env bash\n",
            "scripts/smoke-world-resource-keys.sh": "#!/usr/bin/env bash\n",
            "scripts/smoke-world-g0-conformance.sh": "#!/usr/bin/env bash\n",
            "scripts/release_evidence.py": "#!/usr/bin/env python3\n",
            "scripts/world_alpha_evidence.py": "#!/usr/bin/env python3\n",
            "src/effects.rs": "// fixture governed effect vocabulary\n",
            "src/bin/olangc.rs": "// fixture olangc project planner CLI\n",
            "src/bin/ocorec.rs": "// fixture O-core compiler CLI\n",
            "src/ocore/codegen.rs": "// fixture x86_64 O-core code generator\n",
            "src/ocore/codegen_aarch64.rs": "// fixture AArch64 O-core code generator\n",
            "src/ocore/driver.rs": "// fixture O-core target driver\n",
            "src/ocore/mod.rs": "// fixture O-core module exports\n",
            "src/executor/mod.rs": "// fixture public executor effects surface\n",
            "src/hgraph/graph.rs": "// fixture HGraph validation\n",
            "src/hgraph/kinds.rs": "// fixture HGraph operation vocabulary\n",
            "src/hgraph/from_oir.rs": "// fixture HGraph effect lowering\n",
            "src/project/mod.rs": "pub mod plan;\n",
            "src/project/model.rs": "// fixture project model\n",
            "src/project/plan.rs": "// fixture project HGraph planner\n",
            "src/project/runtime.rs": "// fixture shared project selection\n",
            "src/world/grounding.rs": "// fixture World grounding projection\n",
            "src/world/identity.rs": "// fixture World identities\n",
            "src/world/identity_wire.rs": "// fixture World identity wire oracle\n",
            "src/world/codec.rs": "// fixture World protocol codec oracle\n",
            "src/world/mod.rs": "pub mod identity;\n",
            "src/world/protocol.rs": "// fixture World protocol vocabulary\n",
            "src/world/receipt.rs": "// fixture canonical World receipt vocabulary\n",
            "src/world/receipt_codec.rs": "// fixture canonical World receipt codec\n",
            "src/world/value.rs": "// fixture portable World value vocabulary\n",
            "src/world/value_codec.rs": "// fixture portable World value codec\n",
            "tests/example_manifest.py": "# fixture example manifest consumer\n",
            "tests/fixtures/world_identity_v1.hex": "4f574944454e5431\n",
            "tests/fixtures/project_hgraph/input.txt": "fixture input\n",
            "tests/fixtures/project_hgraph/olang.project.toml": "[project]\nname = \"fixture\"\n",
            "tests/fixtures/world_protocol_v1.hex": "4f5750524f544f31\n",
            "tests/fixtures/world_receipt_v1.hex": "4f57524543454950\n",
            "tests/fixtures/world_value_v1.hex": "4f5756414c554531\n",
            "tests/test_example_manifest.py": "# fixture example manifest tests\n",
            "tests/test_mcp_smoke.py": "# fixture MCP smoke tests\n",
            "tests/test_release_evidence.py": "# fixture release evidence tests\n",
            "tests/test_world_alpha_evidence.py": "# fixture World evidence tests\n",
            "tests/project_hgraph.rs": "#[test] fn project_hgraph_fixture() {}\n",
            "tests/world_resource_keys.rs": "#[test] fn resource_key_fixture() {}\n",
            "tests/world_identity.rs": "#[test] fn identity_fixture() {}\n",
            "tests/world_identity_wire.rs": "#[test] fn wire_fixture() {}\n",
            "tests/world_protocol.rs": "#[test] fn protocol_fixture() {}\n",
            "tests/world_receipt.rs": "#[test] fn receipt_fixture() {}\n",
            "tests/world_value.rs": "#[test] fn value_fixture() {}\n",
        }
        for path in WORLD_EVIDENCE_RELEASE_PATHS:
            contents[path] = (PROJECT_ROOT / path).read_bytes()
        if files:
            contents.update(files)
        for index in range(23):
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
                    "ocore/kernel/build.sh",
                    "ocore/kernel/build-aarch64-g2.sh",
                    "ocore/kernel/smoke-world-receipt-qemu.sh",
                    "ocore/kernel/smoke-world-value-qemu.sh",
                    "ocore/kernel/smoke-world-protocol-qemu.sh",
                    "ocore/kernel/smoke-world-identity-qemu.sh",
                    "ocore/kernel/smoke-aarch64-g2-qemu.sh",
                    "scripts/o-cli.sh",
                    "scripts/install-o-cli-wrapper.sh",
                    "scripts/smoke-project-hgraph.sh",
                    "scripts/smoke-world-resource-keys.sh",
                    "scripts/smoke-world-g0-conformance.sh",
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
                "docs/CLAIMS.md",
                "docs/HOSTED_LIVE_REFERENCE.md",
                "docs/HOSTED_WORLD_REFERENCE_PROFILE.md",
                "docs/OSTADIX_WORLD.md",
                "evidence/gates.toml",
                "evidence/world_alpha_gates.toml",
                "evidence/world_contract_v1.toml",
                "evidence/world/g0-repository-conformance.toml",
                "evidence/world/g0-repository-conformance-2026-08-03.toml",
                "evidence/world/g0-schema-v3-supersession-2026-08-03.toml",
                "evidence/world/g2-aarch64-qemu.toml",
                "evidence/world/g2-aarch64-qemu-2026-08-03.toml",
                "evidence/world/g2-counter-wording-supersession-2026-08-03.toml",
                "evidence/world/transcripts/g0-repository-conformance.log",
                "evidence/world/transcripts/g0-repository-conformance-2026-08-03.log",
                "evidence/world/transcripts/g2-aarch64-qemu.log",
                "evidence/world/transcripts/g2-aarch64-qemu-2026-08-03.log",
                "examples/manifest.json",
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
                "ocore/kernel/build.sh",
                "ocore/kernel/main.oc",
                "ocore/kernel/smoke-world-receipt-qemu.sh",
                "ocore/kernel/smoke-world-value-qemu.sh",
                "ocore/kernel/smoke-world-protocol-qemu.sh",
                "ocore/kernel/smoke-world-identity-qemu.sh",
                "ocore/kernel/smoke-aarch64-g2-qemu.sh",
                "ocore/kernel/world_protocol_semantics.oc",
                "ocore/kernel/world_protocol_semantics_stub.oc",
                "ocore/kernel/world_value_semantics.oc",
                "ocore/kernel/world_value_semantics_stub.oc",
                "ocore/kernel/world_identity_semantics.oc",
                "ocore/kernel/world_identity_semantics_stub.oc",
                "ocore/kernel/world_receipt_semantics.oc",
                "ocore/kernel/world_receipt_semantics_stub.oc",
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
                "scripts/install-o-cli-wrapper.sh",
                "scripts/o-cli.sh",
                "scripts/smoke-project-hgraph.sh",
                "scripts/smoke-world-resource-keys.sh",
                "scripts/smoke-world-g0-conformance.sh",
                "scripts/release_evidence.py",
                "scripts/world_alpha_evidence.py",
                "src/effects.rs",
                "src/bin/olangc.rs",
                "src/bin/ocorec.rs",
                "src/ocore/codegen.rs",
                "src/ocore/codegen_aarch64.rs",
                "src/ocore/driver.rs",
                "src/ocore/mod.rs",
                "src/executor/mod.rs",
                "src/hgraph/graph.rs",
                "src/hgraph/kinds.rs",
                "src/hgraph/from_oir.rs",
                "src/project/mod.rs",
                "src/project/model.rs",
                "src/project/plan.rs",
                "src/project/runtime.rs",
                "src/world/grounding.rs",
                "src/world/identity.rs",
                "src/world/identity_wire.rs",
                "src/world/codec.rs",
                "src/world/mod.rs",
                "src/world/protocol.rs",
                "src/world/receipt.rs",
                "src/world/receipt_codec.rs",
                "src/world/value.rs",
                "src/world/value_codec.rs",
                "tests/example_manifest.py",
                "tests/fixtures/world_identity_v1.hex",
                "tests/fixtures/project_hgraph/input.txt",
                "tests/fixtures/project_hgraph/olang.project.toml",
                "tests/fixtures/world_protocol_v1.hex",
                "tests/fixtures/world_receipt_v1.hex",
                "tests/fixtures/world_value_v1.hex",
                "tests/test_example_manifest.py",
                "tests/test_mcp_smoke.py",
                "tests/test_release_evidence.py",
                "tests/test_world_alpha_evidence.py",
                "tests/project_hgraph.rs",
                "tests/world_resource_keys.rs",
                "tests/world_identity.rs",
                "tests/world_identity_wire.rs",
                "tests/world_protocol.rs",
                "tests/world_receipt.rs",
                "tests/world_value.rs",
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
                for index in range(23)
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
            "src/ocore/codegen_aarch64.rs",
            "src/ocore/driver.rs",
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
            "src/ocore/codegen_aarch64.rs",
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
            "src/world/identity.rs",
            "src/world/identity_wire.rs",
            "tests/fixtures/world_identity_v1.hex",
            "tests/world_identity_wire.rs",
        )
        self._git("commit", "-q", "-m", "remove World identity surface")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): .*smoke-world-identity-qemu\.sh.*"
            r"ocore/world/identity\.oc.*src/world/identity\.rs.*identity_wire\.rs.*"
            r"world_identity_v1\.hex.*world_identity_wire\.rs",
        ):
            self._build("missing-world-identity.zip")

    def test_world_protocol_cross_language_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "ocore/kernel/smoke-world-protocol-qemu.sh",
            "ocore/kernel/world_protocol_semantics.oc",
            "ocore/kernel/world_protocol_semantics_stub.oc",
            "ocore/world/codec.oc",
            "ocore/world/protocol.oc",
            "src/world/codec.rs",
            "src/world/protocol.rs",
            "tests/fixtures/world_protocol_v1.hex",
            "tests/world_protocol.rs",
        )
        self._git("commit", "-q", "-m", "remove World protocol surface")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): .*smoke-world-protocol-qemu\.sh.*"
            r"world_protocol_semantics\.oc.*world_protocol_semantics_stub\.oc.*"
            r"ocore/world/codec\.oc.*ocore/world/protocol\.oc.*"
            r"src/world/codec\.rs.*src/world/protocol\.rs.*"
            r"world_protocol_v1\.hex.*world_protocol\.rs",
        ):
            self._build("missing-world-protocol.zip")

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
            "src/world/value.rs",
            "src/world/value_codec.rs",
            "tests/fixtures/world_value_v1.hex",
            "tests/world_value.rs",
        )
        self._git("commit", "-q", "-m", "remove World value surface")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): .*smoke-world-value-qemu\.sh.*"
            r"world_value_semantics\.oc.*world_value_semantics_stub\.oc.*"
            r"ocore/world/sha256\.oc.*ocore/world/value\.oc.*"
            r"ocore/world/value_codec\.oc.*src/world/value\.rs.*"
            r"src/world/value_codec\.rs.*world_value_v1\.hex.*world_value\.rs",
        ):
            self._build("missing-world-value.zip")

    def test_world_receipt_cross_language_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "ocore/kernel/smoke-world-receipt-qemu.sh",
            "ocore/kernel/world_receipt_semantics.oc",
            "ocore/kernel/world_receipt_semantics_stub.oc",
            "ocore/world/receipt.oc",
            "ocore/world/receipt_codec.oc",
            "src/world/receipt.rs",
            "src/world/receipt_codec.rs",
            "tests/fixtures/world_receipt_v1.hex",
            "tests/world_receipt.rs",
        )
        self._git("commit", "-q", "-m", "remove World receipt surface")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): .*smoke-world-receipt-qemu\.sh.*"
            r"world_receipt_semantics\.oc.*world_receipt_semantics_stub\.oc.*"
            r"ocore/world/receipt\.oc.*ocore/world/receipt_codec\.oc.*"
            r"src/world/receipt\.rs.*src/world/receipt_codec\.rs.*"
            r"world_receipt_v1\.hex.*world_receipt\.rs",
        ):
            self._build("missing-world-receipt.zip")

    def test_world_resource_key_hosted_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "scripts/smoke-world-resource-keys.sh",
            "src/effects.rs",
            "src/executor/mod.rs",
            "src/hgraph/from_oir.rs",
            "src/world/grounding.rs",
            "tests/world_resource_keys.rs",
        )
        self._git("commit", "-q", "-m", "remove World ResourceKey surface")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): .*smoke-world-resource-keys\.sh.*"
            r"src/effects\.rs.*src/executor/mod\.rs.*src/hgraph/from_oir\.rs.*"
            r"src/world/grounding\.rs.*tests/world_resource_keys\.rs",
        ):
            self._build("missing-world-resource-keys.zip")

    def test_project_hgraph_hosted_surface_is_required(self) -> None:
        self._commit()
        self._git(
            "rm",
            "scripts/o-cli.sh",
            "scripts/install-o-cli-wrapper.sh",
            "scripts/smoke-project-hgraph.sh",
            "src/bin/olangc.rs",
            "src/hgraph/graph.rs",
            "src/hgraph/kinds.rs",
            "src/project/mod.rs",
            "src/project/model.rs",
            "src/project/plan.rs",
            "src/project/runtime.rs",
            "tests/fixtures/project_hgraph/input.txt",
            "tests/fixtures/project_hgraph/olang.project.toml",
            "tests/project_hgraph.rs",
        )
        self._git("commit", "-q", "-m", "remove hosted project HGraph surface")

        with self.assertRaisesRegex(
            release.ReleaseError,
            r"missing required path\(s\): .*install-o-cli-wrapper\.sh.*o-cli\.sh.*smoke-project-hgraph\.sh.*"
            r"src/bin/olangc\.rs.*src/hgraph/graph\.rs.*src/hgraph/kinds\.rs.*"
            r"src/project/mod\.rs.*src/project/model\.rs.*src/project/plan\.rs.*"
            r"src/project/runtime\.rs.*project_hgraph/input\.txt.*"
            r"project_hgraph/olang\.project\.toml.*tests/project_hgraph\.rs",
        ):
            self._build("missing-project-hgraph.zip")

    def test_world_normative_bytes_are_sealed_before_packaging(self) -> None:
        for path, data in WORLD_NORMATIVE_BYTES.items():
            with self.subTest(path=path):
                self._commit({path: data + b"\n"})
                with self.assertRaises(release.ReleaseError) as raised:
                    self._build(f"tampered-world-{Path(path).name}.zip")
                message = str(raised.exception)
                self.assertIn(path, message)
                self.assertIn("SHA-256 differs from sealed World Alpha v2 bytes", message)

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
                self.assertIn("SHA-256 differs from sealed World Alpha v2 bytes", message)

    def test_world_attestation_rejects_transcript_tamper(self) -> None:
        path = "evidence/world/transcripts/g2-aarch64-qemu.log"
        self._commit({path: (PROJECT_ROOT / path).read_bytes() + b"tamper\n"})
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"g2-aarch64-qemu\.toml\.transcript digest does not match",
        ):
            self._build("tampered-g2-transcript.zip")

    def test_world_attestation_rejects_source_byte_tamper(self) -> None:
        path = "src/ocore/codegen_aarch64.rs"
        self._commit({path: (PROJECT_ROOT / path).read_bytes() + b"\n"})
        with self.assertRaisesRegex(
            release.ReleaseError,
            r"g2-aarch64-qemu-2026-08-03\.toml\.source\[[0-9]+\].*"
            r"does not match released src/ocore/codegen_aarch64\.rs",
        ):
            self._build("tampered-g2-source.zip")

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
            release.ReleaseError, r"required_gate_count must be 22"
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
            release.ReleaseError, r"required_gate_count must be 22"
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
