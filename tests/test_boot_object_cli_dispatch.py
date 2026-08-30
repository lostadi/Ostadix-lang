"""Focused source-level dispatch checks for ``o object``.

These tests deliberately replace the compiled front door, so they exercise the
repository-owned shell dispatcher without building in the live checkout.
"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


PROJECT_ROOT = Path(__file__).resolve().parents[1]
DISPATCHER = PROJECT_ROOT / "scripts" / "o-cli.sh"


class BootObjectDispatchTests(unittest.TestCase):
    def test_object_argv_reaches_the_compiled_front_door_exactly(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            capture = Path(directory) / "capture"
            capture.write_text(
                "#!/bin/sh\nfor arg in \"$@\"; do printf 'arg=<%s>\\n' \"$arg\"; done\n",
                encoding="utf-8",
            )
            capture.chmod(0o755)
            environment = os.environ.copy()
            environment["O_LANG_OCLI_BIN"] = str(capture)
            for arguments in (
                ("object", "root"),
                ("object", "list", "--prefix", "crates/"),
                ("object", "stat", "README.md", "--json"),
                ("object", "get", "README.md", "--output", "-"),
                (
                    "object",
                    "--store",
                    "/usr/share/ostadix/boot-objects/v1",
                    "verify",
                ),
            ):
                with self.subTest(arguments=arguments):
                    result = subprocess.run(
                        ["/bin/bash", str(DISPATCHER), *arguments],
                        cwd=PROJECT_ROOT,
                        env=environment,
                        text=True,
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        check=False,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(
                        result.stdout.splitlines(),
                        [f"arg=<{argument}>" for argument in arguments],
                    )


if __name__ == "__main__":
    unittest.main()
