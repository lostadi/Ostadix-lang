#!/usr/bin/env python3
"""Backend-owned state protocol conformance for all bundled Python shims."""

import os
import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BACKENDS = ROOT / "backends"
sys.path.insert(0, str(BACKENDS))
import o_shim_common as wire  # noqa: E402


SEMANTIC = {"python_shim.py", "sql_shim.py"}
EXTERNAL = {"ubuntu_vm_shim.py"}
ALL_SHIMS = sorted(path.name for path in BACKENDS.glob("*_shim.py"))
STATELESS = set(ALL_SHIMS) - SEMANTIC - EXTERNAL


class ShimProcess:
    def __init__(self, shim_name, session_byte="11"):
        environment = os.environ.copy()
        environment["PYTHONDONTWRITEBYTECODE"] = "1"
        environment["O_BACKEND_SESSION_ID"] = session_byte * 32
        self.process = subprocess.Popen(
            [sys.executable, str(BACKENDS / shim_name)],
            cwd=BACKENDS,
            env=environment,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def request(self, message):
        payload = wire.cbor_encode(message)
        self.process.stdin.write(len(payload).to_bytes(4, "big") + payload)
        self.process.stdin.flush()
        response = wire.read_wire_message(self.process.stdout)
        if response is None:
            stderr = self.process.stderr.read().decode("utf-8", errors="replace")
            raise AssertionError(f"shim closed without a response: {stderr}")
        return response

    def close(self):
        if self.process.poll() is None:
            response = self.request({"cmd": "shutdown"})
            if response != {"status": "ok", "value": {"t": "null"}}:
                raise AssertionError(f"invalid shutdown acknowledgement: {response!r}")
        self.process.stdin.close()
        try:
            returncode = self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
            raise
        stderr = self.process.stderr.read().decode("utf-8", errors="replace")
        self.process.stdout.close()
        self.process.stderr.close()
        if returncode != 0:
            raise AssertionError(f"shim exited {returncode}: {stderr}")


class BackendStateProtocolTests(unittest.TestCase):
    maxDiff = None

    def test_all_22_shims_report_their_truthful_state_tier(self):
        self.assertEqual(22, len(ALL_SHIMS))
        for shim_name in ALL_SHIMS:
            with self.subTest(shim=shim_name):
                shim = ShimProcess(shim_name)
                try:
                    response = shim.request({"cmd": "state_capabilities_v1"})
                    self.assertEqual("state_capabilities_v1", response.get("status"))
                    capabilities = response["capabilities"]
                    expected = (
                        "semantic_snapshot"
                        if shim_name in SEMANTIC
                        else "external_pinned"
                        if shim_name in EXTERNAL
                        else "stateless"
                    )
                    self.assertEqual(expected, capabilities["tier"])
                    self.assertEqual(
                        shim_name.removesuffix("_shim.py"), capabilities["backend"]
                    )
                    self.assertEqual(
                        shim_name not in EXTERNAL, capabilities["restore_supported"]
                    )
                finally:
                    shim.close()

    def test_all_19_stateless_shims_round_trip_canonical_empty_state(self):
        self.assertEqual(19, len(STATELESS))
        for shim_name in sorted(STATELESS):
            with self.subTest(shim=shim_name):
                shim = ShimProcess(shim_name)
                try:
                    response = shim.request(
                        {"cmd": "checkpoint_v1", "max_bytes": 1024 * 1024}
                    )
                    self.assertEqual("checkpoint_v1", response.get("status"))
                    checkpoint = response["checkpoint"]
                    self.assertEqual("stateless", checkpoint["tier"])
                    self.assertEqual({"kind": "empty"}, checkpoint["payload"])
                    restored = shim.request(
                        {"cmd": "restore_v1", "checkpoint": checkpoint}
                    )
                    self.assertEqual("restore_v1", restored.get("status"))
                    self.assertTrue(restored["receipt"]["restored"])
                finally:
                    shim.close()

    def test_python_graph_checkpoint_preserves_aliases_and_cycles(self):
        source = ShimProcess("python_shim.py")
        try:
            ready = source.request({
                "cmd": "exec",
                "code": "x = []\nx.append(x)\ny = x\n'ready'",
                "bindings": {},
            })
            self.assertEqual({"t": "str", "v": "ready"}, ready["value"])
            response = source.request(
                {"cmd": "checkpoint_v1", "max_bytes": 1024 * 1024}
            )
            self.assertEqual("checkpoint_v1", response.get("status"))
            checkpoint = response["checkpoint"]
        finally:
            source.close()

        target = ShimProcess("python_shim.py", session_byte="22")
        try:
            restored = target.request({"cmd": "restore_v1", "checkpoint": checkpoint})
            self.assertEqual("restore_v1", restored.get("status"))
            result = target.request({
                "cmd": "exec",
                "code": "x is y and x[0] is x",
                "bindings": {},
            })
            self.assertEqual({"t": "bool", "v": True}, result["value"])
        finally:
            target.close()

    def test_python_unsupported_object_pins_without_destroying_session(self):
        shim = ShimProcess("python_shim.py")
        try:
            shim.request({
                "cmd": "exec",
                "code": "f = lambda: 41 + 1\n'ready'",
                "bindings": {},
            })
            response = shim.request(
                {"cmd": "checkpoint_v1", "max_bytes": 1024 * 1024}
            )
            self.assertEqual("state_pin_required_v1", response.get("status"))
            self.assertEqual("$globals['f']", response["reason"]["path"])
            still_live = shim.request({
                "cmd": "exec", "code": "f()", "bindings": {}
            })
            self.assertEqual({"t": "int", "v": 42}, still_live["value"])
        finally:
            shim.close()

    def test_sql_database_checkpoint_restores_rows(self):
        source = ShimProcess("sql_shim.py")
        try:
            source.request({
                "cmd": "exec",
                "code": "CREATE TABLE items(value INTEGER); INSERT INTO items VALUES (42);",
                "bindings": {},
            })
            response = source.request(
                {"cmd": "checkpoint_v1", "max_bytes": 4 * 1024 * 1024}
            )
            self.assertEqual("checkpoint_v1", response.get("status"))
            checkpoint = response["checkpoint"]
            self.assertEqual("ostadix.sqlite-python-main/v1", checkpoint["codec"])
        finally:
            source.close()

        target = ShimProcess("sql_shim.py", session_byte="22")
        try:
            restored = target.request({"cmd": "restore_v1", "checkpoint": checkpoint})
            self.assertEqual("restore_v1", restored.get("status"))
            result = target.request({
                "cmd": "exec", "code": "SELECT value FROM items;", "bindings": {}
            })
            self.assertEqual({"t": "int", "v": 42}, result["value"])
        finally:
            target.close()

    def test_ubuntu_checkpoint_is_a_distinct_external_resource_manifest(self):
        checkpoints = []
        for session_byte in ("11", "22"):
            shim = ShimProcess("ubuntu_vm_shim.py", session_byte=session_byte)
            try:
                response = shim.request(
                    {"cmd": "checkpoint_v1", "max_bytes": 1024 * 1024}
                )
                self.assertEqual("checkpoint_v1", response.get("status"))
                checkpoints.append(response["checkpoint"])
            finally:
                shim.close()
        first, second = checkpoints
        self.assertNotEqual(first["payload"]["vm_name"], second["payload"]["vm_name"])
        self.assertNotEqual(
            first["external_resources"][0]["identity"],
            second["external_resources"][0]["identity"],
        )

        shim = ShimProcess("ubuntu_vm_shim.py")
        try:
            response = shim.request({"cmd": "restore_v1", "checkpoint": first})
            self.assertEqual("state_pin_required_v1", response.get("status"))
            self.assertEqual("continue-pinned", response["reason"]["recovery"])
        finally:
            shim.close()

    def test_unknown_state_version_and_too_small_bound_fail_explicitly(self):
        shim = ShimProcess("bash_shim.py")
        try:
            unknown = shim.request({"cmd": "checkpoint_v2", "max_bytes": 1024})
            self.assertEqual("err", unknown.get("status"))
            bounded = shim.request({"cmd": "checkpoint_v1", "max_bytes": 1})
            self.assertEqual("state_error_v1", bounded.get("status"))
            self.assertEqual("state.checkpoint-failed", bounded["error"]["code"])
        finally:
            shim.close()


if __name__ == "__main__":
    unittest.main()
