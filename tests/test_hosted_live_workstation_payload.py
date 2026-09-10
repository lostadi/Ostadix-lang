#!/usr/bin/env python3
"""Focused structural gates for the Hosted Live development workstation."""

from __future__ import annotations

from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "scripts" / "build-x86_64-hosted-live-linux.sh"
PREPARE = ROOT / "scripts" / "prepare-x86_64-capacity-host.sh"
ISO_BUILDER = ROOT / "ocore" / "kernel" / "build-x86_64-hosted-live-iso.sh"
ISO_PROFILE = ROOT / "evidence" / "hosted_live_physical_iso.toml"
ISO_TOOL = ROOT / "scripts" / "ostadix_capacity_iso.py"
RELEASE = ROOT / "scripts" / "ostadix_hosted_live_release.py"
QEMU_RUNNER = ROOT / "ocore" / "kernel" / "run-x86_64-capacity-iso-qemu.sh"
SERIAL_SMOKE = ROOT / "ocore" / "kernel" / "smoke-x86_64-hosted-live-qemu.py"
VGA_SMOKE = ROOT / "ocore" / "kernel" / "smoke-x86_64-hosted-live-vga-qemu.py"
WORKSTATION_LOCK = ROOT / "evidence" / "hosted_live_workstation_apk_packages.txt"
PHYSICAL_LOCK = ROOT / "evidence" / "hosted_live_physical_apk_packages.txt"

STANDARD_BINARIES = (
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
    "ostadix-device",
)
DECLARED_ROOT_BINARIES = (
    "O",
    "o-cli",
    "olangc",
    "ocorec",
    "o-link",
    "o-unlink",
    "o-notebook",
    "ogit",
    "o-live-host",
    "o-node",
    "octl",
    "o-registry",
    "o-info",
    "ostadix-device",
    "ocore-kernel-world-record",
)
LEGACY_BINARIES = ("O", "o-cli", "olangc", "o-link")
WORKSTATION_PACKAGE_ROOTS = (
    "build-base=0.5-r4",
    "cargo=1.96.1-r0",
    "clang22=22.1.3-r2",
    "eudev=3.2.14-r6",
    "firefox-esr=140.12.0-r0",
    "git=2.54.0-r0",
    "lld22=22.1.3-r0",
    "openbox=3.6.1-r8",
    "openssl=3.5.8-r0",
    "rust=1.96.1-r0",
    "rust-clippy=1.96.1-r0",
    "rust-wasm=1.96.1-r0",
    "rustfmt=1.96.1-r0",
    "wasm-tools=1.236.0-r0",
    "wasmtime=44.0.1-r0",
    "xdg-utils=1.2.1-r1",
    "xf86-input-libinput=1.5.0-r0",
    "xinit=1.4.4-r0",
    "xorg-server=21.1.24-r0",
    "xset=1.2.5-r1",
    "xsetroot=1.1.3-r1",
    "xterm=410-r0",
)


def package_specs(path: Path) -> list[str]:
    return [
        line.strip()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]


def shell_array(source: str, name: str) -> tuple[str, ...]:
    match = re.search(rf"(?m)^{re.escape(name)}=\(\n(?P<body>.*?)\n\)$", source, re.S)
    if match is None:
        raise AssertionError(f"missing shell array {name}")
    return tuple(shlex.split(match.group("body")))


