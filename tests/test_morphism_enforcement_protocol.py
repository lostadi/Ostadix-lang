#!/usr/bin/env python3
"""Exercise native-side enforcement before ordinary OValue lifting."""

import tempfile
import unittest
from pathlib import Path

if __package__:
    from .test_backend_state_protocol import ShimProcess
else:
    from test_backend_state_protocol import ShimProcess


def enforced(source, bindings=None):
    return {
        "cmd": "exec_morphism_v1",
        "contract": "python-plain-data-lossless",
        "request_id": "19" * 32,
        "code": source,
        "bindings": bindings or {},
    }


class MorphismEnforcementProtocolTests(unittest.TestCase):
    def setUp(self):
        self.shim = ShimProcess("python_shim.py")

    def tearDown(self):
        self.shim.close()

    def test_native_input_witness_precedes_program_mutation(self):
        original = {"t": "list", "v": [{"t": "number", "v": {"kind": "int", "v": "1"}}]}
        response = self.shim.request(enforced("payload.append(2)\npayload", {"payload": original}))
        self.assertEqual("morphism_result_v1", response["status"])
        self.assertEqual(original, response["receipt"]["input_witnesses"]["payload"])
        self.assertEqual(2, len(response["receipt"]["value"]["v"]))

    def test_incompatible_input_rejects_before_source_or_actor_binding_mutation(self):
        self.shim.request({"cmd": "exec", "code": "sentinel = 41\nNone", "bindings": {}})
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "effect"
            response = self.shim.request(enforced(
                f"open({str(marker)!r}, 'w').write('effect')\n42",
                {"sentinel": {"t": "bytes", "v": {"bytes": [1], "media_type": None}}},
            ))
            self.assertEqual("err", response["status"])
            self.assertIn("morphism.unsupported-input", response["message"])
            self.assertFalse(marker.exists())
        response = self.shim.request(enforced("sentinel + 1"))
        self.assertEqual("42", response["receipt"]["value"]["v"]["v"])

    def test_shared_cycles_subclasses_and_opaque_results_never_lift(self):
        cases = [
            ("shared=[]\n[shared, shared]", "morphism.identity-required"),
            ("cycle=[]\ncycle.append(cycle)\ncycle", "morphism.identity-required"),
            ("class Sub(int): pass\nSub(42)", "morphism.unsupported-native"),
            ("(1, 2)", "morphism.unsupported-native"),
            ("lambda: 42", "morphism.unsupported-native"),
            ("float('nan')", "morphism.nonfinite-float"),
        ]
        for source, reason in cases:
            with self.subTest(source=source):
                response = self.shim.request(enforced(source))
                self.assertEqual("err", response["status"])
                self.assertIn(reason, response["message"])
                self.assertNotIn("receipt", response)

    def test_unknown_contract_and_nonparticipating_adapter_reject_before_source(self):
        message = enforced("raise AssertionError('source ran')")
        message["contract"] = "invented"
        response = self.shim.request(message)
        self.assertIn("morphism.unsupported-contract", response["message"])
        self.assertNotIn("source ran", response["message"])
        other = ShimProcess("javascript_shim.py")
        try:
            response = other.request(enforced("throw new Error('source ran')"))
            self.assertIn("morphism.unsupported-backend", response["message"])
        finally:
            other.close()


if __name__ == "__main__":
    unittest.main()
