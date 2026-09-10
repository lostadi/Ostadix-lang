#!/usr/bin/env python3
"""Actual Python shim ownership, identity, lifetime, and checkpoint boundaries."""

import copy
import unittest

if __package__:
    from .test_backend_state_protocol import ShimProcess
else:
    from test_backend_state_protocol import ShimProcess


class PythonNativeHandleTests(unittest.TestCase):
    def setUp(self):
        self.owner = ShimProcess("python_shim.py")

    def tearDown(self):
        self.owner.close()

    def execute(self, code, bindings=None, shim=None):
        return (shim or self.owner).request({
            "cmd": "exec", "code": code, "bindings": bindings or {},
        })

    def exported(self, source="O.native([1, 2])"):
        response = self.execute(source)
        self.assertEqual("ok", response.get("status"), response)
        capsule = response["value"]
        self.assertEqual("native", capsule["t"])
        self.assertEqual("live_handle", capsule["v"]["safety"])
        self.assertEqual("same_process", capsule["v"]["rehydrate"])
        self.assertIsNone(capsule["v"]["payload"])
        return capsule

    def test_arbitrary_instance_closure_cycles_and_aliases_keep_python_identity(self):
        handle = self.exported("""
class NativeThing:
    def __repr__(self):
        raise RuntimeError('repr must not be used to export an object')
    def __reduce__(self):
        raise RuntimeError('pickle must not be used to export an object')
def factory(base):
    return lambda extra: base + extra
instance = NativeThing()
instance.self = instance
shared = []
shared.append(shared)
value = {'instance': instance, 'closure': factory(40), 'left': shared, 'right': shared,
         'generator': (x for x in [2]), 'arbitrary': object()}
O.native(value)
""")
        response = self.execute("""
resolved = O.resolve_native(handle)
again = O.resolve_native(handle)
resolved is value and again is value and resolved['instance'] is instance and \
resolved['instance'].self is instance and resolved['left'] is resolved['right'] and \
resolved['left'][0] is resolved['left'] and resolved['closure'](2) == 42 and \
next(resolved['generator']) == 2 and resolved['arbitrary'] is value['arbitrary']
""", {"handle": handle})
        self.assertEqual({"status": "ok", "value": {"t": "bool", "v": True}}, response)

    def test_export_metadata_does_not_execute_metaclass_properties_or_lookup(self):
        handle = self.exported("""
metadata_effects = []
class Meta(type):
    @property
    def __module__(cls):
        metadata_effects.append('module-property')
        raise RuntimeError('metadata property must not execute')
    def __getattribute__(cls, name):
        if name in ('__module__', '__qualname__'):
            metadata_effects.append(name)
            raise RuntimeError('metadata lookup must not execute')
        return super().__getattribute__(name)
class NativeThing(metaclass=Meta):
    pass
instance = NativeThing()
O.native(instance)
""")
        self.assertTrue(handle["v"]["type_name"].endswith(".NativeThing"), handle)
        response = self.execute(
            "O.resolve_native(handle) is instance and metadata_effects == []",
            {"handle": handle},
        )
        self.assertEqual({"status": "ok", "value": {"t": "bool", "v": True}}, response)

    def test_descriptor_can_be_carried_by_another_owner_but_not_resolved_there(self):
        handle = self.exported("target = lambda: 42\nO.native(target)")
        other = ShimProcess("python_shim.py")  # Same session label must not identify an owner.
        try:
            carried = self.execute("handle", {"handle": handle}, other)
            self.assertEqual(handle, carried["value"])
            refused = self.execute("O.resolve_native(handle)", {"handle": handle}, other)
            self.assertEqual("err", refused["status"])
            self.assertIn("native.owner-mismatch", refused["message"])
            resolved = self.execute("O.resolve_native(handle)()", {"handle": carried["value"]})
            self.assertEqual({"t": "int", "v": 42}, resolved["value"])
        finally:
            other.close()

    def test_altered_descriptor_cannot_resolve_or_release_the_original(self):
        handle = self.exported()
        for mutation in ("type_name", "codec", "metadata", "safety"):
            changed = copy.deepcopy(handle)
            if mutation == "metadata":
                changed["v"][mutation]["owner"]["v"]["utf8"] = "altered"
            else:
                changed["v"][mutation] = "altered"
            for operation in ("resolve_native", "release_native"):
                response = self.execute(f"O.{operation}(handle)", {"handle": changed})
                self.assertEqual("err", response["status"])
                self.assertIn("native.altered-handle", response["message"])
        response = self.execute("O.resolve_native(handle) == [1, 2]", {"handle": handle})
        self.assertEqual({"t": "bool", "v": True}, response["value"])

    def test_release_invalidates_all_descriptor_copies_and_unpins_checkpoint(self):
        handle = self.exported()
        pinned = self.owner.request({"cmd": "checkpoint_v1", "max_bytes": 1024 * 1024})
        self.assertEqual("state_pin_required_v1", pinned["status"])
        self.assertEqual("$native_handles", pinned["reason"]["path"])
        released = self.execute("O.release_native(handle)", {"handle": handle})
        self.assertEqual({"t": "null"}, released["value"])
        for operation in ("resolve_native", "release_native"):
            stale = self.execute(f"O.{operation}(handle)", {"handle": copy.deepcopy(handle)})
            self.assertEqual("err", stale["status"])
            self.assertIn("native.handle-expired", stale["message"])
        checkpoint = self.owner.request({"cmd": "checkpoint_v1", "max_bytes": 1024 * 1024})
        self.assertEqual("checkpoint_v1", checkpoint["status"], checkpoint)

    def test_cleanup_invalidates_live_exports(self):
        handle = self.exported()
        self.assertEqual("ok", self.owner.request({"cmd": "cleanup"})["status"])
        stale = self.execute("O.resolve_native(handle)", {"handle": handle})
        self.assertEqual("err", stale["status"])
        self.assertIn("native.handle-expired", stale["message"])

    def test_new_process_cannot_resurrect_handle_after_original_owner_exits(self):
        owner = ShimProcess("python_shim.py")
        try:
            handle = self.execute("O.native(lambda: 42)", shim=owner)["value"]
        finally:
            owner.close()
        stale = self.execute("O.resolve_native(handle)", {"handle": handle})
        self.assertEqual("err", stale["status"])
        self.assertIn("native.owner-mismatch", stale["message"])

    def test_capacity_exhaustion_preserves_existing_handles_and_release_returns_capacity(self):
        response = self.execute("""
from o_native_objects import NativeObjectStore
store = NativeObjectStore(max_handles=2)
first = store.export(object())
second = store.export(object())
original = store.resolve(first)
try:
    store.export(object())
    bounded = False
except ValueError as error:
    bounded = 'native.capacity-exhausted' in str(error)
unchanged = store.resolve(first) is original
store.release(second)
third = store.export(object())
bounded and unchanged and store.live_count == 2 and store.resolve(first) is original
""")
        self.assertEqual({"status": "ok", "value": {"t": "bool", "v": True}}, response)

    def test_plain_data_contract_rejects_export_without_allocating_an_owner_handle(self):
        response = self.owner.request({
            "cmd": "exec_morphism_v1", "contract": "python-plain-data-lossless",
            "request_id": "31" * 32, "code": "O.native([1, 2])", "bindings": {},
        })
        self.assertEqual("err", response["status"])
        self.assertIn("morphism.unsupported-native", response["message"])
        checkpoint = self.owner.request({"cmd": "checkpoint_v1", "max_bytes": 1024 * 1024})
        self.assertEqual("checkpoint_v1", checkpoint["status"], checkpoint)

    def operation(self, handle, operation, arguments=(), request_id="ab" * 32):
        response = self.owner.request({"cmd": "native_operation_v1", "request_id": request_id,
                                       "handle": handle, "operation": operation,
                                       "arguments": list(arguments)})
        self.assertEqual("native_operation_result_v1", response.get("status"), response)
        self.assertEqual(request_id, response["request_id"])
        return response["value"]

    def test_owner_operations_preserve_mutable_result_identity_and_validate_before_effects(self):
        handle = self.exported("""
effects = []
class Box:
    def __call__(self, other):
        effects.append('call')
        return other
box = Box()
O.native(box)
""")
        invalid = copy.deepcopy(handle)
        invalid["v"]["codec"] = "altered"
        rejected = self.operation(handle, "call", [invalid])
        self.assertEqual("error", rejected["t"])
        self.assertIn("native.altered-handle", rejected["msg"])
        self.assertEqual({"t": "bool", "v": True}, self.execute("effects == []")["value"])
        result = self.operation(handle, "call", [handle])
        self.assertEqual("native", result["t"])
        self.assertEqual({"t": "bool", "v": True}, self.execute(
            "O.resolve_native(result) is box and effects == ['call']", {"result": result})["value"])
        self.operation(result, "release")
        expired = self.operation(result, "call")
        self.assertEqual("error", expired["t"])
        self.assertIn("native.handle-expired", expired["msg"])

    def test_native_operations_can_interleave_at_an_explicit_eval_callback_boundary(self):
        handle = self.exported("O.native(lambda: 42)")
        callback = self.execute("O.eval('text' + '^(callback)_text')")
        self.assertEqual("eval_request", callback["status"])
        result = self.operation(handle, "call")
        self.assertEqual({"t": "number", "v": {"kind": "int", "v": "42"}}, result)
        settled = self.owner.request({"cmd": "eval_result", "value": {"t": "int", "v": 7}})
        self.assertEqual({"status": "ok", "value": {"t": "int", "v": 7}}, settled)


if __name__ == "__main__":
    unittest.main()
