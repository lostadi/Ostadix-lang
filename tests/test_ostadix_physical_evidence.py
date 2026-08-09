from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS))
MODULE_PATH = SCRIPTS / "ostadix_physical_evidence.py"
SPEC = importlib.util.spec_from_file_location("ostadix_physical_evidence", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
EVIDENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVIDENCE)

import ostadix_boot_media as MEDIA
import ostadix_media_writer as MEDIA_WRITER


CHALLENGE = "12" * 32
COMMIT = "ab" * 20
CREATED = "2026-08-09T12:00:00Z"


class PhysicalEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.image = self.root / "ostadix.img"
        self.machine = self.root / "machine.json"
        self.media_write = self.root / "media-write.json"
        self.intent_path = self.root / "intent.json"
        self.transcript = self.root / "serial.log"
        self._write_image(CHALLENGE)
        self._write_machine()
        self._write_media_record()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _write_image(
        self,
        challenge: str,
        *,
        duplicate: bool = False,
        source_commit: str = COMMIT,
        duplicate_source: bool = False,
    ) -> None:
        esp = bytearray(1024 * 1024)
        esp[0:3] = b"\xeb\x58\x90"
        esp[3:11] = b"MSDOS5.0"
        esp[510:512] = b"\x55\xaa"
        token = f"ostadix.challenge={challenge}".encode("ascii")
        esp[1024 : 1024 + len(token)] = token
        source = f"ostadix.source_commit={source_commit}".encode("ascii")
        esp[2048 : 2048 + len(source)] = source
        if duplicate:
            esp[3072 : 3072 + len(token)] = token
        if duplicate_source:
            esp[4096 : 4096 + len(source)] = source
        self.image.write_bytes(MEDIA.build_image(bytes(esp))[0])

    def _write_machine(self, **updates: object) -> None:
        value: dict[str, object] = {
            "schema": EVIDENCE.MACHINE_SCHEMA,
            "architecture": "x86_64",
            "manufacturer": "Example Systems",
            "model": "Evidence Workstation",
            "board": "Board v1",
            "cpu_model": "Example x86_64 CPU",
            "firmware": "Example UEFI 1.0",
            "serial_identity_sha256": "34" * 32,
        }
        value.update(updates)
        self.machine.write_text(json.dumps(value), encoding="utf-8")

    def _write_media_record(self, target_extra_bytes: int = 128 * 512) -> None:
        image = self.image.read_bytes()
        plan = MEDIA.plan_target_image(image, len(image) + target_extra_bytes)
        device = {
            "path": "/dev/sdz",
            "identity": "serial:fixture-usb-device",
            "bytes": plan.target_bytes,
            "model": "Fixture USB",
            "transport": "usb",
            "platform": "linux",
            "device_number": "8:240",
        }
        record = {
            "schema": MEDIA_WRITER.WRITE_SCHEMA,
            "device": device,
            "image": str(self.image),
            "image_bytes": plan.source_bytes,
            "image_sha256": plan.source_sha256,
            "source_image_sha256": plan.source_sha256,
            "target_bytes": plan.target_bytes,
            "target_plan_sha256": plan.target_plan_sha256,
            "target_image_sha256": plan.target_image_sha256,
            "esp_sha256": plan.esp_sha256,
            "target_extents": [extent.public() for extent in plan.extents],
            "unwritten_policy": MEDIA.UNWRITTEN_POLICY,
            "unwritten_ranges": [
                {"offset": offset, "bytes": size}
                for offset, size in plan.unwritten_ranges
            ],
            "target_plan": plan.public(),
            "confirmation": MEDIA_WRITER.confirmation_token_from_public(
                device, plan.public()
            ),
            "written": True,
        }
        self.media_write.write_bytes(EVIDENCE._canonical_bytes(record))

    def _intent(self, cpus: int = 1) -> dict[str, object]:
        value = EVIDENCE.make_intent(
            image_path=self.image,
            media_write_path=self.media_write,
            machine_path=self.machine,
            expected_cpus=cpus,
            source_commit=COMMIT,
            created_utc=CREATED,
        )
        self.intent_path.write_bytes(EVIDENCE._canonical_bytes(value))
        return value

    @staticmethod
    def _base_lines() -> list[str]:
        return EVIDENCE._profile_causal_lines(EVIDENCE.MODE0_PROFILE, CHALLENGE, COMMIT)

    @staticmethod
    def _smp_lines(
        apics: tuple[int, int, int, int] = (0, 1, 2, 3),
        stacks: tuple[int, int, int, int] = (0x8000, 0x9000, 0xA000, 0xB000),
    ) -> list[str]:
        lines = EVIDENCE._profile_causal_lines(EVIDENCE.SMP4_PROFILE, CHALLENGE, COMMIT)
        insertion = lines.index("SMP BSP/AP barrier: 4 CPUs PASS")
        cpu_lines = [
            f"OSTADIX SMP CPU logical={logical} apic={apics[logical]} "
            f"stack=0x{stacks[logical]:x} online"
            for logical in range(4)
        ]
        return [*lines[:insertion], *cpu_lines, *lines[insertion:]]

    def _write_transcript(self, lines: list[str]) -> None:
        self.transcript.write_text("\r\n".join(lines) + "\r\n", encoding="ascii")

    def test_single_cpu_intent_and_observation_are_digest_bound(self) -> None:
        intent = self._intent()
        self._write_transcript(self._base_lines())
        observation = EVIDENCE.make_observation(
            intent_path=self.intent_path,
            transcript_path=self.transcript,
            image_override=None,
            operator_assertion=EVIDENCE.OPERATOR_ASSERTION,
            created_utc=CREATED,
        )
        self.assertEqual(intent["challenge"], CHALLENGE)
        self.assertEqual(observation["authority"], "none")
        self.assertEqual(observation["admission"], "not-performed")
        self.assertEqual(observation["observed_cpu_identities"], [])
        EVIDENCE._verify_seal(observation, EVIDENCE.OBSERVATION_SCHEMA)

    def test_four_cpu_observation_requires_unique_hardware_identities(self) -> None:
        self._intent(4)
        self._write_transcript(
            self._smp_lines(apics=(0, 2, 4, 6), stacks=(0x8000, 0x9000, 0xA000, 0xB000))
        )
        observation = EVIDENCE.make_observation(
            intent_path=self.intent_path,
            transcript_path=self.transcript,
            image_override=None,
            operator_assertion=EVIDENCE.OPERATOR_ASSERTION,
            created_utc=CREATED,
        )
        identities = observation["observed_cpu_identities"]
        assert isinstance(identities, list)
        self.assertEqual([item["logical"] for item in identities], [0, 1, 2, 3])

    def test_missing_or_duplicate_embedded_challenge_is_rejected(self) -> None:
        self._write_image("0" * 64)
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "all zero"):
            EVIDENCE.inspect_challenged_image(self.image)
        self._write_image(CHALLENGE, duplicate=True)
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "exactly one"):
            EVIDENCE.inspect_challenged_image(self.image)

    def test_source_commit_is_unique_and_matches_preparation_head(self) -> None:
        self._write_image(CHALLENGE, duplicate_source=True)
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "source_commit"):
            EVIDENCE.inspect_challenged_image(self.image)
        self._write_image(CHALLENGE, source_commit="cd" * 20)
        self._write_media_record()
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "repository HEAD"):
            self._intent()

    def test_machine_profile_is_strict_and_privacy_preserving(self) -> None:
        self._write_machine(serial_identity_sha256="0" * 64)
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "all zero"):
            EVIDENCE.load_machine_profile(self.machine)
        self._write_machine(serial_identity_sha256="SERIAL-123")
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "lowercase hex"):
            EVIDENCE.load_machine_profile(self.machine)
        value = json.loads(self.machine.read_text(encoding="utf-8"))
        value["ambient_authority"] = True
        self.machine.write_text(json.dumps(value), encoding="utf-8")
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "keys differ"):
            EVIDENCE.load_machine_profile(self.machine)

    def test_tampered_intent_and_changed_image_are_rejected(self) -> None:
        intent = self._intent()
        intent["expected_cpu_count"] = 2
        self.intent_path.write_bytes(EVIDENCE._canonical_bytes(intent))
        self._write_transcript(self._base_lines())
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "does not match"):
            EVIDENCE.make_observation(
                intent_path=self.intent_path,
                transcript_path=self.transcript,
                image_override=None,
                operator_assertion=EVIDENCE.OPERATOR_ASSERTION,
                created_utc=CREATED,
            )

        self._intent()
        self._write_image("56" * 32)
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "challenge differs"):
            EVIDENCE.make_observation(
                intent_path=self.intent_path,
                transcript_path=self.transcript,
                image_override=None,
                operator_assertion=EVIDENCE.OPERATOR_ASSERTION,
                created_utc=CREATED,
            )

    def test_unsuccessful_or_stale_media_write_record_is_rejected(self) -> None:
        record = json.loads(self.media_write.read_text(encoding="utf-8"))
        record["written"] = False
        self.media_write.write_bytes(EVIDENCE._canonical_bytes(record))
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "successful"):
            self._intent()

        self._write_media_record()
        record = json.loads(self.media_write.read_text(encoding="utf-8"))
        record["target_plan_sha256"] = "ff" * 32
        self.media_write.write_bytes(EVIDENCE._canonical_bytes(record))
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "recomputed plan"):
            self._intent()

    def test_forged_or_incomplete_media_write_record_is_rejected(self) -> None:
        record = json.loads(self.media_write.read_text(encoding="utf-8"))
        record["confirmation"] = "OSTADIX-WRITE-" + "F" * 32
        self.media_write.write_bytes(EVIDENCE._canonical_bytes(record))
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "confirmation"):
            self._intent()

        self._write_media_record()
        record = json.loads(self.media_write.read_text(encoding="utf-8"))
        del record["device"]["device_number"]
        self.media_write.write_bytes(EVIDENCE._canonical_bytes(record))
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "not canonical"):
            self._intent()

        self._write_media_record()
        record = json.loads(self.media_write.read_text(encoding="utf-8"))
        record["ambient_authority"] = True
        self.media_write.write_bytes(EVIDENCE._canonical_bytes(record))
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "keys differ"):
            self._intent()

    def test_duplicate_marker_and_wrong_operator_assertion_are_rejected(self) -> None:
        self._intent()
        lines = self._base_lines()
        lines.append(EVIDENCE.BASE_REQUIRED_MARKERS[0])
        self._write_transcript(lines)
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "marker count"):
            EVIDENCE.make_observation(
                intent_path=self.intent_path,
                transcript_path=self.transcript,
                image_override=None,
                operator_assertion=EVIDENCE.OPERATOR_ASSERTION,
                created_utc=CREATED,
            )
        self._write_transcript(self._base_lines())
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "must exactly equal"):
            EVIDENCE.make_observation(
                intent_path=self.intent_path,
                transcript_path=self.transcript,
                image_override=None,
                operator_assertion="yes",
                created_utc=CREATED,
            )

    def test_smp_duplicate_apic_or_stack_is_rejected(self) -> None:
        self._intent(4)
        for lines in (
            self._smp_lines(apics=(1, 1, 2, 3)),
            self._smp_lines(stacks=(0x8000, 0x8000, 0xA000, 0xB000)),
        ):
            with self.subTest(lines=lines[-2:]):
                self._write_transcript(lines)
                with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "not unique"):
                    EVIDENCE.make_observation(
                        intent_path=self.intent_path,
                        transcript_path=self.transcript,
                        image_override=None,
                        operator_assertion=EVIDENCE.OPERATOR_ASSERTION,
                        created_utc=CREATED,
                    )

    def test_smp_numeric_fields_are_bounded_and_fail_closed(self) -> None:
        self._intent(4)
        for bad_line in (
            "OSTADIX SMP CPU logical=" + "9" * 5000 + " apic=1 stack=0x8000 online",
            "OSTADIX SMP CPU logical=0 apic=4294967296 stack=0x8000 online",
            "OSTADIX SMP CPU logical=0 apic=1 stack=0x800000000000 online",
        ):
            with self.subTest(line=bad_line[:80]):
                lines = self._smp_lines()
                first_cpu = next(
                    index for index, line in enumerate(lines) if line.startswith("OSTADIX SMP CPU")
                )
                lines[first_cpu] = bad_line
                self._write_transcript(lines)
                with self.assertRaises(EVIDENCE.PhysicalEvidenceError):
                    EVIDENCE.make_observation(
                        intent_path=self.intent_path,
                        transcript_path=self.transcript,
                        image_override=None,
                        operator_assertion=EVIDENCE.OPERATOR_ASSERTION,
                        created_utc=CREATED,
                    )

    def test_record_publication_is_exclusive(self) -> None:
        output = self.root / "observation.json"
        record = {"schema": "fixture", "value": 1}
        EVIDENCE._write_record(output, record)
        self.assertEqual(json.loads(output.read_text(encoding="ascii")), record)
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "refusing to replace"):
            EVIDENCE._write_record(output, {"schema": "fixture", "value": 2})
        self.assertEqual(json.loads(output.read_text(encoding="ascii")), record)

    def test_qemu_transcript_check_uses_the_physical_record_grammar(self) -> None:
        self._write_transcript(self._base_lines())
        transcript, identities = EVIDENCE.validate_transcript(
            transcript_path=self.transcript,
            challenge=CHALLENGE,
            source_commit=COMMIT,
            expected_cpus=1,
            required_markers=EVIDENCE.BASE_REQUIRED_MARKERS,
        )
        self.assertEqual(identities, [])
        self.assertEqual(transcript["sha256"], EVIDENCE._sha256(self.transcript.read_bytes()))
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "challenge line"):
            EVIDENCE.validate_transcript(
                transcript_path=self.transcript,
                challenge="56" * 32,
                source_commit=COMMIT,
                expected_cpus=1,
                required_markers=EVIDENCE.BASE_REQUIRED_MARKERS,
            )

    def test_profiles_reject_unsupported_width_mismatch_and_contradiction(self) -> None:
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "exactly 1.*or 4"):
            self._intent(2)
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "requires exactly 4"):
            EVIDENCE.make_intent(
                image_path=self.image,
                media_write_path=self.media_write,
                machine_path=self.machine,
                expected_cpus=1,
                source_commit=COMMIT,
                created_utc=CREATED,
                profile=EVIDENCE.SMP4_PROFILE,
            )
        lines = self._base_lines()
        lines.append("BootInfoV1: rejected")
        self._write_transcript(lines)
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "contains rejection"):
            EVIDENCE.validate_transcript(
                transcript_path=self.transcript,
                challenge=CHALLENGE,
                source_commit=COMMIT,
                expected_cpus=1,
                required_markers=EVIDENCE.MODE0_REQUIRED_MARKERS,
            )

    def test_transcript_profile_enforces_causal_order(self) -> None:
        lines = self._base_lines()
        lines.reverse()
        self._write_transcript(lines)
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "causal order"):
            EVIDENCE.validate_transcript(
                transcript_path=self.transcript,
                challenge=CHALLENGE,
                source_commit=COMMIT,
                expected_cpus=1,
                required_markers=EVIDENCE.MODE0_REQUIRED_MARKERS,
            )

    def test_media_write_record_must_name_the_supplied_image(self) -> None:
        record = json.loads(self.media_write.read_text(encoding="utf-8"))
        other = self.root / "same-bytes-different-path.img"
        other.write_bytes(self.image.read_bytes())
        record["image"] = str(other.resolve())
        self.media_write.write_bytes(EVIDENCE._canonical_bytes(record))
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "image path differs"):
            self._intent()

    def test_challenge_generator_retries_an_all_zero_draw(self) -> None:
        with mock.patch.object(
            EVIDENCE.secrets,
            "token_hex",
            side_effect=["0" * 64, CHALLENGE],
        ):
            with mock.patch("builtins.print") as print_value:
                self.assertEqual(EVIDENCE.main(["challenge", "--raw"]), 0)
        print_value.assert_called_once_with(CHALLENGE)

    def test_symlink_inputs_are_rejected(self) -> None:
        link = self.root / "image-link"
        try:
            link.symlink_to(self.image)
        except OSError:
            self.skipTest("symlink creation unavailable")
        with self.assertRaisesRegex(EVIDENCE.PhysicalEvidenceError, "not a regular file"):
            EVIDENCE.inspect_challenged_image(link)


if __name__ == "__main__":
    unittest.main()
