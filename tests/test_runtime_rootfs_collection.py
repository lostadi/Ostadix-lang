"""Static ELF closure tests use a synthetic filesystem; no runtime executes."""

import importlib.util
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "collect_runtime_rootfs", Path(__file__).resolve().parents[1] / "scripts/collect_runtime_rootfs.py")
collector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(collector)


def elf_info(needed=(), interpreter=None, rpath=None, runpath=None, machine="Advanced Micro Devices X86-64"):
    text = f"  Class: ELF64\n  Machine: {machine}\n"
    if interpreter:
        text += f" [Requesting program interpreter: {interpreter}]\n"
    for name in needed:
        text += f" 0x0001 (NEEDED) Shared library: [{name}]\n"
    for name, value in (("RPATH", rpath), ("RUNPATH", runpath)):
        if value is not None:
            text += f" 0x0002 ({name}) Library path: [{value}]\n"
    return text


class RootfsCollectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.host = self.root / "host"
        self.image = self.root / "image"
        self.host.mkdir()
        self.image.mkdir()
        self.metadata = {}
        self.calls = []
        self.file("/etc/ld.so.cache", b"synthetic cache; never interpreted by a loader in these tests")

    def tearDown(self):
        for root, dirs, _ in os.walk(self.root, followlinks=False):
            Path(root).chmod(0o700)
            for name in dirs:
                path = Path(root) / name
                if not path.is_symlink():
                    path.chmod(0o700)
        self.temp.cleanup()

    def file(self, path, content=b"data", info=None, mode=0o644):
        target = self.host / path.lstrip("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(b"\x7fELFfixture" if info is not None else content)
        target.chmod(mode)
        if info is not None:
            self.metadata[str(target)] = info

    def link(self, path, target):
        entry = self.host / path.lstrip("/")
        entry.parent.mkdir(parents=True, exist_ok=True)
        entry.symlink_to(target)

    def inspect_command(self, command, **kwargs):
        self.calls.append(command)
        self.assertEqual(["/trusted/readelf", "-hW", "-lW", "-dW"], command[:-1])
        return SimpleNamespace(stdout=self.metadata[command[-1]])

    def collect(self, commands=None, paths=(), cache=None, runner=None):
        spec = {"schema": collector.SCHEMA, "commands": commands or {"runtime": "/usr/bin/runtime"},
                "paths": list(paths)}
        if runner:
            spec["runner"] = runner
        worker = collector.Collector(self.image, "/trusted/readelf", cache or {},
                                     source_root=self.host, run=self.inspect_command)
        return worker.collect(spec)

    def test_loader_and_needed_dependencies_and_absolute_command_aliases_are_copied(self):
        self.file("/usr/bin/runtime", info=elf_info(["libc.so.6"], "/lib64/ld.so"), mode=0o755)
        self.file("/usr/lib/libc.so.6", info=elf_info())
        self.file("/usr/lib/ld-real.so", info=elf_info())
        self.link("/bin", "usr/bin")
        self.link("/lib64/ld.so", "/usr/lib/ld-real.so")
        report = self.collect({"runtime": "/bin/runtime"}, cache={"libc.so.6": ["/usr/lib/libc.so.6"]})
        self.assertFalse((self.image / "bin").is_symlink())
        self.assertEqual("../usr/bin/runtime", os.readlink(self.image / "bin/runtime"))
        self.assertEqual("../usr/lib/ld-real.so", os.readlink(self.image / "lib64/ld.so"))
        self.assertEqual({"/usr/bin/runtime", "/usr/lib/libc.so.6", "/usr/lib/ld-real.so", "/etc/ld.so.cache"},
                         {record["path"] for record in report["files"]})
        manifest = json.loads((self.image / "runtime.json").read_text())
        self.assertEqual("linux-rootfs-v1", manifest["execution"])
        self.assertTrue(self.calls)

    def test_inherited_rpath_collects_transitive_deps_and_extension_modules_in_data(self):
        self.file("/usr/bin/runtime", info=elf_info(["liba.so"], rpath="$ORIGIN/../private"), mode=0o755)
        self.file("/usr/private/liba.so", info=elf_info(["libb.so"]))
        self.file("/usr/private/libb.so", info=elf_info())
        self.file("/opt/modules/extension.so", info=elf_info(["libextension.so"]))
        self.file("/usr/lib/libextension.so", info=elf_info())
        (self.host / "opt/modules/empty").mkdir()
        (self.host / "opt/modules").chmod(0o555)
        report = self.collect(paths=["/opt/modules"], cache={"libextension.so": ["/usr/lib/libextension.so"]})
        self.assertTrue((self.image / "usr/private/libb.so").is_file())
        self.assertTrue((self.image / "usr/lib/libextension.so").is_file())
        self.assertTrue((self.image / "opt/modules/empty").is_dir())
        self.assertEqual(0o555, (self.image / "opt/modules").stat().st_mode & 0o777)
        self.assertFalse(report["dynamic_dlopen_closure_verified"])

    def test_runpath_does_not_inherit_into_transitive_dependencies(self):
        self.file("/usr/bin/runtime", info=elf_info(["liba.so"], runpath="$ORIGIN/../private"), mode=0o755)
        self.file("/usr/private/liba.so", info=elf_info(["libb.so"]))
        self.file("/usr/private/libb.so", info=elf_info())
        with self.assertRaisesRegex(collector.CollectionError, "unresolved ELF dependency 'libb.so'"):
            self.collect()

    def test_runner_dependencies_are_collected_and_abi_mismatch_refuses(self):
        self.file("/usr/bin/runtime", info=elf_info(), mode=0o755)
        self.file("/opt/O", info=elf_info(["librunner.so"]), mode=0o755)
        self.file("/usr/lib/librunner.so", info=elf_info())
        report = self.collect(runner="/opt/O", cache={"librunner.so": ["/usr/lib/librunner.so"]})
        self.assertEqual("/opt/O", report["runner"])
        self.assertTrue((self.image / "usr/lib/librunner.so").is_file())
        self.assertFalse((self.image / "opt/O").exists(), "runner is only a dependency seed")

    def test_data_directory_symlinks_materialize_and_cycles_refuse(self):
        self.file("/usr/bin/runtime", info=elf_info(), mode=0o755)
        self.file("/opt/versions/current/data", b"payload")
        self.link("/opt/latest", "versions/current")
        self.collect(paths=["/opt/latest"])
        self.assertTrue((self.image / "opt/latest").is_dir())
        self.assertFalse((self.image / "opt/latest").is_symlink())
        self.assertEqual(b"payload", (self.image / "opt/latest/data").read_bytes())
        self.link("/opt/versions/current/cycle", ".")
        worker = collector.Collector(self.image, "/trusted/readelf", {}, source_root=self.host, run=self.inspect_command)
        with self.assertRaisesRegex(collector.CollectionError, "directory symlink cycle"):
            worker.copy(collector.absolute_path("/opt/latest/cycle"), ancestors=(collector.absolute_path("/opt/versions/current"),))

    def test_reserved_root_missing_dependencies_and_unsupported_platform_fail(self):
        self.file("/usr/bin/runtime", info=elf_info(["libmissing.so"]), mode=0o755)
        with self.assertRaisesRegex(collector.CollectionError, "unresolved ELF dependency"):
            self.collect()
        self.file("/tmp/data")
        worker = collector.Collector(self.image, "/trusted/readelf", {}, source_root=self.host, run=self.inspect_command)
        with self.assertRaisesRegex(collector.CollectionError, "launcher-reserved"):
            worker.copy(collector.absolute_path("/tmp/data"))
        with mock.patch.object(collector.sys, "platform", "darwin"):
            with self.assertRaisesRegex(collector.CollectionError, "requires Linux"):
                collector.main(["unused-spec", "--output", str(self.root / "unused")])

    def test_env_shebang_copies_declared_interpreter_without_execution(self):
        self.file("/usr/bin/runtime", b"#!/usr/bin/env python3\nprint(42)\n", mode=0o755)
        self.file("/usr/bin/env", info=elf_info(), mode=0o755)
        self.file("/usr/bin/python3", info=elf_info(), mode=0o755)
        self.collect({"runtime": "/usr/bin/runtime", "python3": "/usr/bin/python3"})
        self.assertTrue((self.image / "usr/bin/env").is_file())
        self.assertTrue(all(command[0] == "/trusted/readelf" for command in self.calls))

    def test_nondefault_cache_and_hwcaps_candidates_carry_the_loader_cache(self):
        self.file("/usr/bin/runtime", info=elf_info(["libcustom.so"]), mode=0o755)
        self.file("/opt/lib/libcustom.so", info=elf_info())
        self.file("/opt/lib/glibc-hwcaps/x86-64-v3/libcustom.so", info=elf_info())
        report = self.collect(cache={"libcustom.so": [
            "/opt/lib/libcustom.so", "/opt/lib/glibc-hwcaps/x86-64-v3/libcustom.so"]})
        self.assertTrue((self.image / "opt/lib/glibc-hwcaps/x86-64-v3/libcustom.so").is_file())
        self.assertEqual("/etc/ld.so.cache", report["library_cache"]["path"])
        self.assertEqual(64, len(report["library_cache"]["sha256"]))
        self.assertEqual((self.host / "etc/ld.so.cache").read_bytes(),
                         (self.image / "etc/ld.so.cache").read_bytes())

    def test_nodeflib_keeps_nondefault_cache_search_and_skips_default_candidates(self):
        self.file("/usr/bin/runtime", info=elf_info(["libcustom.so"]) + " 0x0003 (FLAGS_1) Flags: NODEFLIB\n", mode=0o755)
        self.file("/usr/lib/libcustom.so", info=elf_info())
        self.file("/opt/lib/libcustom.so", info=elf_info())
        self.collect(cache={"libcustom.so": ["/usr/lib/libcustom.so", "/opt/lib/libcustom.so"]})
        self.assertTrue((self.image / "opt/lib/libcustom.so").is_file())
        self.assertFalse((self.image / "usr/lib/libcustom.so").exists())

    def test_runner_abi_mismatch_refuses(self):
        self.file("/usr/bin/runtime", info=elf_info(), mode=0o755)
        self.file("/opt/O", info=elf_info(machine="AArch64"), mode=0o755)
        with self.assertRaisesRegex(collector.CollectionError, "runner ELF ABI differs"):
            self.collect(runner="/opt/O")


if __name__ == "__main__":
    unittest.main()
