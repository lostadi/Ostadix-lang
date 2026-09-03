"""Process-level tests for the repository-owned lowercase ``o`` dispatcher."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


PROJECT_ROOT = Path(__file__).resolve().parents[1]
O_CLI = PROJECT_ROOT / "scripts" / "o-cli.sh"
O_KERNEL_CLI = PROJECT_ROOT / "scripts" / "o-kernel.sh"
QUICKSTART = PROJECT_ROOT / "o-node-quickstart.sh"


class LowercaseCliDispatchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.fake = Path(self.temporary.name) / "capture"
        self.fake.write_text(
            "#!/bin/sh\nfor arg in \"$@\"; do printf 'arg=<%s>\\n' \"$arg\"; done\n",
            encoding="utf-8",
        )
        self.fake.chmod(0o755)
        self.environment = os.environ.copy()
        for variable in (
            "O_LANG_OCLI_BIN",
            "O_LANG_OLANGC_BIN",
            "O_LANG_EVALUATOR_BIN",
            "O_LANG_DEVICE_BIN",
            "O_LANG_CAPACITY_BIN",
            "O_LANG_LIVE_BIN",
            "O_LANG_OGIT_BIN",
            "O_LANG_NODE_BIN",
            "O_LANG_OCTL_BIN",
            "O_LANG_REGISTRY_BIN",
            "O_LANG_INFO_BIN",
        ):
            self.environment[variable] = str(self.fake)

    def run_cli(
        self,
        *arguments: str,
        environment: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(O_CLI), *arguments],
            cwd=PROJECT_ROOT,
            env=environment or self.environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def assert_dispatch(
        self,
        arguments: tuple[str, ...],
        expected: list[str],
        environment: dict[str, str] | None = None,
    ) -> None:
        result = self.run_cli(*arguments, environment=environment)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [f"arg=<{arg}>" for arg in expected])

    def test_intent_commands_route_whole_argv_to_the_compiled_front_door(self) -> None:
        for arguments in (
            ("run", "program.O", "backends"),
            ("run", "project", "--parallel", "auto"),
            ("optimize", "project", "--route", "main", "--json"),
            ("plan", "--parallel", "auto", "project", "--live"),
            ("explain", "last-run"),
            ("inspect", "last-run", "--trace"),
        ):
            with self.subTest(arguments=arguments):
                self.assert_dispatch(arguments, list(arguments))

    def test_android_commands_use_the_explicit_device_namespace(self) -> None:
        non_device_environment = self.environment.copy()
        non_device_environment["O_LANG_DEVICE_BIN"] = str(
            Path(self.temporary.name) / "missing-device-controller"
        )
        self.assert_dispatch(
            ("doctor", "--json"),
            ["doctor", "--json"],
            non_device_environment,
        )
        self.assert_dispatch(
            ("device", "doctor", "--json"),
            ["doctor", "--json"],
        )

    def test_why_preserves_arguments_under_posix_sh(self) -> None:
        self.assert_dispatch(
            ("why", "program.O", "P7", "--json"),
            ["program.O", "--target", "ir", "--why", "P7", "--json"],
        )

    def test_hosted_client_and_service_have_unambiguous_routes(self) -> None:
        self.assert_dispatch(("node", "doctor", "--address", "node:7337"), [
            "node",
            "doctor",
            "--address",
            "node:7337",
        ])
        self.assert_dispatch(("node", "start"), ["start"])
        self.assert_dispatch(
            (
                "node",
                "start",
                "--startup-timeout-seconds",
                "30",
                "--fresh-pki-key-algorithm",
                "ec-p256",
            ),
            [
                "start",
                "--startup-timeout-seconds",
                "30",
                "--fresh-pki-key-algorithm",
                "ec-p256",
            ],
        )
        self.assert_dispatch(("node", "status"), ["status"])
        self.assert_dispatch(("node-host", "serve", "--bind", "127.0.0.1:7337"), [
            "serve",
            "--bind",
            "127.0.0.1:7337",
        ])
        self.assert_dispatch(("node", "session", "status", "--capability", "session.json"), [
            "node",
            "session",
            "status",
            "--capability",
            "session.json",
        ])

    def test_node_pairing_routes_all_arguments_to_the_service_cli(self) -> None:
        self.assert_dispatch(("node", "pair"), ["pair"])
        self.assert_dispatch(
            (
                "node",
                "pair",
                "ostadix-peer",
                "--passcode-stdin",
                "--replace",
                "--address",
                "203.0.113.8:7340",
            ),
            [
                "pair",
                "ostadix-peer",
                "--passcode-stdin",
                "--replace",
                "--address",
                "203.0.113.8:7340",
            ],
        )

    def test_help_spellings_share_the_compiled_front_door(self) -> None:
        self.assert_dispatch(("help",), ["help"])
        self.assert_dispatch(("--help",), ["--help"])
        self.assert_dispatch(("-h",), ["-h"])

    def test_registry_and_live_commands_forward_exact_arguments(self) -> None:
        self.assert_dispatch(("registry", "verify", "--state", "store.cbor"), [
            "verify",
            "--state",
            "store.cbor",
        ])
        self.assert_dispatch(("live", "demo", "--state", "state"), [
            "demo",
            "--state",
            "state",
        ])

    def test_absorbed_capacity_commands_forward_exact_arguments(self) -> None:
        self.assert_dispatch(("capacity", "install", "guix-system"), [
            "install",
            "guix-system",
        ])
        self.assert_dispatch(("capacity", "plan", "guix-system", "openbsd"), [
            "plan",
            "guix-system",
            "openbsd",
        ])

    def test_information_command_forwards_exact_local_cli_arguments(self) -> None:
        self.assert_dispatch(("info", "head", "--state", "facts"), [
            "head",
            "--state",
            "facts",
        ])

    def test_receipt_default_is_explicit_and_custom_ogit_commands_remain_available(self) -> None:
        self.assert_dispatch(("receipt",), ["demo", "semantic-receipt"])
        self.assert_dispatch(("receipt", "log", "--limit", "2"), [
            "log",
            "--limit",
            "2",
        ])

    def test_unknown_command_forms_still_fall_through_to_the_evaluator(self) -> None:
        self.assert_dispatch(("program.O", "backends"), ["program.O", "backends"])

    def test_packaged_dispatchers_route_optimize_to_the_compiled_front_door(self) -> None:
        dockerfile = (PROJECT_ROOT / "Dockerfile").read_text(encoding="utf-8")
        capacity_host = (
            PROJECT_ROOT / "scripts" / "prepare-x86_64-capacity-host.sh"
        ).read_text(encoding="utf-8")
        expected = "run|optimize|plan|explain|inspect"
        self.assertIn(expected, dockerfile)
        self.assertIn(expected, capacity_host)


class KernelCapacityCliDispatchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.fake = Path(self.temporary.name) / "capture"
        self.fake.write_text(
            "#!/bin/sh\nfor arg in \"$@\"; do printf 'arg=<%s>\\n' \"$arg\"; done\n",
            encoding="utf-8",
        )
        self.fake.chmod(0o755)
        self.environment = os.environ.copy()
        for variable in (
            "O_KERNEL_CAPACITY_ISO_BUILD_SCRIPT",
            "O_KERNEL_CAPACITY_ISO_BOOT_SCRIPT",
            "O_KERNEL_CAPACITY_ISO_INSPECT_SCRIPT",
            "O_KERNEL_HOSTED_LIVE_RELEASE_SCRIPT",
            "O_KERNEL_HOSTED_LIVE_SMOKE_SCRIPT",
            "O_KERNEL_VENTOY_INSTALLER_SCRIPT",
        ):
            self.environment[variable] = str(self.fake)

    def run_kernel(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["/bin/bash", str(O_KERNEL_CLI), *arguments],
            cwd=PROJECT_ROOT,
            env=self.environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def assert_dispatch(self, arguments: tuple[str, ...], expected: list[str]) -> None:
        result = self.run_kernel(*arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [f"arg=<{arg}>" for arg in expected])

    def test_capacity_iso_commands_forward_exact_arguments(self) -> None:
        explicit = "/tmp/capacity.iso"
        default = str(
            PROJECT_ROOT
            / "target/ostadix-capacity-iso/x86_64/ostadix-absorbed-capacity-x86_64-uefi.iso"
        )
        for arguments, expected in (
            (("capacity-iso",), []),
            (("capacity-iso", explicit), [explicit]),
            (("inspect-capacity-iso",), ["inspect", default]),
            (("inspect-capacity-iso", explicit), ["inspect", explicit]),
            (("boot-capacity-iso",), []),
            (("boot-capacity-iso", explicit), [explicit]),
        ):
            with self.subTest(arguments=arguments):
                self.assert_dispatch(arguments, expected)

    def test_capacity_iso_commands_reject_extra_paths(self) -> None:
        for command in ("capacity-iso", "inspect-capacity-iso", "boot-capacity-iso"):
            with self.subTest(command=command):
                result = self.run_kernel(command, "one.iso", "two.iso")
                self.assertEqual(result.returncode, 2)
                self.assertIn("accepts at most one path argument", result.stderr)

    def test_hosted_live_release_forwards_arbitrary_options(self) -> None:
        output = "/tmp/ostadix-hosted-live-x86_64-uefi-0123456789ab_VTGRUB2.iso"
        self.assert_dispatch(("hosted-live-release",), [])
        self.assert_dispatch(
            ("hosted-live-release", "--vm", "moral-gaur", "--output", output),
            ["--vm", "moral-gaur", "--output", output],
        )

    def test_hosted_live_smoke_accepts_at_most_one_iso_path(self) -> None:
        image = "/tmp/ostadix-hosted-live-x86_64-uefi-0123456789ab_VTGRUB2.iso"
        self.assert_dispatch(("smoke-hosted-live",), [])
        self.assert_dispatch(("smoke-hosted-live", image), [image])

        result = self.run_kernel("smoke-hosted-live", image, "unexpected.iso")
        self.assertEqual(result.returncode, 2)
        self.assertIn("accepts at most one path argument", result.stderr)

    def test_ventoy_commands_preserve_the_bound_target_arguments(self) -> None:
        image = "/tmp/ostadix-hosted-live-x86_64-uefi-0123456789ab_VTGRUB2.iso"
        target = [
            "--iso",
            image,
            "--device",
            "/dev/disk4",
            "--volume",
            "/Volumes/Ventoy",
            "--name",
            "OSTADIX-Hosted-Live-x86_64-UEFI_VTGRUB2.iso",
        ]
        self.assert_dispatch(("prepare-ventoy", *target), ["prepare", *target])
        self.assert_dispatch(
            ("install-ventoy", *target, "--confirm", "exact-token", "--eject"),
            ["install", *target, "--confirm", "exact-token", "--eject"],
        )
        self.assert_dispatch(("verify-ventoy", *target), ["verify", *target])


class NodeQuickstartDispatchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "repo"
        (self.root / "scripts").mkdir(parents=True)
        (self.root / "target" / "release").mkdir(parents=True)
        (self.root / "backends").mkdir()
        self.quickstart = self.root / "o-node-quickstart.sh"
        self.quickstart.write_text(QUICKSTART.read_text(encoding="utf-8"), encoding="utf-8")
        self.quickstart.chmod(0o755)
        self.capture = self.root / "capture.txt"
        dispatcher = self.root / "scripts" / "o-cli.sh"
        dispatcher.write_text(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>\"$CAPTURE\"\n",
            encoding="utf-8",
        )
        dispatcher.chmod(0o755)
        for name in ("O", "o-node", "octl"):
            binary = self.root / "target" / "release" / name
            binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            binary.chmod(0o755)
        setup = self.root / "setup.sh"
        setup.write_text("#!/bin/sh\nexit 99\n", encoding="utf-8")
        setup.chmod(0o755)
        self.environment = os.environ.copy()
        self.environment["CAPTURE"] = str(self.capture)

    def run_quickstart(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        self.capture.unlink(missing_ok=True)
        return subprocess.run(
            ["/bin/bash", str(self.quickstart), *arguments],
            cwd=self.root,
            env=self.environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def captured(self) -> list[str]:
        if not self.capture.exists():
            return []
        return self.capture.read_text(encoding="utf-8").splitlines()

    def test_default_start_and_managed_run_expose_only_user_intent(self) -> None:
        result = self.run_quickstart()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.captured(), ["node start", "node status"])

        result = self.run_quickstart("--run", "program.O")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.captured(), ["node session run program.O"])

    def test_manual_surface_requires_the_explicit_manual_switch(self) -> None:
        result = self.run_quickstart("--manual", "serve", "--bind", "127.0.0.1:7337")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.captured(),
            ["node-host serve --bind 127.0.0.1:7337"],
        )

    def test_ordinary_quickstart_contains_no_embedded_transport_coordinates(self) -> None:
        source = QUICKSTART.read_text(encoding="utf-8")
        for forbidden in (
            "--address",
            "--server-name",
            "--ca",
            "--cert",
            "--key",
            "--signing-key",
            "--capability",
            "--lease",
            "PKI=",
            "AUTH=",
            "BIND=",
            "NODE_ID=",
        ):
            self.assertNotIn(forbidden, source)


class InstalledWrapperDispatchTests(unittest.TestCase):
    def test_generated_wrapper_does_not_depend_on_invocation_case(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "O"
            result = subprocess.run(
                [
                    str(PROJECT_ROOT / "scripts" / "install-o-cli-wrapper.sh"),
                    str(destination),
                ],
                cwd=PROJECT_ROOT,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            source = destination.read_text(encoding="utf-8")
            self.assertTrue(source.startswith("#!/bin/sh\n"))
            self.assertIn('exec "', source)
            self.assertIn('/scripts/o-cli.sh" "$@"', source)
            self.assertNotIn('${0##*/}', source)

            true_command = shutil.which("true")
            self.assertIsNotNone(true_command)
            environment = os.environ.copy()
            environment["O_LANG_OCLI_BIN"] = true_command
            result = subprocess.run(
                [str(destination), "help"],
                cwd=PROJECT_ROOT,
                env=environment,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_repository_dispatcher_has_an_android_safe_interpreter(self) -> None:
        source = O_CLI.read_text(encoding="utf-8")
        self.assertTrue(source.startswith("#!/bin/sh\n"))
        self.assertNotIn("/usr/bin/env", source)


if __name__ == "__main__":
    unittest.main()
