"""Regression tests for the side-effect-free ``setup.sh`` planning interface."""

from __future__ import annotations

from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


PROJECT_ROOT = Path(__file__).resolve().parents[1]
SETUP = PROJECT_ROOT / "setup.sh"
BASH = Path("/bin/bash")
SYSTEM_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"


class SetupScriptTests(unittest.TestCase):
    maxDiff = None

    def run_setup(
        self,
        *arguments: str,
        home: Path,
        platform: str = "linux",
        distro: str = "debian",
        extra_env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        env = {
            "HOME": str(home),
            "PATH": SYSTEM_PATH,
            "LANG": "C",
            "LC_ALL": "C",
            "SHELL": str(BASH),
            "TMPDIR": str(home),
            "CARGO_HOME": str(home / "cargo"),
            "XDG_DATA_HOME": str(home / "data"),
            "OSTADIX_ENV_FILE": str(home / "config" / "env.sh"),
            "OSTADIX_GUESTS_DIR": str(home / "guests"),
            "OSTADIX_SHELL_RC": str(home / "shellrc"),
            "OSTADIX_SETUP_PLATFORM": platform,
            "OSTADIX_SETUP_DISTRO": distro,
            "OSTADIX_SETUP_TEST_OVERRIDES": "1",
        }
        if extra_env:
            env.update(extra_env)
        return subprocess.run(
            [str(BASH), str(SETUP), *arguments],
            cwd=PROJECT_ROOT,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
            check=False,
        )

    @staticmethod
    def combined_output(result: subprocess.CompletedProcess[str]) -> str:
        return result.stdout + result.stderr

    @staticmethod
    def dry_run_packages(output: str, command: str) -> list[str]:
        line = next(
            candidate
            for candidate in output.splitlines()
            if candidate.startswith("[DRY]") and command in candidate
        )
        words = shlex.split(line)
        marker = words.index("--no-install-recommends")
        return words[marker + 1 :]

    def test_unknown_option_exits_two(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup("--not-a-real-option", home=Path(temp_dir))

        self.assertEqual(result.returncode, 2, self.combined_output(result))
        self.assertIn("Unknown option: --not-a-real-option", result.stderr)

    def test_platform_override_is_rejected_without_dry_run(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup("--deps-only", home=Path(temp_dir))

        self.assertEqual(result.returncode, 2, self.combined_output(result))
        self.assertIn("test-only and require --dry-run", result.stderr)

    def test_unmanaged_env_target_fails_before_package_plan(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            home = Path(temp_dir)
            env_file = home / "unmanaged.sh"
            env_file.write_text("export USER_DATA=keep\n", encoding="utf-8")
            result = self.run_setup(
                "--dry-run",
                "--deps-only",
                "--env-file",
                str(env_file),
                home=home,
            )

        output = self.combined_output(result)
        self.assertEqual(result.returncode, 1, output)
        self.assertIn("refusing to overwrite unmanaged environment file", output)
        self.assertNotIn("apt-get", output)

    def test_minimal_and_full_conflict_exits_two(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup("--minimal", "--full", home=Path(temp_dir))

        self.assertEqual(result.returncode, 2, self.combined_output(result))
        self.assertIn("--minimal and --full are mutually exclusive", result.stdout)

    def test_full_macos_plan_has_ocore_and_nix_but_not_guest_lab(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup(
                "--full",
                "--yes",
                "--dry-run",
                "--deps-only",
                "--no-env",
                home=Path(temp_dir),
                platform="macos",
                distro="unknown",
            )

        output = self.combined_output(result)
        self.assertEqual(result.returncode, 0, output)
        brew_line = next(
            line
            for line in output.splitlines()
            if line.startswith("[DRY]") and "brew install --quiet" in line
        )
        brew_words = shlex.split(brew_line)
        quiet_index = brew_words.index("--quiet")
        self.assertEqual(
            brew_words[quiet_index + 1 :],
            [
                "gcc",
                "make",
                "python@3.12",
                "curl",
                "git",
                "pkg-config",
                "openssl",
                "sqlite",
                "racket",
                "llvm",
                "lld",
                "binutils",
                "qemu",
                "cmake",
            ],
        )
        self.assertIn("nix=true", output)
        self.assertIn("ocore=true", output)
        self.assertIn("guest_tools=false", output)
        self.assertIn("ubuntu_vm=false", output)
        self.assertNotIn("Guest lab directory:", output)
        self.assertNotIn("multipass", output.lower())

    def test_debian_composed_native_kernel_guest_plan(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup(
                "--with-ocore",
                "--with-linux-kernel-tools",
                "--with-guest-tools",
                "--dry-run",
                "--deps-only",
                "--no-env",
                home=Path(temp_dir),
            )

        output = self.combined_output(result)
        self.assertEqual(result.returncode, 0, output)
        self.assertEqual(
            self.dry_run_packages(output, "apt-get install"),
            [
                "build-essential",
                "gcc",
                "g++",
                "make",
                "python3",
                "python3-pip",
                "python3-venv",
                "curl",
                "git",
                "pkg-config",
                "libssl-dev",
                "sqlite3",
                "ca-certificates",
                "perl",
                "file",
                "clang",
                "lld",
                "llvm",
                "binutils",
                "qemu-system-x86",
                "qemu-system-arm",
                "cmake",
                "qemu-efi-aarch64",
                "qemu-utils",
                "gzip",
                "xz-utils",
                "zstd",
                "xorriso",
                "cpio",
                "squashfs-tools",
                "openssl",
                "bc",
                "bison",
                "flex",
                "libelf-dev",
                "dwarves",
                "rsync",
                "kmod",
                "libncurses-dev",
            ],
        )
        self.assertIn("ocore=true", output)
        self.assertIn("linux_kernel_tools=true", output)
        self.assertIn("guest_tools=true", output)
        self.assertNotIn("multipass", output.lower())

    def test_fedora_guest_tools_dry_run_requires_manual_install(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup(
                "--with-guest-tools",
                "--dry-run",
                "--deps-only",
                "--no-env",
                home=Path(temp_dir),
                platform="linux",
                distro="fedora",
            )

        output = self.combined_output(result)
        self.assertEqual(result.returncode, 2, output)
        self.assertIn(
            "automatic --with-guest-tools installation is validated only for "
            "macOS/Homebrew and Debian-family Linux hosts",
            output,
        )
        self.assertIn("then use --with-guest-tools --check", output)
        self.assertNotIn("dnf install", output)

    def test_windows_ubuntu_vm_plan_remains_independent_of_guest_lab(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup(
                "--with-ubuntu-vm",
                "--dry-run",
                "--deps-only",
                "--no-env",
                home=Path(temp_dir),
                platform="windows",
                distro="unknown",
            )

        output = self.combined_output(result)
        self.assertEqual(result.returncode, 0, output)
        self.assertIn("winget install --id Canonical.Multipass", output)
        self.assertNotIn("SoftwareFreedomConservancy.QEMU", output)
        self.assertIn("guest_tools=false", output)
        self.assertIn("ubuntu_vm=true", output)

    def test_dry_run_does_not_create_env_hook_or_guest_files(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            home = Path(temp_dir)
            env_file = home / "nested" / "ostadix-env.sh"
            shell_rc = home / "custom-shell.rc"
            guests_dir = home / "guest-media"
            result = self.run_setup(
                "--with-guest-tools",
                "--persist-env",
                "--dry-run",
                "--deps-only",
                "--env-file",
                str(env_file),
                home=home,
                extra_env={
                    "OSTADIX_SHELL_RC": str(shell_rc),
                    "OSTADIX_GUESTS_DIR": str(guests_dir),
                },
            )
            output = self.combined_output(result)
            self.assertEqual(result.returncode, 0, output)
            self.assertIn(f"[DRY] write managed environment file: {env_file}", output)
            self.assertIn(f"[DRY] append managed environment hook to {shell_rc}", output)
            self.assertIn(f"[DRY] mkdir -p {guests_dir}", output)
            self.assertFalse(env_file.exists())
            self.assertFalse(shell_rc.exists())
            self.assertFalse(guests_dir.exists())
            self.assertEqual(list(home.iterdir()), [])

    def test_guest_plan_states_explicit_nonclaim(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup(
                "--with-guest-tools",
                "--dry-run",
                "--deps-only",
                "--no-env",
                home=Path(temp_dir),
            )

        output = self.combined_output(result)
        self.assertEqual(result.returncode, 0, output)
        self.assertIn(
            "Pinned Alpine/FreeBSD/9front/Guix/Redox media is fetched only by an explicit lab command.",
            output,
        )
        self.assertIn("Supply any additional foreign media explicitly.", output)
        self.assertIn(
            "No foreign OS image is downloaded or booted by setup.sh.",
            output,
        )
        self.assertIn(
            "These host-side boots do not establish foreign-kernel support in O-core.",
            output,
        )

    def test_help_lists_profile_and_safety_options(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup("--help", home=Path(temp_dir))

        self.assertEqual(result.returncode, 0, self.combined_output(result))
        for option in (
            "--minimal",
            "--full",
            "--with-nix",
            "--no-nix",
            "--with-ocore",
            "--with-ocore-media",
            "--with-hosted-runtimes",
            "--with-linux-kernel-tools",
            "--with-guest-tools",
            "--with-ubuntu-vm",
            "--verify-ocore",
            "--check",
            "--deps-only",
            "--env-file PATH",
            "--no-env",
            "--persist-env",
            "--dry-run",
        ):
            with self.subTest(option=option):
                self.assertIn(option, result.stdout)

    def test_macos_ocore_media_profile_is_explicit_and_composable(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup(
                "--with-ocore-media",
                "--dry-run",
                "--deps-only",
                "--no-env",
                home=Path(temp_dir),
                platform="macos",
                distro="unknown",
            )

        output = self.combined_output(result)
        self.assertEqual(result.returncode, 0, output)
        brew_line = next(
            line
            for line in output.splitlines()
            if line.startswith("[DRY]") and "brew install --quiet" in line
        )
        packages = shlex.split(brew_line)
        self.assertIn("x86_64-elf-grub", packages)
        self.assertIn("mtools", packages)
        self.assertIn("xorriso", packages)
        self.assertIn("qemu", packages)
        self.assertIn("ocore=true", output)
        self.assertIn("ocore_media=true", output)

    def test_debian_ocore_media_profile_has_firmware_and_fat_tools(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup(
                "--with-ocore-media",
                "--dry-run",
                "--deps-only",
                "--no-env",
                home=Path(temp_dir),
            )

        output = self.combined_output(result)
        self.assertEqual(result.returncode, 0, output)
        packages = self.dry_run_packages(output, "apt-get install")
        self.assertIn("grub-efi-amd64-bin", packages)
        self.assertIn("mtools", packages)
        self.assertIn("ovmf", packages)
        self.assertIn("xorriso", packages)

    def test_ocore_media_check_requires_iso_grub_platform_modules(self) -> None:
        setup = SETUP.read_text(encoding="utf-8")

        self.assertIn('${OSTADIX_GRUB_EFI_DIRECTORY:-}', setup)
        self.assertIn('"GRUB x86_64 EFI platform"', setup)
        for module in ("modinfo.sh", "normal.mod", "multiboot2.mod"):
            with self.subTest(module=module):
                self.assertIn(module, setup)
        self.assertIn("GRUB x86_64 EFI/rescue modules, mtools, xorriso, and OVMF", setup)
        self.assertIn("resolve-x86_64-ovmf-code.sh", setup)
        self.assertIn("resolve_ostadix_x86_64_ovmf_code qemu-system-x86_64", setup)

    def test_normal_build_plans_every_public_rust_binary(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            home = Path(temp_dir)
            result = self.run_setup(
                "--minimal",
                "--dry-run",
                "--no-env",
                home=home,
            )

            output = self.combined_output(result)
            self.assertEqual(result.returncode, 0, output)
            self.assertIn("cargo build --release --locked", output)
            for binary in (
                "O",
                "o-cli",
                "olangc",
                "ocorec",
                "o-link",
                "o-unlink",
                "ogit",
                "o-live-host",
                "o-node",
                "octl",
                "o-registry",
                "o-info",
            ):
                with self.subTest(binary=binary):
                    self.assertIn(f"--bin {binary}", output)
                    installed = home / "cargo" / "bin" / binary
                    self.assertIn(f"replace {installed}", output)
            evaluator_alias = home / "cargo" / "bin" / "ostadix-evaluator"
            self.assertIn(
                f"replace {evaluator_alias} from {PROJECT_ROOT / 'target' / 'release' / 'O'}",
                output,
            )
            local_alias = home / ".local" / "bin" / "ostadix-evaluator"
            self.assertIn(
                f"replace {local_alias} from {PROJECT_ROOT / 'target' / 'release' / 'O'}",
                output,
            )

    def test_verify_preflights_installed_hosted_v2_command_surfaces_in_temp_state(self) -> None:
        setup = SETUP.read_text(encoding="utf-8")

        self.assertIn('"$CARGO_BIN_DIR/o-node" serve --help', setup)
        self.assertIn('"$CARGO_BIN_DIR/octl" node session --help', setup)
        self.assertIn("ostadix-hosted-verify.XXXXXX", setup)
        self.assertIn('"$CARGO_BIN_DIR/o-node" pki init', setup)
        self.assertIn('"$CARGO_BIN_DIR/o-node" identity init', setup)
        self.assertIn('"$CARGO_BIN_DIR/octl" node session principal', setup)
        self.assertIn("trap 'rm -rf -- \"$hosted_verify_dir\"' EXIT HUP INT TERM", setup)

    def test_python_setup_is_repository_local_and_never_runs_pip(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result = self.run_setup(
                "--minimal",
                "--dry-run",
                "--no-env",
                home=Path(temp_dir),
            )

        output = self.combined_output(result)
        self.assertEqual(result.returncode, 0, output)
        self.assertIn("repository-local o_lang package", output)
        self.assertNotIn("pip install", output)

    def test_python_reference_version_matches_rust_package(self) -> None:
        cargo_toml = (PROJECT_ROOT / "Cargo.toml").read_text(encoding="utf-8")
        package_version = next(
            line.split("=", 1)[1].strip().strip('"')
            for line in cargo_toml.splitlines()
            if line.startswith("version")
        )
        python_version = next(
            line.split("=", 1)[1].strip().strip('"')
            for line in (PROJECT_ROOT / "o_lang" / "__init__.py")
            .read_text(encoding="utf-8")
            .splitlines()
            if line.startswith("__version__")
        )
        self.assertEqual(python_version, package_version)

    def test_platform_entrypoints_are_thin_canonical_delegates(self) -> None:
        scripts = sorted((PROJECT_ROOT / "setup" / "os").glob("setup-*.sh"))
        self.assertEqual(len(scripts), 12)
        for script in scripts:
            with self.subTest(script=script.name):
                text = script.read_text(encoding="utf-8")
                self.assertIn('exec "$SCRIPT_DIR/../../setup.sh" "$@"', text)
                self.assertNotIn("cargo build", text)
                self.assertNotIn("pip install", text)


if __name__ == "__main__":
    unittest.main()
