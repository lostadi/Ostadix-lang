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
        self.assertIn(
            "--bin O --bin olangc --bin o-info",
            workflow,
            "MCP smoke requires all three fixed local release binaries",
        )

    def test_catalog_schema_is_read_from_the_independent_engine(self) -> None:
        self.assertEqual(
            smoke._current_catalog_schema(ROOT),
            "ostadix.backend-catalog/v5",
        )
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('"crates" / "ostadix-api" / "src"', source)
        self.assertNotIn('root / "src" / "backend_catalog.inc.rs"', source)

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
