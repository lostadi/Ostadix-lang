#!/usr/bin/env python3

from __future__ import annotations

import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CHECKER = ROOT / "scripts" / "check_architecture_boundaries.py"
MANIFEST = ROOT / "ci" / "architecture-roots.toml"


def run_checker(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(CHECKER), "--root", str(root)],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def write_minimal_tree(root: Path) -> None:
    for relative in (
        "crates/ostadix-api/src/parser.rs",
        "crates/ostadix-api/src/syntax_dialect.rs",
        "crates/ostadix-api/src/ir.rs",
        "crates/ostadix-api/src/backend.rs",
        "crates/ostadix-api/src/backend_catalog.rs",
        "crates/ostadix-api/src/backend_state.rs",
        "crates/ostadix-api/src/capability.rs",
        "crates/ostadix-api/src/environment.rs",
        "crates/ostadix-api/src/execution_contract.rs",
        "crates/ostadix-api/src/eval_core.rs",
        "crates/ostadix-api/src/effects.rs",
        "crates/ostadix-api/src/value.rs",
        "crates/ostadix-api/src/dispatch_model.rs",
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
        "crates/ostadix-api/src/registry/bundle/mod.rs",
        "crates/ostadix-api/src/registry/model.rs",
        "crates/ostadix-api/src/registry/placement_compat.rs",
        "crates/ostadix-api/src/registry/store.rs",
        "crates/ostadix-api/src/eval.rs",
        "crates/ostadix-api/src/executor/actor.rs",
        "crates/ostadix-api/src/executor/cancellation.rs",
        "crates/ostadix-api/src/executor/coordinator.rs",
        "crates/ostadix-api/src/executor/effects.rs",
        "crates/ostadix-api/src/executor/mod.rs",
        "crates/ostadix-api/src/executor/parallel.rs",
        "crates/ostadix-api/src/executor/pool.rs",
        "crates/ostadix-api/src/executor/task.rs",
        "crates/ostadix-api/src/executor/trace.rs",
        "crates/ostadix-api/src/runtime_exec.rs",
        "crates/ostadix-api/src/process.rs",
        "crates/ostadix-api/src/wire.rs",
        "crates/ostadix-api/src/evidence/admit.rs",
        "crates/ostadix-api/src/evidence/analyze.rs",
        "crates/ostadix-api/src/evidence/fact.rs",
        "crates/ostadix-api/src/evidence/intent.rs",
        "crates/ostadix-api/src/evidence/mod.rs",
        "crates/ostadix-api/src/evidence/profile.rs",
        "crates/ostadix-api/src/world/grounding.rs",
    ):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("pub struct Boundary;\n", encoding="utf-8")

    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    for root_spec in manifest["root"]:
        path = (
            root
            / manifest["production"]["source_root"]
            / f"{root_spec['name']}.rs"
        )
        if not path.exists():
            path.write_text("pub struct Boundary;\n", encoding="utf-8")

    # The manifest validates compatibility facades as real owner-to-target
    # edges. Keep those edges in every synthetic repository as well.
    facade_sources = {
        "crates/ostadix-api/src/backend.rs": "pub use crate::backend_state as state;\n",
        "crates/ostadix-api/src/placement/mod.rs": (
            "pub mod protocol { pub use crate::placement_protocol::*; }\n"
        ),
        "crates/ostadix-api/src/registry.rs": "pub mod bundle;\n",
        "crates/ostadix-api/src/registry/bundle/mod.rs": "pub use crate::backend_catalog::*;\n",
        "crates/ostadix-api/src/world/mod.rs": "pub use crate::resource_identity as identity;\n",
    }
    for relative, source in facade_sources.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source, encoding="utf-8")

    for relative in (
        "crates/ostadix-api/src/backend_catalog.inc.rs",
        "crates/ostadix-api/src/lib.rs",
        "crates/ostadix-api/src/world/identity.rs",
    ):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists():
            path.write_text("pub struct Boundary;\n", encoding="utf-8")
    source_prefix = manifest["production"]["source_root"] + "/"
    override_declarations = {
        entry["module_path"][0]: (
            entry["path"].removeprefix(source_prefix)
            if entry["kind"] == "file"
            else f"{entry['path'].removeprefix(source_prefix)}/mod.rs"
        )
        for entry in manifest["physical_override"]
    }
    crate_root_lines = []
    for root_spec in manifest["root"]:
        name = root_spec["name"]
        if name in override_declarations:
            crate_root_lines.append(f'#[path = "{override_declarations[name]}"]')
        crate_root_lines.append(f"pub mod {name};")
    (root / "crates/ostadix-api/src/lib.rs").write_text(
        "\n".join(crate_root_lines) + "\n", encoding="utf-8"
    )
    fragment_owner = root / "crates/ostadix-api/src/backend_catalog/fragment_owner.rs"
    fragment_owner.parent.mkdir(parents=True, exist_ok=True)
    fragment_owner.write_text(
        'include!("../backend_catalog.inc.rs");\n', encoding="utf-8"
    )
    engine_manifest = root / manifest["production"]["package_manifest"]
    engine_manifest.parent.mkdir(parents=True, exist_ok=True)
    engine_manifest.write_text(
        '[package]\nname = "ostadix-api"\nversion = "0.0.0"\n',
        encoding="utf-8",
    )
    (root / "src/bin").mkdir(parents=True, exist_ok=True)
    (root / "src/main.rs").write_text("fn main() {}\n", encoding="utf-8")
    public_roots = [root_spec["name"] for root_spec in manifest["root"]]
    (root / "src/lib.rs").write_text(
        "pub use ostadix_api::{" + ", ".join(public_roots) + "};\n",
        encoding="utf-8",
    )
    (root / "Cargo.toml").write_text(
        '[package]\nname = "o-lang"\nversion = "0.0.0"\n'
        '\n[dependencies]\nostadix-api = { path = "crates/ostadix-api", '
        'version = "=0.0.0" }\n',
        encoding="utf-8",
    )
    (root / "ci").mkdir(parents=True, exist_ok=True)
    (root / "ci/architecture-roots.toml").write_text(
        MANIFEST.read_text(encoding="utf-8").replace(
            'included_from = "crates/ostadix-api/src/backend_catalog.rs"',
            'included_from = "crates/ostadix-api/src/backend_catalog/fragment_owner.rs"',
            1,
        ),
        encoding="utf-8",
    )


