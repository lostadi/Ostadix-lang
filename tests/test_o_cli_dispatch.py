"""Process-level tests for the repository-owned lowercase ``o`` dispatcher."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


PROJECT_ROOT = Path(__file__).resolve().parents[1]
O_CLI = PROJECT_ROOT / "scripts" / "o-cli.sh"


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
        self.assert_dispatch(("node-host", "serve", "--bind", "127.0.0.1:7337"), [
            "serve",
            "--bind",
            "127.0.0.1:7337",
        ])

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


if __name__ == "__main__":
    unittest.main()
