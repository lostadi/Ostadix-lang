"""Input binding and terminal evidence controls for the real Linux native gate."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "kernel_world_real_linux_smoke",
    ROOT / "ocore/kernel/smoke-aarch64-kernel-world-linux-qemu.py",
)
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


class RealLinuxEvidenceTests(unittest.TestCase):
    def transcript(self):
        lines = list(SMOKE.MARKERS)
        linux = next(i for i, line in enumerate(lines) if line.startswith("Linux version "))
        lines[linux] += " (fixture compiler metadata)"
        lines.insert(11, "KW revoked access esr/ipa/pc 0000000082000086 000000004023f9c0 ffffffc0801879c0")
        return "\n".join(lines) + "\n"

    def test_qemu_snapshot_keeps_admitted_bytes_when_source_is_rebuilt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "Image"
            snapshots = root / "private"
            snapshots.mkdir(mode=0o700)
            original = b"original executable payload\x00"
            source.write_bytes(original)
            records = SMOKE.snapshot_inputs({"Image": source}, snapshots)
            source.write_bytes(b"concurrent rebuild with different bytes")
            record = records["Image"]
            self.assertEqual(Path(record["snapshot_path"]).read_bytes(), original)
            self.assertEqual(record["sha256"], hashlib.sha256(original).hexdigest())
            self.assertEqual(record["bytes"], len(original))
            self.assertEqual(record["source_path"], str(source.resolve()))
            self.assertEqual(Path(record["snapshot_path"]).stat().st_mode & 0o222, 0)

    def test_complete_ordered_transcript_accepts_only_expected_kernel(self):
        transcript = self.transcript()
        self.assertTrue(SMOKE.validate_transcript(transcript))
        self.assertFalse(SMOKE.validate_transcript(transcript.replace("6.12.43", "6.12.44")))
        self.assertFalse(SMOKE.validate_transcript(transcript.replace("-kernelworld ", "-kernelworld-other ")))

    def test_partial_final_or_prefixed_marker_is_not_completion(self):
        transcript = self.transcript()
        self.assertFalse(SMOKE.validate_transcript(transcript[:-1]))
        self.assertFalse(SMOKE.validate_transcript(transcript.replace(SMOKE.MARKERS[-1], "echo " + SMOKE.MARKERS[-1])))
        self.assertFalse(SMOKE.validate_transcript(transcript.replace(SMOKE.MARKERS[-1], SMOKE.MARKERS[-1] + " partial")))
        self.assertFalse(SMOKE.validate_transcript(transcript.replace(SMOKE.MARKERS[-1], "prefix\v" + SMOKE.MARKERS[-1])))

    def test_missing_duplicate_and_reordered_semantic_controls_fail(self):
        transcript = self.transcript()
        for marker in SMOKE.MARKERS:
            if marker.startswith("Linux version "):
                marker += " (fixture compiler metadata)"
            with self.subTest(marker=marker):
                self.assertFalse(SMOKE.validate_transcript(transcript.replace(marker + "\n", "")))
                self.assertFalse(SMOKE.validate_transcript(transcript + marker + "\n"))
        first, second = SMOKE.MARKERS[0:2]
        changed = transcript.replace(first + "\n" + second, second + "\n" + first)
        self.assertFalse(SMOKE.validate_transcript(changed))

    def test_terminal_guest_or_monitor_failures_override_success_markers(self):
        for failure in SMOKE.FORBIDDEN:
            with self.subTest(failure=failure):
                self.assertFalse(SMOKE.validate_transcript(self.transcript() + failure + "\n"))

    def test_fault_record_requires_actual_guest_ram_translation_fault(self):
        transcript = self.transcript()
        for original, substitute in (
            ("0000000082000086", "0000000058000000"),
            ("0000000082000086", "000000008200000d"),
            ("000000004023f9c0", "0000000009000000"),
            ("ffffffc0801879c0", "0000000000000000"),
        ):
            with self.subTest(substitute=substitute):
                self.assertFalse(SMOKE.validate_transcript(transcript.replace(original, substitute)))
        detail = next(line for line in transcript.splitlines() if line.startswith("KW revoked access"))
        self.assertFalse(SMOKE.validate_transcript(transcript.replace(detail + "\n", "")))
        self.assertFalse(SMOKE.validate_transcript(transcript + detail + "\n"))


if __name__ == "__main__":
    unittest.main()
