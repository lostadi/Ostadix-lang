#!/usr/bin/env python3

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CHECKER = ROOT / "scripts" / "check_architecture_boundaries.py"


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
        "src/parser.rs",
        "src/syntax_dialect.rs",
        "src/ir.rs",
        "src/backend_catalog.rs",
        "src/capability.rs",
        "src/execution_contract.rs",
        "src/eval_core.rs",
        "src/effects.rs",
        "src/value.rs",
        "src/dispatch_model.rs",
        "src/placement/mod.rs",
        "src/placement/projection.rs",
        "src/placement/protocol/candidate.rs",
        "src/placement/protocol/catalog.rs",
        "src/placement/protocol/digest.rs",
        "src/placement/protocol/error.rs",
        "src/placement/protocol/mod.rs",
        "src/placement/protocol/records.rs",
        "src/placement/protocol/requirement.rs",
        "src/placement/protocol/state.rs",
        "src/placement/protocol/target.rs",
        "src/placement/protocol/warrant.rs",
        "src/registry/bundle/mod.rs",
        "src/registry/model.rs",
        "src/registry/placement_compat.rs",
        "src/registry/store.rs",
        "src/eval.rs",
        "src/executor/actor.rs",
        "src/executor/cancellation.rs",
        "src/executor/coordinator.rs",
        "src/executor/effects.rs",
        "src/executor/mod.rs",
        "src/executor/parallel.rs",
        "src/executor/pool.rs",
        "src/executor/task.rs",
        "src/executor/trace.rs",
        "src/runtime_exec.rs",
        "src/evidence/admit.rs",
        "src/evidence/analyze.rs",
        "src/evidence/fact.rs",
        "src/evidence/intent.rs",
        "src/evidence/mod.rs",
        "src/evidence/profile.rs",
        "src/world/grounding.rs",
    ):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("pub struct Boundary;\n", encoding="utf-8")


