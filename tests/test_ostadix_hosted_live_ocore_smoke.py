#!/usr/bin/env python3

import importlib.util
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "ocore/kernel/smoke-x86_64-hosted-live-ocore-qemu.py"


def _load():
    spec = importlib.util.spec_from_file_location("hosted_live_ocore_smoke", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


SMOKE = _load()


class HostedLiveOcoreSmokeTests(unittest.TestCase):
    def _fixture(self, root: Path, *, forbidden: bool = False) -> tuple[Path, Path, Path]:
        iso = root / "hosted.iso"
        firmware = root / "OVMF.fd"
        qemu = root / "fake-qemu"
        iso.write_bytes(b"exact hosted ISO fixture")
        firmware.write_bytes(b"exact OVMF fixture")
        marker_lines = "\n".join(
            f"print({marker.decode('ascii')!r}, flush=True)"
            for marker in SMOKE.REQUIRED_MARKERS
        )
        forbidden_line = "print('Kernel panic', flush=True)" if forbidden else ""
        qemu.write_text(
            textwrap.dedent(
                f"""\
                #!/usr/bin/env python3
                import sys
                import time

                arguments = sys.argv[1:]
                if "-kernel" in arguments or arguments[arguments.index("-nic") + 1] != "none":
                    raise SystemExit(9)
                print("OSTADIX O-core [direct Multiboot2]", flush=True)
                if sys.stdin.buffer.read(1) != b"o":
                    raise SystemExit(8)
                {marker_lines.replace(chr(10), chr(10) + '                ')}
                {forbidden_line}
                time.sleep(1.2)
                if sys.stdin.buffer.read(2) != b"\\x01x":
                    raise SystemExit(7)
                """
            ),
            encoding="utf-8",
        )
        qemu.chmod(0o755)
        return iso, firmware, qemu

    def test_gate_selects_grub_ocore_and_requires_sustained_liveness(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            iso, firmware, qemu = self._fixture(Path(directory))
            result = SMOKE.run_ocore_gate(
                iso,
                firmware,
                qemu=str(qemu),
                timeout_seconds=5,
            )
        self.assertEqual(result["schema"], "ostadix.hosted-live-ocore-qemu-smoke/v1")
        self.assertEqual(result["selected_entry"], "ocore")
        self.assertEqual(result["selection_method"], "grub-hotkey-o")
        self.assertEqual(result["markers"], [m.decode("ascii") for m in SMOKE.REQUIRED_MARKERS])
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(result["network"], "none")
        self.assertFalse(result["physical_hardware_proof"])

    def test_gate_rejects_a_failure_after_heartbeat(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            iso, firmware, qemu = self._fixture(Path(directory), forbidden=True)
            with self.assertRaisesRegex(SMOKE.OcoreSmokeError, "forbidden fragment"):
                SMOKE.run_ocore_gate(
                    iso,
                    firmware,
                    qemu=str(qemu),
                    timeout_seconds=5,
                )

    def test_deadline_after_heartbeat_is_a_typed_gate_failure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            iso, firmware, qemu = self._fixture(Path(directory))
            with self.assertRaisesRegex(
                SMOKE.OcoreSmokeError, "post-heartbeat liveness"
            ):
                SMOKE.run_ocore_gate(
                    iso,
                    firmware,
                    qemu=str(qemu),
                    timeout_seconds=1,
                )

    def test_gate_command_cannot_bypass_firmware_media_or_enable_network(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('"-nic", "none"', source)
        self.assertIn('if "-kernel" in command', source)
        self.assertIn('"grub-hotkey-o"', source)
        self.assertNotIn('"-kernel", str(', source)


if __name__ == "__main__":
    unittest.main()
