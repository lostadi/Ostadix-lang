"""Regression tests for the release MCP stdio smoke client."""

from __future__ import annotations

import importlib.util
import io
import json
from pathlib import Path
import sys
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "smoke_ostadix_mcp.py"
ROOT = SCRIPT.parents[1]
CI_WORKFLOW = SCRIPT.parents[1] / ".github/workflows/ci.yml"
SPEC = importlib.util.spec_from_file_location("ostadix_mcp_smoke", SCRIPT)
if SPEC is None or SPEC.loader is None:  # pragma: no cover - import failure is fatal
    raise RuntimeError(f"cannot import {SCRIPT}")
smoke = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = smoke
SPEC.loader.exec_module(smoke)


def frame(request_id: int, value: str) -> bytes:
    return (
        json.dumps(
            {"jsonrpc": "2.0", "id": request_id, "result": {"value": value}},
            separators=(",", ":"),
        ).encode("utf-8")
        + b"\n"
    )


class ResponseReaderTests(unittest.TestCase):
    def test_batched_frames_remain_available_after_first_response(self) -> None:
        reader = smoke.ResponseReader(io.BytesIO(frame(1, "first") + frame(2, "second")))
        self.assertEqual(reader.response(1, 0.5), {"value": "first"})
        self.assertEqual(reader.response(2, 0.5), {"value": "second"})
        reader.join(0.5)

    def test_ci_builds_every_binary_required_by_the_mcp_smoke(self) -> None:
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        for required_binary in ("O", "olangc", "o-info"):
            self.assertIn(
                f"--bin {required_binary}",
                workflow,
                f"MCP smoke requires the fixed local {required_binary} binary",
            )

    def test_catalog_schema_is_read_from_the_independent_engine(self) -> None:
        self.assertEqual(
            smoke._current_catalog_schema(ROOT),
            "ostadix.backend-catalog/v6",
        )
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('"crates" / "ostadix-api" / "src"', source)
        self.assertNotIn('root / "src" / "backend_catalog.inc.rs"', source)

    def test_installed_layout_uses_explicit_runtime_and_information_paths(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('parser.add_argument("--o-info", type=Path)', source)
        self.assertIn('parser.add_argument("--runtime-bin-dir", type=Path)', source)
        self.assertIn('parser.add_argument("--server-cwd", type=Path)', source)
        self.assertIn("wasm_mode = parser.add_mutually_exclusive_group()", source)
        self.assertIn(
            'wasm_mode.add_argument("--require-wasm", action="store_true")', source
        )
        self.assertIn(
            'wasm_mode.add_argument("--require-wasm-materialization", '
            'action="store_true")',
            source,
        )
        for release_binding in (
            "--wasm-release-manifest",
            "--wasm-release-artifact",
            "--wasm-source-tree",
            "--wasm-base-commit",
            "--wasm-source-archive-sha256",
        ):
            self.assertIn(release_binding, source)
        self.assertIn('environment["OSTADIX_O_INFO_BIN"]', source)
        self.assertIn('environment["PYTHONDONTWRITEBYTECODE"] = "1"', source)
        self.assertIn('environment.pop("A18_WORK", None)', source)
        self.assertIn("restricted_path.insert(0", source)
        self.assertIn("for index, path in enumerate(restricted_path)", source)
        self.assertIn('"name": "o_doctor"', source)
        self.assertIn('"name": "o_search_run"', source)
        self.assertIn('"target": "wasm"', source)
        self.assertIn('wasm_output.read_bytes()[:4] != b"\\x00asm"', source)
        self.assertIn('"materialize_only"', source)
        self.assertIn("o_olangc schema omitted materialize_only", source)
        self.assertIn(
            "olangc: materialize-only target=wasm rust-target=wasm32-wasip1 ",
            source,
        )
        self.assertIn("cargo-invoked=false dir={materialize_project}", source)
        self.assertIn('(materialize_project / "target").exists()', source)
        self.assertIn("or materialize_output.exists()", source)
        self.assertIn(
            '(materialize_project / "src/program.O").read_bytes()', source
        )
        self.assertIn('"path": "examples/wasm_hello.O"', source)
        self.assertIn('root / "examples/wasm_hello.O"', source)
        self.assertIn(
            "MCP-materialized WASM project failed its release binding", source
        )
        self.assertIn("return wasm_materialization_evidence", source)
        self.assertIn(
            "ostadix-mcp o_olangc wasm materialization: PASS ", source
        )
        self.assertIn("ostadix-mcp o_olangc wasm artifact: PASS ", source)
        self.assertNotIn('prefix=".mcp-intent-smoke-", dir=root', source)
        self.assertNotIn(
            '"runtime-search-entry index=0 source=inherited:0 path=/usr/bin"',
            source,
        )

    def test_out_of_order_response_is_retained_by_id(self) -> None:
        reader = smoke.ResponseReader(io.BytesIO(frame(2, "second") + frame(1, "first")))
        self.assertEqual(reader.response(1, 0.5), {"value": "first"})
        self.assertEqual(reader.response(2, 0.5), {"value": "second"})
        reader.join(0.5)

    def test_protocol_error_is_reported(self) -> None:
        payload = b'{"jsonrpc":"2.0","id":7,"error":{"code":-1}}\n'
        reader = smoke.ResponseReader(io.BytesIO(payload))
        with self.assertRaisesRegex(smoke.SmokeError, "MCP request 7 failed"):
            reader.response(7, 0.5)
        reader.join(0.5)


if __name__ == "__main__":
    unittest.main()