class HostedLiveWorkstationPayloadTests(unittest.TestCase):
    def test_workstation_apk_lock_is_full_sorted_versioned_closure(self) -> None:
        packages = package_specs(WORKSTATION_LOCK)
        self.assertEqual(packages, sorted(set(packages)))
        self.assertEqual(len(packages), 251)
        validator_source = PREPARE.read_text(encoding="utf-8")
        validator_match = re.search(
            r're\.fullmatch\(r"(?P<pattern>[^"]+)", value\)',
            validator_source,
        )
        self.assertIsNotNone(validator_match)
        assert validator_match is not None
        package_pattern = re.compile(validator_match.group("pattern"))
        for package in packages:
            with self.subTest(package=package):
                self.assertIsNotNone(package_pattern.fullmatch(package))
        self.assertIn("libSvtAv1Enc=4.1.0-r0", packages)
        for invalid in ("../escape=1-r0", "pkg", "pkg=1/escape", "=1-r0"):
            with self.subTest(invalid=invalid):
                self.assertIsNone(package_pattern.fullmatch(invalid))
        physical_names = {
            package.partition("=")[0] for package in package_specs(PHYSICAL_LOCK)
        }
        workstation_names = {package.partition("=")[0] for package in packages}
        self.assertTrue(physical_names.issubset(workstation_names))
        for package in WORKSTATION_PACKAGE_ROOTS:
            self.assertIn(package, packages)
        for x_dependency in (
            "eudev-libs=3.2.14-r6",
            "libx11=1.8.13-r0",
            "libxext=1.3.7-r0",
            "libxmu=1.3.1-r0",
            "libinput-libs=1.31.3-r0",
            "xauth=1.1.5-r0",
            "xkbcomp=1.5.0-r0",
            "xorg-server-common=21.1.24-r0",
            "xmodmap=1.0.11-r1",
            "xrdb=1.2.2-r0",
        ):
            self.assertIn(x_dependency, packages)
        self.assertFalse(any(package.startswith("qemu") for package in packages))

    def test_worker_builds_every_declared_root_binary_and_independent_mcp(self) -> None:
        cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        declared = tuple(binary["name"] for binary in cargo["bin"])
        self.assertEqual(declared, DECLARED_ROOT_BINARIES)

        source = BUILD.read_text(encoding="utf-8")
        self.assertEqual(shell_array(source, "HOSTED_STANDARD_BINARIES"), STANDARD_BINARIES)
        self.assertEqual(shell_array(source, "HOSTED_ROOT_BINARIES"), declared)
        self.assertIn('HOSTED_BINARIES=("${HOSTED_ROOT_BINARIES[@]}" ostadix-mcp)', source)
        self.assertIn("--features notebook", source)
        self.assertIn(
            '--manifest-path "$SOURCE_ROOT/mcp/ostadix_lang_mcp_server/Cargo.toml"',
            source,
        )
        self.assertIn("--package ostadix-mcp-server", source)
        self.assertIn("--bin ostadix-mcp", source)
        self.assertIn("for binary in \"${HOSTED_BINARIES[@]}\"; do", source)
        self.assertIn("cargo vendor", source)
        self.assertIn("--versioned-dirs", source)
        self.assertIn(
            '--sync "$SOURCE_ROOT/mcp/ostadix_lang_mcp_server/Cargo.toml"', source
        )
        self.assertEqual(source.count("CARGO_NET_OFFLINE=true"), 4)
        self.assertIn('"schema": "ostadix.cargo-vendor-manifest/v1"', source)
        self.assertIn('OCORE_BUILD_ROOT="$RUN_ROOT/ocore-kernel"', source)
        self.assertIn("OCORE_BOOT_INFO_ENABLED=1", source)
        self.assertIn("OCORE_PROBE_MODE=0", source)
        self.assertIn('OCORE_LLD="$OCORE_LLD_PATH"', source)
        self.assertIn('OSTADIX_HOSTED_LIVE_OCORE_KERNEL="$OCORE_KERNEL"', source)
        self.assertIn("smoke-x86_64-hosted-live-ocore-qemu.py", source)
        self.assertIn('"schema": "ostadix.hosted-live-boot-gates/v6"', source)

    def test_prepare_requires_all_binaries_and_embeds_exact_source_copy(self) -> None:
        source = PREPARE.read_text(encoding="utf-8")
        self.assertEqual(shell_array(source, "HOSTED_STANDARD_BINARIES"), STANDARD_BINARIES)
        self.assertEqual(
            shell_array(source, "HOSTED_ROOT_BINARIES"), DECLARED_ROOT_BINARIES
        )
        self.assertEqual(shell_array(source, "HOSTED_LEGACY_BINARIES"), LEGACY_BINARIES)
        self.assertIn('HOSTED_BINARIES=("${HOSTED_ROOT_BINARIES[@]}" ostadix-mcp)', source)
        self.assertIn('HOSTED_IMAGE_BINARIES=("${HOSTED_BINARIES[@]}")', source)
        self.assertIn('HOSTED_IMAGE_BINARIES=("${HOSTED_LEGACY_BINARIES[@]}")', source)
        self.assertIn('for binary in "${HOSTED_IMAGE_BINARIES[@]}"; do', source)
        self.assertIn("OSTADIX_HOSTED_SOURCE_ARCHIVE_SHA256", source)
        self.assertIn("cp -R --preserve=mode --no-preserve=ownership \\", source)
        self.assertIn(
            '"$HOSTED_SOURCE_ROOT/." "$STAGE/usr/src/ostadix/"', source
        )
        self.assertEqual(
            source.count("cp -R --preserve=mode --no-preserve=ownership"), 3
        )
        self.assertIn(
            "embedded /usr/src/ostadix differs from the tracked source snapshot", source
        )
        self.assertIn("source.path=/usr/src/ostadix", source)
        self.assertIn("source.archive.sha256=%s", source)
        self.assertNotIn('>"$HOSTED_SOURCE_ROOT', source)
        self.assertIn("/usr/share/ostadix/cargo/vendor", source)
        self.assertIn("/usr/share/ostadix/cargo/cargo-vendor-manifest.json", source)
        self.assertIn("/root/.cargo/config.toml", source)
        self.assertIn('directory = "/usr/share/ostadix/cargo/vendor"', source)
        self.assertIn("/usr/share/ostadix/wasm/hello.wasm", source)
        self.assertIn("/usr/share/ostadix/wasm/hello.release.json", source)
        self.assertIn("scripts/ostadix_wasm_release.py", source)

    def test_worker_selects_workstation_lock_and_binds_source_archive(self) -> None:
        source = BUILD.read_text(encoding="utf-8")
        self.assertIn("evidence/hosted_live_workstation_apk_packages.txt", source)
        self.assertIn(
            'OSTADIX_HOSTED_SOURCE_ARCHIVE_SHA256="$ARCHIVE_SHA256"', source
        )
        for package in WORKSTATION_PACKAGE_ROOTS:
            self.assertIn(f'"{package}"', source)
        self.assertIn('"schema": "ostadix.hosted-live-release/v6"', source)
        self.assertIn('"rootfs": rootfs_identity', source)
        self.assertIn('"ventoy_modloop": ventoy_modloop_identity', source)
        self.assertIn(
            "strict ISO inspection disagrees with the Ventoy modloop", source
        )
        self.assertIn('"rootfs_layout": "verified-squashfs-plus-tmpfs-overlay"', source)
        self.assertIn(
            '"ventoy_compatibility": "alpine-hook-plus-minimal-modloop"', source
        )
        self.assertIn('"cargo_vendor_manifest": identity(', source)
        self.assertIn('"boot_objects_archive_sha256":', source)
        self.assertIn('"operation": "verify"', source)
        self.assertIn('"kind": "physical-hosted-workstation-plus-capacity"', source)
        self.assertIn('"rootfs_objects": {', source)
        self.assertIn('"olangc_wasm_hello": {', source)
        self.assertIn('"descriptor": olangc_wasm', source)

    def test_olangc_wasm_is_a_source_bound_first_class_rootfs_object(self) -> None:
        worker = BUILD.read_text(encoding="utf-8")
        prepare = PREPARE.read_text(encoding="utf-8")

        materialize = worker.index('--materialize-only "$WASM_PROJECT"')
        native_build = worker.index('--locked --release --target "$WASM_TARGET"')
        descriptor_create = worker.index(
            'python3 "$SOURCE_ROOT/scripts/ostadix_wasm_release.py" create'
        )
        descriptor_verify = worker.index(
            'python3 "$SOURCE_ROOT/scripts/ostadix_wasm_release.py" verify',
            descriptor_create,
        )
        prepare_rootfs = worker.index(
            'OSTADIX_HOSTED_WASM_ARTIFACT="$WASM_ARTIFACT"'
        )
        squashfs_extract = worker.index(
            'unsquashfs -f -d "$ROOTFS_WASM_EXTRACT" "$ROOTFS_IMAGE"'
        )
        embedded_verify = worker.index(
            'python3 "$SOURCE_ROOT/scripts/ostadix_wasm_release.py" verify',
            squashfs_extract,
        )
        self.assertEqual(
            [
                materialize,
                native_build,
                descriptor_create,
                descriptor_verify,
                prepare_rootfs,
                squashfs_extract,
                embedded_verify,
            ],
            sorted(
                [
                    materialize,
                    native_build,
                    descriptor_create,
                    descriptor_verify,
                    prepare_rootfs,
                    squashfs_extract,
                    embedded_verify,
                ]
            ),
        )
        self.assertIn("WASM_TARGET=wasm32-wasip1", worker)
        self.assertIn(
            'rustup target add --toolchain "$RUST_TOOLCHAIN" "$WASM_TARGET"',
            worker,
        )
        self.assertIn('! -e "$WASM_PROJECT/target"', worker)
        self.assertIn('! -e "$WASM_ARTIFACT"', worker)
        self.assertIn("CARGO_NET_OFFLINE=true", worker)
        self.assertIn("CARGO_PROFILE_RELEASE_LTO=false", worker)
        self.assertIn("CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16", worker)
        self.assertIn("CARGO_PROFILE_RELEASE_OPT_LEVEL=1", worker)
        self.assertIn('cmp -s "$WASM_ARTIFACT" "$ROOTFS_WASM_ARTIFACT"', worker)
        self.assertIn('cmp -s "$WASM_MANIFEST" "$ROOTFS_WASM_MANIFEST"', worker)

        self.assertIn('[[ -f "$HOSTED_WASM_ARTIFACT" && ! -L', prepare)
        self.assertIn('[[ -f "$HOSTED_WASM_MANIFEST" && ! -L', prepare)
        self.assertIn('! -e "$HOSTED_WASM_PROJECT/target"', prepare)
        self.assertIn(
            'install -m 0444 "$HOSTED_WASM_ARTIFACT" \\\n'
            '    "$STAGE/usr/share/ostadix/wasm/hello.wasm"',
            prepare,
        )
        self.assertIn(
            'install -m 0444 "$HOSTED_WASM_MANIFEST" \\\n'
            '    "$STAGE/usr/share/ostadix/wasm/hello.release.json"',
            prepare,
        )
        self.assertIn('--project "$HOSTED_WASM_PROJECT"', prepare)
        self.assertIn(
            '--input "$STAGE/usr/src/ostadix/examples/wasm_hello.O"', prepare
        )
        self.assertIn('--generator "$STAGE/usr/local/bin/olangc"', prepare)
        self.assertIn('mount_read_only_tree /usr/share/ostadix/wasm', prepare)
        self.assertIn("wasm.artifact.bytes=%s", prepare)
        self.assertIn("wasm.artifact.sha256=%s", prepare)
        self.assertIn("wasm.manifest.sha256=%s", prepare)

    def test_boot_gates_prove_toolchain_compile_and_binary_presence(self) -> None:
        source = PREPARE.read_text(encoding="utf-8")
        markers = (
            "OSTADIX HOSTED ROOTFS OVERLAY: PASS",
            "OSTADIX HOSTED READ-ONLY TREES: PASS",
            "OSTADIX HOSTED LOOPBACK: PASS",
            "OSTADIX HOSTED APK: PASS",
            "OSTADIX HOSTED RUSTC: PASS",
            "OSTADIX HOSTED CARGO: PASS",
            "OSTADIX HOSTED RUSTFMT: PASS",
            "OSTADIX HOSTED CLIPPY: PASS",
            "OSTADIX HOSTED CARGO HELLO: PASS",
            "OSTADIX HOSTED ENTROPY: PASS",
            "OSTADIX HOSTED O-NODE: PASS",
            "OSTADIX HOSTED NOTEBOOK: PASS",
            "OSTADIX HOSTED STANDARD BINARIES: PASS",
            "OSTADIX HOSTED DECLARED ROOT BINARIES: PASS",
            "OSTADIX HOSTED UNIFIED ROUTES: PASS",
            "OSTADIX HOSTED SOURCE SNAPSHOT: PASS",
            "OSTADIX HOSTED OLANGC MATERIALIZATION: PASS",
            "OSTADIX HOSTED OLANGC WASM ARTIFACT: PASS",
            "OSTADIX HOSTED RUST WASM: PASS",
            "OSTADIX HOSTED WASM RUNTIME: PASS",
            "OSTADIX HOSTED OLANGC WASM EXECUTION: PASS",
            "OSTADIX HOSTED WEBASSEMBLY BACKEND: PASS",
            "OSTADIX HOSTED MCP: PASS",
            "OSTADIX BOOT OBJECTS: PASS",
            "OSTADIX HOSTED SOURCE OBJECT CLOSURE: PASS",
            "OSTADIX HOSTED LIVE READY",
        )
        positions = [source.index(marker) for marker in markers]
        self.assertEqual(positions, sorted(positions))
        self.assertIn("entropy_device=virtio-rng-pci", source)
        self.assertIn("ip link set dev lo up", source)
        self.assertIn("*'<LOOPBACK,UP,'*", source)
        self.assertLess(
            source.index("ip link set dev lo up"),
            source.index("o node start --startup-timeout-seconds 30"),
        )
        self.assertIn("probe = os.getrandom(32)", source)
        self.assertIn("crng_bytes=32 available=$entropy_available", source)
        for marker in markers[:-1]:
            self.assertIn(marker.replace(": PASS", ": FAIL"), source)
        self.assertIn("cargo run --offline --quiet", source)
        self.assertIn("cargo fmt --manifest-path", source)
        self.assertIn("cargo clippy --offline --quiet", source)
        self.assertIn("o node start --startup-timeout-seconds 30", source)
        self.assertIn("--fresh-pki-key-algorithm ec-p256", source)
        self.assertIn("node_smoke_stage=start-command", source)
        self.assertIn("status=$node_smoke_status pki=ec-p256", source)
        self.assertIn("development PKI key algorithm: ec-p256", source)
        self.assertIn("pairing CA key algorithm: ec-p256", source)
        self.assertIn('$node_smoke_state/ostadix/node/o-node.log', source)
        self.assertIn('tail -c 16384 "$node_diagnostic_path"', source)
        diagnostic = 'emit_line "O-NODE DIAGNOSTIC: stage=$node_smoke_stage'
        terminal = 'emit_error "OSTADIX HOSTED O-NODE: FAIL:'
        self.assertLess(source.index(diagnostic), source.index(terminal))
        self.assertIn("OSTADIX_NOTEBOOK_NO_OPEN=1", source)
        self.assertIn('root_url = "http://127.0.0.1:8888/"', source)
        self.assertIn("ostadix-cargo-hello", source)
        self.assertIn("/usr/src/ostadix/Cargo.toml", source)
        self.assertIn('/usr/src/ostadix/scripts/o-cli.sh "$@"', source)
        self.assertIn("o object verify", source)
        self.assertIn(
            "python3 /usr/src/ostadix/scripts/ostadix_boot_objects.py verify", source
        )
        self.assertIn("--source-root /usr/src/ostadix --json", source)
        self.assertIn("OSTADIX_O_INFO_BIN=/usr/local/bin/o-info", source)
        self.assertIn("PYTHONDONTWRITEBYTECODE=1", source)
        self.assertIn("mount -o remount,bind,ro", source)
        self.assertIn("for (option_index in options)", source)
        self.assertNotIn("for (index in options)", source)
        self.assertIn("CARGO_PROFILE_RELEASE_LTO=false", source)
        self.assertIn("python3 /usr/src/ostadix/scripts/smoke_ostadix_mcp.py", source)
        self.assertIn("--binary /usr/local/bin/ostadix-mcp", source)
        self.assertIn("--server-cwd /workspace", source)
        self.assertIn("--require-wasm-materialization", source)
        self.assertIn(
            "--wasm-release-manifest /usr/share/ostadix/wasm/hello.release.json",
            source,
        )
        self.assertIn(
            "--wasm-release-artifact /usr/share/ostadix/wasm/hello.wasm", source
        )
        self.assertIn('--wasm-source-tree "$hosted_source_tree"', source)
        self.assertIn('--wasm-base-commit "$hosted_base_commit"', source)
        self.assertIn(
            '--wasm-source-archive-sha256 "$hosted_archive_sha256"', source
        )
        self.assertIn("--timeout 60", source)
        self.assertNotRegex(source, r"(?m)^\s*--require-wasm(?:\s|\\|$)")
        self.assertNotIn("--wasm-timeout 1500", source)
        self.assertNotIn("--wasm-timeout 720", source)
        self.assertIn("rustc --edition 2021 --target wasm32-wasip1", source)
        self.assertIn(
            "verify-module /tmp/ostadix-rust-wasm-probe.wasm", source
        )
        self.assertIn("wasmtime /usr/share/ostadix/wasm/hello.wasm", source)
        wasm_execution = source.index("olangc_wasm_run_status=0")
        wasm_diagnostic = source.index(
            "OLANGC WASM DIAGNOSTIC: status=", wasm_execution
        )
        wasm_failure = source.index(
            "OSTADIX HOSTED OLANGC WASM EXECUTION: FAIL", wasm_diagnostic
        )
        self.assertLess(wasm_execution, wasm_diagnostic)
        self.assertLess(wasm_diagnostic, wasm_failure)
        self.assertIn("timeout -s TERM -k 5 300", source[wasm_execution:wasm_failure])
        self.assertIn(
            'emit_line "OLANGC WASM DIAGNOSTIC: $olangc_wasm_diagnostic_line"',
            source[wasm_diagnostic:wasm_failure],
        )
        self.assertNotIn(
            "tail -c 16384 /tmp/ostadix-olangc-wasm-run.err >&2",
            source[wasm_execution:wasm_failure],
        )
        self.assertIn("wasm-tools --version", source)
        self.assertIn(
            'O /opt/ostadix/examples/webassembly_hello.O "$O_BACKENDS_DIR"',
            source,
        )
        webassembly_start = source.index("webassembly_backend_status=0")
        webassembly_failure = source.index(
            "OSTADIX HOSTED WEBASSEMBLY BACKEND: FAIL", webassembly_start
        )
        webassembly_gate = source[webassembly_start:webassembly_failure]
        self.assertIn("timeout -s TERM -k 5 120", webassembly_gate)
        self.assertIn("grep -Fqx 'OSTADIX WEBASSEMBLY BACKEND PASS'", webassembly_gate)
        self.assertIn("WEBASSEMBLY BACKEND DIAGNOSTIC: status=", webassembly_gate)
        self.assertIn(
            'emit_line "WEBASSEMBLY BACKEND DIAGNOSTIC: $webassembly_backend_diagnostic_line"',
            webassembly_gate,
        )
        shim = (ROOT / "backends" / "webassembly_shim.py").read_text(
            encoding="utf-8"
        )
        self.assertIn('"wasm-tools", "parse"', shim)
        embedded_shim = (
            ROOT / "crates" / "ostadix-api" / "backends" / "webassembly_shim.py"
        ).read_text(encoding="utf-8")
        self.assertEqual(shim, embedded_shim)
        native_backend = (ROOT / "crates" / "ostadix-api" / "src" / "backend.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn('tools.contains("wasm-tools")', native_backend)
        self.assertIn('command.arg("parse")', native_backend)
        self.assertIn('tail -c 16384 "$mcp_diagnostic_path"', source)
        self.assertLess(
            source.index('emit_line "MCP DIAGNOSTIC: $mcp_diagnostic_path"'),
            source.index("emit_error 'OSTADIX HOSTED OLANGC MATERIALIZATION: FAIL'"),
        )
        self.assertIn("--o-info /usr/local/bin/o-info", source)
        self.assertIn("--runtime-bin-dir /usr/local/bin", source)
        self.assertIn("ostadix-mcp stdio release smoke: PASS", source)

    def test_notebook_boot_probe_queues_one_bounded_backend_evaluation(self) -> None:
        source = PREPARE.read_text(encoding="utf-8")
        probe_start = source.index("notebook_probe_status=0")
        probe_end = source.index(
            "OSTADIX HOSTED STANDARD BINARIES: PASS", probe_start
        )
        probe = source[probe_start:probe_end]

        readiness = probe.index("deadline = time.monotonic() + 30")
        request = probe.index("request = urllib.request.Request(", readiness)
        evaluation = probe.index(
            "urllib.request.urlopen(request, timeout=120)", request
        )
        success = probe.index("OSTADIX HOSTED NOTEBOOK: PASS", evaluation)
        diagnostic = probe.index("NOTEBOOK DIAGNOSTIC: probe_status=", success)
        failure = probe.index("OSTADIX HOSTED NOTEBOOK: FAIL", diagnostic)
        self.assertEqual(
            [readiness, request, evaluation, success, diagnostic, failure],
            sorted([readiness, request, evaluation, success, diagnostic, failure]),
        )
        self.assertIn("timeout -s TERM -k 5 180 python3", probe)
        self.assertEqual(probe.count("urllib.request.Request("), 1)
        self.assertEqual(probe.count("urlopen(request"), 1)
        self.assertIn("notebook root did not become ready", probe)
        self.assertIn("notebook evaluation returned an invalid response", probe)
        self.assertIn("/tmp/ostadix-notebook-probe.err", probe)
        self.assertIn('tail -c 16384 "$notebook_diagnostic_path"', probe)
        self.assertIn(
            'emit_line "NOTEBOOK DIAGNOSTIC: $notebook_diagnostic_line"', probe
        )
        self.assertNotIn(
            'tail -c 16384 "$notebook_diagnostic_path" >&2', probe
        )
        self.assertNotIn("for attempt in $(seq 1 100)", probe)

    def test_small_initramfs_bootstraps_digest_bound_squashfs_before_stage_two(self) -> None:
        source = PREPARE.read_text(encoding="utf-8")
        bootstrap = source.index("tee \"$BOOT_STAGE/init\"")
        bootstrap_source = source[bootstrap:]
        hook_begin = source.index("ebegin 'Mounting boot media'", bootstrap)
        hook_end = source.index("eend 0", hook_begin)
        dm_mod = source.index("modprobe dm_mod", hook_end)
        modloop_argument = source.index(
            '[ "$modloop_argument" = "$OSTADIX_VENTOY_MODLOOP_PATH" ]', dm_mod
        )
        retry = source.index('while [ "$attempt" -lt 30 ] && [ -z "$media" ]; do', modloop_argument)
        blkid = source.index(
            'block_identity=" $("$BB" blkid "$device" 2>/dev/null || true) "', retry
        )
        label_token = source.index('*\' LABEL="OSTADIX_CAPACITY" \'*)', blkid)
        byte_check = source.index('[ "$1" = "$OSTADIX_ROOTFS_BYTES" ]', label_token)
        hash_check = source.index('[ "$1" = "$OSTADIX_ROOTFS_SHA256" ]', byte_check)
        retained_rootfs = source.index('rootfs_file=$candidate_rootfs', hash_check)
        stale_unmount = source.index('"$BB" umount /media/ostadix || true', retained_rootfs)
        retry_increment = source.index('attempt=$((attempt + 1))', stale_unmount)
        retry_sleep = source.index('[ -n "$media" ] || "$BB" sleep 1', retry_increment)
        loop = source.index('losetup -r "$loop_device" "$rootfs_file"', hash_check)
        squashfs = source.index('mount -t squashfs -o ro "$loop_device" /lower', loop)
        tmpfs = source.index("mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /upper", squashfs)
        overlay = source.index("mount -t overlay", tmpfs)
        move_media = source.index("mount --move /media/ostadix", overlay)
        move_lower = source.index("mount --move /lower", move_media)
        move_upper = source.index("mount --move /upper", move_lower)
        rootfs_ready = source.index("OSTADIX HOSTED ROOTFS: PASS bytes=", move_upper)
        switch_root = source.index("switch_root /newroot /init", rootfs_ready)
        self.assertEqual(
            [
                hook_begin,
                hook_end,
                dm_mod,
                modloop_argument,
                retry,
                blkid,
                label_token,
                byte_check,
                hash_check,
                retained_rootfs,
                stale_unmount,
                retry_increment,
                retry_sleep,
                loop,
                squashfs,
                tmpfs,
                overlay,
                move_media,
                move_lower,
                move_upper,
                rootfs_ready,
                switch_root,
            ],
            sorted(
                [
                    hook_begin,
                    hook_end,
                    dm_mod,
                    modloop_argument,
                    retry,
                    blkid,
                    label_token,
                    byte_check,
                    hash_check,
                    retained_rootfs,
                    stale_unmount,
                    retry_increment,
                    retry_sleep,
                    loop,
                    squashfs,
                    tmpfs,
                    overlay,
                    move_media,
                    move_lower,
                    move_upper,
                    rootfs_ready,
                    switch_root,
                ]
            ),
        )
        self.assertIn("OSTADIX_ROOTFS_BYTES=%s", source)
        self.assertIn("OSTADIX_ROOTFS_SHA256=%s", source)
        self.assertIn("OSTADIX_VENTOY_MODLOOP_PATH=%s", source)
        self.assertIn('"ostadix.capacity-host-initramfs/v2"', source)
        self.assertIn("cannot locate OSTADIX_CAPACITY media by label", source)
        self.assertIn("attempt=0", bootstrap_source)
        self.assertIn('*\' LABEL="OSTADIX_CAPACITY" \'*)', bootstrap_source)
        self.assertNotIn("blkid -s", bootstrap_source)
        self.assertNotIn("blkid -o", bootstrap_source)
        self.assertIn('pack_cpio "$BOOT_STAGE" "$CANDIDATE"', source)
        self.assertIn('"kernel/drivers/md/dm-mod.ko"', source)
        for controller in ("ehci-pci.ko", "ohci-pci.ko", "uhci-hcd.ko", "xhci-pci.ko"):
            self.assertIn(controller, source)
        self.assertIn("ehci_pci ohci_pci uhci_hcd xhci_pci", bootstrap_source)
        self.assertIn('VENTOY_MODLOOP_ROOT="$WORK_DIR/ventoy-modloop-root"', source)
        self.assertIn(
            'mksquashfs "$VENTOY_MODLOOP_ROOT" "$VENTOY_MODLOOP_CANDIDATE"',
            source,
        )
        self.assertEqual(source.count("env -u SOURCE_DATE_EPOCH"), 2)
        rootfs_build = source.index('mksquashfs "$STAGE"')
        for first_class_content in (
            'chroot "$STAGE" /sbin/apk',
            '"$HOSTED_SOURCE_ROOT/." "$STAGE/usr/src/ostadix/"',
            '"$STAGE/usr/share/ostadix/cargo/vendor"',
            '"$STAGE/usr/share/ostadix/boot-objects/v1"',
            '"$STAGE/usr/share/ostadix/wasm/hello.wasm"',
            '"$STAGE/usr/share/ostadix/wasm/hello.release.json"',
            '"$STAGE/usr/local/bin/ostadix-desktop"',
        ):
            self.assertLess(source.index(first_class_content), rootfs_build)
        self.assertLess(rootfs_build, bootstrap)
        self.assertLess(source.index('chmod 0444 "$BOOT_STAGE/etc/ostadix-rootfs.identity"'), bootstrap)
        self.assertIn("capacity-host outputs must resolve to distinct paths", source)
        self.assertIn("mksquashfs tar tee unsquashfs", BUILD.read_text(encoding="utf-8"))

    def test_capacity_host_initramfs_admits_labeled_ventoy_mapper_media(self) -> None:
        source = PREPARE.read_text(encoding="utf-8")
        init_start = source.index('tee "$STAGE/init"')
        init_end = source.index('\nINIT\nchmod 0755 "$STAGE/init"', init_start)
        init_source = source[init_start:init_end]

        hook_begin = init_source.index("ebegin 'Mounting boot media'")
        hook_end = init_source.index("eend 0", hook_begin)
        dm_mod = init_source.index("modprobe dm_mod", hook_end)
        retry = init_source.index(
            'while [ "$attempt" -lt 30 ] && [ -z "$media" ]; do', dm_mod
        )
        mapper = init_source.index("/dev/mapper/ventoy /dev/dm-*", retry)
        label = init_source.index('*\' LABEL="OSTADIX_CAPACITY" \'*)', mapper)
        lock = init_source.index(
            "/media/ostadix/ostadix/capacity.lock.json", label
        )
        retry_sleep = init_source.index('[ -n "$media" ] || sleep 1', lock)

        self.assertEqual(
            [hook_begin, hook_end, dm_mod, retry, mapper, label, lock, retry_sleep],
            sorted(
                [
                    hook_begin,
                    hook_end,
                    dm_mod,
                    retry,
                    mapper,
                    label,
                    lock,
                    retry_sleep,
                ]
            ),
        )
        self.assertIn("mdev -s 2>/dev/null || true", init_source)
        self.assertIn("cdrom sr_mod isofs dm_mod", init_source)
        self.assertNotIn(
            "for device in /dev/sr0 /dev/cdrom /dev/sda /dev/vda", init_source
        )

        virt_start = source.index('if [[ "$ALPINE_KERNEL_FLAVOR" == virt ]]')
        virt_end = source.index("\nelse\n", virt_start)
        self.assertIn(
            "kernel/drivers/md/dm-mod.ko", source[virt_start:virt_end]
        )

    def test_physical_iso_keeps_rootfs_separate_and_binds_14_typed_artifacts(self) -> None:
        profile = tomllib.loads(ISO_PROFILE.read_text(encoding="utf-8"))
        self.assertEqual(len(profile["artifacts"]), 14)
        self.assertEqual(
            [entry["id"] for entry in profile["entries"]],
            ["hosted", "ocore", "alpine", "guix", "openbsd", "plan9", "redox"],
        )
        hosted = profile["entries"][0]
        self.assertEqual(hosted["adapter"], "linux-live-rootfs")
        self.assertEqual(hosted["initrd_paths"], ["/boot/hosted/initramfs.cpio.gz"])
        self.assertEqual(hosted["rootfs_path"], "/boot/hosted/rootfs.squashfs")
        self.assertEqual(hosted["modloop_path"], "/boot/modloop-lts")
        for separate_artifact in (hosted["rootfs_path"], hosted["modloop_path"]):
            self.assertNotIn(separate_artifact, hosted["initrd_paths"])
        artifacts = {
            (artifact["iso_path"], artifact["role"])
            for artifact in profile["artifacts"]
        }
        self.assertEqual(
            artifacts,
            {
                ("/boot/hosted/vmlinuz-lts", "linux-kernel"),
                ("/boot/hosted/initramfs.cpio.gz", "linux-initrd"),
                ("/boot/hosted/rootfs.squashfs", "linux-rootfs"),
                ("/boot/modloop-lts", "linux-modloop"),
                ("/boot/ocore/kernel.elf", "ocore-kernel"),
                ("/boot/capacity-host/vmlinuz-virt", "linux-kernel"),
                ("/boot/capacity-host/initramfs.cpio.gz", "linux-initrd"),
                ("/boot/entry/010-alpine/initramfs-virt", "linux-initrd"),
                ("/ostadix/guix/linux-libre-6.17.12-bzimage", "linux-kernel"),
                ("/ostadix/guix/guix-1.5.0-initrd.cpio.gz", "linux-initrd"),
                (
                    "/ostadix/guix/guix-system-install-1.5.0.x86_64-linux.iso",
                    "guest-rootfs",
                ),
                ("/ostadix/openbsd/install79.iso", "guest-raw-cd"),
                ("/ostadix/9front/9front-11983.amd64.qcow2", "guest-qcow2"),
                ("/ostadix/redox/redox-server-0.9.0-livedisk.iso", "guest-raw-cd"),
            },
        )
        iso_builder = ISO_BUILDER.read_text(encoding="utf-8")
        self.assertIn('install_artifact "$ROOTFS" boot/hosted/rootfs.squashfs', iso_builder)
        self.assertIn('install_artifact "$VENTOY_MODLOOP" boot/modloop-lts', iso_builder)
        self.assertIn(
            'install_artifact "$CAPACITY_HOST_INITRAMFS" boot/capacity-host/initramfs.cpio.gz',
            iso_builder,
        )
        self.assertIn("guix-system-install-1.5.0.x86_64-linux.iso", iso_builder)
        self.assertIn("install79.iso", iso_builder)
        self.assertIn("9front-11983.amd64.qcow2", iso_builder)
        self.assertIn("redox-server-0.9.0-livedisk.iso", iso_builder)
        iso_tool = ISO_TOOL.read_text(encoding="utf-8")
        self.assertIn('injected.append(f"ostadix.rootfs={entry[\'rootfs_path\']}")', iso_tool)
        self.assertIn('injected.append(f"modloop={entry[\'modloop_path\']}")', iso_tool)
        self.assertIn('lines.append("    initrd " + " ".join(entry["initrd_paths"]))', iso_tool)

    def test_release_and_smokes_pin_split_rootfs_schemas_and_four_gib_regression(self) -> None:
        worker = BUILD.read_text(encoding="utf-8")
        release = RELEASE.read_text(encoding="utf-8")
        serial = SERIAL_SMOKE.read_text(encoding="utf-8")
        visual = VGA_SMOKE.read_text(encoding="utf-8")
        runner = QEMU_RUNNER.read_text(encoding="utf-8")
        self.assertIn('"schema": "ostadix.hosted-live-release/v6"', worker)
        self.assertIn('"schema": "ostadix.hosted-live-boot-gates/v6"', worker)
        self.assertIn('"ostadix.hosted-live-release/v6"', release)
        self.assertIn('"ostadix.hosted-live-boot-gates/v6"', release)
        self.assertIn(
            'identity(payload.get("ventoy_modloop"), "Ventoy compatibility modloop")',
            release,
        )
        self.assertIn('("/boot/modloop-lts", "linux-modloop"),', release)
        self.assertIn('SMOKE_SCHEMA = "ostadix.hosted-live-qemu-smoke/v4"', serial)
        self.assertIn(
            'VISUAL_SMOKE_SCHEMA = "ostadix.hosted-live-qemu-visual-smoke/v7"',
            visual,
        )
        for smoke in (serial, visual):
            self.assertIn("OSTADIX HOSTED ROOTFS: PASS bytes=", smoke)
            self.assertIn("OSTADIX HOSTED ROOTFS OVERLAY: PASS", smoke)
            self.assertIn("OSTADIX HOSTED APK: PASS", smoke)
        self.assertIn('"-m", "4096M"', runner)
        self.assertIn('"-m", "4096M"', visual)
        for smoke_command in (runner, visual):
            self.assertIn(
                '"-object", "rng-random,filename=/dev/urandom,id=ostadix_rng"',
                smoke_command,
            )
            self.assertIn(
                '"-device", "virtio-rng-pci,rng=ostadix_rng"', smoke_command
            )
            self.assertLess(
                smoke_command.index("virtio-rng-pci"),
                smoke_command.index('"-nic"'),
            )
        self.assertIn('"entropy": {', serial)
        self.assertIn('"entropy": entropy_identity', visual)
        self.assertIn('timeout -s TERM -k 5 900 o node start', PREPARE.read_text())
        self.assertIn('--fresh-pki-key-algorithm ec-p256', PREPARE.read_text())
        self.assertIn("DEFAULT_TIMEOUT_SECONDS = 1800.0", serial)
        self.assertIn("MAX_TIMEOUT_SECONDS = 1800.0", serial)
        self.assertIn("DEFAULT_TIMEOUT_SECONDS = 1800.0", visual)
        self.assertIn("MAX_TIMEOUT_SECONDS = 1800.0", visual)
        self.assertIn(
            "HOSTED_SMOKE_TIMEOUT=${OSTADIX_HOSTED_LIVE_SMOKE_TIMEOUT:-1800}",
            worker,
        )
        self.assertIn(
            "OCORE_SMOKE_TIMEOUT=${OSTADIX_HOSTED_LIVE_OCORE_SMOKE_TIMEOUT:-900}",
            worker,
        )
        self.assertIn('--timeout "$OCORE_SMOKE_TIMEOUT"', worker)
        self.assertIn("virtio_pci virtio_rng", PREPARE.read_text())
        self.assertIn("/proc/sys/kernel/random/entropy_avail", PREPARE.read_text())

    def test_desktop_helper_and_full_x_toolchain_are_installed_after_cli_gate(self) -> None:
        source = PREPARE.read_text(encoding="utf-8")
        for command in (
            "clang",
            "ld.lld",
            "mkfontdir",
            "mkfontscale",
            "openvt",
            "startx",
            "Xorg",
            "openbox",
            "xsetroot",
            "xterm",
            "udevd",
            "udevadm",
        ):
            self.assertIn(command, source)
        self.assertIn("/usr/lib/xorg/modules/input/libinput_drv.so", source)
        self.assertIn("kernel/drivers/input/evdev.ko", source)
        self.assertIn(
            'MODLOOP="$CACHE_ROOT/modloop-$ALPINE_KERNEL_FLAVOR-3.24.1-x86_64"',
            source,
        )
        self.assertIn("ALPINE_MODLOOP_BYTES=303034368", source)
        self.assertIn(
            "ALPINE_MODLOOP_SHA256=871ef51ed6378283db9462947bb7fb84c1ec31376611eb1a2281b02b9404c0f6",
            source,
        )
        self.assertIn("hid_generic evdev simpledrm", source)
        self.assertIn("/usr/share/fonts/misc/fonts.dir", source)
        self.assertIn("/usr/share/fonts/misc/fonts.alias", source)
        self.assertIn("misc-fixed", source)
        self.assertIn("mkdir -p /dev/pts", source)
        self.assertIn(
            "mount -t devpts -o gid=5,mode=0620,ptmxmode=0666 devpts /dev/pts",
            source,
        )
        self.assertIn(
            'install -m 0555 "$HOSTED_DESKTOP_HELPER" "$STAGE/usr/local/bin/ostadix-desktop"',
            source,
        )
        ready = source.index("OSTADIX HOSTED LIVE READY")
        launch = source.index("ostadix-desktop launch")
        self.assertLess(ready, launch)
        self.assertIn("OSTADIX HOSTED DESKTOP: FAIL: launcher returned nonzero", source)

    def test_live_o_wrapper_delegates_every_repository_route(self) -> None:
        source = PREPARE.read_text(encoding="utf-8")
        self.assertIn('exec /usr/src/ostadix/scripts/o-cli.sh "$@"', source)
        for variable in (
            "O_LANG_OCLI_BIN",
            "O_LANG_OLANGC_BIN",
            "O_LANG_EVALUATOR_BIN",
            "O_LANG_KERNEL_CLI_BIN",
            "O_LANG_CAPACITY_BIN",
            "O_LANG_LIVE_BIN",
            "O_LANG_OGIT_BIN",
            "O_LANG_NODE_BIN",
            "O_LANG_OCTL_BIN",
            "O_LANG_REGISTRY_BIN",
            "O_LANG_INFO_BIN",
        ):
            self.assertIn(f"export {variable}=", source)
        for route in (
            "kernel help",
            "capacity --help",
            "node --help",
            "node-host --help",
            "registry --help",
            "info --help",
            "live --help",
            "receipt --help",
        ):
            self.assertIn(f"o {route}", source)
        self.assertIn("OSTADIX HOSTED UNIFIED ROUTES: PASS", source)
        self.assertIn("OSTADIX HOSTED UNIFIED ROUTES: FAIL", source)

    def test_workstation_only_commands_do_not_break_legacy_virt_payload(self) -> None:
        source = PREPARE.read_text(encoding="utf-8")
        rust_check = source.index(
            "for command in rustc cargo rustfmt cargo-clippy cc git openssl "
            "wasm-tools wasmtime; do"
        )
        lts_guard = source.rfind(
            'if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then', 0, rust_check
        )
        self.assertGreater(lts_guard, source.index("resolved Alpine package closure differs"))
        source_hash = source.index('[[ "$HOSTED_SOURCE_ARCHIVE_SHA256" =~ ^[0-9a-f]{64}$ ]]')
        self.assertGreater(
            source.rfind('if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then', 0, source_hash),
            source.index('if [[ ! "$HOSTED_REVISION" =~ ^[0-9a-f]{40}$ ]]'),
        )
        source_copy = source.index('"$HOSTED_SOURCE_ROOT/." "$STAGE/usr/src/ostadix/"')
        self.assertGreater(
            source.rfind('if [[ "$ALPINE_KERNEL_FLAVOR" == lts ]]; then', 0, source_copy),
            source.index('for binary in "${HOSTED_IMAGE_BINARIES[@]}"; do'),
        )
        runtime_guard = source.index('if [ "$hosted_flavor" = lts ]; then')
        rust_runtime = source.index("rustc_version=$(rustc --version", runtime_guard)
        desktop_runtime = source.index("ostadix-desktop launch", runtime_guard)
        legacy_ready = source.index("\n  hosted_ready\n  hosted_shell\nfi", desktop_runtime)
        self.assertLess(runtime_guard, rust_runtime)
        self.assertLess(rust_runtime, desktop_runtime)
        self.assertLess(desktop_runtime, legacy_ready)
        self.assertIn("hosted-live-kernel-flavor", source)
        self.assertNotIn('for binary in "${HOSTED_BINARIES[@]}"; do', source)

    def test_webassembly_backend_fixture_executes_through_o(self) -> None:
        o_runtime = ROOT / "target" / "release" / "O"
        missing = [
            name
            for name, available in (
                ("target/release/O", o_runtime.is_file()),
                (
                    "WAT converter",
                    shutil.which("wat2wasm") is not None
                    or shutil.which("wasm-tools") is not None,
                ),
                ("wasmtime", shutil.which("wasmtime") is not None),
            )
            if not available
        ]
        if missing:
            self.skipTest(f"local integration prerequisites unavailable: {missing}")
        result = subprocess.run(
            [
                o_runtime,
                ROOT / "examples" / "webassembly_hello.O",
                ROOT / "backends",
            ],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "OSTADIX WEBASSEMBLY BACKEND PASS")

    def test_notebook_uses_installed_backend_and_browser_contracts(self) -> None:
        source = (ROOT / "src/bin/o-notebook.rs").read_text(encoding="utf-8")
        self.assertIn('std::env::var_os("O_BACKENDS_DIR")', source)
        self.assertIn('std::env::var_os("OSTADIX_NOTEBOOK_BROWSER")', source)
        self.assertIn('std::env::var_os("OSTADIX_NOTEBOOK_NO_OPEN")', source)
        self.assertIn("select_shim_dir", source)

    def test_boot_object_archive_is_bound_extracted_and_verified_database_free(self) -> None:
        worker = BUILD.read_text(encoding="utf-8")
        prepare = PREPARE.read_text(encoding="utf-8")
        self.assertIn("OSTADIX_HOSTED_BOOT_OBJECTS_ARCHIVE_SHA256", worker)
        self.assertIn('OSTADIX_HOSTED_BASE_COMMIT="$BASE_COMMIT"', worker)
        self.assertIn("tarfile.open(archive, \"r:\")", prepare)
        self.assertIn("unsafe boot-object archive member", prepare)
        self.assertIn('--commit "$HOSTED_BASE_COMMIT"', prepare)
        self.assertIn('--tree "$HOSTED_REVISION"', prepare)
        self.assertIn('--source-root "$STAGE/usr/src/ostadix"', prepare)
        self.assertIn("/usr/share/ostadix/boot-objects/v1", prepare)
        self.assertIn(
            'payload["store"] = "/usr/share/ostadix/boot-objects/v1"', prepare
        )

    def test_changed_shell_builders_parse(self) -> None:
        for script in (BUILD, PREPARE, ISO_BUILDER):
            result = subprocess.run(
                ["bash", "-n", str(script)],
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
