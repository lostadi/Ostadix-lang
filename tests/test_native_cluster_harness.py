"""Guard evidence interpretation and ensure the network fault tool cannot forge replies."""
import importlib.util
import hashlib
import hmac
from pathlib import Path
import struct
import unittest

PATH = Path(__file__).resolve().parents[1] / "ocore/kernel/native-cluster/verify.py"
SPEC = importlib.util.spec_from_file_location("native_cluster_verify", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

GRAPH = hashlib.sha256(struct.pack(">6Q", 2, 1, 2, 3, 0, 0)).hexdigest()
GRAPH_LINE = f"NATIVE_CLUSTER GRAPH {GRAPH}\n"
TASK_LINES = ""
prior = bytes(32)
value = 1
for task, operand in enumerate((2, 3), 1):
    source = hashlib.sha256(struct.pack(">4Q", 1, value, operand, task - 1) + prior).hexdigest()
    value += operand
    prior = hashlib.sha256(struct.pack(">Q", value)).digest()
    TASK_LINES += f"NATIVE_CLUSTER TASK task={task} input={source} output={prior.hex()} result={value}\n"


def check(coordinator, worker, initial, operands, faults):
    return MODULE.result_observation(GRAPH_LINE + "NATIVE_CLUSTER DRAIN_ACK_CANONICAL_OK\n" + coordinator, GRAPH_LINE + TASK_LINES + worker, initial, operands, faults)


class NativeClusterHarnessTests(unittest.TestCase):
    def test_changed_intermediate_digest_cannot_pass_with_same_final_total(self):
        coordinator = GRAPH_LINE + "NATIVE_CLUSTER DRAIN_ACK_CANONICAL_OK\nNATIVE_CLUSTER COMPLETE tasks=2 result=6\nNATIVE_CLUSTER NIC_QUIESCED\n"
        worker = GRAPH_LINE + TASK_LINES.replace("input=", "input=f", 1) + "NATIVE_CLUSTER DRAINED executions=2 duplicates=1 rejected=1\nNATIVE_CLUSTER NIC_QUIESCED\n"
        with self.assertRaises(ValueError):
            MODULE.result_observation(coordinator, worker, 1, [2, 3], False)
    def test_valid_native_observations_include_full_lines(self):
        observation = check("NATIVE_CLUSTER COMPLETE tasks=2 result=6\nNATIVE_CLUSTER NIC_QUIESCED\n", "NATIVE_CLUSTER DRAINED executions=2 duplicates=1 rejected=1\nNATIVE_CLUSTER NIC_QUIESCED\n", 1, [2, 3], True)
        self.assertEqual(observation["executions"], 2)

    def test_evidence_rejects_reexecution_even_if_result_matches(self):
        with self.assertRaises(ValueError):
            check("NATIVE_CLUSTER COMPLETE tasks=2 result=6\nNATIVE_CLUSTER NIC_QUIESCED\n", "NATIVE_CLUSTER DRAINED executions=3 duplicates=1 rejected=1\nNATIVE_CLUSTER NIC_QUIESCED\n", 1, [2, 3], True)

    def test_evidence_requires_observed_fault_denials(self):
        with self.assertRaises(ValueError):
            check("NATIVE_CLUSTER COMPLETE tasks=2 result=6\nNATIVE_CLUSTER NIC_QUIESCED\n", "NATIVE_CLUSTER DRAINED executions=2 duplicates=1 rejected=0\nNATIVE_CLUSTER NIC_QUIESCED\n", 1, [2, 3], True)

    def test_evidence_rejects_duplicate_completion(self):
        with self.assertRaises(ValueError):
            check("NATIVE_CLUSTER COMPLETE tasks=2 result=6\n" * 2 + "NATIVE_CLUSTER NIC_QUIESCED\n", "NATIVE_CLUSTER DRAINED executions=2 duplicates=1 rejected=1\nNATIVE_CLUSTER NIC_QUIESCED\n", 1, [2, 3], False)

    def test_truncated_serial_line_cannot_become_completion(self):
        with self.assertRaises(ValueError):
            check("NATIVE_CLUSTER COMPLETE tasks=2 result=6", "NATIVE_CLUSTER DRAINED executions=2 duplicates=1 rejected=1\n", 1, [2, 3], False)

    def test_task_completion_does_not_substitute_for_device_quiescence(self):
        with self.assertRaises(ValueError):
            check("NATIVE_CLUSTER COMPLETE tasks=2 result=6\n", "NATIVE_CLUSTER DRAINED executions=2 duplicates=1 rejected=1\n", 1, [2, 3], False)

    def test_boot_record_keeps_identity_and_operands_exact(self):
        config = MODULE.boot_config(2, b"n" * 32, b"k" * 32, 0xFFFFFFFF, [1, 2])
        self.assertEqual(len(config), 160)
        self.assertEqual(struct.unpack_from(">QQQQ", config, 8), (2, 1, 1, 1))
        self.assertEqual(struct.unpack_from(">QQQ4Q", config, 104), (2, 2, 0xFFFFFFFF, 1, 2, 0, 0))

    def test_relay_changes_only_authentication_byte_and_never_constructs_result(self):
        frame = bytearray(334)
        frame[12:14] = b"\x88\xb5"
        frame[24:26] = b"\0\1"
        frame = bytes(frame)
        perturbation = MODULE.FramePerturbation()
        damaged, original, duplicate = perturbation.apply(frame)
        self.assertEqual(original, frame)
        self.assertEqual(duplicate, frame)
        self.assertEqual(damaged[:-1], frame[:-1])
        self.assertEqual(damaged[-1], frame[-1] ^ 1)
        result = bytearray(frame)
        result[24:26] = b"\0\2"
        self.assertEqual(perturbation.apply(bytes(result)), [])
        self.assertEqual(perturbation.apply(bytes(result)), [])
        self.assertEqual(perturbation.apply(bytes(result)), [bytes(result)])

    def test_delegation_challenges_have_valid_hmac_and_remain_requests(self):
        frame = bytearray(334)
        frame[12:14] = b"\x88\xb5"
        frame[24:26] = b"\0\1"
        perturbation = MODULE.FramePerturbation(b"k" * 32)
        challenges = perturbation.apply(bytes(frame))[1:-2]
        self.assertEqual(len(challenges), 5)
        for challenge in challenges:
            self.assertEqual(challenge[24:26], b"\0\1")
            self.assertEqual(challenge[-32:], hmac.digest(b"k" * 32, challenge[14:-32], "sha256"))


if __name__ == "__main__":
    unittest.main()
