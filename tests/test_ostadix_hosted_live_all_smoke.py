#!/usr/bin/env python3

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "ocore/kernel/smoke-x86_64-hosted-live-all.py"


def _load():
    spec = importlib.util.spec_from_file_location("hosted_live_all_smoke", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


SMOKE = _load()


class HostedLiveAllSmokeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.iso = self.root / "input.iso"
        self.iso.write_bytes(b"one exact Hosted Live ISO")
        self.firmware = self.root / "OVMF_CODE.fd"
        self.firmware.write_bytes(b"one exact OVMF code image")
        self.qemu = self.root / "qemu-system-x86_64"
        self.qemu.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.qemu.chmod(0o755)
        self.serial = self._program("serial.py")
        self.visual = self._program("visual.py")
        self.ocore = self._program("ocore.py")
        self.runner = self._program("runner.sh", executable=True)
        self.iso_identity = self._identity(self.iso)
        self.firmware_identity = self._identity(self.firmware)
        self.wasm_evidence = {
            "staged_tree": "7" * 40,
            "bytes": 123,
            "sha256": "6" * 64,
            "materialized_project_sha256": "8" * 64,
        }

    def _program(self, name: str, *, executable: bool = False) -> Path:
        path = self.root / name
        path.write_text("fixture\n", encoding="utf-8")
        if executable:
            path.chmod(0o755)
        return path

    def _identity(self, path: Path) -> dict[str, object]:
        payload = path.read_bytes()
        return {"bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}

    def _payload(self, label: str, iso: Path) -> dict[str, object]:
        result = {
            "schema": SMOKE.GATE_SCHEMAS[label],
            "iso": self._identity(iso),
            "acceleration": "tcg",
            "physical_hardware_proof": False,
        }
        if label == "serial":
            result.update(
                {
                    "markers": ["OSTADIX HOSTED LIVE READY"],
                    "transcript_bytes": 23,
                    "transcript_sha256": "1" * 64,
                    "rootfs": {"bytes": 17, "sha256": "2" * 64},
                    "entropy": {
                        "device": "virtio-rng-pci",
                        "crng_bytes": 32,
                        "available": 256,
                    },
                    "olangc_wasm": dict(self.wasm_evidence),
                    "firmware_path": "ovmf-through-capacity-runner",
                }
            )
        else:
            result["network"] = "none"
            if label == "graphical":
                result["olangc_wasm"] = dict(self.wasm_evidence)
        if label in ("serial", "ocore"):
            result["exit_code"] = 0
        if label in ("graphical", "ocore"):
            result["firmware"] = self.firmware_identity
        return result

    def _run(
        self,
        behavior=None,
    ) -> tuple[dict[str, object], list[tuple[list[str], dict[str, object]]]]:
        calls: list[tuple[list[str], dict[str, object]]] = []

        def invoke(command, **kwargs):
            command = list(command)
            calls.append((command, kwargs))
            label = {
                str(self.serial): "serial",
                str(self.visual): "graphical",
                str(self.ocore): "ocore",
            }[command[1]]
            if behavior is not None:
                custom = behavior(label, command, kwargs, len(calls))
                if custom is not None:
                    return custom
            return subprocess.CompletedProcess(
                command,
                0,
                stdout=json.dumps(self._payload(label, Path(command[2]))),
            )

        result = SMOKE.run_all_gates(
            self.iso,
            qemu=self.qemu,
            firmware=self.firmware,
            hosted_timeout_seconds=321,
            ocore_timeout_seconds=87,
            serial_smoke=self.serial,
            visual_smoke=self.visual,
            ocore_smoke=self.ocore,
            capacity_runner=self.runner,
            run_process=invoke,
        )
        return result, calls

    def test_runs_exact_snapshot_in_order_with_bound_arguments_and_timeouts(self) -> None:
        observed_snapshots: list[Path] = []

        def inspect_snapshot(label, command, _kwargs, _count):
            snapshot = Path(command[2])
            observed_snapshots.append(snapshot)
            self.assertNotEqual(snapshot, self.iso)
            self.assertEqual(snapshot.read_bytes(), self.iso.read_bytes())
            self.assertEqual(stat.S_IMODE(snapshot.stat().st_mode), 0o400)
            self.assertEqual(stat.S_IMODE(snapshot.parent.stat().st_mode), 0o700)
            return None

        result, calls = self._run(inspect_snapshot)
        self.assertEqual(
            [Path(command[1]).name for command, _ in calls],
            [self.serial.name, self.visual.name, self.ocore.name],
        )
        self.assertEqual(len(set(observed_snapshots)), 1)
        self.assertFalse(observed_snapshots[0].exists())
        serial_args, visual_args, ocore_args = [command for command, _ in calls]
        self.assertEqual(
            serial_args[3:],
            ["--runner", str(self.runner), "--timeout", "321"],
        )
        self.assertEqual(visual_args[-2:], ["--timeout", "321"])
        self.assertEqual(ocore_args[-2:], ["--timeout", "87"])
        for _command, kwargs in calls:
            self.assertEqual(kwargs["env"]["OCORE_QEMU_BIN"], str(self.qemu))
            self.assertEqual(kwargs["env"]["OSTADIX_OVMF_CODE"], str(self.firmware))
        self.assertEqual(result["schema"], SMOKE.AGGREGATE_SCHEMA)
        self.assertEqual(
            SMOKE.AGGREGATE_SCHEMA,
            "ostadix.hosted-live-qemu-smoke-all/v2",
        )
        self.assertEqual(
            SMOKE.GATE_SCHEMAS,
            {
                "serial": "ostadix.hosted-live-qemu-smoke/v4",
                "graphical": "ostadix.hosted-live-qemu-visual-smoke/v7",
                "ocore": "ostadix.hosted-live-ocore-qemu-smoke/v1",
            },
        )
        self.assertEqual(result["gate_order"], ["serial", "graphical", "ocore"])
        self.assertEqual(result["iso"], self.iso_identity)
        self.assertEqual(result["firmware"], self.firmware_identity)
        self.assertEqual(set(result["smoke"]), {"serial", "graphical", "ocore"})
        self.assertNotIn("network", result["smoke"]["serial"])
        self.assertEqual(
            result["smoke"]["serial"]["firmware_path"],
            "ovmf-through-capacity-runner",
        )
        self.assertEqual(result["smoke"]["serial"]["olangc_wasm"], self.wasm_evidence)
        self.assertEqual(
            result["smoke"]["graphical"]["olangc_wasm"],
            self.wasm_evidence,
        )
        self.assertNotIn("olangc_wasm", result["smoke"]["ocore"])
        self.assertEqual(
            set(result["smoke"]["serial"]),
            {
                "schema",
                "markers",
                "transcript_bytes",
                "transcript_sha256",
                "exit_code",
                "iso",
                "rootfs",
                "acceleration",
                "entropy",
                "olangc_wasm",
                "firmware_path",
                "physical_hardware_proof",
            },
        )

    def test_timeout_domains_are_independent_and_bounded(self) -> None:
        base = dict(
            iso=self.iso,
            qemu=self.qemu,
            firmware=self.firmware,
            serial_smoke=self.serial,
            visual_smoke=self.visual,
            ocore_smoke=self.ocore,
            capacity_runner=self.runner,
        )
        for hosted in (0, 1801, float("nan")):
            with self.subTest(hosted=hosted), self.assertRaisesRegex(
                SMOKE.AggregateSmokeError, "Hosted timeout"
            ):
                SMOKE.run_all_gates(**base, hosted_timeout_seconds=hosted)
        for ocore in (0, 901, float("inf")):
            with self.subTest(ocore=ocore), self.assertRaisesRegex(
                SMOKE.AggregateSmokeError, "O-core timeout"
            ):
                SMOKE.run_all_gates(**base, ocore_timeout_seconds=ocore)

    def test_documented_timeout_environment_overrides_are_parsed_before_bounds(self) -> None:
        old_hosted = os.environ.get("OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT")
        old_ocore = os.environ.get("OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT")
        try:
            os.environ["OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT"] = "1200.5"
            os.environ["OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT"] = "87"
            self.assertEqual(
                SMOKE._environment_timeout(
                    "OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT",
                    SMOKE.DEFAULT_HOSTED_TIMEOUT_SECONDS,
                ),
                1200.5,
            )
            self.assertEqual(
                SMOKE._environment_timeout(
                    "OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT",
                    SMOKE.DEFAULT_OCORE_TIMEOUT_SECONDS,
                ),
                87,
            )
            os.environ["OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT"] = "not-a-number"
            with self.assertRaisesRegex(
                SMOKE.AggregateSmokeError,
                "OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT must be a number",
            ):
                SMOKE._environment_timeout(
                    "OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT",
                    SMOKE.DEFAULT_HOSTED_TIMEOUT_SECONDS,
                )
        finally:
            if old_hosted is None:
                os.environ.pop("OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT", None)
            else:
                os.environ["OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT"] = old_hosted
            if old_ocore is None:
                os.environ.pop("OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT", None)
            else:
                os.environ["OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT"] = old_ocore

    def test_nonzero_gate_fails_fast(self) -> None:
        calls = []

        def fail_serial(label, command, _kwargs, _count):
            calls.append(label)
            return subprocess.CompletedProcess(command, 23, stdout="{}")

        with self.assertRaisesRegex(SMOKE.AggregateSmokeError, "serial gate returned status 23"):
            self._run(fail_serial)
        self.assertEqual(calls, ["serial"])

    def test_malformed_json_fails_fast(self) -> None:
        calls = []

        def malformed(label, command, _kwargs, _count):
            calls.append(label)
            return subprocess.CompletedProcess(command, 0, stdout="not-json")

        with self.assertRaisesRegex(SMOKE.AggregateSmokeError, "malformed JSON"):
            self._run(malformed)
        self.assertEqual(calls, ["serial"])

    def test_unexpected_schema_is_rejected(self) -> None:
        def wrong_schema(label, command, _kwargs, _count):
            payload = self._payload(label, Path(command[2]))
            payload["schema"] = "ostadix.untrusted/v99"
            return subprocess.CompletedProcess(command, 0, stdout=json.dumps(payload))

        with self.assertRaisesRegex(SMOKE.AggregateSmokeError, "unexpected schema"):
            self._run(wrong_schema)

    def test_iso_hash_mismatch_is_rejected(self) -> None:
        def wrong_hash(label, command, _kwargs, _count):
            payload = self._payload(label, Path(command[2]))
            payload["iso"] = dict(payload["iso"])
            payload["iso"]["sha256"] = "0" * 64
            return subprocess.CompletedProcess(command, 0, stdout=json.dumps(payload))

        with self.assertRaisesRegex(SMOKE.AggregateSmokeError, "does not match the snapshot"):
            self._run(wrong_hash)

    def test_qemu_and_explicit_firmware_are_resolved(self) -> None:
        old_path = os.environ.get("PATH")
        try:
            os.environ["PATH"] = str(self.root)
            self.assertEqual(SMOKE.resolve_qemu(self.qemu.name), self.qemu.resolve())
        finally:
            if old_path is None:
                os.environ.pop("PATH", None)
            else:
                os.environ["PATH"] = old_path
        self.assertEqual(
            SMOKE.resolve_firmware(self.qemu, explicit=self.firmware),
            self.firmware.resolve(),
        )

    def test_o_kernel_default_dispatches_the_complete_suite(self) -> None:
        source = (ROOT / "scripts/o-kernel.sh").read_text(encoding="utf-8")
        self.assertIn(
            'HOSTED_LIVE_SMOKE_SCRIPT=${O_KERNEL_HOSTED_LIVE_SMOKE_SCRIPT:-"$ROOT/ocore/kernel/smoke-x86_64-hosted-live-all.py"}',
            source,
        )
        self.assertIn(
            "Boot and assert serial, graphical, and direct O-core readiness",
            source,
        )


if __name__ == "__main__":
    unittest.main()