class ArchitectureBoundaryTests(unittest.TestCase):
    def test_current_tree_respects_frozen_boundaries(self) -> None:
        result = run_checker(ROOT)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "architecture dependency boundaries: PASS\n")

    def test_wrong_way_production_dependency_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/parser.rs").write_text(
                "use crate::ir::PlanNodeId;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("narrow dialect projection", result.stderr)

    def test_ir_cannot_reenter_catalog_through_registry_facade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/ir.rs").write_text(
                "use crate::registry::bundle::BackendRegistry;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("canonical backend catalog", result.stderr)

    def test_ir_cannot_depend_on_its_execution_contract_projection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/ir.rs").write_text(
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
                    (root / "src/backend_catalog.rs").write_text(
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
            (root / "src/backend_catalog.rs").write_text(
                "".join(f"use crate::{module}::Boundary;\n" for module in allowed),
                encoding="utf-8",
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
                    (root / "src/execution_contract.rs").write_text(
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
                    (root / "src/execution_contract.rs").write_text(
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
                    (root / "src/execution_contract.rs").write_text(
                        source, encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(f"forbidden dependency `{expected}`", result.stderr)

    def test_rules_without_allowlists_remain_deny_only(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/parser.rs").write_text(
                "use crate::future_low_layer::Boundary;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_execution_contract_accepts_only_its_frozen_lower_seams(self) -> None:
        allowed = ("backend_catalog", "effects", "ir", "value")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/execution_contract.rs").write_text(
                "".join(f"use crate::{module}::Boundary;\n" for module in allowed),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_execution_contract_lower_seams_cannot_form_reverse_cycles(self) -> None:
        for relative in (
            "src/parser.rs",
            "src/syntax_dialect.rs",
            "src/effects.rs",
            "src/value.rs",
            "src/placement/protocol/target.rs",
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
                    (root / "src/eval_core.rs").write_text(
                        f"use crate::{module}::Boundary;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("graph-evaluation contract", result.stderr)

    def test_eval_core_allowlist_rejects_an_unanticipated_root(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/eval_core.rs").write_text(
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
            (root / "src/eval_core.rs").write_text(
                "".join(f"use crate::{module}::Boundary;\n" for module in allowed),
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_every_executor_module_requires_eval_core_instead_of_evaluator(self) -> None:
        for relative in (
            "src/executor/actor.rs",
            "src/executor/cancellation.rs",
            "src/executor/coordinator.rs",
            "src/executor/effects.rs",
            "src/executor/mod.rs",
            "src/executor/parallel.rs",
            "src/executor/pool.rs",
            "src/executor/task.rs",
            "src/executor/trace.rs",
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
            (root / "src/executor/coordinator.rs").write_text(
                "use crate::eval_core::GraphEvaluationHost;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_eval_core_lower_dependencies_predeny_reverse_edges(self) -> None:
        for relative in (
            "src/backend_catalog.rs",
            "src/capability.rs",
            "src/execution_contract.rs",
            "src/ir.rs",
            "src/value.rs",
            "src/evidence/analyze.rs",
            "src/evidence/mod.rs",
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
            "src/evidence/admit.rs",
            "src/evidence/analyze.rs",
            "src/evidence/intent.rs",
            "src/world/grounding.rs",
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
            (root / "src/evidence/analyze.rs").write_text(
                "use crate::execution_contract::Policy;\n", encoding="utf-8"
            )
            (root / "src/world/grounding.rs").write_text(
                "use crate::execution_contract::BlockOptions;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unit_test_import_does_not_define_production_geometry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
                "use crate :: { ir :: PlanNodeId, value::OValue };\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("forbidden dependency `crate::{ir::...}`", result.stderr)

    def test_raw_identifiers_cannot_obscure_forbidden_root_modules(self) -> None:
        cases = (
            ("src/parser.rs", "use crate::r#ir::PlanNodeId;\n", "crate::ir"),
            (
                "src/placement/protocol/target.rs",
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
                    (root / "src/parser.rs").write_text(
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
                    (root / "src/parser.rs").write_text(
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
                        (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
            (root / "src/parser.rs").write_text(
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
                    (root / "src/parser.rs").write_text(source, encoding="utf-8")
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
                    (root / "src/evidence/admit.rs").write_text(source, encoding="utf-8")
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn(expected, result.stderr)
                self.assertIn("require explicit root paths", result.stderr)

    def test_visibility_and_extern_crate_roots_are_not_dependencies(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/parser.rs").write_text(
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
                    (root / "src/evidence/admit.rs").write_text(
                        source, encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_precise_capture_use_keyword_is_not_an_import(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/evidence/admit.rs").write_text(
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
            (root / "src/evidence/admit.rs").write_text(
                "macro_rules! keyword { () => { use } }\n"
                "pub(crate) struct CrateVisible;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_direct_super_chains_expose_forbidden_modules(self) -> None:
        cases = (
            ("src/parser.rs", "use super::ir::PlanNodeId;\n", "super::ir"),
            ("src/eval.rs", "use super::world::ArtifactId;\n", "super::world"),
            (
                "src/placement/protocol/target.rs",
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
            (root / "src/placement/protocol/target.rs").write_text(
                "use super::digest::validate_token;\n",
                encoding="utf-8",
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_excessive_super_hops_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/placement/protocol/target.rs").write_text(
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
            (root / "src/placement/protocol/target.rs").write_text(
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
            (root / "src/placement/protocol/target.rs").write_text(
                "use super::digest::validate_token;\n", encoding="utf-8"
            )
            (root / "src/evidence/admit.rs").write_text(
                "use super::fact::EvidenceFact;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_malformed_test_item_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/parser.rs").write_text(
                "#[cfg(test)]\nmod tests {\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("could not analyze Rust tokens", result.stderr)

    def test_malformed_cfg_operator_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/parser.rs").write_text(
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
                    (root / "src/parser.rs").write_text(source, encoding="utf-8")
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("could not analyze Rust tokens", result.stderr)

    def test_unclosed_literal_cannot_hide_a_dependency(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/parser.rs").write_text(
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
            (root / "src/placement/protocol/target.rs").write_text(
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
                    (root / "src/placement/protocol/catalog.rs").write_text(
                        source, encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("canonical placement protocol", result.stderr)

    def test_protocol_can_depend_on_shared_resource_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/placement/protocol/catalog.rs").write_text(
                "use crate::resource_identity::ArtifactId;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_registry_cannot_import_public_placement_facade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/registry/model.rs").write_text(
                "use crate::placement::NodeProfileV1;\n", encoding="utf-8"
            )
            result = run_checker(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("canonical placement_protocol", result.stderr)

    def test_registry_can_import_canonical_placement_protocol(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_minimal_tree(root)
            (root / "src/registry/model.rs").write_text(
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
                    (root / "src/ir.rs").write_text(
                        f"use crate::{module}::SemanticDigestV1;\n", encoding="utf-8"
                    )
                    result = run_checker(root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("HGraph or placement projections", result.stderr)


if __name__ == "__main__":
    unittest.main()
