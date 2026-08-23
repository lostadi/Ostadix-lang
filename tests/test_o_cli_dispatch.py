"""Process-level tests for the repository-owned lowercase ``o`` dispatcher."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


PROJECT_ROOT = Path(__file__).resolve().parents[1]
O_CLI = PROJECT_ROOT / "scripts" / "o-cli.sh"
QUICKSTART = PROJECT_ROOT / "o-node-quickstart.sh"


class LowercaseCliDispatchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.fake = Path(self.temporary.name) / "capture"
        self.fake.write_text(
            "#!/bin/sh\nprintf 'arg=<%s>\\n' \"$@\"\n",
            encoding="utf-8",
        )
        self.fake.chmod(0o755)
        self.environment = os.environ.copy()
        for variable in (
            "O_LANG_EVALUATOR_BIN",
            "O_LANG_LIVE_BIN",
            "O_LANG_OGIT_BIN",
            "O_LANG_NODE_BIN",
            "O_LANG_OCTL_BIN",
            "O_LANG_REGISTRY_BIN",
            "O_LANG_INFO_BIN",
        ):
            self.environment[variable] = str(self.fake)

    def run_cli(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["/bin/bash", str(O_CLI), *arguments],
            cwd=PROJECT_ROOT,
            env=self.environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def assert_dispatch(self, arguments: tuple[str, ...], expected: list[str]) -> None:
        result = self.run_cli(*arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [f"arg=<{arg}>" for arg in expected])

    def test_local_run_does_not_forward_the_dispatch_word(self) -> None:
        self.assert_dispatch(("run", "program.O", "backends"), ["program.O", "backends"])

    def test_hosted_client_and_service_have_unambiguous_routes(self) -> None:
        self.assert_dispatch(("node", "doctor", "--address", "node:7337"), [
            "node",
            "doctor",
            "--address",
            "node:7337",
        ])
        self.assert_dispatch(("node", "start"), ["start"])
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

    def test_help_advertises_hosted_v2_sessions(self) -> None:
        result = self.run_cli("help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("node start|stop|status|restart", result.stdout)
        self.assertIn("node pair [NODE_ID]", result.stdout)
        self.assertIn("node list|use|profile|doctor|run|session", result.stdout)
        self.assertNotIn("node pair PASSCODE", result.stdout)
        self.assertNotIn("node pair [NODE_ID] PASSCODE", result.stdout)

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

    def test_run_without_a_source_fails_before_invoking_the_evaluator(self) -> None:
        result = self.run_cli("run")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage: o run FILE.O", result.stderr)


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
                    "/bin/bash",
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
            self.assertIn('exec "', source)
            self.assertIn('/scripts/o-cli.sh" "$@"', source)
            self.assertNotIn('${0##*/}', source)


if __name__ == "__main__":
    unittest.main()
