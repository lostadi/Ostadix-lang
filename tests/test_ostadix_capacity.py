#!/usr/bin/env python3
"""Offline tests for the absorbed-capacity package manager."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import tomllib
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "ostadix_capacity.py"
SPEC = importlib.util.spec_from_file_location("ostadix_capacity", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
CAPACITY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CAPACITY
SPEC.loader.exec_module(CAPACITY)


def quoted(value: str) -> str:
    return json.dumps(value)


def render_catalog(packages: list[dict[str, object]], name: str = "offline-test") -> str:
    lines = [
        f"schema = {quoted(CAPACITY.CATALOG_SCHEMA)}",
        f"name = {quoted(name)}",
        "",
    ]
    for package in packages:
        lines.extend(
            [
                "[[packages]]",
                f"id = {quoted(str(package['id']))}",
                f"name = {quoted(str(package.get('name', package['id'])))}",
                f"version = {quoted(str(package.get('version', '1')))}",
                f"kind = {quoted(str(package.get('kind', 'kernel')))}",
                f"architecture = {quoted(str(package.get('architecture', 'x86_64')))}",
                f"loader = {quoted(str(package.get('loader', 'linux')))}",
                f"license = {quoted(str(package.get('license', 'MIT')))}",
                f"redistribution = {quoted(str(package.get('redistribution', 'permitted')))}",
                "requires_acceptance = "
                + ("true" if package.get("requires_acceptance", False) else "false"),
                "aliases = ["
                + ", ".join(quoted(str(item)) for item in package.get("aliases", []))
                + "]",
                f"description = {quoted(str(package.get('description', 'offline fixture')))}",
            ]
        )
        dependencies = list(package.get("dependencies", []))
        artifacts = list(package.get("artifacts", []))
        if not dependencies:
            lines.append("dependencies = []")
        if not artifacts:
            lines.append("artifacts = []")
        lines.append("")
        for dependency in dependencies:
            lines.extend(
                [
                    "[[packages.dependencies]]",
                    f"package = {quoted(str(dependency['package']))}",
                    f"kind = {quoted(str(dependency['kind']))}",
                    "",
                ]
            )
        for artifact in artifacts:
            lines.extend(
                [
                    "[[packages.artifacts]]",
                    f"id = {quoted(str(artifact.get('id', 'blob')))}",
                    f"role = {quoted(str(artifact.get('role', 'kernel-image')))}",
                    f"filename = {quoted(str(artifact.get('filename', 'blob.bin')))}",
                    f"source = {quoted(str(artifact['source']))}",
                    f"size_bytes = {artifact['size_bytes']}",
                    f"sha256 = {quoted(str(artifact['sha256']))}",
                    f"integrity = {quoted(str(artifact.get('integrity', 'offline exact bytes')))}",
                    "",
                ]
            )
    return "\n".join(lines)


def artifact(path: Path, payload: bytes, *, artifact_id: str = "blob") -> dict[str, object]:
    path.write_bytes(payload)
    return {
        "id": artifact_id,
        "filename": path.name,
        "source": str(path),
        "size_bytes": len(payload),
        "sha256": hashlib.sha256(payload).hexdigest(),
    }


class FakeHttpsResponse(io.BytesIO):
    def __init__(self, payload: bytes, url: str):
        super().__init__(payload)
        self.url = url
        self.headers = {"Content-Length": str(len(payload))}
        self.read_sizes: list[int] = []

    def geturl(self) -> str:
        return self.url

    def read(self, size: int = -1) -> bytes:
        self.read_sizes.append(size)
        return super().read(size)


class GuardedReader:
    def __init__(self, wrapped: object):
        self.wrapped = wrapped
        self.read_sizes: list[int] = []

    def read(self, size: int = -1) -> bytes:
        if size < 0:
            raise AssertionError("streaming code attempted an unbounded read")
        self.read_sizes.append(size)
        return self.wrapped.read(size)


class AbsorbedCapacityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.state = self.root / "state"
        self.store = CAPACITY.CapacityStore(self.state)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_catalog(
        self, packages: list[dict[str, object]], filename: str = "catalog.toml"
    ) -> CAPACITY.Catalog:
        path = self.root / filename
        path.write_text(render_catalog(packages), encoding="utf-8")
        return CAPACITY.load_catalog(path)

    def install_single(
        self,
        package_id: str,
        payload: bytes,
        *,
        alias: str | None = None,
        filename: str = "payload.bin",
    ) -> tuple[str, CAPACITY.Catalog]:
        source = self.root / f"{package_id}-{filename}"
        package = {
            "id": package_id,
            "aliases": [] if alias is None else [alias],
            "artifacts": [artifact(source, payload)],
        }
        catalog = self.write_catalog([package], f"{package_id}.toml")
        result = CAPACITY.install_catalog_package(self.store, catalog, package_id)
        return result["target"], catalog

    def test_initial_catalog_pins_openbsd_without_qualification(self) -> None:
        catalog = CAPACITY.load_catalog(
            ROOT / "evidence" / "absorbed_capacity_catalog.toml"
        )
        inspected = CAPACITY.inspect_catalog_package(
            catalog, "openbsd-7.9-amd64-install"
        )
        media = inspected["artifacts"][0]
        self.assertEqual(media["size_bytes"], 798625792)
        self.assertEqual(
            media["sha256"],
            "7a4a92e953618035097c796a90b54424a0f3ae775552e1e7d102cf8a5130449f",
        )
        self.assertFalse(inspected["qualification_claimed"])
        self.assertEqual(
            CAPACITY.KINDS, {"os", "kernel", "userspace", "firmware", "bundle"}
        )

        by_id = catalog.by_id
        x86_kernel_package = by_id["alpine-linux-3.24.1-x86_64-kernel"]
        x86_kernel = x86_kernel_package.artifacts[0]
        x86_modloop = x86_kernel_package.artifacts[1]
        x86_initramfs = by_id["alpine-linux-3.24.1-x86_64-initramfs"].artifacts[0]
        self.assertEqual(
            (x86_kernel.size_bytes, x86_kernel.sha256),
            (
                12575744,
                "1e6bf9027720c75c3ed0d79171f21b5791ee40ca9795d07c7c6e04dc5ea2ae90",
            ),
        )
        self.assertEqual(
            (x86_initramfs.size_bytes, x86_initramfs.sha256),
            (
                9637032,
                "6d80a739fedeeb6cd63e24dd208845e22199c41a5fb2054941ef61ec30264fa9",
            ),
        )
        self.assertEqual(x86_modloop.id, "modloop")
        self.assertEqual(
            (x86_modloop.size_bytes, x86_modloop.sha256),
            (
                22867968,
                "78907e7cc812d555f08d4e1133d090cf11fa197370882adfe67b0a5986ccb3f9",
            ),
        )
        records = CAPACITY.catalog_records(catalog)
        bundle = CAPACITY.inspect_catalog_package(
            catalog, "ostadix-foreign-x86_64-bundle"
        )
        self.assertIn(
            records["alpine-linux-3.24.1-x86_64"]["digest"], bundle["closure"]
        )

        foreign = tomllib.loads(
            (ROOT / "evidence" / "foreign_kernel_lab.toml").read_text(
                encoding="utf-8"
            )
        )
        foreign_pins = {
            (entry["sha256"], entry["size_bytes"])
            for guest in foreign["guests"]
            for entry in guest["artifacts"]
        }
        catalog_pins = {
            (entry.sha256, entry.size_bytes)
            for package in catalog.packages
            for entry in package.artifacts
        }
        self.assertTrue(foreign_pins.issubset(catalog_pins))

    def test_local_install_is_immutable_exact_and_idempotent(self) -> None:
        payload = b"immutable-kernel" * 257
        digest, catalog = self.install_single(
            "kernel-one", payload, alias="kernel/current"
        )
        record = self.store.load_package(digest, verify_blobs=True)
        blob_path = self.store.blob_path(record["artifacts"][0]["sha256"])
        package_path = self.store.package_path(digest)
        self.assertEqual(blob_path.read_bytes(), payload)
        self.assertEqual(blob_path.stat().st_mode & 0o222, 0)
        self.assertEqual(package_path.stat().st_mode & 0o222, 0)

        repeated = CAPACITY.install_catalog_package(
            self.store, catalog, "kernel-one"
        )
        self.assertFalse(repeated["blobs"][0]["installed"])
        self.assertFalse(repeated["packages"][0]["installed"])

    def test_tampered_blob_is_rejected(self) -> None:
        payload = b"trusted bytes"
        digest, _ = self.install_single("tamper-kernel", payload)
        record = self.store.load_package(digest)
        path = self.store.blob_path(record["artifacts"][0]["sha256"])
        path.chmod(0o644)
        path.write_bytes(b"X" * len(payload))
        path.chmod(0o444)
        with self.assertRaisesRegex(CAPACITY.CapacityError, "identity mismatch"):
            self.store.load_package(digest, verify_blobs=True)

    def test_alias_is_resolved_into_exact_plan_and_never_reconsulted(self) -> None:
        first_digest, _ = self.install_single(
            "kernel-v1", b"first", alias="kernel/current"
        )
        old_plan = CAPACITY.create_plan(self.store, ["kernel/current"])
        self.assertEqual(old_plan["roots"], [first_digest])
        self.assertNotIn("kernel/current", json.dumps(old_plan))

        second_source = self.root / "second.bin"
        second_catalog = self.write_catalog(
            [
                {
                    "id": "kernel-v2",
                    "aliases": ["kernel/current"],
                    "artifacts": [artifact(second_source, b"second")],
                }
            ],
            "second.toml",
        )
        second = CAPACITY.install_catalog_package(
            self.store,
            second_catalog,
            "kernel-v2",
            replace_aliases=True,
        )["target"]
        self.assertNotEqual(first_digest, second)
        self.assertEqual(self.store.resolve_ref("kernel/current")[0], second)

        applied = CAPACITY.apply_plan(self.store, old_plan)
        self.assertEqual(applied["generation"]["roots"], [first_digest])
        self.assertEqual(applied["generation"]["qualified_packages"], [])

    def test_stale_plan_is_rejected_and_rollback_swaps_retained_generation(self) -> None:
        source_a = self.root / "a.bin"
        source_b = self.root / "b.bin"
        catalog = self.write_catalog(
            [
                {
                    "id": "kernel-a",
                    "aliases": ["kernel/a"],
                    "artifacts": [artifact(source_a, b"A")],
                },
                {
                    "id": "kernel-b",
                    "aliases": ["kernel/b"],
                    "artifacts": [artifact(source_b, b"B")],
                },
            ]
        )
        CAPACITY.install_catalog_package(self.store, catalog, "kernel-a")
        CAPACITY.install_catalog_package(self.store, catalog, "kernel-b")
        plan_a = CAPACITY.create_plan(self.store, ["kernel/a"])
        stale_b = CAPACITY.create_plan(self.store, ["kernel/b"])
        first = CAPACITY.apply_plan(self.store, plan_a)
        with self.assertRaisesRegex(CAPACITY.CapacityError, "stale activation plan"):
            CAPACITY.apply_plan(self.store, stale_b)

        plan_b = CAPACITY.create_plan(self.store, ["kernel/b"])
        second = CAPACITY.apply_plan(self.store, plan_b)
        before_rollback = CAPACITY.status(self.store)
        self.assertEqual(before_rollback["current_generation"], second["generation"]["digest"])
        self.assertEqual(before_rollback["previous_generation"], first["generation"]["digest"])
        self.assertEqual(before_rollback["qualified_packages"], [])

        rolled_back = CAPACITY.rollback(self.store)
        self.assertEqual(rolled_back["current"], first["generation"]["digest"])
        self.assertEqual(rolled_back["previous"], second["generation"]["digest"])
        self.assertEqual(rolled_back["revision"], 3)

    def test_catalog_rejects_cycles_type_architecture_and_loader_mismatches(self) -> None:
        cases = {
            "cycle": [
                {
                    "id": "bundle-a",
                    "kind": "bundle",
                    "loader": "none",
                    "dependencies": [{"package": "bundle-b", "kind": "bundle"}],
                },
                {
                    "id": "bundle-b",
                    "kind": "bundle",
                    "loader": "none",
                    "dependencies": [{"package": "bundle-a", "kind": "bundle"}],
                },
            ],
            "requires .* as kernel": [
                {
                    "id": "system",
                    "kind": "os",
                    "loader": "linux",
                    "dependencies": [{"package": "user", "kind": "kernel"}],
                },
                {"id": "user", "kind": "userspace", "loader": "none"},
            ],
            "architecture mismatch": [
                {
                    "id": "system",
                    "kind": "os",
                    "loader": "linux",
                    "architecture": "x86_64",
                    "dependencies": [{"package": "kernel", "kind": "kernel"}],
                },
                {
                    "id": "kernel",
                    "kind": "kernel",
                    "loader": "linux",
                    "architecture": "aarch64",
                },
            ],
            "does not match kernel loader": [
                {
                    "id": "system",
                    "kind": "os",
                    "loader": "linux",
                    "dependencies": [{"package": "kernel", "kind": "kernel"}],
                },
                {
                    "id": "kernel",
                    "kind": "kernel",
                    "loader": "multiboot2",
                },
            ],
        }
        for expected, packages in cases.items():
            with self.subTest(expected=expected):
                path = self.root / (expected.split()[0] + ".toml")
                path.write_text(render_catalog(packages), encoding="utf-8")
                with self.assertRaisesRegex(CAPACITY.CapacityError, expected):
                    CAPACITY.load_catalog(path)

    def test_catalog_admits_all_five_kinds_in_a_valid_exact_composition(self) -> None:
        catalog = self.write_catalog(
            [
                {"id": "firmware", "kind": "firmware", "loader": "none"},
                {
                    "id": "kernel",
                    "kind": "kernel",
                    "loader": "linux",
                    "dependencies": [{"package": "firmware", "kind": "firmware"}],
                },
                {"id": "userspace", "kind": "userspace", "loader": "none"},
                {
                    "id": "system",
                    "kind": "os",
                    "loader": "linux",
                    "dependencies": [
                        {"package": "kernel", "kind": "kernel"},
                        {"package": "userspace", "kind": "userspace"},
                    ],
                },
                {
                    "id": "bundle",
                    "kind": "bundle",
                    "loader": "none",
                    "dependencies": [{"package": "system", "kind": "os"}],
                },
            ]
        )
        self.assertEqual({package.kind for package in catalog.packages}, CAPACITY.KINDS)
        records = CAPACITY.catalog_records(catalog)
        self.assertEqual(len(records), 5)

    def test_license_acceptance_is_exact_and_does_not_qualify(self) -> None:
        source = self.root / "restricted.bin"
        catalog = self.write_catalog(
            [
                {
                    "id": "restricted-kernel",
                    "aliases": ["kernel/restricted"],
                    "redistribution": "restricted",
                    "requires_acceptance": True,
                    "license": "LicenseRef-Test-Restricted",
                    "artifacts": [artifact(source, b"restricted")],
                }
            ]
        )
        digest = CAPACITY.install_catalog_package(
            self.store, catalog, "restricted-kernel"
        )["target"]
        with self.assertRaisesRegex(CAPACITY.CapacityError, "license acceptance"):
            CAPACITY.create_plan(self.store, ["kernel/restricted"])
        plan = CAPACITY.create_plan(
            self.store,
            ["kernel/restricted"],
            accepted_license_refs=[digest],
        )
        applied = CAPACITY.apply_plan(self.store, plan)
        self.assertEqual(applied["generation"]["qualified_packages"], [])

    def test_parser_enforces_bounded_catalog_dimensions(self) -> None:
        too_long = [
            {
                "id": "large-description",
                "description": "x" * (CAPACITY.MAX_DESCRIPTION_BYTES + 1),
            }
        ]
        path = self.root / "too-long.toml"
        path.write_text(render_catalog(too_long), encoding="utf-8")
        with self.assertRaisesRegex(CAPACITY.CapacityError, "exceeds"):
            CAPACITY.load_catalog(path)

        two_packages = self.root / "two.toml"
        two_packages.write_text(
            render_catalog([{"id": "one"}, {"id": "two"}]), encoding="utf-8"
        )
        with mock.patch.object(CAPACITY, "MAX_PACKAGES", 1):
            with self.assertRaisesRegex(CAPACITY.CapacityError, "exceeds 1 entries"):
                CAPACITY.load_catalog(two_packages)

        source = self.root / "oversized.bin"
        source.write_bytes(b"x")
        oversized = self.root / "oversized.toml"
        oversized.write_text(
            render_catalog(
                [
                    {
                        "id": "oversized",
                        "artifacts": [
                            {
                                "source": str(source),
                                "size_bytes": CAPACITY.MAX_BLOB_BYTES + 1,
                                "sha256": hashlib.sha256(b"x").hexdigest(),
                            }
                        ],
                    }
                ]
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(CAPACITY.CapacityError, "exceeds"):
            CAPACITY.load_catalog(oversized)

    def test_local_install_reads_in_bounded_streaming_chunks(self) -> None:
        payload = bytes(range(251)) * 13
        source = self.root / "stream.bin"
        catalog = self.write_catalog(
            [{"id": "stream-kernel", "artifacts": [artifact(source, payload)]}]
        )
        original_copy = CAPACITY._copy_stream
        guarded_read_sizes: list[int] = []

        def guarded_copy(stream: object, descriptor: int, **kwargs: object) -> object:
            guarded = GuardedReader(stream)
            result = original_copy(guarded, descriptor, **kwargs)
            guarded_read_sizes.extend(guarded.read_sizes)
            return result

        with mock.patch.object(CAPACITY, "STREAM_CHUNK_BYTES", 31), mock.patch.object(
            CAPACITY, "_copy_stream", side_effect=guarded_copy
        ):
            CAPACITY.install_catalog_package(self.store, catalog, "stream-kernel")
        self.assertGreater(len(guarded_read_sizes), 2)
        self.assertLessEqual(max(guarded_read_sizes), 31)

    def test_https_install_is_streamed_offline_through_mocked_response(self) -> None:
        payload = b"https artifact" * 113
        source = "https://example.invalid/pinned.bin"
        catalog = self.write_catalog(
            [
                {
                    "id": "https-kernel",
                    "artifacts": [
                        {
                            "source": source,
                            "size_bytes": len(payload),
                            "sha256": hashlib.sha256(payload).hexdigest(),
                        }
                    ],
                }
            ]
        )
        response = FakeHttpsResponse(payload, source)
        with mock.patch.object(CAPACITY, "STREAM_CHUNK_BYTES", 29), mock.patch.object(
            CAPACITY.urllib.request, "urlopen", return_value=response
        ) as urlopen:
            CAPACITY.install_catalog_package(self.store, catalog, "https-kernel")
        urlopen.assert_called_once()
        self.assertGreater(len(response.read_sizes), 2)
        self.assertLessEqual(max(response.read_sizes), 29)

    def test_gc_is_report_only_and_retains_installed_objects(self) -> None:
        digest, _ = self.install_single("gc-kernel", b"retained")
        orphan_sha = hashlib.sha256(b"orphan").hexdigest()
        orphan_path = self.store.blob_path(orphan_sha)
        CAPACITY._publish_immutable_bytes(orphan_path, b"orphan")
        report = CAPACITY.gc_dry_run(self.store)
        self.assertFalse(report["destructive"])
        self.assertIn(orphan_sha, report["blob_candidates"])
        self.assertIn(digest, report["installed_packages_retained"])
        self.assertTrue(orphan_path.exists())

    def test_cli_exposes_required_v1_commands(self) -> None:
        parser = CAPACITY.build_parser()
        subparsers = [
            action
            for action in parser._actions
            if isinstance(action, argparse._SubParsersAction)
        ]
        self.assertEqual(len(subparsers), 1)
        self.assertEqual(
            set(subparsers[0].choices),
            {
                "install",
                "inspect",
                "list",
                "show",
                "verify",
                "status",
                "plan",
                "apply",
                "rollback",
                "gc",
            },
        )


if __name__ == "__main__":
    unittest.main()