class ArchitectureBoundaryTests(unittest.TestCase):
    def test_current_tree_respects_frozen_boundaries(self) -> None:
        result = run_checker(ROOT)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout,
            "architecture dependency boundaries: PASS "
            "(159 production files, 42 roots, 193 cross-root edges)\n",
        )

    def test_manifest_inventories_every_current_root_edge_override_and_facade(self) -> None:
        manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
        roots = manifest["root"]
        self.assertEqual(
            manifest["production"]["package_manifest"],
            "crates/ostadix-api/Cargo.toml",
        )
        self.assertEqual(manifest["production"]["crate_root"], "crates/ostadix-api/src/lib.rs")
        self.assertEqual(
            manifest["compiled_fragment"],
            [
                {
                    "path": "crates/ostadix-api/src/backend_catalog.inc.rs",
                    "owner": "backend_catalog",
                    "included_from": "crates/ostadix-api/src/backend_catalog.rs",
                }
            ],
        )
        self.assertEqual(len(roots), 42)
        self.assertEqual(
            sum(len(root["allowed_dependencies"]) for root in roots), 193
        )
        api_root = next(root for root in roots if root["name"] == "api")
        self.assertIn("ir", api_root["allowed_dependencies"])
        provenance_root = next(
            root for root in roots if root["name"] == "information_provenance"
        )
        self.assertEqual(provenance_root["layer"], 13)
        self.assertEqual(
            provenance_root["allowed_dependencies"],
            ["evidence", "information", "ir", "world"],
        )
        intent_root = next(root for root in roots if root["name"] == "intent")
        self.assertEqual(intent_root["layer"], 14)
        self.assertEqual(
            intent_root["allowed_dependencies"],
            [
                "backend_catalog",
                "canonical_cbor",
                "eval",
                "evidence",
                "execution_contract",
                "hgraph",
                "hosted_remote",
                "ir",
                "parser",
                "project",
                "runtime_exec",
                "value",
            ],
        )
        self.assertEqual(
            {
                (entry["path"], entry["kind"], tuple(entry["module_path"]))
                for entry in manifest["physical_override"]
            },
            {
                (
                    "crates/ostadix-api/src/placement/protocol",
                    "directory",
                    ("placement_protocol",),
                ),
                ("crates/ostadix-api/src/world/identity.rs", "file", ("resource_identity",)),
            },
        )
        self.assertEqual(
            {(entry["path"], entry["kind"]) for entry in manifest["facade"]},
            {
                ("backend::state", "alias"),
                ("placement::protocol", "inline_module"),
                ("registry::bundle", "module"),
                ("world::identity", "alias"),
            },
        )

    def test_physical_overrides_must_map_one_crate_root(self) -> None:
        replacements = (
            (
                'module_path = ["placement_protocol"]',
                'module_path = ["placement_protocol", "nested"]',
            ),
            (
                'module_path = ["resource_identity"]',
                'module_path = ["resource_identity", "nested"]',
            ),
        )
        for original, replacement in replacements:
            with self.subTest(replacement=replacement):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    manifest_path = root / "ci/architecture-roots.toml"
                    manifest_path.write_text(
                        manifest_path.read_text(encoding="utf-8").replace(
                            original, replacement, 1
                        ),
                        encoding="utf-8",
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(
                    "module_path must contain exactly one Rust root identifier",
                    result.stderr,
                )

    def test_physical_overrides_cannot_overlap_production_exclusions(self) -> None:
        overrides = (
            ("crates/ostadix-api/src/lib.rs", "file"),
            ("crates/ostadix-api/src/backend_catalog.inc.rs", "file"),
        )
        for path, kind in overrides:
            with self.subTest(path=path, kind=kind):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    manifest_path = root / "ci/architecture-roots.toml"
                    with manifest_path.open("a", encoding="utf-8") as manifest:
                        manifest.write(
                            "\n[[physical_override]]\n"
                            f'path = "{path}"\n'
                            f'kind = "{kind}"\n'
                            'module_path = ["api"]\n'
                        )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(
                    "path overlaps an excluded production source", result.stderr
                )

    def test_physical_directory_override_requires_mod_rs_entrypoint(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/ghost_api").mkdir()
            manifest_path = root / "ci/architecture-roots.toml"
            with manifest_path.open("a", encoding="utf-8") as manifest:
                manifest.write(
                    "\n[[physical_override]]\n"
                    'path = "crates/ostadix-api/src/ghost_api"\n'
                    'kind = "directory"\n'
                    'module_path = ["api"]\n'
                )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "has no regular non-symlink mod.rs entrypoint", result.stderr
        )

    def test_physical_overrides_cannot_duplicate_conventional_module_ownership(
        self,
    ) -> None:
        cases = (
            ("crates/ostadix-api/src/evidence/fact.rs", "file", "fact", "evidence/fact.rs"),
            (
                "crates/ostadix-api/src/evidence/nested",
                "directory",
                "nested",
                "evidence/nested/mod.rs",
            ),
        )
        for override_path, kind, child, crate_path in cases:
            with self.subTest(kind=kind):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/evidence.rs").unlink()
                    (root / "crates/ostadix-api/src/evidence/mod.rs").write_text(
                        f"pub mod {child};\n", encoding="utf-8"
                    )
                    entrypoint = root / override_path
                    if kind == "directory":
                        entrypoint /= "mod.rs"
                    entrypoint.parent.mkdir(parents=True, exist_ok=True)
                    entrypoint.write_text(
                        "use crate::version::Boundary;\n", encoding="utf-8"
                    )
                    manifest_path = root / "ci/architecture-roots.toml"
                    with manifest_path.open("a", encoding="utf-8") as manifest:
                        manifest.write(
                            "\n[[physical_override]]\n"
                            f'path = "{override_path}"\n'
                            f'kind = "{kind}"\n'
                            'module_path = ["api"]\n'
                        )
                    crate_root = root / "crates/ostadix-api/src/lib.rs"
                    crate_root.write_text(
                        crate_root.read_text(encoding="utf-8").replace(
                            "pub mod api;",
                            f'#[path = "{crate_path}"]\npub mod api;',
                            1,
                        ),
                        encoding="utf-8",
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(
                    f"also has conventional module ownership through "
                    f"`crates/ostadix-api/src/evidence/mod.rs` (`mod {child};`)",
                    result.stderr,
                )

    def test_physical_file_overrides_cannot_declare_external_children(self) -> None:
        for suffix in ("rs", "source"):
            with self.subTest(suffix=suffix):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    override_path = root / f"crates/ostadix-api/src/world/identity.{suffix}"
                    if suffix != "rs":
                        (root / "crates/ostadix-api/src/world/identity.rs").rename(override_path)
                        manifest_path = root / "ci/architecture-roots.toml"
                        manifest_path.write_text(
                            manifest_path.read_text(encoding="utf-8").replace(
                                'path = "crates/ostadix-api/src/world/identity.rs"',
                                f'path = "crates/ostadix-api/src/world/identity.{suffix}"',
                                1,
                            ),
                            encoding="utf-8",
                        )
                        crate_root = root / "crates/ostadix-api/src/lib.rs"
                        crate_root.write_text(
                            crate_root.read_text(encoding="utf-8").replace(
                                'path = "world/identity.rs"',
                                f'path = "world/identity.{suffix}"',
                                1,
                            ),
                            encoding="utf-8",
                        )
                    override_path.write_text("pub mod child;\n", encoding="utf-8")
                    child = root / "crates/ostadix-api/src/world/identity/child.rs"
                    child.parent.mkdir(parents=True, exist_ok=True)
                    child.write_text(
                        "use crate::effects::Boundary;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(
                    f"physical file override `crates/ostadix-api/src/world/identity.{suffix}` "
                    "declares external modules ['child']",
                    result.stderr,
                )

    def test_inline_modules_cannot_declare_external_source_children(self) -> None:
        cases = (
            ("crates/ostadix-api/src/evidence/outer/fact.rs", "file", "fact", "evidence/outer/fact.rs"),
            (
                "crates/ostadix-api/src/evidence/outer/nested",
                "directory",
                "nested",
                "evidence/outer/nested/mod.rs",
            ),
        )
        for override_path, kind, child, crate_path in cases:
            with self.subTest(kind=kind):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/evidence.rs").unlink()
                    (root / "crates/ostadix-api/src/evidence/mod.rs").write_text(
                        f"pub mod outer {{ pub mod {child}; }}\n", encoding="utf-8"
                    )
                    entrypoint = root / override_path
                    if kind == "directory":
                        entrypoint /= "mod.rs"
                    entrypoint.parent.mkdir(parents=True, exist_ok=True)
                    entrypoint.write_text(
                        "use crate::version::Boundary;\n", encoding="utf-8"
                    )
                    manifest_path = root / "ci/architecture-roots.toml"
                    with manifest_path.open("a", encoding="utf-8") as manifest:
                        manifest.write(
                            "\n[[physical_override]]\n"
                            f'path = "{override_path}"\n'
                            f'kind = "{kind}"\n'
                            'module_path = ["api"]\n'
                        )
                    crate_root = root / "crates/ostadix-api/src/lib.rs"
                    crate_root.write_text(
                        crate_root.read_text(encoding="utf-8").replace(
                            "pub mod api;",
                            f'#[path = "{crate_path}"]\npub mod api;',
                            1,
                        ),
                        encoding="utf-8",
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(
                    f"external module `{child}`", result.stderr
                )
                self.assertIn(
                    "is nested inside an inline module", result.stderr
                )

    def test_block_items_cannot_declare_external_source_children(self) -> None:
        cases = (
            ("crates/ostadix-api/src/evidence/outer/fact.rs", "file", "fact", "evidence/outer/fact.rs"),
            (
                "crates/ostadix-api/src/evidence/outer/nested",
                "directory",
                "nested",
                "evidence/outer/nested/mod.rs",
            ),
        )
        for override_path, kind, child, crate_path in cases:
            with self.subTest(kind=kind):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/evidence.rs").unlink()
                    (root / "crates/ostadix-api/src/evidence/mod.rs").write_text(
                        f"pub fn container() {{ mod outer {{ pub mod {child}; }} }}\n",
                        encoding="utf-8",
                    )
                    entrypoint = root / override_path
                    if kind == "directory":
                        entrypoint /= "mod.rs"
                    entrypoint.parent.mkdir(parents=True, exist_ok=True)
                    entrypoint.write_text(
                        "use crate::version::Boundary;\n", encoding="utf-8"
                    )
                    manifest_path = root / "ci/architecture-roots.toml"
                    with manifest_path.open("a", encoding="utf-8") as manifest:
                        manifest.write(
                            "\n[[physical_override]]\n"
                            f'path = "{override_path}"\n'
                            f'kind = "{kind}"\n'
                            'module_path = ["api"]\n'
                        )
                    crate_root = root / "crates/ostadix-api/src/lib.rs"
                    crate_root.write_text(
                        crate_root.read_text(encoding="utf-8").replace(
                            "pub mod api;",
                            f'#[path = "{crate_path}"]\npub mod api;',
                            1,
                        ),
                        encoding="utf-8",
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(f"external module `{child}`", result.stderr)
                self.assertIn("inline module or block", result.stderr)

    def test_delimiter_wrappers_cannot_hide_nested_module_blocks(self) -> None:
        sources = (
            "pub const X: () = ({ mod outer { pub mod fact; } });\n",
            "pub static X: [(); 1] = [{ mod outer { pub mod fact; } }];\n",
            "pub fn f() { consume({ mod outer { pub mod fact; } }); }\n",
        )
        for source in sources:
            with self.subTest(source=source):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/evidence.rs").unlink()
                    (root / "crates/ostadix-api/src/evidence/mod.rs").write_text(
                        source, encoding="utf-8"
                    )
                    target = root / "crates/ostadix-api/src/evidence/outer/fact.rs"
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text(
                        "use crate::version::Boundary;\n", encoding="utf-8"
                    )
                    manifest_path = root / "ci/architecture-roots.toml"
                    with manifest_path.open("a", encoding="utf-8") as manifest:
                        manifest.write(
                            "\n[[physical_override]]\n"
                            'path = "crates/ostadix-api/src/evidence/outer/fact.rs"\n'
                            'kind = "file"\n'
                            'module_path = ["api"]\n'
                        )
                    crate_root = root / "crates/ostadix-api/src/lib.rs"
                    crate_root.write_text(
                        crate_root.read_text(encoding="utf-8").replace(
                            "pub mod api;",
                            '#[path = "evidence/outer/fact.rs"]\npub mod api;',
                            1,
                        ),
                        encoding="utf-8",
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("external module `fact`", result.stderr)
                self.assertIn("inline module or block", result.stderr)

    def test_macro_tokens_cannot_generate_physical_module_ownership(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/evidence.rs").unlink()
            (root / "crates/ostadix-api/src/evidence/mod.rs").write_text(
                "macro_rules! declare_fact { () => { pub mod fact; } }\n"
                "declare_fact!();\n",
                encoding="utf-8",
            )
            (root / "crates/ostadix-api/src/evidence/fact.rs").write_text(
                "use crate::version::Boundary;\n", encoding="utf-8"
            )
            manifest_path = root / "ci/architecture-roots.toml"
            with manifest_path.open("a", encoding="utf-8") as manifest:
                manifest.write(
                    "\n[[physical_override]]\n"
                    'path = "crates/ostadix-api/src/evidence/fact.rs"\n'
                    'kind = "file"\n'
                    'module_path = ["api"]\n'
                )
            crate_root = root / "crates/ostadix-api/src/lib.rs"
            crate_root.write_text(
                crate_root.read_text(encoding="utf-8").replace(
                    "pub mod api;",
                    '#[path = "evidence/fact.rs"]\npub mod api;',
                    1,
                ),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "macro-generated physical module geometry is unsupported", result.stderr
        )

    def test_ordinary_macros_and_test_only_nested_modules_preserve_ownership(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "macro_rules! keep { ($mod:ident) => { stringify!($mod); } }\n"
                "keep!(harmless);\n"
                "macro_rules! accept { (mod $name:ident) => "
                "{ pub struct Accepted; } }\n"
                "macro_rules! inspect { (macro_rules! inner { mod }) => "
                "{ pub struct Harmless; }; }\n"
                "macro_rules! outer { ($name:ident) => "
                "{ macro_rules! $name { (mod) => {} } }; }\n"
                "outer!(inner);\n"
                "mod inline { #[cfg(test)] mod test_child; }\n"
                "pub struct Syntax;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_facade_manifest_path_must_match_the_public_projection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            manifest_path = root / "ci/architecture-roots.toml"
            manifest_path.write_text(
                manifest_path.read_text(encoding="utf-8").replace(
                    'path = "backend::state"',
                    'path = "backend::renamed_state"',
                    1,
                ),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "does not contain a top-level public alias", result.stderr
        )

    def test_projection_facades_require_an_exact_public_glob(self) -> None:
        cases = (
            (
                "crates/ostadix-api/src/registry/bundle/mod.rs",
                "pub use crate::backend_catalog as renamed;\n",
                "facade `registry::bundle` source does not publicly project",
            ),
            (
                "crates/ostadix-api/src/registry/bundle/mod.rs",
                "pub use crate::backend_catalog::Boundary;\n",
                "facade `registry::bundle` source does not publicly project",
            ),
            (
                "crates/ostadix-api/src/placement/mod.rs",
                "pub mod protocol { pub use crate::placement_protocol as renamed; }\n",
                "does not contain a public inline module projecting",
            ),
            (
                "crates/ostadix-api/src/placement/mod.rs",
                "pub mod protocol { pub use crate::placement_protocol::Boundary; }\n",
                "does not contain a public inline module projecting",
            ),
        )
        for relative, source, expected in cases:
            with self.subTest(relative=relative, source=source), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                write_minimal_tree(root)
                (root / relative).write_text(source, encoding="utf-8")
                result = run_checker(root)
            self.assertEqual(result.returncode, 1)
            self.assertIn(expected, result.stderr)

    def test_module_facade_source_must_be_publicly_declared_by_its_parent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            registry = root / "crates/ostadix-api/src/registry.rs"
            registry.write_text(
                registry.read_text(encoding="utf-8").replace(
                    "pub mod bundle;\n", "", 1
                ),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "facade `registry::bundle` parent does not publicly declare external "
            "module `bundle`",
            result.stderr,
        )

    def test_compiled_include_fragment_cannot_hide_a_root_edge(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/backend_catalog.inc.rs").write_text(
                "use crate::api::Boundary;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "root edge `backend_catalog -> api` is not declared", result.stderr
        )

    def test_compiled_fragment_cannot_declare_external_modules_in_inline_body(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/backend_catalog.inc.rs").write_text(
                "mod outer { mod fact; }\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "could not analyze fragment module ownership", result.stderr
        )
        self.assertIn(
            "external module `fact`", result.stderr
        )

    def test_crate_root_cannot_compile_a_module_outside_declared_geometry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            crate_root = root / "crates/ostadix-api/src/lib.rs"
            crate_root.write_text(
                crate_root.read_text(encoding="utf-8").replace(
                    "pub mod api;",
                    '#[path = "../outside_root.rs"]\npub mod api;',
                    1,
                ),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "must remain normalized beneath `crates/ostadix-api/src`", result.stderr
        )

    def test_crate_root_cannot_hide_edges_in_an_inline_root_module(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            crate_root = root / "crates/ostadix-api/src/lib.rs"
            crate_root.write_text(
                crate_root.read_text(encoding="utf-8").replace(
                    "pub mod parser;",
                    "pub mod parser { use crate::ir::PlanNodeId; }",
                    1,
                ),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "crate-root module `parser` must be an external semicolon declaration",
            result.stderr,
        )

    def test_crate_root_cannot_contain_unowned_production_items(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            crate_root = root / "crates/ostadix-api/src/lib.rs"
            crate_root.write_text(
                crate_root.read_text(encoding="utf-8")
                + "use crate::ir::PlanNodeId;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "crate root must contain only attributes and external module declarations",
            result.stderr,
        )

    def test_crate_root_cannot_inject_a_macro_from_one_root_into_another(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            crate_root = root / "crates/ostadix-api/src/lib.rs"
            crate_root.write_text(
                crate_root.read_text(encoding="utf-8").replace(
                    "pub mod ir;", "#[macro_use]\npub mod ir;", 1
                ),
                encoding="utf-8",
            )
            (root / "crates/ostadix-api/src/ir.rs").write_text(
                "macro_rules! from_ir { () => { pub struct ExpandedFromIr; }; }\n",
                encoding="utf-8",
            )
            (root / "crates/ostadix-api/src/parser.rs").write_text("from_ir!();\n", encoding="utf-8")
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "crate-root module attributes are forbidden except exact #[path]",
            result.stderr,
        )

    def test_facade_owner_roots_must_be_publicly_reachable_from_the_crate(self) -> None:
        for owner in ("backend", "placement", "registry", "world"):
            with self.subTest(owner=owner), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                write_minimal_tree(root)
                crate_root = root / "crates/ostadix-api/src/lib.rs"
                crate_root.write_text(
                    crate_root.read_text(encoding="utf-8").replace(
                        f"pub mod {owner};", f"mod {owner};", 1
                    ),
                    encoding="utf-8",
                )
                result = run_checker(root)
            self.assertEqual(result.returncode, 1)
            self.assertIn(
                f"facade owner root `{owner}` must be a plain public crate-root module",
                result.stderr,
            )

    def test_cargo_library_target_must_match_the_governed_crate_root(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            hidden = root / "crates/ostadix-api/hidden/lib.rs"
            hidden.parent.mkdir(parents=True)
            hidden.write_text(
                "pub mod parser { use crate::ir::Boundary; }\npub mod ir {}\n",
                encoding="utf-8",
            )
            cargo = root / "crates/ostadix-api/Cargo.toml"
            cargo.write_text(
                cargo.read_text(encoding="utf-8")
                + '\n[lib]\npath = "hidden/lib.rs"\n',
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "library target `crates/ostadix-api/hidden/lib.rs` does not match governed crate root "
            "`crates/ostadix-api/src/lib.rs`",
            result.stderr,
        )

    def test_engine_cannot_depend_on_the_compatibility_shell_in_any_scope(self) -> None:
        dependency_cases = (
            '\n[dependencies]\no-lang = "0.0.0"\n',
            '\n[dev-dependencies]\nshell = { package = "o-lang", version = "0" }\n',
            (
                "\n[target.'cfg(unix)'.build-dependencies]\n"
                'shell = { package = "o-lang", version = "0" }\n'
            ),
            '\n[build-dependencies]\nshell = { path = "../.." }\n',
        )
        for addition in dependency_cases:
            with self.subTest(addition=addition), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                write_minimal_tree(root)
                cargo = root / "crates/ostadix-api/Cargo.toml"
                cargo.write_text(
                    cargo.read_text(encoding="utf-8") + addition,
                    encoding="utf-8",
                )
                result = run_checker(root)
            self.assertEqual(result.returncode, 1)
            self.assertIn(
                "must not depend on the `o-lang` compatibility shell",
                result.stderr,
            )

    def test_engine_cannot_reenter_root_through_a_workspace_path_dependency(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            root_cargo = root / "Cargo.toml"
            root_cargo.write_text(
                root_cargo.read_text(encoding="utf-8")
                + '\n[workspace.dependencies]\nshell = { path = "." }\n',
                encoding="utf-8",
            )
            engine_cargo = root / "crates/ostadix-api/Cargo.toml"
            engine_cargo.write_text(
                engine_cargo.read_text(encoding="utf-8")
                + '\n[dependencies]\nshell = { workspace = true }\n',
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "must not depend on the `o-lang` compatibility shell",
            result.stderr,
        )

    def test_shell_dependency_must_be_canonical_path_and_exact_same_version(self) -> None:
        cases = (
            (
                'ostadix-api = { path = "crates/ostadix-api", version = "=0.0.0" }',
                "",
                "must directly depend on `ostadix-api`",
            ),
            (
                'path = "crates/ostadix-api"',
                'path = "crates/other-api"',
                "dependency path must exactly match `crates/ostadix-api`",
            ),
            (
                'version = "=0.0.0"',
                'version = "0.0.0"',
                "must use exact same-version requirement `=0.0.0`",
            ),
            (
                'path = "crates/ostadix-api"',
                'package = "other-api", path = "crates/ostadix-api"',
                "must name package `ostadix-api`",
            ),
            (
                'version = "=0.0.0"',
                'version = "=0.0.0", optional = true',
                "dependency must be unconditional, not optional",
            ),
        )
        for original, replacement, expected in cases:
            with self.subTest(expected=expected), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                write_minimal_tree(root)
                cargo = root / "Cargo.toml"
                cargo.write_text(
                    cargo.read_text(encoding="utf-8").replace(
                        original, replacement, 1
                    ),
                    encoding="utf-8",
                )
                result = run_checker(root)
            self.assertEqual(result.returncode, 1)
            self.assertIn(expected, result.stderr)

    def test_shell_and_engine_package_versions_must_match(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            engine = root / "crates/ostadix-api/Cargo.toml"
            engine.write_text(
                engine.read_text(encoding="utf-8").replace(
                    'version = "0.0.0"', 'version = "0.0.1"', 1
                ),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("package versions must match exactly", result.stderr)

    def test_shell_cannot_add_a_renamed_engine_dependency(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            cargo = root / "Cargo.toml"
            cargo.write_text(
                cargo.read_text(encoding="utf-8")
                + '\nengine-alias = { package = "ostadix-api", '
                'path = "crates/ostadix-api", version = "=0.0.0" }\n',
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "must not add renamed `ostadix-api` dependencies: engine-alias",
            result.stderr,
        )

    def test_root_shell_cannot_duplicate_runtime_implementation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/value.rs").write_text(
                "pub struct DuplicateRuntimeValue;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "runtime implementation source outside its entrypoints: src/value.rs",
            result.stderr,
        )

    def test_root_shell_library_is_only_the_exact_public_module_projection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            shell = root / "src/lib.rs"
            shell.write_text(
                shell.read_text(encoding="utf-8") + "pub fn duplicate() {}\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "must contain only one explicit `pub use ostadix_api::{...};`",
            result.stderr,
        )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            shell = root / "src/lib.rs"
            shell.write_text(
                shell.read_text(encoding="utf-8").replace("api, ", "", 1),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("public roots (missing api)", result.stderr)

    def test_root_shell_library_target_and_source_geometry_are_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            cargo = root / "Cargo.toml"
            cargo.write_text(
                cargo.read_text(encoding="utf-8")
                + '\n[lib]\npath = "src/bin/alternate.rs"\n',
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "compatibility shell library target must remain `src/lib.rs`",
            result.stderr,
        )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/hidden.rs").symlink_to(
                root / "crates/ostadix-api/src/value.rs"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "compatibility shell source geometry must not contain symlinks: src/hidden.rs",
            result.stderr,
        )

    def test_root_cli_sources_remain_outside_the_engine_implementation_guard(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/bin/tool.rs").write_text("fn main() {}\n", encoding="utf-8")
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_nested_module_cannot_escape_through_an_undeclared_path_attribute(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                '#[path = "../outside.rs"]\nmod hidden;\n', encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("undeclared #[path = '../outside.rs']", result.stderr)

    def test_undeclared_include_macro_cannot_add_compiled_source(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                'include!("parser_extra.rs");\n', encoding="utf-8"
            )
            (root / "crates/ostadix-api/src/parser_extra.rs").write_text(
                "use crate::ir::PlanNodeId;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "include! source `crates/ostadix-api/src/parser_extra.rs` is not declared", result.stderr
        )

    def test_undeclared_include_macro_delimiters_cannot_escape_source_geometry(self) -> None:
        for invocation in (
            'include! { "../outside.rs" }\n',
            'include!["../outside.rs"]\n',
        ):
            with self.subTest(invocation=invocation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                write_minimal_tree(root)
                (root / "crates/ostadix-api/src/parser.rs").write_text(invocation, encoding="utf-8")
                (root / "outside.rs").write_text(
                    "use crate::ir::PlanNodeId;\n", encoding="utf-8"
                )
                result = run_checker(root)
            self.assertEqual(result.returncode, 1)
            self.assertIn(
                "include! source `crates/ostadix-api/outside.rs` is not declared",
                result.stderr,
            )

    def test_raw_identifier_include_macro_is_still_source_inclusion(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                'r#include!("../outside.rs");\n', encoding="utf-8"
            )
            (root / "outside.rs").write_text(
                "use crate::ir::PlanNodeId;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "include! source `crates/ostadix-api/outside.rs` is not declared",
            result.stderr,
        )

    def test_include_macro_aliases_cannot_escape_source_analysis(self) -> None:
        for import_source in (
            "use std::include as inc;\n",
            "use std::{include as inc};\n",
        ):
            with self.subTest(import_source=import_source), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                write_minimal_tree(root)
                (root / "crates/ostadix-api/src/parser.rs").write_text(
                    import_source + 'inc!("../outside.rs");\n',
                    encoding="utf-8",
                )
                (root / "outside.rs").write_text(
                    "use crate::ir::PlanNodeId;\n", encoding="utf-8"
                )
                result = run_checker(root)
            self.assertEqual(result.returncode, 1)
            self.assertIn(
                "include macro reference", result.stderr
            )
            self.assertIn("aliases are forbidden", result.stderr)

    def test_production_cfg_attr_cannot_change_module_source_geometry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                '#[cfg_attr(all(), path = "../outside.rs")]\nmod hidden;\n',
                encoding="utf-8",
            )
            (root / "outside.rs").write_text(
                "use crate::ir::PlanNodeId;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("undeclared #[path = '../outside.rs']", result.stderr)

    def test_definitely_test_only_cfg_attr_does_not_change_production_geometry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            parser = root / "crates/ostadix-api/src/parser.rs"
            parser.write_text(
                '#[cfg_attr(test, path = "../outside.rs")]\nmod hidden;\n',
                encoding="utf-8",
            )
            hidden = root / "crates/ostadix-api/src/parser/hidden.rs"
            hidden.parent.mkdir(parents=True, exist_ok=True)
            hidden.write_text("pub struct Hidden;\n", encoding="utf-8")
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_test_only_nested_reexport_cannot_certify_a_facade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/mod.rs").write_text(
                "pub mod protocol {\n"
                "    #[cfg(test)]\n"
                "    pub use crate::placement_protocol::*;\n"
                "}\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "does not contain a public inline module projecting", result.stderr
        )

    def test_cfg_attr_test_only_reexport_cannot_certify_a_facade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/mod.rs").write_text(
                "pub mod protocol {\n"
                "    #[cfg_attr(all(), cfg(test))]\n"
                "    pub use crate::placement_protocol::*;\n"
                "}\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "does not contain a public inline module projecting", result.stderr
        )

    def test_conditionally_absent_reexport_cannot_certify_a_facade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/mod.rs").write_text(
                "pub mod protocol {\n"
                '    #[cfg(test = "only")]\n'
                "    pub use crate::placement_protocol::*;\n"
                "}\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "does not contain a public inline module projecting", result.stderr
        )

    def test_nested_module_reexport_cannot_certify_its_parent_facade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/mod.rs").write_text(
                "pub mod protocol {\n"
                "    mod hidden {\n"
                "        pub use crate::placement_protocol::*;\n"
                "    }\n"
                "}\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "does not contain a public inline module projecting", result.stderr
        )

    def test_cycle_in_previously_ungoverned_files_is_rejected_by_tarjan(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/information.rs").write_text(
                "use crate::canonical_cbor::Boundary;\n", encoding="utf-8"
            )
            (root / "crates/ostadix-api/src/canonical_cbor.rs").write_text(
                "use crate::information::Boundary;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "multi-root strongly connected component detected: "
            "canonical_cbor, information",
            result.stderr,
        )

    def test_facade_dependency_participates_in_cycle_detection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/information.rs").write_text(
                "use crate::world::identity::ArtifactId;\n", encoding="utf-8"
            )
            (root / "crates/ostadix-api/src/world/mod.rs").write_text(
                "pub use crate::resource_identity as identity;\n"
                "use crate::information::Boundary;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("root edge `information -> world`", result.stderr)
        self.assertIn(
            "multi-root strongly connected component detected: information, world",
            result.stderr,
        )

    def test_novel_edge_between_known_roots_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/canonical_cbor.rs").write_text(
                "use crate::value::OValue;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "root edge `canonical_cbor -> value` is not declared", result.stderr
        )

    def test_native_roots_cannot_depend_back_on_information_bridge(self) -> None:
        for native_root in (
            "information",
            "information_provenance",
            "parser",
            "value",
            "hgraph",
            "evidence",
            "registry",
            "world",
            "project",
            "hosted_remote",
        ):
            with self.subTest(native_root=native_root):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    path = root / "crates/ostadix-api/src" / native_root
                    source = path / "mod.rs" if path.is_dir() else path.with_suffix(".rs")
                    source.write_text(
                        "use crate::information_bridge::Boundary;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(
                    f"root edge `{native_root} -> information_bridge` is not declared",
                    result.stderr,
                )

    def test_information_bridge_cannot_import_authority_or_execution_roots(self) -> None:
        for forbidden in (
            "capability",
            "eval",
            "executor",
            "information_provenance",
            "placement",
            "runtime_exec",
        ):
            with self.subTest(forbidden=forbidden):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/information_bridge.rs").write_text(
                        f"use crate::{forbidden}::Boundary;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(
                    f"root edge `information_bridge -> {forbidden}` is not declared",
                    result.stderr,
                )

    def test_new_production_root_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/future_private_model.rs").write_text(
                "pub struct PrivateModel;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "production path resolves to unknown root `future_private_model`",
            result.stderr,
        )

    def test_manifest_root_without_a_production_source_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/shims.rs").unlink()
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "manifest root `shims` has no production Rust source", result.stderr
        )

    def test_manifest_cannot_exclude_an_arbitrary_compiled_module_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text("mod hidden;\n", encoding="utf-8")
            hidden = root / "crates/ostadix-api/src/parser/hidden.rs"
            hidden.parent.mkdir(parents=True, exist_ok=True)
            hidden.write_text(
                "use crate::ir::PlanNodeId;\n", encoding="utf-8"
            )
            manifest_path = root / "ci/architecture-roots.toml"
            manifest_path.write_text(
                manifest_path.read_text(encoding="utf-8").replace(
                    'excluded_files = ["crates/ostadix-api/src/backend_catalog.inc.rs", "crates/ostadix-api/src/lib.rs"]',
                    'excluded_files = ["crates/ostadix-api/src/backend_catalog.inc.rs", "crates/ostadix-api/src/lib.rs", '
                    '"crates/ostadix-api/src/parser/hidden.rs"]',
                    1,
                ),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "production.excluded_files must contain exactly", result.stderr
        )
        self.assertIn("unexpected crates/ostadix-api/src/parser/hidden.rs", result.stderr)

    def test_manifest_cannot_exclude_an_arbitrary_compiled_module_directory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            hidden = root / "crates/ostadix-api/src/parser/hidden.rs"
            hidden.parent.mkdir(parents=True, exist_ok=True)
            hidden.write_text(
                "use crate::ir::PlanNodeId;\n", encoding="utf-8"
            )
            manifest_path = root / "ci/architecture-roots.toml"
            manifest_path.write_text(
                manifest_path.read_text(encoding="utf-8").replace(
                    'excluded_directories = []',
                    'excluded_directories = ["crates/ostadix-api/src/parser"]',
                    1,
                ),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "production.excluded_directories must be empty", result.stderr
        )
        self.assertIn("unexpected crates/ostadix-api/src/parser", result.stderr)

    def test_symlinked_module_directory_cannot_escape_source_enumeration(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text("mod hidden;\n", encoding="utf-8")
            outside = root / "outside_parser"
            outside.mkdir()
            (outside / "hidden.rs").write_text(
                "use crate::ir::PlanNodeId;\n", encoding="utf-8"
            )
            (root / "crates/ostadix-api/src/parser").symlink_to(outside, target_is_directory=True)
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "production source path `crates/ostadix-api/src/parser` must not be a symlink",
            result.stderr,
        )

    def test_comments_raw_strings_and_cfg_test_are_not_root_edges(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/canonical_cbor.rs").write_text(
                '// use crate::api::Boundary;\n'
                'const COOKED: &str = "crate::api::Boundary";\n'
                'const RAW: &str = r###"use crate::api::Boundary;"###;\n'
                "#[cfg(test)]\nuse crate::api::Boundary;\n"
                "pub struct Production;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_dependency_through_facade_remains_an_edge_to_facade_owner(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/canonical_cbor.rs").write_text(
                "use crate::world::identity::ArtifactId;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "root edge `canonical_cbor -> world` is not declared", result.stderr
        )
        self.assertNotIn("canonical_cbor -> resource_identity", result.stderr)

    def test_physical_directory_override_uses_canonical_protocol_root(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/protocol/future.rs").write_text(
                "use crate::resource_identity::Boundary;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_physical_file_override_prevents_world_identity_misclassification(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/world/identity.rs").write_text(
                "use crate::value::OValue;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "root edge `resource_identity -> value` is not declared", result.stderr
        )
        self.assertNotIn("root edge `world -> value`", result.stderr)

    def test_non_rs_physical_file_override_is_still_production_source(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            old_path = root / "crates/ostadix-api/src/world/identity.rs"
            override_path = root / "crates/ostadix-api/src/world/identity.source"
            old_path.rename(override_path)
            override_path.write_text(
                "use crate::api::Boundary;\n", encoding="utf-8"
            )
            manifest_path = root / "ci/architecture-roots.toml"
            manifest_path.write_text(
                manifest_path.read_text(encoding="utf-8").replace(
                    'path = "crates/ostadix-api/src/world/identity.rs"',
                    'path = "crates/ostadix-api/src/world/identity.source"',
                    1,
                ),
                encoding="utf-8",
            )
            crate_root = root / "crates/ostadix-api/src/lib.rs"
            crate_root.write_text(
                crate_root.read_text(encoding="utf-8").replace(
                    'path = "world/identity.rs"',
                    'path = "world/identity.source"',
                    1,
                ),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "root edge `resource_identity -> api` is not declared", result.stderr
        )

    def test_wrong_way_production_dependency_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "use crate::ir::PlanNodeId;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("narrow dialect projection", result.stderr)

    def test_ir_cannot_reenter_catalog_through_registry_facade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/ir.rs").write_text(
                "use crate::registry::bundle::BackendRegistry;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("canonical backend catalog", result.stderr)

    def test_ir_cannot_depend_on_its_execution_contract_projection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/ir.rs").write_text(
                "use crate::execution_contract::Policy;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("execution-contract", result.stderr)

    def test_canonical_catalog_rejects_every_frozen_higher_layer(self) -> None:
        for module in (
            "backend",
            "eval",
            "eval_core",
            "evidence",
            "execution_contract",
            "executor",
            "hgraph",
            "ir",
            "placement",
            "registry",
            "runtime_exec",
            "scheduler",
            "world",
        ):
            with self.subTest(module=module):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/backend_catalog.rs").write_text(
                        f"use crate::{module}::Boundary;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("canonical backend catalog must remain below", result.stderr)

    def test_canonical_catalog_accepts_only_its_frozen_lower_seams(self) -> None:
        allowed = (
            "value",
            "syntax_dialect",
            "resource_identity",
            "placement_protocol",
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/backend_catalog.rs").write_text(
                "".join(f"use crate::{module}::Boundary;\n" for module in allowed),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_backend_state_is_compiled_once_with_a_legacy_alias(self) -> None:
        lib_source = (ROOT / "crates/ostadix-api/src/lib.rs").read_text(encoding="utf-8")
        backend_source = (ROOT / "crates/ostadix-api/src/backend.rs").read_text(encoding="utf-8")

        self.assertEqual(lib_source.count("pub mod backend_state;"), 1)
        self.assertIn("pub use crate::backend_state as state;", backend_source)
        self.assertNotIn('#[path = "backend_state.rs"]', backend_source)

    def test_backend_state_cannot_reenter_backend_or_process_realizations(self) -> None:
        for module in ("backend", "process"):
            with self.subTest(module=module):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/backend_state.rs").write_text(
                        f"use crate::{module}::Boundary;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("canonical backend-state protocol", result.stderr)

    def test_backend_state_allowlist_rejects_novel_roots_in_every_form(self) -> None:
        cases = (
            ("use crate::future_state_owner::Boundary;\n", "crate::future_state_owner"),
            (
                "use crate::{future_state_owner::Boundary};\n",
                "crate::{future_state_owner::...}",
            ),
            (
                "use crate :: future_state_owner :: Boundary;\n",
                "crate::future_state_owner",
            ),
        )
        for source, expected in cases:
            with self.subTest(source=source):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/backend_state.rs").write_text(
                        source, encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(f"forbidden dependency `{expected}`", result.stderr)

    def test_backend_state_accepts_exactly_its_three_lower_roots(self) -> None:
        allowed = ("environment", "value", "wire")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/backend_state.rs").write_text(
                "".join(f"use crate::{module}::Boundary;\n" for module in allowed),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_backend_state_lower_dependencies_predeny_reverse_edges(self) -> None:
        for relative in ("crates/ostadix-api/src/environment.rs", "crates/ostadix-api/src/value.rs", "crates/ostadix-api/src/wire.rs"):
            with self.subTest(relative=relative):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / relative).write_text(
                        "use crate::backend_state::BackendStateTierV1;\n",
                        encoding="utf-8",
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(
                    "forbidden dependency `crate::backend_state`", result.stderr
                )

    def test_process_cannot_import_backend_realization_facade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/process.rs").write_text(
                "use crate::backend::RustBackend;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("canonical backend-state protocol", result.stderr)

    def test_process_can_import_canonical_backend_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/process.rs").write_text(
                "use crate::backend_state::BackendStateTierV1;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_execution_contract_rejects_every_frozen_higher_layer(self) -> None:
        for module in (
            "api",
            "backend",
            "backend_morphism",
            "backend_state",
            "canonical_cbor",
            "capability",
            "dispatch_model",
            "environment",
            "eval",
            "eval_core",
            "evidence",
            "executor",
            "hgraph",
            "hosted_remote",
            "information",
            "information_provenance",
            "kernel_world",
            "live_system",
            "nix_ops",
            "nixos_ops",
            "ocore",
            "parser",
            "placement",
            "placement_protocol",
            "process",
            "project",
            "registry",
            "resource_identity",
            "runtime_exec",
            "scheduler",
            "shims",
            "syntax_dialect",
            "version",
            "wire",
            "world",
        ):
            with self.subTest(module=module):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/execution_contract.rs").write_text(
                        f"use crate::{module}::Boundary;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("canonical execution contract", result.stderr)

    def test_execution_contract_forbidden_roots_cannot_be_obscured(self) -> None:
        cases = (
            ("use crate::process::Boundary;\n", "crate::process"),
            ("use crate::{process::Boundary};\n", "crate::{process::...}"),
            ("use crate :: process :: Boundary;\n", "crate::process"),
            (
                "fn boundary() { let _ = crate::process::Boundary; }\n",
                "crate::process",
            ),
        )
        for source, expected in cases:
            with self.subTest(source=source):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/execution_contract.rs").write_text(
                        source, encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(f"forbidden dependency `{expected}`", result.stderr)

    def test_execution_contract_allowlist_rejects_novel_roots_in_every_form(self) -> None:
        cases = (
            ("use crate::future_high_layer::Boundary;\n", "crate::future_high_layer"),
            (
                "use crate::{future_high_layer::Boundary};\n",
                "crate::{future_high_layer::...}",
            ),
            (
                "use crate :: future_high_layer :: Boundary;\n",
                "crate::future_high_layer",
            ),
        )
        for source, expected in cases:
            with self.subTest(source=source):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/execution_contract.rs").write_text(
                        source, encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(f"forbidden dependency `{expected}`", result.stderr)

    def test_repository_manifest_closes_rules_that_were_previously_deny_only(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "use crate::future_low_layer::Boundary;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("unknown root `future_low_layer`", result.stderr)

    def test_execution_contract_accepts_only_its_frozen_lower_seams(self) -> None:
        allowed = ("backend_catalog", "effects", "ir", "value")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/execution_contract.rs").write_text(
                "".join(f"use crate::{module}::Boundary;\n" for module in allowed),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_execution_contract_lower_seams_cannot_form_reverse_cycles(self) -> None:
        for relative in (
            "crates/ostadix-api/src/parser.rs",
            "crates/ostadix-api/src/syntax_dialect.rs",
            "crates/ostadix-api/src/effects.rs",
            "crates/ostadix-api/src/value.rs",
            "crates/ostadix-api/src/placement/protocol/target.rs",
        ):
            with self.subTest(relative=relative):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / relative).write_text(
                        "use crate::execution_contract::Policy;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(
                    "forbidden dependency `crate::execution_contract`", result.stderr
                )

    def test_eval_core_cannot_reenter_evaluator_or_executor_realizations(self) -> None:
        for module in ("eval", "executor"):
            with self.subTest(module=module):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/eval_core.rs").write_text(
                        f"use crate::{module}::Boundary;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("graph-evaluation contract", result.stderr)

    def test_eval_core_allowlist_rejects_an_unanticipated_root(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/eval_core.rs").write_text(
                "use crate::information::InformationRootV1;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("forbidden dependency `crate::information`", result.stderr)

    def test_eval_core_accepts_exactly_its_six_lower_roots(self) -> None:
        allowed = (
            "backend_catalog",
            "capability",
            "evidence",
            "execution_contract",
            "ir",
            "value",
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/eval_core.rs").write_text(
                "".join(f"use crate::{module}::Boundary;\n" for module in allowed),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_every_executor_module_requires_eval_core_instead_of_evaluator(self) -> None:
        for relative in (
            "crates/ostadix-api/src/executor/actor.rs",
            "crates/ostadix-api/src/executor/cancellation.rs",
            "crates/ostadix-api/src/executor/coordinator.rs",
            "crates/ostadix-api/src/executor/effects.rs",
            "crates/ostadix-api/src/executor/mod.rs",
            "crates/ostadix-api/src/executor/parallel.rs",
            "crates/ostadix-api/src/executor/pool.rs",
            "crates/ostadix-api/src/executor/task.rs",
            "crates/ostadix-api/src/executor/trace.rs",
        ):
            with self.subTest(relative=relative):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / relative).write_text(
                        "use crate::eval::Evaluator;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("canonical catalogs", result.stderr)

    def test_executor_accepts_the_eval_core_host_contract(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/executor/coordinator.rs").write_text(
                "use crate::eval_core::GraphEvaluationHost;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_eval_core_lower_dependencies_predeny_reverse_edges(self) -> None:
        for relative in (
            "crates/ostadix-api/src/backend_catalog.rs",
            "crates/ostadix-api/src/capability.rs",
            "crates/ostadix-api/src/execution_contract.rs",
            "crates/ostadix-api/src/ir.rs",
            "crates/ostadix-api/src/value.rs",
            "crates/ostadix-api/src/evidence/analyze.rs",
            "crates/ostadix-api/src/evidence/mod.rs",
        ):
            with self.subTest(relative=relative):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / relative).write_text(
                        "use crate::eval_core::GraphEvalFrame;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("forbidden dependency `crate::eval_core`", result.stderr)

    def test_evidence_and_world_cannot_reenter_evaluator_for_contract_types(self) -> None:
        for relative in (
            "crates/ostadix-api/src/evidence/admit.rs",
            "crates/ostadix-api/src/evidence/analyze.rs",
            "crates/ostadix-api/src/evidence/intent.rs",
            "crates/ostadix-api/src/world/grounding.rs",
        ):
            with self.subTest(relative=relative):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / relative).write_text(
                        "use crate::eval::Policy;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("canonical execution contract", result.stderr)

    def test_evidence_and_world_accept_canonical_execution_contract(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/evidence/analyze.rs").write_text(
                "use crate::execution_contract::Policy;\n", encoding="utf-8"
            )
            (root / "crates/ostadix-api/src/world/grounding.rs").write_text(
                "use crate::execution_contract::BlockOptions;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unit_test_import_does_not_define_production_geometry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "pub struct Syntax;\n# [ cfg ( test ) ]\n"
                "mod tests {\n"
                "    const MARKER: &str = r###\"} #[cfg(test)] {\"###;\n"
                "    /* nested /* comment */ remains test-only */\n"
                "    use crate::ir::PlanNodeId;\n"
                "}\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_production_after_unit_test_module_is_still_checked(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(test)]\n"
                "mod tests { use crate::ir::PlanNodeId; }\n"
                "use crate::registry::BackendRegistry;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("forbidden dependency `crate::registry`", result.stderr)

    def test_test_only_const_unsafe_function_does_not_mask_following_code(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(test)]\n"
                "const unsafe fn helper() { use crate::ir::PlanNodeId; }\n"
                "use crate::registry::BackendRegistry;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("forbidden dependency `crate::ir`", result.stderr)
        self.assertIn("forbidden dependency `crate::registry`", result.stderr)

    def test_test_only_impl_const_expression_signature_masks_its_full_body(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(test)]\n"
                "impl Trait for Foo<{ 1 + { 2 } }> { use crate::ir::PlanNodeId; }\n"
                "use crate::registry::BackendRegistry;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("forbidden dependency `crate::ir`", result.stderr)
        self.assertIn("forbidden dependency `crate::registry`", result.stderr)

    def test_test_only_function_macro_type_signature_masks_its_full_body(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(test)]\n"
                "fn helper() -> ty!{} { use crate::ir::PlanNodeId; }\n"
                "use crate::registry::BackendRegistry;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("forbidden dependency `crate::ir`", result.stderr)
        self.assertIn("forbidden dependency `crate::registry`", result.stderr)

    def test_cfg_test_text_in_comments_literals_and_macros_cannot_hide_code(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                'const COOKED: &str = "#[cfg(test)]";\n'
                "const RAW: &str = r###\"#[cfg(test)]\"###;\n"
                "// #[cfg(test)]\n"
                "/* #[cfg(test)] */\n"
                "macro_rules! marker { () => { #[cfg(test)] mod tests {} }; }\n"
                "use crate :: ir :: PlanNodeId;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("forbidden dependency `crate::ir`", result.stderr)

    def test_dependency_text_in_comments_and_literals_is_not_code(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                'const NOTE: &str = "use crate::ir::PlanNodeId;";\n'
                "const RAW: &str = r#\"crate::registry::BackendRegistry\"#;\n"
                "// use crate::ir::PlanNodeId;\n"
                "/* use crate::registry::BackendRegistry; */\n"
                "pub struct Syntax;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_grouped_and_spaced_crate_import_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "use crate :: { ir :: PlanNodeId, value::OValue };\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("forbidden dependency `crate::{ir::...}`", result.stderr)

    def test_macro_cannot_split_a_crate_dependency_across_tokens(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "macro_rules! dep {\n"
                "    ($root:tt, $module:ident) => {\n"
                "        use $root::$module::Boundary;\n"
                "    };\n"
                "}\n"
                "dep!(crate, ir);\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "crate token inside a macro invocation is not analyzable",
            result.stderr,
        )

    def test_macro_visibility_shape_cannot_exempt_a_captured_crate_root(self) -> None:
        for invocation in ("dep!(pub(crate), ir);\n", "dep!(pub(in crate), ir);\n"):
            with self.subTest(invocation=invocation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                write_minimal_tree(root)
                (root / "crates/ostadix-api/src/parser.rs").write_text(
                    "macro_rules! dep {\n"
                    "    (pub($root:tt), $module:ident) => {\n"
                    "        pub($root) struct Local;\n"
                    "        use $root::$module::Boundary;\n"
                    "    };\n"
                    "}\n"
                    + invocation,
                    encoding="utf-8",
                )
                result = run_checker(root)
            self.assertEqual(result.returncode, 1)
            self.assertIn(
                "crate token inside a macro invocation is not analyzable",
                result.stderr,
            )

    def test_macro_invocation_cannot_retarget_an_apparently_allowed_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "macro_rules! dep {\n"
                "    ($root:tt :: $visible:ident, $hidden:ident) => {\n"
                "        use $root::$hidden::Boundary;\n"
                "    };\n"
                "}\n"
                "dep!(crate::syntax_dialect, ir);\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "crate token inside a macro invocation is not analyzable",
            result.stderr,
        )

    def test_nested_super_in_grouped_use_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/protocol/target.rs").write_text(
                "use super::{super::ir::Boundary};\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "nested `super` root inside a grouped use is not analyzable",
            result.stderr,
        )

    def test_raw_identifiers_cannot_obscure_forbidden_root_modules(self) -> None:
        cases = (
            ("crates/ostadix-api/src/parser.rs", "use crate::r#ir::PlanNodeId;\n", "crate::ir"),
            (
                "crates/ostadix-api/src/placement/protocol/target.rs",
                "use crate::{r#world::ArtifactId};\n",
                "crate::{world::...}",
            ),
        )
        for relative, source, expected in cases:
            with self.subTest(source=source):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / relative).write_text(source, encoding="utf-8")
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(f"forbidden dependency `{expected}`", result.stderr)

    def test_cfg_that_can_exist_in_production_is_not_discarded(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                '#[cfg(any(test, feature = "fixture"))]\n'
                "mod maybe_production { use crate::ir::PlanNodeId; }\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("forbidden dependency `crate::ir`", result.stderr)

    def test_cfg_all_with_false_test_is_definitely_disabled(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(all(test, unix))]\n"
                "mod tests { use crate::ir::PlanNodeId; }\n"
                "pub struct Production;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_cfg_not_test_is_definitely_enabled(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(not(test))]\nuse crate::ir::PlanNodeId;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("forbidden dependency `crate::ir`", result.stderr)

    def test_valid_cfg_name_value_is_unknown_and_opaque(self) -> None:
        literals = ('"crate::ir must remain opaque"', 'r#"crate::ir must remain opaque"#')
        for literal in literals:
            with self.subTest(literal=literal):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/parser.rs").write_text(
                        f"#[cfg(feature = {literal})]\n"
                        "mod maybe_production { use crate::ir::PlanNodeId; }\n",
                        encoding="utf-8",
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertNotIn("requires exactly one ordinary string", result.stderr)
                self.assertEqual(result.stderr.count("forbidden dependency `crate::ir`"), 1)

    def test_false_cfg_with_valid_name_value_is_definitely_disabled(self) -> None:
        for literal in ('"fixture"', 'r#"fixture"#'):
            with self.subTest(literal=literal):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/parser.rs").write_text(
                        f"#[cfg(all(test, feature = {literal}))]\n"
                        "mod tests { use crate::ir::PlanNodeId; }\n",
                        encoding="utf-8",
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_cfg_rejects_non_string_literal_kinds_before_evaluation(self) -> None:
        invalid_literals = ("'x'", 'b"x"', 'c"x"', 'br#"x"#', 'cr#"x"#')
        for literal in invalid_literals:
            for predicate in (
                f"feature = {literal}",
                f"all(test, feature = {literal})",
            ):
                with self.subTest(predicate=predicate):
                    with tempfile.TemporaryDirectory() as directory:
                        root = Path(directory)
                        write_minimal_tree(root)
                        (root / "crates/ostadix-api/src/parser.rs").write_text(
                            f"#[cfg({predicate})]\nmod invalid {{}}\n",
                            encoding="utf-8",
                        )
                        result = run_checker(root)
                    self.assertEqual(result.returncode, 1)
                    self.assertIn(
                        "requires exactly one ordinary string literal value",
                        result.stderr,
                    )

    def test_injected_literal_sentinel_cannot_hide_a_dependency(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(all(test, feature = \0))]\n"
                "mod hidden { use crate::ir::PlanNodeId; }\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("reserved literal sentinel U+0000", result.stderr)
        self.assertNotIn("architecture dependency boundaries: PASS", result.stdout)

    def test_nested_cfg_false_is_masked_under_production_projection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(all(unix, any(test, all(windows, test))))]\n"
                "mod tests { use crate::ir::PlanNodeId; }\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_nested_cfg_true_remains_visible_under_production_projection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(not(any(test, all(test, unix))))]\n"
                "mod production { use crate::ir::PlanNodeId; }\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("forbidden dependency `crate::ir`", result.stderr)

    def test_nested_cfg_unknown_remains_visible_under_production_projection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                '#[cfg(not(any(test, all(unix, feature = "fixture"))))]\n'
                "mod maybe_production { use crate::ir::PlanNodeId; }\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("forbidden dependency `crate::ir`", result.stderr)

    def test_root_aliases_and_globs_fail_closed(self) -> None:
        cases = (
            "use crate as root;\n",
            "use crate::{self as root};\n",
            "extern crate self as root;\n",
            "use crate::*;\n",
            "use crate::{*};\n",
            "use super as root;\n",
            "use super::{self as root};\n",
            "use super::*;\n",
            "use super::{*};\n",
        )
        for source in cases:
            with self.subTest(source=source):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/parser.rs").write_text(source, encoding="utf-8")
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("is not analyzable", result.stderr)
                self.assertIn("require explicit root paths", result.stderr)

    def test_bare_root_imports_fail_closed(self) -> None:
        cases = (
            ("use crate;\n", "bare crate root path"),
            ("use {crate};\n", "bare crate root path"),
            ("use super;\n", "bare super root path"),
            (
                "use super::super;\n",
                "bare super::super root path",
            ),
        )
        for source, expected in cases:
            with self.subTest(source=source):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/evidence/admit.rs").write_text(source, encoding="utf-8")
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(expected, result.stderr)
                self.assertIn("require explicit root paths", result.stderr)

    def test_visibility_and_extern_crate_roots_are_not_dependencies(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "extern crate dependency;\n"
                "pub(crate) struct CrateVisible;\n"
                "pub(super) struct ParentVisible;\n"
                "pub(in crate) struct RestrictedToCrate;\n"
                "crate fn LegacyCrateVisible() {}\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_raw_use_identifier_is_not_promoted_to_use_keyword(self) -> None:
        cases = (
            "fn r#use() {}\npub(crate) struct CrateVisible;\n",
            "mod r#use {}\npub(super) struct ParentVisible;\n",
            "mod r#use {}\nextern crate dependency;\n",
        )
        for source in cases:
            with self.subTest(source=source):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/evidence/admit.rs").write_text(
                        source, encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_precise_capture_use_keyword_is_not_an_import(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/evidence/admit.rs").write_text(
                "fn f<'a>(x: &'a ()) -> impl Sized + use<'a> { x }\n"
                "pub(crate) struct CrateVisible;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_macro_literal_use_keyword_is_not_an_import(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/evidence/admit.rs").write_text(
                "macro_rules! keyword { () => { use } }\n"
                "pub(crate) struct CrateVisible;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_direct_super_chains_expose_forbidden_modules(self) -> None:
        cases = (
            ("crates/ostadix-api/src/parser.rs", "use super::ir::PlanNodeId;\n", "super::ir"),
            ("crates/ostadix-api/src/eval.rs", "use super::world::ArtifactId;\n", "super::world"),
            (
                "crates/ostadix-api/src/placement/protocol/target.rs",
                "use super::super::world::ArtifactId;\n",
                "super::super::world",
            ),
        )
        for relative, source, expected in cases:
            with self.subTest(source=source):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / relative).write_text(source, encoding="utf-8")
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(f"forbidden dependency `{expected}`", result.stderr)

    def test_nested_super_paths_remain_local_before_reaching_crate_root(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/protocol/target.rs").write_text(
                "use super::digest::validate_token;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_excessive_super_hops_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/protocol/target.rs").write_text(
                "use super::super::super::world::ArtifactId;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("exceeds file module depth 2", result.stderr)

    def test_inline_module_super_path_fails_closed_as_ambiguous(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/protocol/target.rs").write_text(
                "mod nested { use super::world::ArtifactId; }\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("inline production module has ambiguous module depth", result.stderr)

    def test_explicit_allowed_super_dependencies_remain_analyzable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/protocol/target.rs").write_text(
                "use super::digest::validate_token;\n", encoding="utf-8"
            )
            (root / "crates/ostadix-api/src/evidence/admit.rs").write_text(
                "use super::fact::EvidenceFact;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_malformed_test_item_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(test)]\nmod tests {\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("could not analyze Rust tokens", result.stderr)

    def test_malformed_cfg_operator_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                "#[cfg(not(test, unix))]\nmod invalid {}\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("cfg operator `not` requires exactly one predicate", result.stderr)

    def test_malformed_cfg_cannot_be_hidden_by_false_test_operand(self) -> None:
        cases = (
            "#[cfg(all(test, unix extra))]\nmod invalid {}\n",
            "#[cfg(all(test, feature =))]\nmod invalid {}\n",
            "#[cfg(all(test, bogus(,)))]\nmod invalid {}\n",
            "#[cfg(all(test, bogus()))]\nmod invalid {}\n",
        )
        for source in cases:
            with self.subTest(source=source):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/parser.rs").write_text(source, encoding="utf-8")
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("could not analyze Rust tokens", result.stderr)

    def test_unclosed_literal_cannot_hide_a_dependency(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/parser.rs").write_text(
                'const BROKEN: &str = "#[cfg(test)]\n'
                "use crate::ir::PlanNodeId;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("unclosed string", result.stderr)

    def test_artifact_consumers_cannot_reenter_through_world_facade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/protocol/target.rs").write_text(
                "use crate::{world::ArtifactId};\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("resource_identity", result.stderr)

    def test_protocol_cannot_reenter_any_forbidden_higher_layer(self) -> None:
        cases = (
            ("backend", "use crate :: { backend :: Boundary };\n"),
            ("dispatch_model", "use crate::dispatch_model::Boundary;\n"),
            ("effects", "use crate::effects::Boundary;\n"),
            ("eval", "use crate::eval::Boundary;\n"),
            ("evidence", "use crate::evidence::Boundary;\n"),
            ("executor", "use crate::executor::Boundary;\n"),
            ("hgraph", "use crate::hgraph::Boundary;\n"),
            ("hosted_remote", "use crate::hosted_remote::Boundary;\n"),
            ("ir", "use crate::ir::Boundary;\n"),
            ("placement", "use crate::placement::Boundary;\n"),
            ("project", "use crate::project::Boundary;\n"),
            ("registry", "use crate::registry::Boundary;\n"),
            ("runtime_exec", "use crate::runtime_exec::Boundary;\n"),
            ("value", "use crate::value::Boundary;\n"),
            ("world", "use crate::world::Boundary;\n"),
        )
        for module, source in cases:
            with self.subTest(module=module):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/placement/protocol/catalog.rs").write_text(
                        source, encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("canonical placement protocol", result.stderr)

    def test_protocol_can_depend_on_shared_resource_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/placement/protocol/catalog.rs").write_text(
                "use crate::resource_identity::ArtifactId;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_registry_cannot_import_public_placement_facade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/registry/model.rs").write_text(
                "use crate::placement::NodeProfileV1;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("canonical placement_protocol", result.stderr)

    def test_registry_can_import_canonical_placement_protocol(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "crates/ostadix-api/src/registry/model.rs").write_text(
                "use crate::placement_protocol::NodeProfileV1;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_ir_cannot_import_any_placement_projection(self) -> None:
        for module in ("placement", "placement_protocol"):
            with self.subTest(module=module):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_minimal_tree(root)
                    (root / "crates/ostadix-api/src/ir.rs").write_text(
                        f"use crate::{module}::SemanticDigestV1;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("HGraph or placement projections", result.stderr)


if __name__ == "__main__":
    unittest.main()
