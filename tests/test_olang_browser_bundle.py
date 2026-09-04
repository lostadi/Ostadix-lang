"""Browser-payload contract and opt-in compiler integration tests."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "apps" / "olang-browser-wasi"
EXPECTED_IMPORTS = [
    "args_get",
    "args_sizes_get",
    "clock_time_get",
    "environ_get",
    "environ_sizes_get",
    "fd_close",
    "fd_fdstat_get",
    "fd_filestat_get",
    "fd_prestat_dir_name",
    "fd_prestat_get",
    "fd_read",
    "fd_readdir",
    "fd_seek",
    "fd_write",
    "path_filestat_get",
    "path_open",
    "path_readlink",
    "path_remove_directory",
    "path_rename",
    "path_unlink_file",
    "poll_oneoff",
    "proc_exit",
    "random_get",
    "sched_yield",
]


class OlangBrowserBundleTests(unittest.TestCase):
    def test_dependency_free_node_host_contract(self) -> None:
        node = shutil.which("node")
        if node is None:
            self.skipTest("Node.js is not installed")
        completed = subprocess.run(
            [node, str(APP / "test-host.mjs")],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("olang-browser-wasi host tests: PASS", completed.stdout)

    @unittest.skipUnless(
        os.environ.get("OSTADIX_RUN_BROWSER_BUNDLE_E2E") == "1",
        "set OSTADIX_RUN_BROWSER_BUNDLE_E2E=1 to compile the WASI fixture",
    )
    def test_fresh_pure_text_bundle_executes_without_node_wasi(self) -> None:
        node = shutil.which("node")
        self.assertIsNotNone(node, "Node.js is required for browser-bundle qualification")
        compiler = Path(
            os.environ.get("OLANGC_BROWSER_TEST_BIN", ROOT / "target" / "debug" / "olangc")
        )
        self.assertTrue(compiler.is_file(), f"missing olangc binary: {compiler}")

        with tempfile.TemporaryDirectory(prefix="olang-browser-test-") as temporary:
            temporary_path = Path(temporary)
            source = temporary_path / "fixture.O"
            bundle = temporary_path / "bundle"
            shim_dir = temporary_path / "shim-overrides"
            shim_dir.mkdir()
            override = b"#!/usr/bin/env python3\n# browser adapter override fixture\n"
            (shim_dir / "python_shim.py").write_bytes(override)
            marker = "OSTADIX BROWSER BUNDLE PYTHON TEST PASS"
            source.write_text(f"text^({marker})_text\n", encoding="utf-8")
            built = subprocess.run(
                [
                    str(compiler),
                    str(source),
                    "--target",
                    "wasm",
                    "--browser-bundle",
                    str(bundle),
                    "--shim-dir",
                    str(shim_dir),
                ],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(built.returncode, 0, built.stderr)

            manifest = json.loads((bundle / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest["schema"], "ostadix.olang-browser-bundle/v1")
            self.assertTrue(manifest["compatibility"]["local_execution"])
            self.assertEqual(manifest["compatibility"]["blockers"], [])
            self.assertEqual(manifest["abi"]["imports"], EXPECTED_IMPORTS)
            self.assertEqual(manifest["abi"]["required_exports"], ["memory", "_start"])
            adapter_names = [adapter["name"] for adapter in manifest["adapters"]]
            self.assertEqual(adapter_names, sorted(set(adapter_names)))
            self.assertIn("o_shim_common.py", adapter_names)
            python_adapter = next(
                adapter
                for adapter in manifest["adapters"]
                if adapter["name"] == "python_shim.py"
            )
            self.assertEqual(
                (bundle / python_adapter["file"]["path"]).read_bytes(), override
            )
            for field in ("source", "artifact", "plan"):
                record = manifest[field]
                payload = (bundle / record["path"]).read_bytes()
                self.assertEqual(record["bytes"], len(payload))
                self.assertEqual(record["sha256"], hashlib.sha256(payload).hexdigest())
            for adapter in manifest["adapters"]:
                record = adapter["file"]
                payload = (bundle / record["path"]).read_bytes()
                self.assertEqual(record["bytes"], len(payload))
                self.assertEqual(record["sha256"], hashlib.sha256(payload).hexdigest())

            executed = subprocess.run(
                [node, str(APP / "test-bundle.mjs"), str(bundle), marker],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(executed.returncode, 0, executed.stderr)
            self.assertIn("olang browser bundle local execution: PASS", executed.stdout)

            no_clobber = subprocess.run(
                [
                    str(compiler),
                    str(source),
                    "--target",
                    "wasm",
                    "--browser-bundle",
                    str(bundle),
                    "--shim-dir",
                    str(shim_dir),
                ],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(no_clobber.returncode, 0)
            self.assertIn("directory already exists", no_clobber.stderr)


if __name__ == "__main__":
    unittest.main()
