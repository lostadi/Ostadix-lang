#!/usr/bin/env python3
"""Build and verify deterministic Ostadix-lang source-release ZIP files.

Release contents are read from Git objects at a resolved commit, never from
the working tree.  The explicit allowlist below defines the public source
surface; generated output and local development debris remain excluded even
when they were accidentally committed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import string
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from typing import Iterable, Sequence
from urllib.parse import unquote, urlsplit
import zipfile
import zlib


SCHEMA = "ostadix-source-release-v1"
MANIFEST_NAME = "SOURCE-MANIFEST.json"
CHECKSUMS_NAME = "SHA256SUMS"
FIXED_ZIP_TIMESTAMP = (1980, 1, 1, 0, 0, 0)
ROOT_LICENSE_SPDX = "LGPL-2.1-only"
ROOT_REPOSITORY = "https://github.com/lostadi/Ostadix-lang"
EXISTING_PREPRINT_DOI = "10.5281/zenodo.21544345"

# `olangc` owns the generated-project writer, while the independent engine
# owns the source bytes it embeds.  The compiler still has a few workspace
# compile-time inputs; the engine inventory has ordinary relative includes.
# Derive both closures from their owning Rust source instead of copying a
# second runtime manifest into this release script.
PARENT_RELATIVE_INCLUDE = re.compile(
    r'include_(?:str|bytes)!\(\s*"(?P<path>(?:\.\./)+[^"\r\n]+)"\s*\)'
)
RELATIVE_LITERAL_INCLUDE = re.compile(
    r'include_(?:str|bytes)!\(\s*"(?P<path>[^"\r\n]+)"\s*\)'
)
OSTADIX_API_ROOT = "crates/ostadix-api"
OSTADIX_API_SOURCE_ROOT = f"{OSTADIX_API_ROOT}/src"
OSTADIX_API_AOT_SOURCE = f"{OSTADIX_API_SOURCE_ROOT}/api/aot_source.rs"
OSTADIX_API_ALLOWED_PREFIXES = (
    f"{OSTADIX_API_SOURCE_ROOT}/",
    f"{OSTADIX_API_ROOT}/backends/",
    f"{OSTADIX_API_ROOT}/test-assets/",
)

# Keep this list intentionally narrow.  Adding a new top-level project surface
# requires an explicit release-engineering decision here.
ALLOWED_TOP_LEVEL_FILES = frozenset(
    {
        ".dockerignore",
        ".gitignore",
        ".mcp.json",
        "ARCHITECTURE.md",
        "CITATION.cff",
        "Cargo.lock",
        "Cargo.toml",
        "CHANGELOG.md",
        "CODE_OF_CONDUCT.md",
        "CONTRIBUTING.md",
        "DEVELOPMENT.md",
        "Dockerfile",
        "LICENSE",
        "llms.txt",
        "NOTICE",
        "o-node-quickstart.sh",
        "ORIGIN.md",
        "README.md",
        "SECURITY.md",
        "SPEC.md",
        "big_iron_to_my_texas_red.sh",
        "boot-and-test.sh",
        "rust-toolchain.toml",
        "setup.sh",
        "test_o_lang_examples.sh",
    }
)

# Keep nested exceptions exact so publishing one reviewed surface does not
# implicitly publish every file under a new top-level directory.
HOSTED_HGRAPH_BENCHMARK_PATHS = frozenset(
    {
        "benchmarks/hgraph_hosted/README.md",
        "benchmarks/hgraph_hosted/RESULTS-2026-08-08-be68dfef.md",
        "benchmarks/hgraph_hosted/RESULTS-2026-08-08-f216771.md",
        "benchmarks/hgraph_hosted/TRANSCRIPT-2026-08-08-f216771.log",
        "benchmarks/hgraph_hosted/chained.O",
        "benchmarks/hgraph_hosted/chained.expected.json",
        "benchmarks/hgraph_hosted/heterogeneous.O",
        "benchmarks/hgraph_hosted/heterogeneous.expected.json",
        "benchmarks/hgraph_hosted/mixed_width.O",
        "benchmarks/hgraph_hosted/mixed_width.expected.json",
        "benchmarks/hgraph_hosted/realistic.O",
        "benchmarks/hgraph_hosted/realistic.expected.json",
    }
)
HOSTED_HGRAPH_BENCHMARK_RELEASE_PATHS = HOSTED_HGRAPH_BENCHMARK_PATHS | frozenset(
    {
        "scripts/benchmark_hgraph_hosted.sh",
        "tests/test_benchmark_hgraph_hosted.py",
    }
)
OSTADIX_API_RUNTIME_ASSET_PATHS = frozenset(
    {
        f"{OSTADIX_API_ROOT}/backends/{name}"
        for name in (
            "bash_shim.py",
            "common_lisp_shim.py",
            "cpp_shim.py",
            "csharp_shim.py",
            "haskell_shim.py",
            "java_shim.py",
            "javascript_shim.py",
            "lisp_shim.py",
            "mathematica_shim.py",
            "matlab_shim.py",
            "nix_shim.py",
            "nix_store_shim.py",
            "nixos_test_shim.py",
            "o_shim_common.py",
            "ocaml_shim.py",
            "python_shim.py",
            "racket_shim.py",
            "ruby_shim.py",
            "rust_shim.py",
            "shell_shim.py",
            "sql_shim.py",
            "ubuntu_vm_shim.py",
            "webassembly_shim.py",
        )
    }
) | frozenset(
    {
        f"{OSTADIX_API_ROOT}/test-assets/benchmarks/hgraph_hosted/{name}.O"
        for name in ("chained", "heterogeneous", "mixed_width", "realistic")
    }
) | frozenset(
    {
        f"{OSTADIX_API_ROOT}/test-assets/ocore/runtime/x86_64/capability.oc",
        f"{OSTADIX_API_ROOT}/test-assets/ocore/runtime/x86_64/native_abi.oc",
    }
)
OSTADIX_API_RELEASE_PATHS = frozenset(
    {
        f"{OSTADIX_API_ROOT}/Cargo.toml",
        f"{OSTADIX_API_ROOT}/LICENSE",
        f"{OSTADIX_API_ROOT}/NOTICE",
        f"{OSTADIX_API_ROOT}/README.md",
        f"{OSTADIX_API_SOURCE_ROOT}/api.rs",
        OSTADIX_API_AOT_SOURCE,
        f"{OSTADIX_API_SOURCE_ROOT}/lib.rs",
        f"{OSTADIX_API_SOURCE_ROOT}/shims.rs",
        f"{OSTADIX_API_ROOT}/tests/public_surface.rs",
    }
) | OSTADIX_API_RUNTIME_ASSET_PATHS
OSTADIX_API_ROOT_MODULE_PATHS = {
    "api": f"{OSTADIX_API_SOURCE_ROOT}/api.rs",
    "backend": f"{OSTADIX_API_SOURCE_ROOT}/backend.rs",
    "backend_catalog": f"{OSTADIX_API_SOURCE_ROOT}/backend_catalog.rs",
    "backend_morphism": f"{OSTADIX_API_SOURCE_ROOT}/backend_morphism.rs",
    "backend_state": f"{OSTADIX_API_SOURCE_ROOT}/backend_state.rs",
    "canonical_cbor": f"{OSTADIX_API_SOURCE_ROOT}/canonical_cbor.rs",
    "capability": f"{OSTADIX_API_SOURCE_ROOT}/capability.rs",
    "dispatch_model": f"{OSTADIX_API_SOURCE_ROOT}/dispatch_model.rs",
    "effects": f"{OSTADIX_API_SOURCE_ROOT}/effects.rs",
    "environment": f"{OSTADIX_API_SOURCE_ROOT}/environment.rs",
    "eval": f"{OSTADIX_API_SOURCE_ROOT}/eval.rs",
    "eval_core": f"{OSTADIX_API_SOURCE_ROOT}/eval_core.rs",
    "evidence": f"{OSTADIX_API_SOURCE_ROOT}/evidence/mod.rs",
    "execution_contract": f"{OSTADIX_API_SOURCE_ROOT}/execution_contract.rs",
    "executor": f"{OSTADIX_API_SOURCE_ROOT}/executor/mod.rs",
    "hgraph": f"{OSTADIX_API_SOURCE_ROOT}/hgraph/mod.rs",
    "hosted_remote": f"{OSTADIX_API_SOURCE_ROOT}/hosted_remote/mod.rs",
    "information": f"{OSTADIX_API_SOURCE_ROOT}/information/mod.rs",
    "information_bridge": f"{OSTADIX_API_SOURCE_ROOT}/information_bridge/mod.rs",
    "information_provenance": f"{OSTADIX_API_SOURCE_ROOT}/information_provenance/mod.rs",
    "ir": f"{OSTADIX_API_SOURCE_ROOT}/ir.rs",
    "kernel_world": f"{OSTADIX_API_SOURCE_ROOT}/kernel_world.rs",
    "live_system": f"{OSTADIX_API_SOURCE_ROOT}/live_system/mod.rs",
    "nix_ops": f"{OSTADIX_API_SOURCE_ROOT}/nix_ops.rs",
    "nixos_ops": f"{OSTADIX_API_SOURCE_ROOT}/nixos_ops.rs",
    "ocore": f"{OSTADIX_API_SOURCE_ROOT}/ocore/mod.rs",
    "parser": f"{OSTADIX_API_SOURCE_ROOT}/parser.rs",
    "placement": f"{OSTADIX_API_SOURCE_ROOT}/placement/mod.rs",
    "placement_protocol": f"{OSTADIX_API_SOURCE_ROOT}/placement/protocol/mod.rs",
    "process": f"{OSTADIX_API_SOURCE_ROOT}/process.rs",
    "project": f"{OSTADIX_API_SOURCE_ROOT}/project/mod.rs",
    "registry": f"{OSTADIX_API_SOURCE_ROOT}/registry/mod.rs",
    "resource_identity": f"{OSTADIX_API_SOURCE_ROOT}/world/identity.rs",
    "runtime_exec": f"{OSTADIX_API_SOURCE_ROOT}/runtime_exec.rs",
    "scheduler": f"{OSTADIX_API_SOURCE_ROOT}/scheduler.rs",
    "shims": f"{OSTADIX_API_SOURCE_ROOT}/shims.rs",
    "syntax_dialect": f"{OSTADIX_API_SOURCE_ROOT}/syntax_dialect.rs",
    "value": f"{OSTADIX_API_SOURCE_ROOT}/value.rs",
    "version": f"{OSTADIX_API_SOURCE_ROOT}/version.rs",
    "wire": f"{OSTADIX_API_SOURCE_ROOT}/wire.rs",
    "world": f"{OSTADIX_API_SOURCE_ROOT}/world/mod.rs",
}
ALLOWED_EXACT_PATHS = frozenset(
    {
        "okernel-multikernel/boot-and-test.sh",
        "okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md",
    }
) | HOSTED_HGRAPH_BENCHMARK_PATHS | OSTADIX_API_RELEASE_PATHS

ALLOWED_TOP_LEVEL_DIRECTORIES = frozenset(
    {
        ".github",
        "assets",
        "backends",
        "c_cpp",
        "ci",
        "docs",
        "evidence",
        "examples",
        "fuzz",
        "mcp",
        "o_lang",
        "ocore",
        "scripts",
        "setup",
        "src",
        "tests",
    }
)

EXCLUDED_DIRECTORY_NAMES = frozenset(
    {
        ".cache",
        ".git",
        ".hypothesis",
        ".idea",
        ".mypy_cache",
        ".nox",
        ".ocore-repair-backups",
        ".pytest_cache",
        ".ruff_cache",
        ".tox",
        ".venv",
        ".vscode",
        "CMakeFiles",
        "__pycache__",
        "build",
        "dist",
        "htmlcov",
        "out",
        "target",
    }
)

EXCLUDED_EXACT_PATHS = frozenset(
    {
        "c_cpp/O",
        "c_cpp/olangc",
        "codebase_tape.md",
        "test.html",
    }
)

EXCLUDED_BASENAMES = frozenset({".DS_Store", "Thumbs.db"})
EXCLUDED_SUFFIXES = (
    ".a",
    ".d",
    ".dll",
    ".dylib",
    ".exe",
    ".html",
    ".lib",
    ".o",  # Deliberately case-sensitive: .O files are O language source.
    ".obj",
    ".patch",
    ".pdb",
    ".profdata",
    ".profraw",
    ".pyc",
    ".pyo",
    ".rmeta",
    ".rlib",
    ".so",
    ".wasm",
)

REQUIRED_RELEASE_PATHS = frozenset(
    {
        ".github/workflows/ci.yml",
        ".github/workflows/fuzz.yml",
        ".github/CODEOWNERS",
        ".github/ISSUE_TEMPLATE/bug_report.yml",
        ".github/ISSUE_TEMPLATE/config.yml",
        ".github/ISSUE_TEMPLATE/feature_request.yml",
        ".github/pull_request_template.md",
        ".github/dependabot.yml",
        ".dockerignore",
        ".gitignore",
        ".mcp.json",
        "CITATION.cff",
        "Cargo.lock",
        "Cargo.toml",
        "CHANGELOG.md",
        "CODE_OF_CONDUCT.md",
        "CONTRIBUTING.md",
        "Dockerfile",
        "LICENSE",
        "llms.txt",
        "mcp/ostadix_lang_mcp_server/Cargo.lock",
        "mcp/ostadix_lang_mcp_server/Cargo.toml",
        "mcp/ostadix_lang_mcp_server/README.md",
        "mcp/ostadix_lang_mcp_server/src/main.rs",
        "README.md",
        "SECURITY.md",
        "boot-and-test.sh",
        "ci/architecture-roots.toml",
        "ci/required-jobs.toml",
        "ci/test-suites.toml",
        "rust-toolchain.toml",
        "setup.sh",
        "docs/HOSTED_PLACEMENT_V6.md",
        "docs/HOSTED_WORLD_REFERENCE_PROFILE.md",
        "docs/CI_POSTURE.md",
        "docs/IMAGE_ADMISSION.md",
        "docs/INFORMATION_KERNEL_V1.md",
        "docs/releases/v0.3.0.md",
        "docs/O_MACHINE_CONTRACT.md",
        "docs/OSTADIX_BOOT.md",
        "docs/OSTADIX_WORLD.md",
        "docs/SEMANTIC_CUSTODY.md",
        "docs/VERSIONING.md",
        "evidence/gates.toml",
        "evidence/world_alpha_gates.toml",
        "evidence/world_contract_v1.toml",
        "evidence/world_contract_v2.toml",
        "evidence/o_machine_contract_v1.toml",
        "evidence/world/g0-repository-conformance.toml",
        "evidence/world/g0-repository-conformance-2026-08-03.toml",
        "evidence/world/g0-derivation-rederive-2026-08-03.toml",
        "evidence/world/g0-schema-v3-supersession-2026-08-03.toml",
        "evidence/world/g0-repository-conformance-2026-08-03-v2.toml",
        "evidence/world/g0-machine-contract-supersession-2026-08-03.toml",
        "evidence/world/g0-ostadix-alpha-branding-2026-08-09.toml",
        "evidence/world/g0-ostadix-alpha-branding-supersession-2026-08-09.toml",
        "evidence/world/g0-independent-engine-2026-08-17.toml",
        "evidence/world/g0-independent-engine-supersession-2026-08-17.toml",
        "evidence/world/g2-aarch64-qemu.toml",
        "evidence/world/g2-aarch64-qemu-2026-08-03.toml",
        "evidence/world/g2-derivation-rederive-2026-08-03.toml",
        "evidence/world/g2-counter-wording-supersession-2026-08-03.toml",
        "evidence/world/transcripts/g0-repository-conformance.log",
        "evidence/world/transcripts/g0-repository-conformance-2026-08-03.log",
        "evidence/world/transcripts/g0-repository-conformance-2026-08-03-v2.log",
        "evidence/world/transcripts/g0-ostadix-alpha-branding-2026-08-09.log",
        "evidence/world/transcripts/g0-independent-engine-2026-08-17.log",
        "evidence/world/transcripts/g2-aarch64-qemu.log",
        "evidence/world/transcripts/g2-aarch64-qemu-2026-08-03.log",
        "examples/manifest.json",
        "examples/docker_literal/main.py",
        "examples/semantic_custody.O",
        "okernel-multikernel/boot-and-test.sh",
        "okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md",
        "ocore/kernel/boot.S",
        "ocore/kernel/aarch64/boot.S",
        "ocore/kernel/aarch64/linker.ld",
        "ocore/kernel/aarch64/vectors.S",
        "ocore/kernel/build-aarch64-g2.sh",
        "ocore/kernel/build-x86_64-uefi-media.sh",
        "ocore/kernel/build.sh",
        "ocore/kernel/main.oc",
        "ocore/kernel/m6_mode25_diagnostics.oc",
        "ocore/kernel/m6_mode25_diagnostics_stub.oc",
        "ocore/kernel/resolve-x86_64-ovmf-code.sh",
        "ocore/kernel/smp_probe.oc",
        "ocore/kernel/smp_probe_stub.oc",
        "ocore/kernel/run-x86_64-uefi-media-qemu.sh",
        "ocore/kernel/smoke-x86_64-boot-info-qemu.sh",
        "ocore/kernel/smoke-x86_64-smp-qemu.sh",
        "ocore/kernel/smoke-x86_64-uefi-media-qemu.sh",
        "ocore/kernel/smoke-world-project-receipt-qemu.sh",
        "ocore/kernel/smoke-world-project-runtime-qemu.sh",
        "ocore/kernel/smoke-world-receipt-qemu.sh",
        "ocore/kernel/smoke-world-value-qemu.sh",
        "ocore/kernel/smoke-world-protocol-qemu.sh",
        "ocore/kernel/smoke-world-identity-qemu.sh",
        "ocore/kernel/smoke-aarch64-g2-qemu.sh",
        "ocore/kernel/stress-live-linux-personality-qemu.sh",
        "ocore/kernel/world_value_semantics.oc",
        "ocore/kernel/world_value_semantics_stub.oc",
        "ocore/kernel/world_protocol_semantics.oc",
        "ocore/kernel/world_protocol_semantics_stub.oc",
        "ocore/kernel/world_identity_semantics.oc",
        "ocore/kernel/world_identity_semantics_stub.oc",
        "ocore/kernel/world_project_receipt_semantics.oc",
        "ocore/kernel/world_project_receipt_semantics_stub.oc",
        "ocore/kernel/world_receipt_semantics.oc",
        "ocore/kernel/world_receipt_semantics_stub.oc",
        "ocore/kernel/x86_64/grub.cfg",
        "ocore/kernel/x86_64/boot_info.oc",
        "ocore/kernel/x86_64/boot_info_stub.oc",
        "ocore/runtime/x86_64/trap.oc",
        "ocore/runtime/aarch64/g2_kernel.oc",
        "ocore/runtime/aarch64/g2_user_a.oc",
        "ocore/runtime/aarch64/g2_user_b.oc",
        "ocore/world/codec.oc",
        "ocore/world/identity.oc",
        "ocore/world/protocol.oc",
        "ocore/world/receipt.oc",
        "ocore/world/receipt_codec.oc",
        "ocore/world/sha256.oc",
        "ocore/world/value.oc",
        "ocore/world/value_codec.oc",
        "scripts/smoke_ostadix_mcp.py",
        "scripts/smoke-docker.sh",
        "scripts/semantic_custody_demo.sh",
        "scripts/contract_surfaces.py",
        "scripts/check_architecture_boundaries.py",
        "scripts/local_ci_posture.py",
        "scripts/install-o-cli-wrapper.sh",
        "scripts/o-cli.sh",
        "scripts/o-kernel.sh",
        "scripts/ostadix_boot_media.py",
        "scripts/ostadix_boot_info_qemu.py",
        "scripts/ostadix_media_writer.py",
        "scripts/ostadix_physical_evidence.py",
        "scripts/smoke-project-hgraph-exec.sh",
        "scripts/smoke-project-hgraph.sh",
        "scripts/smoke-world-resource-keys.sh",
        "scripts/smoke-world-g0-conformance.sh",
        "scripts/release_evidence.py",
        "scripts/world_alpha_evidence.py",
        "backends/o_shim_common.py",
        "crates/ostadix-api/src/backend.rs",
        "crates/ostadix-api/src/backend_morphism.rs",
        "crates/ostadix-api/src/api.rs",
        "crates/ostadix-api/src/backend_catalog.rs",
        "crates/ostadix-api/src/backend_catalog.inc.rs",
        "crates/ostadix-api/src/backend_state.rs",
        "crates/ostadix-api/src/canonical_cbor.rs",
        "crates/ostadix-api/src/dispatch_model.rs",
        "crates/ostadix-api/src/evidence/admit.rs",
        "crates/ostadix-api/src/evidence/analyze.rs",
        "crates/ostadix-api/src/evidence/fact.rs",
        "crates/ostadix-api/src/evidence/intent.rs",
        "crates/ostadix-api/src/evidence/mod.rs",
        "crates/ostadix-api/src/evidence/profile.rs",
        "crates/ostadix-api/src/effects.rs",
        "crates/ostadix-api/src/eval.rs",
        "crates/ostadix-api/src/eval_core.rs",
        "crates/ostadix-api/src/execution_contract.rs",
        "crates/ostadix-api/src/hosted_remote/client.rs",
        "crates/ostadix-api/src/hosted_remote/mod.rs",
        "crates/ostadix-api/src/hosted_remote/node.rs",
        "crates/ostadix-api/src/hosted_remote/paths.rs",
        "crates/ostadix-api/src/hosted_remote/protocol.rs",
        "crates/ostadix-api/src/hosted_remote/tls.rs",
        "crates/ostadix-api/src/hosted_remote/v2/auth.rs",
        "crates/ostadix-api/src/hosted_remote/v2/client.rs",
        "crates/ostadix-api/src/hosted_remote/v2/crypto.rs",
        "crates/ostadix-api/src/hosted_remote/v2/dev.rs",
        "crates/ostadix-api/src/hosted_remote/v2/mod.rs",
        "crates/ostadix-api/src/hosted_remote/v2/protocol.rs",
        "crates/ostadix-api/src/hosted_remote/v2/runtime.rs",
        "crates/ostadix-api/src/hosted_remote/v2/server.rs",
        "crates/ostadix-api/src/hosted_remote/v2/store.rs",
        "crates/ostadix-api/src/information/acquisition.rs",
        "crates/ostadix-api/src/information/decision.rs",
        "crates/ostadix-api/src/information/delta.rs",
        "crates/ostadix-api/src/information/exchange.rs",
        "crates/ostadix-api/src/information/id.rs",
        "crates/ostadix-api/src/information/invalidation.rs",
        "crates/ostadix-api/src/information/loss.rs",
        "crates/ostadix-api/src/information/mod.rs",
        "crates/ostadix-api/src/information/model.rs",
        "crates/ostadix-api/src/information/projection.rs",
        "crates/ostadix-api/src/information/provenance.rs",
        "crates/ostadix-api/src/information/root.rs",
        "crates/ostadix-api/src/information/store.rs",
        "crates/ostadix-api/src/information_bridge/mod.rs",
        "crates/ostadix-api/src/information_provenance/mod.rs",
        "crates/ostadix-api/src/ir.rs",
        "src/lib.rs",
        "src/main.rs",
        "crates/ostadix-api/src/placement/mod.rs",
        "crates/ostadix-api/src/placement/projection.rs",
        "crates/ostadix-api/src/placement/protocol/candidate.rs",
        "crates/ostadix-api/src/placement/protocol/catalog.rs",
        "crates/ostadix-api/src/placement/protocol/digest.rs",
        "crates/ostadix-api/src/placement/protocol/error.rs",
        "crates/ostadix-api/src/placement/protocol/mod.rs",
        "crates/ostadix-api/src/placement/protocol/records.rs",
        "crates/ostadix-api/src/placement/protocol/requirement.rs",
        "crates/ostadix-api/src/placement/protocol/state.rs",
        "crates/ostadix-api/src/placement/protocol/target.rs",
        "crates/ostadix-api/src/placement/protocol/warrant.rs",
        "crates/ostadix-api/src/process.rs",
        "crates/ostadix-api/src/version.rs",
        "crates/ostadix-api/src/registry/bundle/mod.rs",
        "crates/ostadix-api/src/registry/placement_compat.rs",
        "crates/ostadix-api/src/registry/store.rs",
        "crates/ostadix-api/src/runtime_exec.rs",
        "crates/ostadix-api/src/syntax_dialect.rs",
        "src/bin/o-node.rs",
        "src/bin/o-info.rs",
        "src/bin/o-registry.rs",
        "src/bin/octl.rs",
        "src/bin/olink.rs",
        "src/bin/olangc.rs",
        "src/bin/ocorec.rs",
        "crates/ostadix-api/src/ocore/codegen.rs",
        "crates/ostadix-api/src/ocore/codegen_aarch64.rs",
        "crates/ostadix-api/src/ocore/boot_info.rs",
        "crates/ostadix-api/src/ocore/driver.rs",
        "crates/ostadix-api/src/ocore/mod.rs",
        "crates/ostadix-api/src/executor/mod.rs",
        "crates/ostadix-api/src/executor/pool.rs",
        "crates/ostadix-api/src/executor/task.rs",
        "crates/ostadix-api/src/hgraph/graph.rs",
        "crates/ostadix-api/src/hgraph/kinds.rs",
        "crates/ostadix-api/src/hgraph/from_oir.rs",
        "crates/ostadix-api/src/hgraph/solve.rs",
        "crates/ostadix-api/src/project/mod.rs",
        "crates/ostadix-api/src/project/model.rs",
        "crates/ostadix-api/src/project/executor.rs",
        "crates/ostadix-api/src/project/deployment.rs",
        "crates/ostadix-api/src/project/launch.rs",
        "crates/ostadix-api/src/project/logical.rs",
        "crates/ostadix-api/src/project/plan.rs",
        "crates/ostadix-api/src/project/runtime.rs",
        "crates/ostadix-api/src/project/runtime_graph.rs",
        "crates/ostadix-api/src/project/trace.rs",
        "crates/ostadix-api/src/project/world_execution.rs",
        "crates/ostadix-api/src/world/grounding.rs",
        "crates/ostadix-api/src/world/identity.rs",
        "crates/ostadix-api/src/world/identity_wire.rs",
        "crates/ostadix-api/src/world/codec.rs",
        "crates/ostadix-api/src/world/mod.rs",
        "crates/ostadix-api/src/world/protocol.rs",
        "crates/ostadix-api/src/world/receipt.rs",
        "crates/ostadix-api/src/world/receipt_codec.rs",
        "crates/ostadix-api/src/world/value.rs",
        "crates/ostadix-api/src/world/value_codec.rs",
        "tests/example_manifest.py",
        "tests/fixtures/world_identity_v1.hex",
        "tests/fixtures/project_hgraph/input.txt",
        "tests/fixtures/project_hgraph/olang.project.toml",
        "tests/fixtures/project_hgraph_exec/input.txt",
        "tests/fixtures/project_hgraph_exec/olang.project.toml",
        "tests/fixtures/project_hgraph_tools/sh",
        "tests/fixtures/world_protocol_v1.hex",
        "tests/fixtures/world_receipt_v1.hex",
        "tests/fixtures/world_value_v1.hex",
        "tests/test_example_manifest.py",
        "tests/test_mcp_smoke.py",
        "tests/test_release_evidence.py",
        "tests/test_ostadix_boot_media.py",
        "tests/test_ostadix_boot_info_qemu.py",
        "tests/test_ostadix_media_writer.py",
        "tests/test_ostadix_physical_evidence.py",
        "tests/test_o_cli_dispatch.py",
        "tests/test_setup.py",
        "tests/test_contract_surfaces.py",
        "tests/test_architecture_boundaries.py",
        "tests/test_governance_surfaces.py",
        "tests/test_local_ci_posture.py",
        "tests/test_backend_state_protocol.py",
        "tests/test_bundled_shim_protocol.py",
        "tests/backend_morphism_v1.rs",
        "tests/test_world_alpha_evidence.py",
        "tests/hosted_remote_cli.rs",
        "tests/hosted_remote_v2.rs",
        "tests/o_info_cli.rs",
        "tests/information_bridge_v1.rs",
        "tests/placement_v6.rs",
        "tests/registry_v1.rs",
        "tests/project_hgraph.rs",
        "tests/project_hgraph_exec.rs",
        "tests/project_deployment_plan.rs",
        "tests/project_logical_hgraph.rs",
        "tests/project_world_runtime.rs",
        "tests/world_resource_keys.rs",
        "tests/world_identity.rs",
        "tests/world_identity_wire.rs",
        "tests/world_protocol.rs",
        "tests/world_receipt.rs",
        "tests/world_value.rs",
    }
) | HOSTED_HGRAPH_BENCHMARK_RELEASE_PATHS | OSTADIX_API_RELEASE_PATHS | frozenset(
    OSTADIX_API_ROOT_MODULE_PATHS.values()
)
VALID_GIT_MODES = frozenset({"100644", "100755"})
SAFE_PREFIX = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")
HEX_DIGEST = re.compile(r"[0-9a-f]{64}\Z")
HEX_COMMIT = re.compile(r"[0-9a-f]{40,64}\Z")
WORLD_LEDGER_ID = re.compile(r"[a-z0-9][a-z0-9._-]*\Z")
URI_SCHEME = re.compile(r"[A-Za-z][A-Za-z0-9+.-]*:")
EXAMPLE_EDITIONS = frozenset({"rust", "c17", "python"})
EXAMPLE_CLASSIFICATIONS = frozenset({"unit", "integration", "manual"})
EXAMPLE_MODES = frozenset({"interpreter", "aot"})
EVIDENCE_CLASSES = frozenset(
    {"portable_tcg", "qemu_tcg_aarch64", "hardware_kvm"}
)
EXPECTED_REQUIRED_EVIDENCE_GATES = 26
EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES = 1
EVIDENCE_COMMON_REQUIRED_TOOLS = frozenset(
    {"bash", "cargo", "rustc", "clang", "lld", "python3"}
)
EVIDENCE_CLASS_REQUIRED_TOOLS = {
    "portable_tcg": frozenset({"qemu-system-x86_64"}),
    "qemu_tcg_aarch64": frozenset({"qemu-system-aarch64"}),
    "hardware_kvm": frozenset({"qemu-system-x86_64"}),
}
G2_AARCH64_GATE_ID = "world-g2-aarch64-native"
G2_AARCH64_SCRIPT = "ocore/kernel/smoke-aarch64-g2-qemu.sh"
G2_AARCH64_REQUIRED_TOOLS = EVIDENCE_COMMON_REQUIRED_TOOLS | frozenset(
    {"cmp", "git", "qemu-system-aarch64", "shasum"}
)
G2_AARCH64_POSITIVE_CLAIMS = (
    "One O-core kernel compiled for AArch64 retains EL2, enters host EL1, "
    "completes one domain-separated HVC return with register and stack integrity, "
    "and in one live QEMU TCG run executes native EL0 process, IPC, capability, "
    "lifecycle, stale-generation, reclamation, and bounded post-lifecycle "
    "counter-progress checks",
)
G2_AARCH64_NONCLAIMS = (
    "This single-vCPU QEMU TCG gate is not physical AArch64, KVM/SVM, SMP, or "
    "G3 evidence",
    "It does not boot Linux or Plan 9 and does not establish a general foreign ABI",
    "It provides no PCI or physical-device assignment, DMA isolation, or "
    "IOMMU/SMMU evidence",
)
G2_AARCH64_EXPECTED_MARKERS = (
    "G2 AArch64 ocorec object: PASS",
    "G2 AArch64 resident EL2 HVC round-trip: PASS",
    "G2 AArch64 EL0 process lifecycle: PASS",
    "G2 AArch64 IPC capability lifecycle: PASS",
    "G2 AArch64 post-lifecycle counter progress: PASS",
    "G2 AArch64 native compiler QEMU smoke: PASS",
)

# These files jointly define the version-3 native World constitution,
# composed executable G0 contracts, and definition-only G0-G13 registry. Source releases are built from arbitrary
# committed refs and archive verification must not execute the Python shipped in
# an untrusted ZIP, so keep trusted byte seals here and recheck the inert data
# below.  Any intentional constitutional edit requires an explicit seal update.
SEALED_WORLD_ALPHA_SHA256 = {
    "docs/OSTADIX_WORLD.md": (
        "e7d47d7a8e0e8f6d35bf3bb6b1f86f2bddffe27a67a3415c3d4ea8c76e13bcea"
    ),
    "docs/HOSTED_WORLD_REFERENCE_PROFILE.md": (
        "647da49edfc4b7d53a9248e8fdcda5cdb62be3c47756c59b7523ee09461d2e1d"
    ),
    "evidence/world_alpha_gates.toml": (
        "1ba4091c43b8e5ba868f3366bfaba675ccabf6bb9950429cafff12c5faa65b6c"
    ),
    "evidence/world_contract_v1.toml": (
        "4b2d92596ab46294894a4127cc5c603b121a3a3d7e942f0013dd419330921bf8"
    ),
    "evidence/world_contract_v2.toml": (
        "af1334bb4d0aca30e7f722890e819c0a597c4c4b42db0006c452dec2e755b74b"
    ),
    "docs/O_MACHINE_CONTRACT.md": (
        "7958677cbf178003b47f475a265857a42dc6e3b51a33fe408c1863b8afa64880"
    ),
    "evidence/o_machine_contract_v1.toml": (
        "eb759ce5695e8080baa3acbd0fcb3f97fc2a97e430679cd8c836aba3a3d2be50"
    ),
}
WORLD_CONTRACT_V2_SEMANTICS_SHA256 = (
    "fedbb397c5b874bf389a376744e1ceb58d9bf418f14fdd155273e8dc561c7bc8"
)
WORLD_MACHINE_CONTRACT_SEMANTICS_SHA256 = (
    "d67c050831b1b52bbb3ec6569d775ce8b0782c6610acc7a8e2e92658af599bf2"
)
WORLD_IMPORTED_CONSTITUTION_V2_SHA256 = (
    "2a56a9b54297c9b6190505055bad3f2e8760a501498b1a55da72a0fd4d298643"
)
WORLD_CLAIM_POLICY_SHA256 = (
    "3a017fee12f6cc7b3c9ef9ec099407f39b5bb143251c21b9937abe47409c9d06"
)
WORLD_DERIVATION_HASH = (
    "sha256:3a017fee12f6cc7b3c9ef9ec099407f39b5bb143251c21b9937abe47409c9d06"
)
WORLD_VALIDATOR_SHA256 = (
    "e3a5adab37962db94ccda38db9ac62570f6ba06dbb9995d16af233af63c8295f"
)
WORLD_REDERIVE_PAYLOAD_DOMAIN = "ostadix.world.evidence.rederive.v1"
WORLD_WITNESS_PAYLOAD_DOMAIN = "ostadix.world.evidence.witness.v1"
WORLD_HISTORICAL_ATTESTATION_SHA256 = {
    "evidence/world/g0-repository-conformance.toml": (
        "f1d1579e8cd7b65e4aa2ce641fe174ff185196c260b65efd4d7cdd1f52d43caa"
    ),
    "evidence/world/g0-repository-conformance-2026-08-03.toml": (
        "a057ec3ab8fb9eac618be635133d106342aff13e85335c74d3e6522e8e46d425"
    ),
    "evidence/world/g0-repository-conformance-2026-08-03-v2.toml": (
        "0016e953bbd353f28b771e8e2d0cfe34867bc7f6561e06c7e1c86fb908a9a8c4"
    ),
    "evidence/world/g0-ostadix-alpha-branding-2026-08-09.toml": (
        "32b76b190aab1c51ba73beccee350ea2a20928798605e980173c86da916450df"
    ),
    "evidence/world/g2-aarch64-qemu.toml": (
        "99414f1cf356b3666c163e0e28172eaf2b46e3f14c8f13f2ce12fa24cc9d30d7"
    ),
    "evidence/world/g2-aarch64-qemu-2026-08-03.toml": (
        "5b2af3bdaad2cdc4f5efb097c784e28ee5915dfa32b3404bc7ba69caa5ff9eb2"
    ),
}
WORLD_CURRENT_ATTESTATION_SHA256 = {
    "evidence/world/g0-independent-engine-2026-08-17.toml": (
        "2c48ef0100bf944e2ce50a70162adff2078836e5e755c92177f366714e7b21be"
    ),
}
# Repository-authored lifecycle and derivation events are immutable ledger
# records.  The release verifier seals their complete bytes independently of
# the payload hash carried by a rederive event.
WORLD_EVIDENCE_EVENT_SHA256 = {
    "evidence/world/g0-independent-engine-supersession-2026-08-17.toml": (
        "aeec68018bd7416cc7b24b1a4d8b102e3df31122a56856784796b73f4a1d90ce"
    ),
    "evidence/world/g0-ostadix-alpha-branding-supersession-2026-08-09.toml": (
        "132a60bb1e42d6debfe68294276e3f8cdea47aa862e0c2f5ca657489191c2227"
    ),
    "evidence/world/g0-derivation-rederive-2026-08-03.toml": (
        "80a0f4805fb4ebf63f6e22d70aa7d01dc5d84856df1bbc2fe4626fca8ac08f7c"
    ),
    "evidence/world/g0-schema-v3-supersession-2026-08-03.toml": (
        "367171fbfe3cf92f0f4f0ec21d4cbccf16f9b4c0582be3ca007633e67090c810"
    ),
    "evidence/world/g0-machine-contract-supersession-2026-08-03.toml": (
        "e4b924570811db48a03a5cae48de7c3061e932feabf89a92f20ce571c82aa047"
    ),
    "evidence/world/g2-counter-wording-supersession-2026-08-03.toml": (
        "c2098f66697e0b0b3435e83e75e6e6fa64d22e24beb93a086d9b1a25a947a01b"
    ),
    "evidence/world/g2-derivation-rederive-2026-08-03.toml": (
        "f8c89bd76f0c428486729f175615ad7a1d4df87b5640cfa2b6fb508a2ab3da3f"
    ),
}
WORLD_REGISTRY_SEMANTICS_SHA256 = (
    "23ead9813a067917ac5ea5d08c7be34616865286e9f45622212eb0ff3676686e"
)
WORLD_LEGACY_ACTIVE_SCHEMA2_IDS = frozenset(
    {"g2-aarch64-qemu-tcg-2026-08-03"}
)
WORLD_DERIVED_CLAIMS = {
    "G0": {
        "evidence.claim_class_guarded",
        "world.contract_schema_consistent",
        "world.crossing_taxonomy_consistent",
        "world.failure_consistency_schema_consistent",
        "world.identity_vocabulary_consistent",
        "world.machine_contract_consistent",
    },
    "G2": {
        "aarch64.el0_execution",
        "aarch64.el1_execution",
        "aarch64.el2_resident",
        "aarch64.hvc_roundtrip",
        "aarch64.native_object",
        "aarch64.svc_eret_roundtrip",
        "capability.attenuation",
        "capability.stale_generation_rejected",
        "counter.progress_after_lifecycle",
        "execution.post_lifecycle_reached",
        "ipc.request_reply",
        "lifecycle.reclamation",
        "lifecycle.terminal",
    },
}
EXPECTED_WORLD_ALPHA_GATE_IDS = tuple(f"G{number}" for number in range(14))
EXPECTED_WORLD_ALPHA_CLASS_IDS = (
    "repository_conformance",
    "hosted_reference",
    "qemu_tcg_x86_64",
    "qemu_tcg_aarch64",
    "qemu_virtualization",
    "hardware_x86_64",
    "hardware_x86_64_iommu",
    "hardware_aarch64",
    "hardware_aarch64_smmu",
    "multinode_virtual",
    "multinode_physical",
    "fault_injection",
    "security_adversarial",
    "performance_characterization",
)


class ReleaseError(RuntimeError):
    """A source release could not be built or verified safely."""


@dataclass(frozen=True)
class SourceEntry:
    path: str
    mode: str
    data: bytes

    @property
    def sha256(self) -> str:
        return hashlib.sha256(self.data).hexdigest()


@dataclass(frozen=True)
class BuildResult:
    output: Path
    commit: str
    prefix: str
    file_count: int
    archive_sha256: str


def _git(repo: Path, *arguments: str) -> bytes:
    command = ["git", "-C", os.fspath(repo), *arguments]
    try:
        result = subprocess.run(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as error:
        raise ReleaseError(f"cannot execute Git: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise ReleaseError(
            f"Git command failed ({' '.join(arguments)}): {detail or 'unknown error'}"
        )
    return result.stdout


def discover_repository(path: Path | str) -> Path:
    candidate = Path(path).expanduser().resolve()
    root = _git(candidate, "rev-parse", "--show-toplevel")
    return Path(root.decode("utf-8", "surrogateescape").strip()).resolve()


def resolve_commit(repo: Path, ref: str) -> str:
    if not ref or "\x00" in ref:
        raise ReleaseError("Git ref must be a non-empty string without NUL bytes")
    commit = _git(repo, "rev-parse", "--verify", f"{ref}^{{commit}}")
    value = commit.decode("ascii", "strict").strip()
    if not HEX_COMMIT.fullmatch(value):
        raise ReleaseError(f"Git returned an invalid commit identifier: {value!r}")
    return value


def assert_clean_worktree(repo: Path, *, allow_dirty: bool) -> None:
    status = _git(
        repo,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--ignore-submodules=none",
        "-z",
    )
    if status and not allow_dirty:
        changed = sum(1 for record in status.split(b"\0") if record)
        raise ReleaseError(
            f"working tree is dirty ({changed} status record(s)); commit or stash "
            "the changes, or pass --allow-dirty to archive the selected commit anyway"
        )


def _validate_release_path(path: str) -> PurePosixPath:
    if not path or "\x00" in path or "\n" in path or "\r" in path:
        raise ReleaseError(f"unsafe release path: {path!r}")
    pure = PurePosixPath(path)
    if pure.is_absolute() or any(part in {"", ".", ".."} for part in pure.parts):
        raise ReleaseError(f"unsafe release path: {path!r}")
    if pure.as_posix() != path:
        raise ReleaseError(f"non-canonical release path: {path!r}")
    return pure


def is_allowed_release_path(path: str) -> bool:
    pure = _validate_release_path(path)
    parts = pure.parts
    top = parts[0]
    if top == "src" and not (
        path in {"src/lib.rs", "src/main.rs"} or path.startswith("src/bin/")
    ):
        return False
    api_scoped = any(path.startswith(prefix) for prefix in OSTADIX_API_ALLOWED_PREFIXES)
    if path not in ALLOWED_EXACT_PATHS and not api_scoped:
        if len(parts) == 1:
            if top not in ALLOWED_TOP_LEVEL_FILES:
                return False
        elif top not in ALLOWED_TOP_LEVEL_DIRECTORIES:
            return False

    if path in EXCLUDED_EXACT_PATHS:
        return False
    if any(
        part in EXCLUDED_DIRECTORY_NAMES
        or part.startswith("cmake-build-")
        or part.endswith(".dSYM")
        for part in parts[:-1]
    ):
        return False

    basename = parts[-1]
    if basename in EXCLUDED_BASENAMES or basename.startswith("cvelist"):
        return False
    if basename.endswith("~") or basename.startswith(".#"):
        return False
    if basename.endswith(EXCLUDED_SUFFIXES):
        return False
    return True


def _decode_git_path(raw_path: bytes) -> str:
    try:
        return raw_path.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError(
            "source releases require UTF-8 Git paths; found an undecodable path"
        ) from error


def _resolve_compile_time_include(owner: str, relative: str) -> str:
    """Resolve one literal Rust include without allowing release-root escape."""

    if PurePosixPath(relative).is_absolute():
        raise ReleaseError(
            f"compile-time include in {owner} is absolute: {relative!r}"
        )
    parts = list(PurePosixPath(owner).parent.parts)
    for part in PurePosixPath(relative).parts:
        if part in {"", "."}:
            continue
        if part == "..":
            if not parts:
                raise ReleaseError(
                    f"compile-time include in {owner} escapes the release root: "
                    f"{relative!r}"
                )
            parts.pop()
        else:
            parts.append(part)
    if not parts:
        raise ReleaseError(
            f"compile-time include in {owner} resolves to the release root: "
            f"{relative!r}"
        )
    return PurePosixPath(*parts).as_posix()


def validate_generated_runtime_source_closure(entries: Sequence[SourceEntry]) -> None:
    """Require the shell compiler and engine-owned AOT source inventories."""

    files = {entry.path: entry.data for entry in entries}
    compiler_path = "src/bin/olangc.rs"
    compiler_bytes = files.get(compiler_path)
    if compiler_bytes is None:
        raise ReleaseError(f"release is missing generated-runtime compiler {compiler_path}")
    try:
        compiler = compiler_bytes.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError(f"{compiler_path} is not valid UTF-8") from error

    embedded = {
        _resolve_compile_time_include(compiler_path, match.group("path"))
        for match in PARENT_RELATIVE_INCLUDE.finditer(compiler)
    }

    aot_bytes = files.get(OSTADIX_API_AOT_SOURCE)
    if aot_bytes is None:
        raise ReleaseError(
            "release is missing engine-owned generated-runtime inventory "
            f"{OSTADIX_API_AOT_SOURCE}"
        )
    try:
        aot_source = aot_bytes.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError(f"{OSTADIX_API_AOT_SOURCE} is not valid UTF-8") from error
    if "o_lang::api::aot_source" not in compiler:
        raise ReleaseError(
            f"{compiler_path} must consume the engine-owned api::aot_source inventory"
        )
    embedded.add(OSTADIX_API_AOT_SOURCE)
    embedded.update(
        _resolve_compile_time_include(OSTADIX_API_AOT_SOURCE, match.group("path"))
        for match in RELATIVE_LITERAL_INCLUDE.finditer(aot_source)
    )
    missing = sorted(embedded - files.keys())
    if missing:
        raise ReleaseError(
            "release omits olangc generated-runtime source closure path(s): "
            + ", ".join(missing)
        )


def collect_source_entries(repo: Path, commit: str) -> list[SourceEntry]:
    tree = _git(repo, "ls-tree", "-r", "-z", "--full-tree", commit)
    selected: list[tuple[str, str, str]] = []

    for record in tree.split(b"\0"):
        if not record:
            continue
        try:
            metadata, raw_path = record.split(b"\t", 1)
            raw_mode, raw_kind, raw_oid = metadata.split(b" ", 2)
        except ValueError as error:
            raise ReleaseError("Git produced a malformed tree record") from error
        path = _decode_git_path(raw_path)
        kind = raw_kind.decode("ascii", "strict")
        mode = raw_mode.decode("ascii", "strict")
        oid = raw_oid.decode("ascii", "strict")
        if kind == "commit" or mode == "160000":
            raise ReleaseError(
                f"self-contained source releases forbid Git gitlinks: {path}"
            )
        if not is_allowed_release_path(path):
            continue
        if kind != "blob":
            raise ReleaseError(f"allowlisted path is not a Git blob: {path}")
        if mode not in VALID_GIT_MODES:
            raise ReleaseError(f"unsupported Git mode {mode} for {path}")
        selected.append((path, mode, oid))

    selected.sort(key=lambda item: item[0].encode("utf-8"))
    paths = {path for path, _mode, _oid in selected}
    missing = sorted(REQUIRED_RELEASE_PATHS - paths)
    if missing:
        raise ReleaseError(
            "commit is not an Ostadix-lang source tree; missing required path(s): "
            + ", ".join(missing)
        )
    if len(paths) != len(selected):
        raise ReleaseError("Git tree contains duplicate release paths")

    entries = [
        SourceEntry(path=path, mode=mode, data=_git(repo, "cat-file", "blob", oid))
        for path, mode, oid in selected
    ]
    validate_generated_runtime_source_closure(entries)
    validate_document_links(entries)
    validate_release_metadata(entries)
    return entries


def _is_markdown_escaped(text: str, index: int) -> bool:
    backslashes = 0
    cursor = index - 1
    while cursor >= 0 and text[cursor] == "\\":
        backslashes += 1
        cursor -= 1
    return backslashes % 2 == 1


def _blank_markdown_range(characters: list[str], start: int, end: int) -> None:
    for index in range(start, end):
        if characters[index] not in "\r\n":
            characters[index] = " "


def _markdown_fence(line: str) -> tuple[str, int, str] | None:
    indent = len(line) - len(line.lstrip(" "))
    if indent > 3 or indent == len(line):
        return None
    marker = line[indent]
    if marker not in {"`", "~"}:
        return None
    cursor = indent
    while cursor < len(line) and line[cursor] == marker:
        cursor += 1
    length = cursor - indent
    if length < 3:
        return None
    return marker, length, line[cursor:]


def _markdown_visible_text(text: str) -> str:
    """Blank Markdown code and comments while preserving offsets and newlines."""

    characters = list(text)
    fence: tuple[str, int] | None = None
    offset = 0
    for line in text.splitlines(keepends=True):
        content = line.rstrip("\r\n")
        candidate = _markdown_fence(content)
        if fence is not None:
            _blank_markdown_range(characters, offset, offset + len(line))
            if (
                candidate is not None
                and candidate[0] == fence[0]
                and candidate[1] >= fence[1]
                and not candidate[2].strip(" \t")
            ):
                fence = None
        elif content.startswith(("    ", "\t")):
            _blank_markdown_range(characters, offset, offset + len(line))
        elif candidate is not None:
            marker, length, remainder = candidate
            # Backticks in a backtick fence's info string make it ordinary text
            # under CommonMark rather than the start of a fenced code block.
            if marker != "`" or "`" not in remainder:
                fence = (marker, length)
                _blank_markdown_range(characters, offset, offset + len(line))
        offset += len(line)

    visible = "".join(characters)
    cursor = 0
    while cursor < len(visible):
        if visible[cursor] != "`" or _is_markdown_escaped(visible, cursor):
            cursor += 1
            continue
        run_end = cursor + 1
        while run_end < len(visible) and visible[run_end] == "`":
            run_end += 1
        run_length = run_end - cursor
        closing = run_end
        while closing < len(visible):
            closing = visible.find("`", closing)
            if closing < 0:
                break
            if _is_markdown_escaped(visible, closing):
                closing += 1
                continue
            closing_end = closing + 1
            while closing_end < len(visible) and visible[closing_end] == "`":
                closing_end += 1
            if closing_end - closing == run_length:
                _blank_markdown_range(characters, cursor, closing_end)
                cursor = closing_end
                break
            closing = closing_end
        else:
            closing = -1
        if closing < 0:
            cursor = run_end

    visible = "".join(characters)
    cursor = 0
    while True:
        opening = visible.find("<!--", cursor)
        if opening < 0:
            break
        closing = visible.find("-->", opening + 4)
        end = len(visible) if closing < 0 else closing + 3
        _blank_markdown_range(characters, opening, end)
        cursor = end
    return "".join(characters)


def _find_matching_markdown_bracket(
    text: str, opening: int, end: int | None = None
) -> int | None:
    limit = len(text) if end is None else end
    depth = 1
    cursor = opening + 1
    while cursor < limit:
        if _is_markdown_escaped(text, cursor):
            cursor += 1
            continue
        if text[cursor] == "[":
            depth += 1
        elif text[cursor] == "]":
            depth -= 1
            if depth == 0:
                return cursor
        cursor += 1
    return None


def _markdown_unescape_destination(value: str) -> str:
    result: list[str] = []
    cursor = 0
    while cursor < len(value):
        if (
            value[cursor] == "\\"
            and cursor + 1 < len(value)
            and value[cursor + 1] in string.punctuation
        ):
            result.append(value[cursor + 1])
            cursor += 2
        else:
            result.append(value[cursor])
            cursor += 1
    return "".join(result)


def _inline_link_close(text: str, start: int) -> int | None:
    cursor = start
    while cursor < len(text) and text[cursor].isspace():
        cursor += 1
    if cursor < len(text) and text[cursor] == ")":
        return cursor
    if cursor >= len(text) or text[cursor] not in {'"', "'", "("}:
        return None

    opener = text[cursor]
    if opener in {'"', "'"}:
        cursor += 1
        while cursor < len(text):
            if text[cursor] == opener and not _is_markdown_escaped(text, cursor):
                cursor += 1
                break
            cursor += 1
        else:
            return None
    else:
        depth = 1
        cursor += 1
        while cursor < len(text) and depth:
            if _is_markdown_escaped(text, cursor):
                cursor += 1
            elif text[cursor] == "(":
                depth += 1
            elif text[cursor] == ")":
                depth -= 1
            cursor += 1
        if depth:
            return None

    while cursor < len(text) and text[cursor].isspace():
        cursor += 1
    return cursor if cursor < len(text) and text[cursor] == ")" else None


def _inline_link_destination(text: str, opening: int) -> tuple[str, int] | None:
    cursor = opening + 1
    while cursor < len(text) and text[cursor].isspace():
        cursor += 1
    if cursor >= len(text):
        return None
    if text[cursor] == ")":
        return "", cursor

    if text[cursor] == "<":
        start = cursor + 1
        cursor = start
        while cursor < len(text):
            if text[cursor] in "\r\n":
                return None
            if text[cursor] == ">" and not _is_markdown_escaped(text, cursor):
                destination = text[start:cursor]
                closing = _inline_link_close(text, cursor + 1)
                if closing is None:
                    return None
                return _markdown_unescape_destination(destination), closing
            cursor += 1
        return None

    start = cursor
    depth = 0
    while cursor < len(text):
        if text[cursor] == "\\" and cursor + 1 < len(text):
            cursor += 2
            continue
        if text[cursor] == "(":
            depth += 1
        elif text[cursor] == ")":
            if depth == 0:
                return _markdown_unescape_destination(text[start:cursor]), cursor
            depth -= 1
        elif text[cursor].isspace() and depth == 0:
            destination = text[start:cursor]
            closing = _inline_link_close(text, cursor)
            if closing is None:
                return None
            return _markdown_unescape_destination(destination), closing
        cursor += 1
    return None


def _reference_destination(text: str, start: int, end: int) -> str | None:
    cursor = start
    while cursor < end and text[cursor] in " \t":
        cursor += 1
    if cursor >= end:
        return None
    if text[cursor] == "<":
        opening = cursor + 1
        cursor = opening
        while cursor < end:
            if text[cursor] == ">" and not _is_markdown_escaped(text, cursor):
                return _markdown_unescape_destination(text[opening:cursor])
            cursor += 1
        return None

    opening = cursor
    depth = 0
    while cursor < end:
        if text[cursor] == "\\" and cursor + 1 < end:
            cursor += 2
            continue
        if text[cursor] == "(":
            depth += 1
        elif text[cursor] == ")":
            if depth == 0:
                break
            depth -= 1
        elif text[cursor].isspace() and depth == 0:
            break
        cursor += 1
    if cursor == opening or depth:
        return None
    return _markdown_unescape_destination(text[opening:cursor])


def _markdown_destinations(text: str) -> list[str]:
    visible = _markdown_visible_text(text)
    destinations: list[str] = []

    offset = 0
    for line in visible.splitlines(keepends=True):
        content_end = offset + len(line.rstrip("\r\n"))
        cursor = offset
        while cursor < content_end and visible[cursor] == " ":
            cursor += 1
        if cursor - offset <= 3 and cursor < content_end and visible[cursor] == "[":
            closing = _find_matching_markdown_bracket(visible, cursor, content_end)
            if (
                closing is not None
                and closing + 1 < content_end
                and visible[closing + 1] == ":"
                and visible[cursor + 1 : closing] != ""
                and not visible[cursor + 1 : closing].startswith("^")
            ):
                destination = _reference_destination(
                    visible, closing + 2, content_end
                )
                if destination is not None:
                    destinations.append(destination)
        offset += len(line)

    cursor = 0
    while cursor < len(visible):
        if visible[cursor] != "[" or _is_markdown_escaped(visible, cursor):
            cursor += 1
            continue
        closing = _find_matching_markdown_bracket(visible, cursor)
        if closing is None:
            cursor += 1
            continue
        if closing + 1 < len(visible) and visible[closing + 1] == "(":
            parsed = _inline_link_destination(visible, closing + 1)
            if parsed is not None:
                destination, link_end = parsed
                destinations.append(destination)
                cursor = link_end + 1
                continue
        cursor = closing + 1
    return destinations


def _resolve_document_target(source: str, destination: str) -> str | None:
    if not destination or destination.startswith(("#", "/", "//")):
        return None
    if URI_SCHEME.match(destination):
        return None

    split = urlsplit(destination)
    if split.scheme or split.netloc or not split.path:
        return None
    decoded = unquote(split.path)
    if (
        PurePosixPath(decoded).is_absolute()
        or "\\" in decoded
        or "\x00" in decoded
    ):
        raise ReleaseError(
            f"documentation link in {source} has an unsafe target: {destination!r}"
        )

    parts: list[str] = []
    for part in (PurePosixPath(source).parent / decoded).parts:
        if part in {"", "."}:
            continue
        if part == "..":
            if not parts:
                raise ReleaseError(
                    f"documentation link in {source} escapes the release root: "
                    f"{destination!r}"
                )
            parts.pop()
        else:
            parts.append(part)
    return "/".join(parts)


def validate_document_links(entries: Sequence[SourceEntry]) -> None:
    """Require every relative Markdown link target to exist in the release."""

    paths = {entry.path for entry in entries}
    directories = {""} | {
        "/".join(PurePosixPath(path).parts[:index])
        for path in paths
        for index in range(1, len(PurePosixPath(path).parts))
    }
    broken: list[str] = []
    for entry in entries:
        if not entry.path.lower().endswith(".md"):
            continue
        try:
            document = entry.data.decode("utf-8", "strict")
        except UnicodeDecodeError as error:
            raise ReleaseError(
                f"release documentation is not UTF-8: {entry.path}"
            ) from error
        for destination in _markdown_destinations(document):
            target = _resolve_document_target(entry.path, destination)
            if target is not None and target not in paths and target not in directories:
                broken.append(f"{entry.path} -> {destination} (resolved {target})")
    if broken:
        raise ReleaseError(
            "release documentation contains missing relative link target(s): "
            + "; ".join(sorted(set(broken)))
        )


def _strict_json(data: bytes, path: str) -> object:
    try:
        text = data.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError(f"{path} is not valid UTF-8") from error

    def object_pairs(pairs: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for key, value in pairs:
            if key in result:
                raise ReleaseError(f"{path} contains duplicate JSON key {key!r}")
            result[key] = value
        return result

    def invalid_constant(value: str) -> object:
        raise ReleaseError(f"{path} contains non-finite JSON number {value}")

    try:
        return json.loads(
            text,
            object_pairs_hook=object_pairs,
            parse_constant=invalid_constant,
        )
    except json.JSONDecodeError as error:
        raise ReleaseError(f"{path} is not valid JSON: {error}") from error


def _strict_toml(data: bytes, path: str) -> dict[str, object]:
    try:
        text = data.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError(f"{path} is not valid UTF-8") from error
    try:
        value = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise ReleaseError(f"{path} is not valid TOML: {error}") from error
    if not isinstance(value, dict):  # pragma: no cover - tomllib roots are tables
        raise ReleaseError(f"{path} root must be a TOML table")
    return value


def _toml_semantics_sha256(value: object) -> str:
    encoded = json.dumps(
        value, ensure_ascii=True, sort_keys=True, separators=(",", ":")
    ).encode("ascii")
    return hashlib.sha256(encoded).hexdigest()


def _required_string(value: object, owner: str) -> str:
    if not isinstance(value, str) or not value or value != value.strip():
        raise ReleaseError(f"{owner} must be a non-empty trimmed string")
    if "\x00" in value or "\r" in value or "\n" in value:
        raise ReleaseError(f"{owner} contains a forbidden control character")
    return value


def _utf8_text(data: bytes, path: str) -> str:
    try:
        return data.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError(f"{path} is not valid UTF-8") from error


def _cff_scalar(
    text: str,
    key: str,
    *,
    indent: str = "",
    required: bool = True,
) -> str | None:
    """Read one deliberately simple CFF scalar without a YAML dependency."""

    prefix = f"{indent}{key}:"
    matches = [
        line[len(prefix) :].strip()
        for line in text.splitlines()
        if line.startswith(prefix)
    ]
    if not matches:
        if required:
            raise ReleaseError(f"CITATION.cff is missing {prefix}")
        return None
    if len(matches) != 1:
        raise ReleaseError(f"CITATION.cff must contain exactly one {prefix}")
    raw = matches[0]
    if not raw:
        raise ReleaseError(f"CITATION.cff {prefix} must be a scalar")
    if raw.startswith('"'):
        try:
            value = json.loads(raw)
        except json.JSONDecodeError as error:
            raise ReleaseError(
                f"CITATION.cff {prefix} is not a valid quoted scalar"
            ) from error
        if not isinstance(value, str):
            raise ReleaseError(f"CITATION.cff {prefix} must be a string")
        return _required_string(value, f"CITATION.cff {prefix}")
    if raw.startswith("'"):
        if len(raw) < 2 or not raw.endswith("'"):
            raise ReleaseError(f"CITATION.cff {prefix} is not a valid quoted scalar")
        return _required_string(
            raw[1:-1].replace("''", "'"), f"CITATION.cff {prefix}"
        )
    if " #" in raw:
        raw = raw.split(" #", 1)[0].rstrip()
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9+./:_-]*", raw):
        raise ReleaseError(f"CITATION.cff {prefix} must be a simple scalar")
    return _required_string(raw, f"CITATION.cff {prefix}")


def _cff_mapping_body(text: str, key: str) -> str:
    """Return one top-level CFF mapping body without accepting relocated keys."""

    header = f"{key}:"
    lines = text.splitlines()
    matches = [index for index, line in enumerate(lines) if line == header]
    if not matches:
        raise ReleaseError(f"CITATION.cff is missing top-level {header}")
    if len(matches) != 1:
        raise ReleaseError(
            f"CITATION.cff must contain exactly one top-level {header}"
        )

    body = []
    for line in lines[matches[0] + 1 :]:
        if line and not line[0].isspace() and not line.startswith("#"):
            break
        body.append(line)
    return "\n".join(body)


def _readme_how_to_cite(text: str) -> str:
    headings = list(re.finditer(r"(?m)^### How to cite[ \t]*$", text))
    if len(headings) != 1:
        raise ReleaseError(
            "README.md must contain exactly one '### How to cite' section"
        )
    start = headings[0].end()
    following = re.search(r"(?m)^#{1,3}[ \t]+", text[start:])
    end = start + following.start() if following else len(text)
    return " ".join(text[start:end].split())


def _validate_root_release_metadata(files: dict[str, bytes]) -> None:
    """Keep root license, Cargo, CFF, and live citation prose coherent."""

    required_paths = ("Cargo.toml", "CITATION.cff", "LICENSE", "README.md")
    missing = [path for path in required_paths if path not in files]
    if missing:
        raise ReleaseError(
            "root release metadata is incomplete; missing: " + ", ".join(missing)
        )

    cargo = _strict_toml(files["Cargo.toml"], "Cargo.toml")
    package = cargo.get("package")
    if not isinstance(package, dict):
        raise ReleaseError("Cargo.toml must contain a package table")
    cargo_version = _required_string(
        package.get("version"), "Cargo.toml package.version"
    )
    cargo_license = _required_string(
        package.get("license"), "Cargo.toml package.license"
    )
    cargo_repository = _required_string(
        package.get("repository"), "Cargo.toml package.repository"
    )
    if cargo_license != ROOT_LICENSE_SPDX:
        raise ReleaseError(
            f"Cargo.toml package.license must be {ROOT_LICENSE_SPDX!r}"
        )
    if cargo_repository != ROOT_REPOSITORY:
        raise ReleaseError(
            f"Cargo.toml package.repository must be {ROOT_REPOSITORY!r}"
        )

    license_text = _utf8_text(files["LICENSE"], "LICENSE")
    for marker in (
        "GNU LESSER GENERAL PUBLIC LICENSE",
        "Version 2.1, February 1999",
    ):
        if marker not in license_text:
            raise ReleaseError(
                f"LICENSE does not contain the expected LGPL-2.1 text: {marker}"
            )

    citation = _utf8_text(files["CITATION.cff"], "CITATION.cff")
    citation_version = _cff_scalar(citation, "version")
    citation_license = _cff_scalar(citation, "license")
    citation_repository = _cff_scalar(citation, "repository-code")
    preferred_citation = _cff_mapping_body(citation, "preferred-citation")
    preferred_doi = _cff_scalar(preferred_citation, "doi", indent="  ")
    preferred_url = _cff_scalar(preferred_citation, "url", indent="  ")
    source_release_doi = _cff_scalar(citation, "doi", required=False)

    if citation_version != cargo_version:
        raise ReleaseError(
            "CITATION.cff version must match Cargo.toml package.version "
            f"({citation_version!r} != {cargo_version!r})"
        )
    if citation_license != cargo_license:
        raise ReleaseError(
            "CITATION.cff license must match Cargo.toml package.license "
            f"({citation_license!r} != {cargo_license!r})"
        )
    if citation_repository != cargo_repository:
        raise ReleaseError(
            "CITATION.cff repository-code must match Cargo.toml package.repository"
        )
    if preferred_doi != EXISTING_PREPRINT_DOI:
        raise ReleaseError(
            "CITATION.cff preferred-citation DOI must remain the existing "
            f"preprint/package DOI {EXISTING_PREPRINT_DOI}"
        )
    expected_preprint_url = f"https://doi.org/{EXISTING_PREPRINT_DOI}"
    if preferred_url != expected_preprint_url:
        raise ReleaseError(
            "CITATION.cff preferred-citation URL must resolve its DOI through "
            f"{expected_preprint_url}"
        )
    if source_release_doi == preferred_doi:
        raise ReleaseError(
            "CITATION.cff top-level source-release DOI must differ from the "
            "preferred preprint/package DOI"
        )

    readme = _utf8_text(files["README.md"], "README.md")
    readme_compact = " ".join(readme.split())
    for phrase in (
        "GNU Lesser General Public License v2.1 only",
        f"`{ROOT_LICENSE_SPDX}`",
    ):
        if phrase not in readme_compact:
            raise ReleaseError(f"README.md license prose must contain {phrase!r}")

    for phrase in (
        "Generated AOT build crates are `publish = false`",
        "component-scoped Cargo metadata identifies the embedded Ostadix "
        "runtime as LGPL-2.1-only",
        "embedded user or project inputs as retaining the licensing attached "
        "to their source",
        "it does not declare one license for the mixed generated package",
    ):
        if phrase not in readme_compact:
            raise ReleaseError(
                "README.md generated-runtime license policy must contain "
                f"{phrase!r}"
            )

    how_to_cite = _readme_how_to_cite(readme)
    required_citation_phrases = (
        expected_preprint_url,
        f"DOI `{EXISTING_PREPRINT_DOI}` identifies that existing "
        "preprint/package record",
        "it is not an archive of a tagged Ostadix-lang source release",
        f"Version {cargo_version}.",
        "Commit: `FULL_COMMIT_SHA_USED`.",
        cargo_repository,
        "the existing preprint/package DOI remains under `preferred-citation`",
        "top-level `doi` field",
    )
    for phrase in required_citation_phrases:
        if phrase not in how_to_cite:
            raise ReleaseError(f"README.md How to cite must contain {phrase!r}")
    if source_release_doi is None:
        if "future tagged source release" not in how_to_cite:
            raise ReleaseError(
                "README.md How to cite must reserve a separate DOI for a future "
                "tagged source release"
            )
    elif source_release_doi not in how_to_cite:
        raise ReleaseError(
            "README.md How to cite must include the top-level tagged "
            f"source-release DOI {source_release_doi}"
        )


def _validate_workspace_facade_release_surface(files: dict[str, bytes]) -> None:
    """Validate the independent engine and its one-way compatibility shell."""

    required = {
        "Cargo.toml",
        "LICENSE",
        "NOTICE",
        "crates/ostadix-api/src/api.rs",
        *OSTADIX_API_RELEASE_PATHS,
    }
    missing = sorted(required - files.keys())
    if missing:
        raise ReleaseError(
            "workspace engine release surface is incomplete; missing: "
            + ", ".join(missing)
        )

    for engine_legal_path, root_legal_path in (
        (f"{OSTADIX_API_ROOT}/LICENSE", "LICENSE"),
        (f"{OSTADIX_API_ROOT}/NOTICE", "NOTICE"),
    ):
        if files[engine_legal_path] != files[root_legal_path]:
            raise ReleaseError(
                f"{engine_legal_path} must be byte-identical to {root_legal_path}"
            )

    root = _strict_toml(files["Cargo.toml"], "Cargo.toml")
    package = root.get("package")
    workspace = root.get("workspace")
    if not isinstance(package, dict) or package.get("name") != "o-lang":
        raise ReleaseError("Cargo.toml package.name must remain 'o-lang'")
    if not isinstance(workspace, dict):
        raise ReleaseError("Cargo.toml must contain a workspace table")
    expected_workspace = {
        "members": [".", "crates/ostadix-api"],
        "default-members": [".", "crates/ostadix-api"],
        "exclude": ["fuzz", "mcp/ostadix_lang_mcp_server"],
        "resolver": "2",
    }
    for field, expected in expected_workspace.items():
        if workspace.get(field) != expected:
            raise ReleaseError(
                f"Cargo.toml workspace.{field} must equal {expected!r}"
            )

    def dependency_occurrences(
        value: object,
        dependency: str,
        path: tuple[str, ...] = (),
    ) -> list[tuple[tuple[str, ...], object]]:
        found: list[tuple[tuple[str, ...], object]] = []
        if not isinstance(value, dict):
            return found
        for key, nested in value.items():
            if key in {"dependencies", "dev-dependencies", "build-dependencies"}:
                if isinstance(nested, dict) and dependency in nested:
                    found.append((path + (key, dependency), nested[dependency]))
            if isinstance(nested, dict):
                found.extend(dependency_occurrences(nested, dependency, path + (key,)))
        return found

    root_version = _required_string(package.get("version"), "Cargo.toml package.version")
    expected_engine_dependency = {
        "path": "crates/ostadix-api",
        "version": f"={root_version}",
        "default-features": False,
    }
    root_engine_dependencies = dependency_occurrences(root, "ostadix-api")
    if root_engine_dependencies != [
        (("dependencies", "ostadix-api"), expected_engine_dependency)
    ]:
        raise ReleaseError(
            "Cargo.toml must depend on ostadix-api exactly once through "
            f"[dependencies] as {expected_engine_dependency!r}"
        )
    root_features = root.get("features")
    if not isinstance(root_features, dict):
        raise ReleaseError("Cargo.toml must contain a features table")
    if root_features.get("graph_executor") != ["ostadix-api/graph_executor"]:
        raise ReleaseError(
            "Cargo.toml feature graph_executor must forward to "
            "ostadix-api/graph_executor"
        )
    notebook = root_features.get("notebook")
    if not isinstance(notebook, list) or "ostadix-api/notebook" not in notebook:
        raise ReleaseError(
            "Cargo.toml feature notebook must forward to ostadix-api/notebook"
        )

    engine_path = f"{OSTADIX_API_ROOT}/Cargo.toml"
    engine = _strict_toml(files[engine_path], engine_path)
    engine_package = engine.get("package")
    if not isinstance(engine_package, dict):
        raise ReleaseError(f"{engine_path} must contain a package table")
    expected_package = {
        "name": "ostadix-api",
        "version": root_version,
        "edition": "2021",
        "rust-version": package.get("rust-version"),
        "repository": package.get("repository"),
        "authors": package.get("authors"),
        "license": package.get("license"),
        "readme": "README.md",
        "publish": True,
    }
    for field, expected in expected_package.items():
        if engine_package.get(field) != expected:
            raise ReleaseError(
                f"{engine_path} package.{field} must equal {expected!r}"
            )
    _required_string(engine_package.get("description"), f"{engine_path} package.description")
    reverse_dependencies = dependency_occurrences(engine, "o-lang")
    if reverse_dependencies:
        raise ReleaseError(
            f"{engine_path} must not depend on the o-lang compatibility shell"
        )
    dependencies = engine.get("dependencies")
    if not isinstance(dependencies, dict) or not dependencies:
        raise ReleaseError(f"{engine_path} must own its runtime dependencies")
    engine_features = engine.get("features")
    expected_engine_features = {
        "default": ["graph_executor"],
        "graph_executor": [],
        "notebook": [],
    }
    if engine_features != expected_engine_features:
        raise ReleaseError(
            f"{engine_path} features must equal {expected_engine_features!r}"
        )

    root_source_path = "src/lib.rs"
    root_source = _utf8_text(files[root_source_path], root_source_path)
    if re.search(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+", root_source):
        raise ReleaseError(f"{root_source_path} must not compile runtime modules")
    if "#[path" in root_source or "o_lang::" in root_source:
        raise ReleaseError(f"{root_source_path} is not a minimal compatibility shell")
    compatibility_match = re.search(
        r"pub\s+use\s+ostadix_api::\s*\{(?P<names>.*?)\};",
        root_source,
        re.DOTALL,
    )
    if compatibility_match is None:
        raise ReleaseError(
            f"{root_source_path} must explicitly reexport ostadix-api modules"
        )
    compatibility_names = {
        name.strip()
        for name in compatibility_match.group("names").split(",")
        if name.strip()
    }
    private_engine_modules = {
        "backend_catalog",
        "canonical_cbor",
        "capability",
        "dispatch_model",
        "eval_core",
        "placement_protocol",
    }
    expected_compatibility_names = (
        set(OSTADIX_API_ROOT_MODULE_PATHS) - private_engine_modules
    )
    if compatibility_names != expected_compatibility_names:
        raise ReleaseError(
            f"{root_source_path} compatibility module closure differs from the "
            "engine public-module set"
        )

    engine_source_path = f"{OSTADIX_API_SOURCE_ROOT}/lib.rs"
    source = _utf8_text(files[engine_source_path], engine_source_path)
    module_names = set(
        re.findall(
            r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;",
            source,
        )
    )
    if module_names != set(OSTADIX_API_ROOT_MODULE_PATHS):
        raise ReleaseError(
            f"{engine_source_path} module closure differs from the complete "
            "independent runtime engine"
        )
    for declaration in ("Runtime", "RuntimeError", "RuntimeStage"):
        if declaration not in source:
            raise ReleaseError(
                f"{engine_source_path} must reexport api::{declaration}"
            )

    api_source_path = f"{OSTADIX_API_SOURCE_ROOT}/api.rs"
    api_source = _utf8_text(files[api_source_path], api_source_path)
    if "o_lang::" in api_source:
        raise ReleaseError(f"{api_source_path} must not depend on o-lang source paths")
    for declaration in (
        "pub struct Runtime",
        "pub struct RuntimeError",
        "pub enum RuntimeStage",
        "pub fn evaluate",
    ):
        if declaration not in api_source:
            raise ReleaseError(f"{api_source_path} must contain {declaration!r}")
    for seam in (
        "pub mod aot_source;",
        "use crate::ir::BackendRegistry;",
        "pub use crate::eval::{",
        "pub use crate::parser::Parser;",
        "pub use num_bigint::BigInt;",
    ):
        if seam not in api_source:
            raise ReleaseError(f"{api_source_path} must retain engine seam {seam!r}")

    aot_source = _utf8_text(files[OSTADIX_API_AOT_SOURCE], OSTADIX_API_AOT_SOURCE)
    if "include_str!" not in aot_source or "RUNTIME_VALUE_RS" not in aot_source:
        raise ReleaseError(
            f"{OSTADIX_API_AOT_SOURCE} must own generated-runtime source bytes"
        )

    engine_tests_path = f"{OSTADIX_API_ROOT}/tests/public_surface.rs"
    engine_tests = _utf8_text(files[engine_tests_path], engine_tests_path)
    for marker in (
        "complete_ovalue_payload_vocabulary_is_nameable_from_the_engine_root",
        "runtime_owns_success_parse_failure_and_evaluate_failure_stages",
        "engine_owns_the_full_runtime_without_a_compatibility_dependency",
    ):
        if marker not in engine_tests:
            raise ReleaseError(f"{engine_tests_path} must retain test {marker}")


def _required_string_list(
    value: object, owner: str, *, minimum: int = 0
) -> list[str]:
    if not isinstance(value, list) or len(value) < minimum:
        raise ReleaseError(f"{owner} must contain at least {minimum} string(s)")
    result = [
        _required_string(item, f"{owner}[{index}]")
        for index, item in enumerate(value)
    ]
    if len(result) != len(set(result)):
        raise ReleaseError(f"{owner} contains a duplicate")
    return result


def _pattern_string_list(value: object, owner: str) -> list[str]:
    if not isinstance(value, list) or not value:
        raise ReleaseError(f"{owner} must contain at least 1 string")
    if any(not isinstance(item, str) or not item or "\x00" in item for item in value):
        raise ReleaseError(f"{owner} must contain non-empty strings without NUL")
    if len(value) != len(set(value)):
        raise ReleaseError(f"{owner} contains a duplicate")
    return value


def _normalized_reference(value: object, owner: str) -> str:
    reference = _required_string(value, owner)
    try:
        _validate_release_path(reference)
    except ReleaseError as error:
        raise ReleaseError(f"{owner} must be a normalized release-relative path") from error
    return reference


def _released_path_for_historical_source(path: str) -> str:
    """Resolve a pre-extraction engine coordinate to its released owner.

    Exact-byte-sealed World records and the imported v1 vocabulary retain the
    source coordinates that existed when they were minted. Moving the engine
    cannot rewrite those records, so verification resolves only their archive
    lookup while preserving the recorded path for claim derivation.
    """
    if (
        path.startswith("src/")
        and not path.startswith("src/bin/")
        and path not in {"src/lib.rs", "src/main.rs"}
    ):
        return f"{OSTADIX_API_ROOT}/{path}"
    return path


def _validate_mcp_release_surface(
    files: dict[str, bytes], modes: dict[str, str]
) -> None:
    config_path = ".mcp.json"
    config = _strict_json(files[config_path], config_path)
    if not isinstance(config, dict) or set(config) != {"mcpServers"}:
        raise ReleaseError(".mcp.json must contain exactly the mcpServers object")
    servers = config["mcpServers"]
    if not isinstance(servers, dict) or set(servers) != {"ostadix"}:
        raise ReleaseError(".mcp.json mcpServers must contain exactly ostadix")
    server = servers["ostadix"]
    if not isinstance(server, dict):
        raise ReleaseError(".mcp.json must define the ostadix server")
    if set(server) != {"command", "args"}:
        raise ReleaseError(".mcp.json ostadix server must contain command and args only")
    if server["command"] != "ostadix-mcp":
        raise ReleaseError(".mcp.json ostadix command must be 'ostadix-mcp'")
    if _required_string_list(server["args"], ".mcp.json ostadix.args"):
        raise ReleaseError(".mcp.json ostadix.args must be empty")

    cargo_path = "mcp/ostadix_lang_mcp_server/Cargo.toml"
    cargo = _strict_toml(files[cargo_path], cargo_path)
    package = cargo.get("package")
    if not isinstance(package, dict):
        raise ReleaseError(f"{cargo_path} must contain a package table")
    if package.get("name") != "ostadix-mcp-server":
        raise ReleaseError(f"{cargo_path} package name must be 'ostadix-mcp-server'")
    root_cargo = _strict_toml(files["Cargo.toml"], "Cargo.toml")
    root_package = root_cargo.get("package")
    if not isinstance(root_package, dict) or package.get("version") != root_package.get(
        "version"
    ):
        raise ReleaseError(
            f"{cargo_path} package version must match Cargo.toml package.version"
        )
    if package.get("license") != "LGPL-2.1-only":
        raise ReleaseError(f"{cargo_path} license must be 'LGPL-2.1-only'")
    if package.get("publish") is not False:
        raise ReleaseError(f"{cargo_path} package must remain publish = false")

    lock_path = "mcp/ostadix_lang_mcp_server/Cargo.lock"
    lock = _strict_toml(files[lock_path], lock_path)
    lock_packages = lock.get("package")
    if not isinstance(lock_packages, list):
        raise ReleaseError(f"{lock_path} must contain a package array")
    lock_roots = [
        item
        for item in lock_packages
        if isinstance(item, dict) and item.get("name") == "ostadix-mcp-server"
    ]
    if len(lock_roots) != 1:
        raise ReleaseError(
            f"{lock_path} must contain exactly one ostadix-mcp-server package"
        )
    if lock_roots[0].get("version") != package.get("version"):
        raise ReleaseError(
            f"{lock_path} ostadix-mcp-server version must match {cargo_path} package.version"
        )
    if "source" in lock_roots[0]:
        raise ReleaseError(f"{lock_path} ostadix-mcp-server must remain a local package")

    binaries = cargo.get("bin")
    if not isinstance(binaries, list):
        raise ReleaseError(f"{cargo_path} must contain an ostadix-mcp bin target")
    matching = [
        binary
        for binary in binaries
        if isinstance(binary, dict) and binary.get("name") == "ostadix-mcp"
    ]
    if len(matching) != 1:
        raise ReleaseError(f"{cargo_path} must define exactly one ostadix-mcp bin target")
    binary_path = _normalized_reference(
        matching[0].get("path"), f"{cargo_path} ostadix-mcp.path"
    )
    referenced_binary = str(PurePosixPath(cargo_path).parent / binary_path)
    if referenced_binary not in files:
        raise ReleaseError(
            f"{cargo_path} references absent binary source {referenced_binary}"
        )
    if modes.get(referenced_binary) not in VALID_GIT_MODES:
        raise ReleaseError(f"{referenced_binary} has an invalid release mode")


def _validate_example_manifest(files: dict[str, bytes]) -> None:
    path = "examples/manifest.json"
    manifest = _strict_json(files[path], path)
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema_version",
        "examples",
    }:
        raise ReleaseError(f"{path} root keys differ from schema")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 1:
        raise ReleaseError(f"{path} schema_version must be 1")
    examples = manifest["examples"]
    if not isinstance(examples, list):
        raise ReleaseError(f"{path} examples must be a list")

    declared: list[str] = []
    required_entry_keys = {
        "path",
        "editions",
        "classification",
        "requirements",
        "expected",
    }
    allowed_entry_keys = required_entry_keys | {"timeout_seconds"}
    allowed_requirement_keys = {
        "backends",
        "programs",
        "guest_programs",
        "python_packages",
        "authorities",
        "opt_in",
        "files",
    }
    for index, entry in enumerate(examples):
        owner = f"{path} examples[{index}]"
        if not isinstance(entry, dict):
            raise ReleaseError(f"{owner} must be an object")
        if not required_entry_keys <= set(entry) or set(entry) - allowed_entry_keys:
            raise ReleaseError(f"{owner} has missing or unknown fields")

        relative = _normalized_reference(entry["path"], f"{owner}.path")
        if "/" in relative and PurePosixPath(relative).parts[0] == "examples":
            raise ReleaseError(f"{owner}.path must be relative to examples/")
        if not relative.endswith(".O"):
            raise ReleaseError(f"{owner}.path must name a .O source")
        declared.append(relative)
        source_path = f"examples/{relative}"
        if source_path not in files:
            raise ReleaseError(f"{owner}.path references absent {source_path}")

        editions = _required_string_list(
            entry["editions"], f"{owner}.editions", minimum=1
        )
        if not set(editions) <= EXAMPLE_EDITIONS:
            raise ReleaseError(f"{owner}.editions contains an unknown edition")
        if entry["classification"] not in EXAMPLE_CLASSIFICATIONS:
            raise ReleaseError(f"{owner}.classification is invalid")

        requirements = entry["requirements"]
        if not isinstance(requirements, dict):
            raise ReleaseError(f"{owner}.requirements must be an object")
        if set(requirements) - allowed_requirement_keys or not {
            "backends",
            "programs",
            "authorities",
        } <= set(requirements):
            raise ReleaseError(f"{owner}.requirements has missing or unknown fields")
        for field, value in requirements.items():
            values = _required_string_list(value, f"{owner}.requirements.{field}")
            if field == "files":
                for reference in values:
                    normalized = _normalized_reference(
                        reference, f"{owner}.requirements.files"
                    )
                    if normalized not in files:
                        raise ReleaseError(
                            f"{owner}.requirements.files references absent {normalized}"
                        )

        expected = entry["expected"]
        if not isinstance(expected, dict) or set(expected) != set(editions):
            raise ReleaseError(f"{owner}.expected keys must exactly match editions")
        for edition, expectation in expected.items():
            expectation_owner = f"{owner}.expected.{edition}"
            if not isinstance(expectation, dict) or set(expectation) - {
                "result",
                "patterns",
                "modes",
            }:
                raise ReleaseError(f"{expectation_owner} has an invalid structure")
            patterns = expectation.get("patterns")
            if patterns is not None:
                _pattern_string_list(patterns, f"{expectation_owner}.patterns")
            if "result" not in expectation and patterns is None:
                raise ReleaseError(f"{expectation_owner} needs result or patterns")
            modes = _required_string_list(
                expectation.get("modes", ["interpreter"]),
                f"{expectation_owner}.modes",
                minimum=1,
            )
            if not set(modes) <= EXAMPLE_MODES:
                raise ReleaseError(f"{expectation_owner}.modes contains an unknown mode")
            if edition != "c17" and "aot" in modes:
                raise ReleaseError(f"{expectation_owner}: only c17 supports aot mode")

        timeout = entry.get("timeout_seconds", 10)
        if type(timeout) is not int or timeout <= 0:
            raise ReleaseError(f"{owner}.timeout_seconds must be a positive integer")

    if declared != sorted(declared) or len(declared) != len(set(declared)):
        raise ReleaseError(f"{path} paths must be unique and sorted")
    actual = sorted(
        member[len("examples/") :]
        for member in files
        if member.startswith("examples/") and member.endswith(".O")
    )
    if declared != actual:
        raise ReleaseError(
            f"{path} coverage differs from release examples; "
            f"missing={sorted(set(actual) - set(declared))}, "
            f"extra={sorted(set(declared) - set(actual))}"
        )


def _validate_evidence_manifest(
    files: dict[str, bytes], modes: dict[str, str]
) -> None:
    path = "evidence/gates.toml"
    manifest = _strict_toml(files[path], path)
    expected_root_keys = {
        "schema_version",
        "required_gate_count",
        "supplemental_gate_count",
        "portable_command",
        "gate",
    }
    if set(manifest) != expected_root_keys:
        raise ReleaseError(f"{path} root keys differ from schema")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 2:
        raise ReleaseError(f"{path} schema_version must be 2")
    if type(manifest["required_gate_count"]) is not int or (
        manifest["required_gate_count"] != EXPECTED_REQUIRED_EVIDENCE_GATES
    ):
        raise ReleaseError(
            f"{path} required_gate_count must be {EXPECTED_REQUIRED_EVIDENCE_GATES}"
        )
    if type(manifest["supplemental_gate_count"]) is not int or (
        manifest["supplemental_gate_count"] != EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES
    ):
        raise ReleaseError(
            f"{path} supplemental_gate_count must be "
            f"{EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES}"
        )
    if manifest["portable_command"] != "./boot-and-test.sh smoke":
        raise ReleaseError(f"{path} portable_command must be './boot-and-test.sh smoke'")
    if modes.get("boot-and-test.sh") != "100755":
        raise ReleaseError(f"{path} portable command must reference executable boot-and-test.sh")

    gates = manifest["gate"]
    if not isinstance(gates, list):
        raise ReleaseError(f"{path} gate must be a list of tables")
    expected_gate_count = (
        EXPECTED_REQUIRED_EVIDENCE_GATES + EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES
    )
    if len(gates) != expected_gate_count:
        raise ReleaseError(f"{path} must contain exactly {expected_gate_count} gate tables")
    expected_gate_keys = {
        "id",
        "required",
        "milestone",
        "script",
        "evidence_class",
        "required_tools",
        "positive_claims",
        "nonclaims",
        "expected_markers",
    }
    identifiers: set[str] = set()
    scripts: set[str] = set()
    required_count = 0
    aarch64_gates: list[dict[str, Any]] = []
    for index, gate in enumerate(gates):
        owner = f"{path} gate[{index}]"
        if not isinstance(gate, dict) or set(gate) != expected_gate_keys:
            raise ReleaseError(f"{owner} keys differ from schema")
        identifier = _required_string(gate["id"], f"{owner}.id")
        if identifier in identifiers:
            raise ReleaseError(f"{owner}.id is duplicated")
        identifiers.add(identifier)
        required = gate["required"]
        if not isinstance(required, bool):
            raise ReleaseError(f"{owner}.required must be a boolean")
        required_count += int(required)
        _required_string(gate["milestone"], f"{owner}.milestone")
        evidence_class = _required_string(
            gate["evidence_class"], f"{owner}.evidence_class"
        )
        if evidence_class not in EVIDENCE_CLASSES:
            raise ReleaseError(f"{owner}.evidence_class is invalid")
        if required and evidence_class == "hardware_kvm":
            raise ReleaseError(
                f"{owner}: required gates must be portable QEMU evidence"
            )
        if not required and evidence_class != "hardware_kvm":
            raise ReleaseError(f"{owner}: the supplemental gate must be hardware_kvm")
        script = _normalized_reference(gate["script"], f"{owner}.script")
        script_path = PurePosixPath(script)
        if script_path.parent != PurePosixPath("ocore/kernel") or script_path.suffix != ".sh":
            raise ReleaseError(f"{owner}.script must name an ocore/kernel shell gate")
        if script in scripts:
            raise ReleaseError(f"{owner}.script is duplicated")
        scripts.add(script)
        if script not in files:
            raise ReleaseError(f"{owner}.script references absent {script}")
        if modes.get(script) != "100755":
            raise ReleaseError(f"{owner}.script references non-executable {script}")
        required_tools = _required_string_list(
            gate["required_tools"], f"{owner}.required_tools", minimum=1
        )
        missing_tools = (
            EVIDENCE_COMMON_REQUIRED_TOOLS
            | EVIDENCE_CLASS_REQUIRED_TOOLS[evidence_class]
        ) - set(required_tools)
        if missing_tools:
            raise ReleaseError(
                f"{owner}.required_tools is missing class requirements "
                f"{sorted(missing_tools)}"
            )
        positive_claims = _required_string_list(
            gate["positive_claims"], f"{owner}.positive_claims", minimum=1
        )
        nonclaims = _required_string_list(
            gate["nonclaims"], f"{owner}.nonclaims", minimum=1
        )
        expected_markers = _required_string_list(
            gate["expected_markers"], f"{owner}.expected_markers", minimum=2
        )
        if evidence_class == "qemu_tcg_aarch64":
            aarch64_gates.append(
                {
                    "id": identifier,
                    "script": script,
                    "positive_claims": positive_claims,
                    "nonclaims": nonclaims,
                    "expected_markers": expected_markers,
                }
            )

    supplemental_count = len(gates) - required_count
    if required_count != EXPECTED_REQUIRED_EVIDENCE_GATES:
        raise ReleaseError(
            f"{path} must contain exactly {EXPECTED_REQUIRED_EVIDENCE_GATES} "
            "required gate tables"
        )
    if supplemental_count != EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES:
        raise ReleaseError(
            f"{path} must contain exactly {EXPECTED_SUPPLEMENTAL_EVIDENCE_GATES} "
            "supplemental gate table"
        )
    if required_count != manifest["required_gate_count"]:
        raise ReleaseError(f"{path} required_gate_count does not match gate tables")
    if supplemental_count != manifest["supplemental_gate_count"]:
        raise ReleaseError(f"{path} supplemental_gate_count does not match gate tables")
    if len(aarch64_gates) != 1:
        raise ReleaseError(
            f"{path} must contain exactly one qemu_tcg_aarch64 gate"
        )
    g2 = aarch64_gates[0]
    if g2["id"] != G2_AARCH64_GATE_ID or g2["script"] != G2_AARCH64_SCRIPT:
        raise ReleaseError(
            f"{path} qemu_tcg_aarch64 evidence must be {G2_AARCH64_GATE_ID} "
            f"at {G2_AARCH64_SCRIPT}"
        )
    if tuple(g2["positive_claims"]) != G2_AARCH64_POSITIVE_CLAIMS:
        raise ReleaseError(f"{path} G2 AArch64 positive claims exceed the sealed boundary")
    if tuple(g2["nonclaims"]) != G2_AARCH64_NONCLAIMS:
        raise ReleaseError(f"{path} G2 AArch64 nonclaims differ from the sealed boundary")
    if tuple(g2["expected_markers"]) != G2_AARCH64_EXPECTED_MARKERS:
        raise ReleaseError(f"{path} G2 AArch64 runtime markers differ from the sealed contract")
    missing_g2_tools = G2_AARCH64_REQUIRED_TOOLS - set(
        next(
            gate["required_tools"]
            for gate in gates
            if gate["id"] == G2_AARCH64_GATE_ID
        )
    )
    if missing_g2_tools:
        raise ReleaseError(
            f"{path} G2 AArch64 required_tools is missing {sorted(missing_g2_tools)}"
        )


def _sealed_world_alpha_text(
    files: dict[str, bytes], modes: dict[str, str], path: str
) -> str:
    if modes.get(path) != "100644":
        raise ReleaseError(f"{path} must use release mode 100644")
    expected = SEALED_WORLD_ALPHA_SHA256[path]
    actual = hashlib.sha256(files[path]).hexdigest()
    if actual != expected:
        raise ReleaseError(
            f"{path} SHA-256 differs from sealed OSTADIX Alpha constitutional bytes; "
            f"expected {expected}, got {actual}"
        )
    try:
        return files[path].decode("utf-8", "strict")
    except UnicodeDecodeError as error:  # The seal makes this corruption-only.
        raise ReleaseError(f"{path} is not valid UTF-8") from error


def _release_world_observations(transcript: str, location: str) -> list[dict[str, str]]:
    observations: list[dict[str, str]] = []
    for line_number, line in enumerate(transcript.splitlines(), 1):
        if not line.startswith("@evidence "):
            continue
        payload = line[len("@evidence ") :]
        tokens = payload.split(" ")
        if not payload or any(not token for token in tokens):
            raise ReleaseError(
                f"{location}:{line_number} contains noncanonical evidence spacing"
            )
        fields: dict[str, str] = {}
        for token in tokens:
            if "=" not in token:
                raise ReleaseError(f"{location}:{line_number} evidence token lacks '='")
            key, value = token.split("=", 1)
            if (
                re.fullmatch(r"[a-z][a-z0-9_]*", key) is None
                or not value
                or any(not 0x21 <= ord(character) <= 0x7E for character in value)
                or key in fields
            ):
                raise ReleaseError(
                    f"{location}:{line_number} has an invalid/duplicate evidence field"
                )
            fields[key] = value
        if "event" not in fields:
            raise ReleaseError(f"{location}:{line_number} evidence observation lacks event")
        observations.append(fields)
    return observations


def _derive_release_world_claims(
    transcript: str,
    location: str,
    evidence_class: str,
    topology: dict[str, object],
    source_paths: set[str],
    artifact_names: set[str],
    artifact_bindings: set[tuple[str, str, str]],
) -> set[str]:
    observations = _release_world_observations(transcript, location)

    def observed(event: str, **fields: str) -> bool:
        return any(
            observation.get("event") == event
            and all(observation.get(key) == value for key, value in fields.items())
            for observation in observations
        )

    claims: set[str] = set()
    repository = (
        evidence_class == "repository_conformance"
        and topology.get("kind") == "repository"
        and topology.get("acceleration") == "none"
    )
    v1_artifact = (
        "world-contract-v1",
        "executable-constitutional-schema",
        "evidence/world_contract_v1.toml",
    )
    v2_artifact = (
        "world-contract-v2",
        "executable-constitutional-schema",
        "evidence/world_contract_v2.toml",
    )
    machine_artifact = (
        "o-machine-contract-v1",
        "executable-machine-contract-schema",
        "evidence/o_machine_contract_v1.toml",
    )
    g0_rules = {
        "world.contract_schema_consistent": (
            "g0_contract_schema",
            {v1_artifact, v2_artifact},
            {
                "evidence/world_contract_v1.toml",
                "evidence/world_contract_v2.toml",
                "evidence/world_alpha_gates.toml",
            },
        ),
        "world.machine_contract_consistent": (
            "g0_machine_contract",
            {machine_artifact},
            {
                "docs/O_MACHINE_CONTRACT.md",
                "evidence/o_machine_contract_v1.toml",
                "evidence/world_contract_v2.toml",
            },
        ),
        "world.crossing_taxonomy_consistent": (
            "g0_crossing_taxonomy",
            {v1_artifact},
            set(),
        ),
        "world.identity_vocabulary_consistent": (
            "g0_identity_vocabulary",
            {v1_artifact},
            set(),
        ),
        "world.failure_consistency_schema_consistent": (
            "g0_failure_consistency",
            {v1_artifact},
            set(),
        ),
        "evidence.claim_class_guarded": (
            "g0_claim_class_guard",
            set(),
            {"evidence/world_alpha_gates.toml", "scripts/world_alpha_evidence.py"},
        ),
    }
    if repository:
        for claim, (event, required_artifacts, required_sources) in g0_rules.items():
            if (
                observed(event, result="pass")
                and required_artifacts <= artifact_bindings
                and required_sources <= source_paths
            ):
                claims.add(claim)

    g2_context = (
        evidence_class == "qemu_tcg_aarch64"
        and topology.get("kind") == "virtual"
        and topology.get("architecture") == "aarch64"
        and topology.get("acceleration") == "tcg"
        and topology.get("cpu_count") == 1
        and {"g2-kernel-object", "g2-kernel-elf"} <= artifact_names
    )
    if g2_context:
        simple = {
            "aarch64.el2_resident": "el2_resident",
            "aarch64.el1_execution": "el1_execution",
            "aarch64.svc_eret_roundtrip": "svc_eret_roundtrip",
            "ipc.request_reply": "ipc_request_reply",
            "capability.attenuation": "capability_attenuation",
            "lifecycle.terminal": "lifecycle_terminal",
            "lifecycle.reclamation": "reclamation",
        }
        for claim, event in simple.items():
            if observed(event, result="pass"):
                claims.add(claim)
        if observed("aarch64_native_object", format="elf64", machine="183", result="pass"):
            claims.add("aarch64.native_object")
        if observed(
            "el2_hvc_roundtrip",
            domain="0x4f4d",
            registers="preserved",
            stack="preserved",
            result="pass",
        ):
            claims.add("aarch64.hvc_roundtrip")
        if observed("el0_execution", principals="2", result="pass"):
            claims.add("aarch64.el0_execution")
        if observed(
            "stale_generation_rejected", kinds="process,capability", result="pass"
        ):
            claims.add("capability.stale_generation_rejected")
        lifecycle = observed("lifecycle_terminal", result="pass")
        progress = observed("counter_progress", phase="post_lifecycle", result="pass")
        bounded = observed(
            "counter_progress",
            phase="post_lifecycle",
            poll_bound="1000000",
            result="pass",
        )
        if lifecycle and progress:
            claims.add("execution.post_lifecycle_reached")
        if lifecycle and bounded:
            claims.add("counter.progress_after_lifecycle")
    return claims


def _validate_world_attestation_release_surface(
    files: dict[str, bytes], modes: dict[str, str], path: str, gate_id: str,
    evidence_class: str,
) -> dict[str, object]:
    if path not in files or modes.get(path) != "100644":
        raise ReleaseError(f"{path} must be a regular non-executable release file")
    attestation = _strict_toml(files[path], path)
    common_keys = {
        "schema_version",
        "id",
        "gate",
        "evidence_class",
        "source_commit",
        "source_state",
        "command",
        "transcript",
        "transcript_sha256",
        "topology",
        "nonclaims",
        "expected_markers",
        "source",
        "artifact",
        "signatures",
    }
    schema_version = attestation.get("schema_version")
    if type(schema_version) is not int or schema_version not in {1, 2, 3}:
        raise ReleaseError(f"{path} schema_version must be 1, 2, or 3")
    if schema_version <= 2:
        expected_historical_digest = WORLD_HISTORICAL_ATTESTATION_SHA256.get(path)
        if expected_historical_digest is None:
            raise ReleaseError(
                f"{path} historical attestation lacks a trusted exact-byte seal"
            )
        if hashlib.sha256(files[path]).hexdigest() != expected_historical_digest:
            raise ReleaseError(f"{path} historical attestation bytes differ from seal")
    current_attestation = path in WORLD_CURRENT_ATTESTATION_SHA256
    if schema_version == 1:
        version_keys = {"claims"}
    else:
        version_keys = {
            "derived_claims",
            "validator_sha256",
            "claim_rule_policy_sha256",
            "registry_semantics_sha256",
        }
        if schema_version == 3:
            version_keys.add("derivation_hash")
    if set(attestation) != common_keys | version_keys:
        raise ReleaseError(f"{path} keys differ from the World attestation schema")
    _required_string(attestation["id"], f"{path}.id")
    if attestation["gate"] != gate_id:
        raise ReleaseError(f"{path}.gate must be {gate_id}")
    if attestation["evidence_class"] != evidence_class:
        raise ReleaseError(f"{path}.evidence_class must be {evidence_class}")
    commit = _required_string(attestation["source_commit"], f"{path}.source_commit")
    if HEX_COMMIT.fullmatch(commit) is None:
        raise ReleaseError(f"{path}.source_commit must be a Git object ID")
    if attestation["source_state"] != "content-addressed-working-tree":
        raise ReleaseError(f"{path}.source_state is invalid")

    command = _required_string_list(attestation["command"], f"{path}.command", minimum=1)
    if len(command) != 1 or not command[0].startswith("./"):
        raise ReleaseError(f"{path}.command must name one repository executable")
    command_path = _normalized_reference(command[0][2:], f"{path}.command[0]")
    if command_path not in files or modes.get(command_path) != "100755":
        raise ReleaseError(f"{path}.command references absent/non-executable {command_path}")

    transcript_path = _normalized_reference(
        attestation["transcript"], f"{path}.transcript"
    )
    if transcript_path not in files or modes.get(transcript_path) != "100644":
        raise ReleaseError(f"{path}.transcript references absent {transcript_path}")
    transcript_digest = _required_string(
        attestation["transcript_sha256"], f"{path}.transcript_sha256"
    )
    if HEX_DIGEST.fullmatch(transcript_digest) is None:
        raise ReleaseError(f"{path}.transcript_sha256 must be a SHA-256 digest")
    if hashlib.sha256(files[transcript_path]).hexdigest() != transcript_digest:
        raise ReleaseError(f"{path}.transcript digest does not match")
    try:
        transcript = files[transcript_path].decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError(f"{transcript_path} must be UTF-8") from error
    transcript_lines = transcript.splitlines()
    for line in (
        "WORLD_ALPHA_ATTESTATION_TRANSCRIPT_V1",
        f"gate={gate_id}",
        f"evidence_class={evidence_class}",
        f"source_commit={commit}",
        f"command={command[0]}",
    ):
        if transcript_lines.count(line) != 1:
            raise ReleaseError(f"{transcript_path} must contain exactly one {line!r}")
    markers = _required_string_list(
        attestation["expected_markers"], f"{path}.expected_markers", minimum=1
    )
    positions: list[int] = []
    for marker in markers:
        if transcript_lines.count(marker) != 1:
            raise ReleaseError(
                f"{transcript_path} must contain exactly one marker {marker!r}"
            )
        positions.append(transcript_lines.index(marker))
    if positions != sorted(positions):
        raise ReleaseError(f"{transcript_path} markers are not in causal order")

    sources = attestation["source"]
    if not isinstance(sources, list) or not sources:
        raise ReleaseError(f"{path}.source must contain content digests")
    seen_sources: set[str] = set()
    source_digests: dict[str, str] = {}
    for index, source in enumerate(sources):
        owner = f"{path}.source[{index}]"
        if not isinstance(source, dict) or set(source) != {"path", "sha256"}:
            raise ReleaseError(f"{owner} keys differ from schema")
        source_path = _normalized_reference(source["path"], f"{owner}.path")
        if source_path in seen_sources:
            raise ReleaseError(f"{path}.source contains duplicate {source_path}")
        seen_sources.add(source_path)
        released_source_path = _released_path_for_historical_source(source_path)
        digest = _required_string(source["sha256"], f"{owner}.sha256")
        if HEX_DIGEST.fullmatch(digest) is None:
            raise ReleaseError(f"{owner}.sha256 must be a SHA-256 digest")
        if released_source_path not in files:
            raise ReleaseError(
                f"{owner} references absent released {released_source_path}"
            )
        source_digests[source_path] = digest
        # Historical records describe immutable prior working trees; their
        # source bytes are not falsely claimed to be the current ZIP members.
        # Only the exact-byte-sealed current attestation must bind released
        # members directly, regardless of the historical schema version.
        if current_attestation:
            if hashlib.sha256(files[released_source_path]).hexdigest() != digest:
                raise ReleaseError(
                    f"{owner} does not match released {released_source_path}"
                )

    artifacts = attestation["artifact"]
    if not isinstance(artifacts, list) or not artifacts:
        raise ReleaseError(f"{path}.artifact must contain artifact digests")
    seen_artifacts: set[str] = set()
    artifact_bindings: set[tuple[str, str, str]] = set()
    for index, artifact in enumerate(artifacts):
        owner = f"{path}.artifact[{index}]"
        if not isinstance(artifact, dict) or set(artifact) != {
            "name", "kind", "sha256", "retained", "path"
        }:
            raise ReleaseError(f"{owner} keys differ from schema")
        name = _required_string(artifact["name"], f"{owner}.name")
        if name in seen_artifacts:
            raise ReleaseError(f"{path}.artifact contains duplicate {name}")
        seen_artifacts.add(name)
        kind = _required_string(artifact["kind"], f"{owner}.kind")
        digest = _required_string(artifact["sha256"], f"{owner}.sha256")
        if HEX_DIGEST.fullmatch(digest) is None:
            raise ReleaseError(f"{owner}.sha256 must be a SHA-256 digest")
        if type(artifact["retained"]) is not bool:
            raise ReleaseError(f"{owner}.retained must be boolean")
        if artifact["retained"]:
            artifact_path = _normalized_reference(artifact["path"], f"{owner}.path")
            artifact_bindings.add((name, kind, artifact_path))
            if artifact_path not in files:
                raise ReleaseError(f"{owner} references absent retained {artifact_path}")
            current_digest = hashlib.sha256(files[artifact_path]).hexdigest()
            historical_digest = source_digests.get(artifact_path)
            if current_digest != digest and (
                current_attestation or historical_digest != digest
            ):
                raise ReleaseError(f"{owner} does not match retained {artifact_path}")
        else:
            if artifact["path"] != "":
                raise ReleaseError(f"{owner}.path must be empty when not retained")
            artifact_bindings.add((name, kind, ""))
            if transcript_lines.count(f"artifact:{name}:sha256={digest}") != 1:
                raise ReleaseError(f"{transcript_path} does not bind artifact {name}")

    topology = attestation["topology"]
    if not isinstance(topology, dict) or set(topology) != {
        "kind", "architecture", "machine", "acceleration", "cpu_count", "inventory"
    }:
        raise ReleaseError(f"{path}.topology keys differ from schema")
    for field in ("kind", "architecture", "machine", "acceleration"):
        _required_string(topology[field], f"{path}.topology.{field}")
    if type(topology["cpu_count"]) is not int or topology["cpu_count"] < 0:
        raise ReleaseError(f"{path}.topology.cpu_count must be nonnegative")
    _required_string_list(topology["inventory"], f"{path}.topology.inventory", minimum=1)
    current_derived_claims = _derive_release_world_claims(
        transcript,
        transcript_path,
        evidence_class,
        topology,
        seen_sources,
        seen_artifacts,
        artifact_bindings,
    )
    recorded_claims: set[str] = set()
    derivation_hash: str | None = None
    if schema_version == 1:
        _required_string_list(attestation["claims"], f"{path}.claims", minimum=1)
    else:
        derived_claims = _required_string_list(
            attestation["derived_claims"], f"{path}.derived_claims", minimum=1
        )
        if derived_claims != sorted(set(derived_claims)):
            raise ReleaseError(f"{path}.derived_claims must be sorted and unique")
        recorded_claims = set(derived_claims)
        validator_digest = _required_string(
            attestation["validator_sha256"], f"{path}.validator_sha256"
        )
        if HEX_DIGEST.fullmatch(validator_digest) is None:
            raise ReleaseError(f"{path}.validator_sha256 must be a SHA-256 digest")
        validator_sources = [
            source["sha256"]
            for source in sources
            if source["path"] == "scripts/world_alpha_evidence.py"
        ]
        if validator_sources and validator_sources != [validator_digest]:
            raise ReleaseError(
                f"{path}.validator_sha256 must bind its historical validator source"
            )
        if not validator_sources and schema_version == 3:
            raise ReleaseError(
                f"{path} current schema-v3 evidence must retain its validator source"
            )
        for field in ("claim_rule_policy_sha256", "registry_semantics_sha256"):
            if HEX_DIGEST.fullmatch(str(attestation[field])) is None:
                raise ReleaseError(f"{path}.{field} must be a SHA-256 digest")
        if schema_version == 3:
            derivation_hash = _required_string(
                attestation["derivation_hash"], f"{path}.derivation_hash"
            )
            if not derivation_hash.startswith("sha256:") or HEX_DIGEST.fullmatch(
                derivation_hash[len("sha256:") :]
            ) is None:
                raise ReleaseError(f"{path}.derivation_hash must be a SHA-256 identifier")
            if current_attestation:
                if validator_digest != WORLD_VALIDATOR_SHA256:
                    raise ReleaseError(
                        f"{path}.validator_sha256 differs from the trusted validator bytes"
                    )
                if recorded_claims != current_derived_claims:
                    raise ReleaseError(
                        f"{path}.derived_claims differs from trusted current rules"
                    )
                if attestation["claim_rule_policy_sha256"] != WORLD_CLAIM_POLICY_SHA256:
                    raise ReleaseError(
                        f"{path}.claim_rule_policy_sha256 differs from trusted rules"
                    )
                if attestation["registry_semantics_sha256"] != WORLD_REGISTRY_SEMANTICS_SHA256:
                    raise ReleaseError(
                        f"{path}.registry_semantics_sha256 differs from trusted registry"
                    )
        else:
            derivation_hash = "sha256:" + validator_digest
    nonclaims = _required_string_list(
        attestation["nonclaims"], f"{path}.nonclaims", minimum=1
    )
    if evidence_class == "qemu_tcg_aarch64":
        if (
            topology["kind"] != "virtual"
            or topology["architecture"] != "aarch64"
            or topology["acceleration"] != "tcg"
            or topology["cpu_count"] != 1
            or "virt" not in topology["machine"]
        ):
            raise ReleaseError(f"{path} does not bind the one-vCPU AArch64 TCG virt topology")
        boundary = " ".join(nonclaims)
        for fragment in ("physical AArch64", "KVM/SVM", "Linux or Plan 9", "PCI/DMA/IOMMU"):
            if fragment not in boundary:
                raise ReleaseError(f"{path}.nonclaims is missing {fragment!r}")
    if attestation["signatures"] != []:
        raise ReleaseError(f"{path}.signatures must be empty for this evidence class")
    return {
        "id": attestation["id"],
        "path": path,
        "gate": gate_id,
        "schema_version": schema_version,
        "recorded_claims": recorded_claims,
        "current_derived_claims": current_derived_claims,
        "derivation_hash": derivation_hash,
        "record_sha256": hashlib.sha256(files[path]).hexdigest(),
    }


def _validate_world_evidence_event_release_surface(
    files: dict[str, bytes], modes: dict[str, str], path: str,
) -> dict[str, object]:
    if path not in files or modes.get(path) != "100644":
        raise ReleaseError(f"{path} must be a regular non-executable release file")
    event = _strict_toml(files[path], path)
    if set(event) != {
        "schema_version",
        "id",
        "event",
        "subject",
        "replacement",
        "reason_code",
        "reason",
        "source_commit",
        "signatures",
    }:
        raise ReleaseError(f"{path} keys differ from evidence-event schema")
    if type(event["schema_version"]) is not int or event["schema_version"] != 1:
        raise ReleaseError(f"{path} schema_version must be 1")
    event_id = _required_string(event["id"], f"{path}.id")
    subject = _required_string(event["subject"], f"{path}.subject")
    for field, value in (("id", event_id), ("subject", subject)):
        if WORLD_LEDGER_ID.fullmatch(value) is None:
            raise ReleaseError(f"{path}.{field} is not normalized")
    kind = event["event"]
    if kind not in {"supersede", "retract"}:
        raise ReleaseError(f"{path}.event must be supersede or retract")
    replacement = event["replacement"]
    if not isinstance(replacement, str):
        raise ReleaseError(f"{path}.replacement must be a string")
    if kind == "supersede":
        replacement = _required_string(replacement, f"{path}.replacement")
        if WORLD_LEDGER_ID.fullmatch(replacement) is None:
            raise ReleaseError(f"{path}.replacement is not normalized")
        if replacement == subject:
            raise ReleaseError(f"{path} cannot supersede an attestation with itself")
    elif replacement != "":
        raise ReleaseError(f"{path}.replacement must be empty for retraction")
    _required_string(event["reason_code"], f"{path}.reason_code")
    _required_string(event["reason"], f"{path}.reason")
    commit = _required_string(event["source_commit"], f"{path}.source_commit")
    if HEX_COMMIT.fullmatch(commit) is None:
        raise ReleaseError(f"{path}.source_commit must be a Git object ID")
    if event["signatures"] != []:
        raise ReleaseError(f"{path}.signatures must be empty")
    return {
        "id": event_id,
        "path": path,
        "event": kind,
        "subject": subject,
        "replacement": replacement,
        "record_sha256": hashlib.sha256(files[path]).hexdigest(),
    }


def _world_rederive_payload_sha256(event: dict[str, object]) -> str:
    payload_keys = (
        "schema_version",
        "id",
        "event",
        "subject",
        "prior_derivation",
        "current_derivation",
        "claims_lost",
        "claims_gained",
        "reason_code",
        "reason",
        "source_commit",
    )
    payload = {key: event[key] for key in payload_keys}
    encoded = json.dumps(
        payload, ensure_ascii=True, sort_keys=True, separators=(",", ":")
    ).encode("ascii")
    return hashlib.sha256(
        WORLD_REDERIVE_PAYLOAD_DOMAIN.encode("ascii") + b"\0" + encoded
    ).hexdigest()


def _validate_world_rederive_event_release_surface(
    files: dict[str, bytes],
    modes: dict[str, str],
    path: str,
) -> dict[str, object]:
    if path not in files or modes.get(path) != "100644":
        raise ReleaseError(f"{path} must be a regular non-executable release file")
    event = _strict_toml(files[path], path)
    if set(event) != {
        "schema_version",
        "id",
        "event",
        "subject",
        "prior_derivation",
        "current_derivation",
        "claims_lost",
        "claims_gained",
        "reason_code",
        "reason",
        "source_commit",
        "payload_sha256",
        "signatures",
    }:
        raise ReleaseError(f"{path} keys differ from rederive-event schema")
    if type(event["schema_version"]) is not int or event["schema_version"] != 1:
        raise ReleaseError(f"{path} schema_version must be 1")
    event_id = _required_string(event["id"], f"{path}.id")
    subject = _required_string(event["subject"], f"{path}.subject")
    for field, value in (("id", event_id), ("subject", subject)):
        if WORLD_LEDGER_ID.fullmatch(value) is None:
            raise ReleaseError(f"{path}.{field} is not normalized")
    if event["event"] != "rederive":
        raise ReleaseError(f"{path}.event must be rederive")
    prior_derivation = _required_string(
        event["prior_derivation"], f"{path}.prior_derivation"
    )
    current_derivation = _required_string(
        event["current_derivation"], f"{path}.current_derivation"
    )
    for field, value in (
        ("prior_derivation", prior_derivation),
        ("current_derivation", current_derivation),
    ):
        if not value.startswith("sha256:") or HEX_DIGEST.fullmatch(
            value[len("sha256:") :]
        ) is None:
            raise ReleaseError(f"{path}.{field} must be a SHA-256 identifier")
    if prior_derivation == current_derivation:
        raise ReleaseError(f"{path} must change the derivation identifier")
    lost = _required_string_list(event["claims_lost"], f"{path}.claims_lost")
    gained = _required_string_list(event["claims_gained"], f"{path}.claims_gained")
    if lost != sorted(set(lost)) or gained != sorted(set(gained)):
        raise ReleaseError(f"{path} claim deltas must be sorted and unique")
    if set(lost) & set(gained):
        raise ReleaseError(f"{path} cannot both lose and gain one claim")
    _required_string(event["reason_code"], f"{path}.reason_code")
    _required_string(event["reason"], f"{path}.reason")
    commit = _required_string(event["source_commit"], f"{path}.source_commit")
    if HEX_COMMIT.fullmatch(commit) is None:
        raise ReleaseError(f"{path}.source_commit must be a Git object ID")
    digest = _required_string(event["payload_sha256"], f"{path}.payload_sha256")
    if digest != _world_rederive_payload_sha256(event):
        raise ReleaseError(f"{path}.payload_sha256 does not bind the rederive event")
    if event["signatures"] != []:
        raise ReleaseError(
            f"{path}.signatures must remain empty; append a separate witness event"
        )
    return {
        "id": event_id,
        "path": path,
        "event": "rederive",
        "subject": subject,
        "prior_derivation": prior_derivation,
        "current_derivation": current_derivation,
        "claims_lost": set(lost),
        "claims_gained": set(gained),
        "payload_sha256": digest,
        "record_sha256": hashlib.sha256(files[path]).hexdigest(),
    }


def _world_witness_payload_sha256(event: dict[str, object]) -> str:
    payload_keys = (
        "schema_version",
        "id",
        "event",
        "subject",
        "subject_record_sha256",
        "algorithm",
        "key_id",
        "public_key",
        "run_identity",
        "source_commit",
        "verification",
    )
    payload = {key: event[key] for key in payload_keys}
    encoded = json.dumps(
        payload, ensure_ascii=True, sort_keys=True, separators=(",", ":")
    ).encode("ascii")
    return hashlib.sha256(
        WORLD_WITNESS_PAYLOAD_DOMAIN.encode("ascii") + b"\0" + encoded
    ).hexdigest()


def _validate_world_witness_event_release_surface(
    files: dict[str, bytes], modes: dict[str, str], path: str,
) -> dict[str, object]:
    if path not in files or modes.get(path) != "100644":
        raise ReleaseError(f"{path} must be a regular non-executable release file")
    event = _strict_toml(files[path], path)
    expected_keys = {
        "schema_version",
        "id",
        "event",
        "subject",
        "subject_record_sha256",
        "witness_payload_sha256",
        "algorithm",
        "key_id",
        "public_key",
        "signature",
        "run_identity",
        "source_commit",
        "verification",
    }
    if set(event) != expected_keys:
        raise ReleaseError(f"{path} keys differ from witness-event schema")
    if type(event["schema_version"]) is not int or event["schema_version"] != 1:
        raise ReleaseError(f"{path}.schema_version must be 1")
    event_id = _required_string(event["id"], f"{path}.id")
    subject = _required_string(event["subject"], f"{path}.subject")
    key_id = _required_string(event["key_id"], f"{path}.key_id")
    for field, value in (("id", event_id), ("subject", subject), ("key_id", key_id)):
        if WORLD_LEDGER_ID.fullmatch(value) is None:
            raise ReleaseError(f"{path}.{field} is not normalized")
    if event["event"] != "witness":
        raise ReleaseError(f"{path}.event must be witness")
    record_sha256 = _required_string(
        event["subject_record_sha256"], f"{path}.subject_record_sha256"
    )
    if HEX_DIGEST.fullmatch(record_sha256) is None:
        raise ReleaseError(f"{path}.subject_record_sha256 must be a SHA-256 digest")
    if event["algorithm"] != "ed25519":
        raise ReleaseError(f"{path}.algorithm must be ed25519")
    if re.fullmatch(r"[0-9a-f]{64}", str(event["public_key"])) is None:
        raise ReleaseError(f"{path}.public_key must be 32-byte lowercase hex")
    if event["public_key"] == "0" * 64:
        raise ReleaseError(f"{path}.public_key must not be all zero")
    if re.fullmatch(r"[0-9a-f]{128}", str(event["signature"])) is None:
        raise ReleaseError(f"{path}.signature must be 64-byte lowercase hex")
    if event["signature"] == "0" * 128:
        raise ReleaseError(f"{path}.signature must not be all zero")
    _required_string(event["run_identity"], f"{path}.run_identity")
    commit = _required_string(event["source_commit"], f"{path}.source_commit")
    if HEX_COMMIT.fullmatch(commit) is None:
        raise ReleaseError(f"{path}.source_commit must be a Git object ID")
    if event["verification"] != "external_unverified":
        raise ReleaseError(f"{path}.verification must state external_unverified")
    witness_payload_sha256 = _required_string(
        event["witness_payload_sha256"], f"{path}.witness_payload_sha256"
    )
    if (
        HEX_DIGEST.fullmatch(witness_payload_sha256) is None
        or witness_payload_sha256 != _world_witness_payload_sha256(event)
    ):
        raise ReleaseError(
            f"{path}.witness_payload_sha256 does not bind the detached signature preimage"
        )
    return {
        "id": event_id,
        "path": path,
        "event": "witness",
        "subject": subject,
        "subject_record_sha256": record_sha256,
        "witness_payload_sha256": witness_payload_sha256,
        "verification": "external_unverified",
        "record_sha256": hashlib.sha256(files[path]).hexdigest(),
    }


def _validate_release_rederive_ledger(
    attestations: list[dict[str, object]], events: list[dict[str, object]]
) -> list[dict[str, object]]:
    identifiers: set[str] = set()
    for item in [*attestations, *events]:
        identifier = str(item["id"])
        if identifier in identifiers:
            raise ReleaseError(f"World evidence ledger ID {identifier} is reused")
        identifiers.add(identifier)
    attestations_by_id = {str(item["id"]): item for item in attestations}
    events_by_id = {str(item["id"]): item for item in events}
    for witness in (item for item in events if item["event"] == "witness"):
        target = events_by_id.get(str(witness["subject"]))
        if target is None or target["event"] == "witness":
            raise ReleaseError(
                f"{witness['path']} must witness one exact non-witness event record"
            )
        if witness["subject_record_sha256"] != target["record_sha256"]:
            raise ReleaseError(
                f"{witness['path']} does not bind its exact subject record"
            )

    lifecycle_by_subject: dict[str, dict[str, object]] = {}
    transitions_by_subject: dict[str, dict[str, dict[str, object]]] = {}
    for event in events:
        if event["event"] == "witness":
            continue
        subject = str(event["subject"])
        if subject not in attestations_by_id:
            raise ReleaseError(f"{event['path']} references missing subject {subject}")
        if event["event"] == "rederive":
            if attestations_by_id[subject]["derivation_hash"] is None:
                raise ReleaseError(f"{event['path']} cannot rederive schema-v1 prose claims")
            transitions = transitions_by_subject.setdefault(subject, {})
            prior = str(event["prior_derivation"])
            if prior in transitions:
                raise ReleaseError(
                    f"attestation {subject} has competing rederive events from {prior}"
                )
            transitions[prior] = event
            continue
        if subject in lifecycle_by_subject:
            raise ReleaseError(
                f"attestation {subject} has multiple competing lifecycle events"
            )
        replacement = str(event["replacement"])
        if replacement:
            if replacement not in attestations_by_id:
                raise ReleaseError(
                    f"{event['path']} references missing replacement {replacement}"
                )
            if attestations_by_id[replacement]["gate"] != attestations_by_id[subject]["gate"]:
                raise ReleaseError(
                    f"{event['path']} supersedes evidence across unrelated gates"
                )
        lifecycle_by_subject[subject] = event

    replacement_predecessors: dict[str, str] = {}
    for subject, event in lifecycle_by_subject.items():
        replacement = str(event["replacement"])
        if replacement:
            previous = replacement_predecessors.setdefault(replacement, subject)
            if previous != subject:
                raise ReleaseError(f"replacement {replacement} has competing predecessors")
        seen_subjects: set[str] = set()
        current = subject
        while current in lifecycle_by_subject:
            if current in seen_subjects:
                raise ReleaseError("World evidence supersession graph contains a cycle")
            seen_subjects.add(current)
            current = str(lifecycle_by_subject[current]["replacement"])
            if not current:
                break

    inactive_ids = set(lifecycle_by_subject)

    for attestation in attestations:
        if attestation["schema_version"] == 1:
            if attestation["id"] not in inactive_ids:
                raise ReleaseError(
                    f"schema-v1 attestation {attestation['id']} cannot be an active ledger head"
                )
            continue
        subject = str(attestation["id"])
        derivation = str(attestation["derivation_hash"])
        claims = set(attestation["recorded_claims"])
        transitions = transitions_by_subject.get(subject, {})
        used: set[str] = set()
        seen = {derivation}
        while derivation in transitions:
            event = transitions[derivation]
            used.add(derivation)
            lost = set(event["claims_lost"])
            gained = set(event["claims_gained"])
            if not lost <= claims:
                raise ReleaseError(f"{event['path']} loses absent claims")
            if gained & claims:
                raise ReleaseError(f"{event['path']} gains existing claims")
            claims = (claims - lost) | gained
            derivation = str(event["current_derivation"])
            if derivation in seen:
                raise ReleaseError(f"attestation {subject} rederive graph contains a cycle")
            seen.add(derivation)
        unreachable = set(transitions) - used
        if unreachable:
            raise ReleaseError(
                f"attestation {subject} has unreachable rederive prior(s) {sorted(unreachable)}"
            )
        if derivation != WORLD_DERIVATION_HASH:
            raise ReleaseError(
                f"attestation {subject} does not reach the released derivation"
            )
        if claims != set(attestation["current_derived_claims"]):
            raise ReleaseError(
                f"attestation {subject} rederive delta differs from trusted released derivation"
            )

    active = [item for item in attestations if item["id"] not in inactive_ids]
    for attestation in active:
        if (
            attestation["schema_version"] != 3
            and attestation["id"] not in WORLD_LEGACY_ACTIVE_SCHEMA2_IDS
        ):
            raise ReleaseError(
                f"active attestation {attestation['id']} must use schema v3; "
                "only the explicitly pinned G2 legacy exception is allowed"
            )
    return active


def _validate_world_evidence_ledger_release_surface(
    files: dict[str, bytes],
    modes: dict[str, str],
    known_gates: set[str],
    known_classes: set[str],
) -> list[dict[str, object]]:
    actual_paths = {
        path
        for path in files
        if PurePosixPath(path).parent == PurePosixPath("evidence/world")
        and path.endswith(".toml")
    }
    expected_paths = (
        set(WORLD_HISTORICAL_ATTESTATION_SHA256)
        | set(WORLD_CURRENT_ATTESTATION_SHA256)
        | set(WORLD_EVIDENCE_EVENT_SHA256)
    )
    if actual_paths != expected_paths:
        missing = sorted(expected_paths - actual_paths)
        extra = sorted(actual_paths - expected_paths)
        raise ReleaseError(
            "World evidence ledger TOML set differs from the exhaustive release set; "
            f"missing={missing}, extra={extra}"
        )

    attestations: list[dict[str, object]] = []
    events: list[dict[str, object]] = []
    for path in sorted(actual_paths):
        if path in WORLD_CURRENT_ATTESTATION_SHA256:
            actual_digest = hashlib.sha256(files[path]).hexdigest()
            if actual_digest != WORLD_CURRENT_ATTESTATION_SHA256[path]:
                raise ReleaseError(
                    f"{path} bytes differ from the immutable current-attestation seal"
                )
        if path in WORLD_EVIDENCE_EVENT_SHA256:
            actual_digest = hashlib.sha256(files[path]).hexdigest()
            expected_digest = WORLD_EVIDENCE_EVENT_SHA256[path]
            if actual_digest != expected_digest:
                raise ReleaseError(
                    f"{path} bytes differ from the immutable evidence-event seal"
                )
        record = _strict_toml(files[path], path)
        if "event" not in record:
            gate_id = _required_string(record.get("gate"), f"{path}.gate")
            evidence_class = _required_string(
                record.get("evidence_class"), f"{path}.evidence_class"
            )
            if gate_id not in known_gates:
                raise ReleaseError(f"{path}.gate references unknown {gate_id}")
            if evidence_class not in known_classes:
                raise ReleaseError(
                    f"{path}.evidence_class references unknown {evidence_class}"
                )
            attestations.append(
                _validate_world_attestation_release_surface(
                    files, modes, path, gate_id, evidence_class
                )
            )
            continue
        kind = _required_string(record.get("event"), f"{path}.event")
        if kind == "rederive":
            events.append(
                _validate_world_rederive_event_release_surface(files, modes, path)
            )
        elif kind == "witness":
            events.append(
                _validate_world_witness_event_release_surface(files, modes, path)
            )
        else:
            events.append(
                _validate_world_evidence_event_release_surface(files, modes, path)
            )

    active = _validate_release_rederive_ledger(attestations, events)
    active_gates = {str(item["gate"]) for item in active}
    if active_gates != {"G0", "G2"}:
        raise ReleaseError(
            f"released active evidence heads must cover exactly G0 and G2, got {sorted(active_gates)}"
        )
    for gate_id, expected_claims in WORLD_DERIVED_CLAIMS.items():
        derived = set().union(
            *(
                set(item["current_derived_claims"])
                for item in active
                if item["gate"] == gate_id
            )
        )
        if derived != expected_claims:
            raise ReleaseError(
                f"released active {gate_id} claims differ from the trusted derivation"
            )
    return active


def _validate_world_alpha_release_surface(
    files: dict[str, bytes], modes: dict[str, str]
) -> None:
    texts = {
        path: _sealed_world_alpha_text(files, modes, path)
        for path in SEALED_WORLD_ALPHA_SHA256
    }
    validator_path = "scripts/world_alpha_evidence.py"
    if (
        validator_path not in files
        or hashlib.sha256(files[validator_path]).hexdigest()
        != WORLD_VALIDATOR_SHA256
    ):
        raise ReleaseError(
            f"{validator_path} differs from the trusted World evidence validator bytes"
        )
    required_document_markers = {
        "docs/OSTADIX_WORLD.md": (
            "# OSTADIX World: Full-Stack Machine-Constructor Roadmap",
            "**Status:** normative OSTADIX Alpha constitution and implementation program,",
            "| **G0 -- constitutional baseline** |",
            "| **G13 -- eight-node OSTADIX Alpha** |",
            "validator derives claims and current",
            "# 28. OSTADIX Alpha non-claims",
        ),
        "docs/HOSTED_WORLD_REFERENCE_PROFILE.md": (
            "# Hosted World Reference Profile",
            "**Status:** design/reference profile with partial hosted foundations;",
            "non-qualifying for native OSTADIX Alpha release gates.",
            "cannot satisfy G0 through G13",
            "## Non-claims",
            "G12, G13, or the name **OSTADIX Alpha**.",
        ),
        "docs/O_MACHINE_CONTRACT.md": (
            "# O-Machine EL2 and O-core Resource Contract",
            "MachineMemory",
            "MachineBlock",
            "Machine9P",
            "no machine effect authorized by s.old_generation remains reachable",
            "no `MachineHandle`, no revocation-completion handle",
            "a doorbell is not an authority-bearing hypercall",
        ),
    }
    for path, markers in required_document_markers.items():
        for marker in markers:
            if marker not in texts[path]:
                raise ReleaseError(f"{path} is missing required OSTADIX Alpha marker {marker!r}")

    wrapper_path = "evidence/world_contract_v2.toml"
    wrapper = _strict_toml(files[wrapper_path], wrapper_path)
    if _toml_semantics_sha256(wrapper) != WORLD_CONTRACT_V2_SEMANTICS_SHA256:
        raise ReleaseError(f"{wrapper_path} composition semantics differ from schema")
    expected_wrapper = {
        "schema": "ostadix.world-contract/v2",
        "schema_version": 2,
        "constitution_version": 3,
        "constitution": "docs/OSTADIX_WORLD.md",
        "constitution_sha256": SEALED_WORLD_ALPHA_SHA256["docs/OSTADIX_WORLD.md"],
        "world_gate_registry": "evidence/world_alpha_gates.toml",
        "imported_vocabulary": "evidence/world_contract_v1.toml",
        "imported_vocabulary_schema_version": 1,
        "imported_vocabulary_constitution_version": 2,
        "imported_vocabulary_constitution_sha256": (
            WORLD_IMPORTED_CONSTITUTION_V2_SHA256
        ),
        "imported_vocabulary_sha256": SEALED_WORLD_ALPHA_SHA256[
            "evidence/world_contract_v1.toml"
        ],
        "machine_contract": "evidence/o_machine_contract_v1.toml",
        "machine_contract_schema_version": 1,
        "machine_contract_sha256": SEALED_WORLD_ALPHA_SHA256[
            "evidence/o_machine_contract_v1.toml"
        ],
        "composition": {
            "crossings": "imported_vocabulary",
            "identity_atoms": "imported_vocabulary",
            "failure_classes": "imported_vocabulary",
            "consistency_rules": "imported_vocabulary",
            "evidence_classes": "imported_vocabulary",
            "machine_authority_and_revocation": "machine_contract",
        },
    }
    if wrapper != expected_wrapper:
        raise ReleaseError(f"{wrapper_path} composition differs from exact schema")
    for field, digest_field in (
        ("constitution", "constitution_sha256"),
        ("imported_vocabulary", "imported_vocabulary_sha256"),
        ("machine_contract", "machine_contract_sha256"),
    ):
        referenced = str(wrapper[field])
        if referenced not in files:
            raise ReleaseError(f"{wrapper_path}.{field} references absent {referenced}")
        if hashlib.sha256(files[referenced]).hexdigest() != wrapper[digest_field]:
            raise ReleaseError(f"{wrapper_path}.{digest_field} does not bind {referenced}")

    machine_path = "evidence/o_machine_contract_v1.toml"
    machine = _strict_toml(files[machine_path], machine_path)
    if _toml_semantics_sha256(machine) != WORLD_MACHINE_CONTRACT_SEMANTICS_SHA256:
        raise ReleaseError(f"{machine_path} semantics differ from exact schema")
    if (
        machine.get("schema") != "ostadix.o-machine/v1"
        or type(machine.get("schema_version")) is not int
        or machine["schema_version"] != 1
        or type(machine.get("constitution_version")) is not int
        or machine["constitution_version"] != 3
        or machine.get("specification") != "docs/O_MACHINE_CONTRACT.md"
        or machine.get("specification_sha256")
        != SEALED_WORLD_ALPHA_SHA256["docs/O_MACHINE_CONTRACT.md"]
    ):
        raise ReleaseError(f"{machine_path} self-description differs from schema")

    contract_path = "evidence/world_contract_v1.toml"
    contract = _strict_toml(files[contract_path], contract_path)
    if set(contract) != {
        "schema_version",
        "constitution_version",
        "constitution",
        "hosted_identity_schema",
        "native_identity_schema",
        "world_gate_registry",
        "crossing",
        "identity_atom",
        "failure_class",
        "consistency_rule",
        "claim_class",
    }:
        raise ReleaseError(f"{contract_path} root keys differ from schema")
    if type(contract["schema_version"]) is not int or contract["schema_version"] != 1:
        raise ReleaseError(f"{contract_path} schema_version must be 1")
    if (
        type(contract["constitution_version"]) is not int
        or contract["constitution_version"] != 2
    ):
        raise ReleaseError(f"{contract_path} constitution_version must be 2")
    for field, expected in {
        "constitution": "docs/OSTADIX_WORLD.md",
        "hosted_identity_schema": "src/world/identity.rs",
        "native_identity_schema": "ocore/world/identity.oc",
        "world_gate_registry": "evidence/world_alpha_gates.toml",
    }.items():
        released_expected = _released_path_for_historical_source(expected)
        if contract[field] != expected or released_expected not in files:
            raise ReleaseError(
                f"{contract_path}.{field} must reference released {released_expected}"
            )
    for field, count in {
        "crossing": 3,
        "identity_atom": 20,
        "failure_class": 7,
        "consistency_rule": 8,
        "claim_class": 14,
    }.items():
        if not isinstance(contract[field], list) or len(contract[field]) != count:
            raise ReleaseError(f"{contract_path}.{field} must contain {count} tables")

    path = "evidence/world_alpha_gates.toml"
    manifest = _strict_toml(files[path], path)
    expected_root_keys = {
        "schema_version",
        "constitution_version",
        "constitution",
        "hosted_reference_profile",
        "contract_schema",
        "machine_contract_schema",
        "alpha_gate",
        "gate_count",
        "evidence_class",
        "gate",
    }
    if set(manifest) != expected_root_keys:
        raise ReleaseError(f"{path} root keys differ from schema")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 4:
        raise ReleaseError(f"{path} schema_version must be 4")
    if (
        type(manifest["constitution_version"]) is not int
        or manifest["constitution_version"] != 3
    ):
        raise ReleaseError(f"{path} constitution_version must be 3")
    if manifest["constitution"] != "docs/OSTADIX_WORLD.md":
        raise ReleaseError(f"{path} constitution must reference docs/OSTADIX_WORLD.md")
    if manifest["hosted_reference_profile"] != (
        "docs/HOSTED_WORLD_REFERENCE_PROFILE.md"
    ):
        raise ReleaseError(
            f"{path} hosted_reference_profile must reference "
            "docs/HOSTED_WORLD_REFERENCE_PROFILE.md"
        )
    if manifest["contract_schema"] != "evidence/world_contract_v2.toml":
        raise ReleaseError(
            f"{path} contract_schema must reference evidence/world_contract_v2.toml"
        )
    if manifest["machine_contract_schema"] != "evidence/o_machine_contract_v1.toml":
        raise ReleaseError(
            f"{path} machine_contract_schema must reference evidence/o_machine_contract_v1.toml"
        )
    if manifest["alpha_gate"] != "G13":
        raise ReleaseError(f"{path} alpha_gate must be G13")
    if type(manifest["gate_count"]) is not int or manifest["gate_count"] != 14:
        raise ReleaseError(f"{path} gate_count must be 14")

    evidence_classes = manifest["evidence_class"]
    if not isinstance(evidence_classes, list):
        raise ReleaseError(f"{path} evidence_class must be a list of tables")
    class_ids: list[str] = []
    for index, evidence_class in enumerate(evidence_classes):
        owner = f"{path} evidence_class[{index}]"
        if not isinstance(evidence_class, dict) or set(evidence_class) != {
            "id",
            "scope",
            "description",
        }:
            raise ReleaseError(f"{owner} keys differ from schema")
        class_ids.append(_required_string(evidence_class["id"], f"{owner}.id"))
        _required_string(evidence_class["scope"], f"{owner}.scope")
        _required_string(evidence_class["description"], f"{owner}.description")
    if tuple(class_ids) != EXPECTED_WORLD_ALPHA_CLASS_IDS:
        raise ReleaseError(f"{path} evidence-class IDs or order differ from schema")
    known_classes = set(class_ids)

    gates = manifest["gate"]
    if not isinstance(gates, list) or len(gates) != 14:
        raise ReleaseError(f"{path} must contain exactly 14 gate tables")
    gate_ids: list[str] = []
    expected_gate_keys = {
        "id",
        "title",
        "depends_on",
        "required_classes",
        "one_of_classes",
        "acceptance",
        "prohibited_substitutes",
    }
    for index, gate in enumerate(gates):
        owner = f"{path} gate[{index}]"
        if not isinstance(gate, dict) or set(gate) != expected_gate_keys:
            raise ReleaseError(f"{owner} keys differ from schema")
        gate_ids.append(_required_string(gate["id"], f"{owner}.id"))
        _required_string(gate["title"], f"{owner}.title")
        dependencies = _required_string_list(gate["depends_on"], f"{owner}.depends_on")
        unknown_dependencies = set(dependencies) - set(EXPECTED_WORLD_ALPHA_GATE_IDS)
        if unknown_dependencies:
            raise ReleaseError(f"{owner}.depends_on references an unknown gate")
        required_classes = _required_string_list(
            gate["required_classes"], f"{owner}.required_classes", minimum=1
        )
        if set(required_classes) - known_classes:
            raise ReleaseError(f"{owner}.required_classes references an unknown class")
        alternatives = gate["one_of_classes"]
        if not isinstance(alternatives, list):
            raise ReleaseError(f"{owner}.one_of_classes must be a list")
        for group_index, group in enumerate(alternatives):
            choices = _required_string_list(
                group, f"{owner}.one_of_classes[{group_index}]", minimum=1
            )
            if set(choices) - known_classes:
                raise ReleaseError(f"{owner}.one_of_classes references an unknown class")
        _required_string(gate["acceptance"], f"{owner}.acceptance")
        _required_string_list(
            gate["prohibited_substitutes"],
            f"{owner}.prohibited_substitutes",
            minimum=1,
        )
    if tuple(gate_ids) != EXPECTED_WORLD_ALPHA_GATE_IDS:
        raise ReleaseError(f"{path} gate IDs or order differ from G0 through G13")
    _validate_world_evidence_ledger_release_surface(
        files,
        modes,
        set(gate_ids),
        known_classes,
    )


def validate_release_metadata(entries: Sequence[SourceEntry]) -> None:
    """Validate inert release metadata and every archive-local reference."""

    files = {entry.path: entry.data for entry in entries}
    modes = {entry.path: entry.mode for entry in entries}
    if len(files) != len(entries):
        raise ReleaseError("release contains duplicate metadata paths")
    _validate_root_release_metadata(files)
    _validate_workspace_facade_release_surface(files)
    _validate_mcp_release_surface(files, modes)
    _validate_example_manifest(files)
    _validate_evidence_manifest(files, modes)
    _validate_world_alpha_release_surface(files, modes)


def _canonical_json(value: object) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
        + "\n"
    ).encode("ascii")


def _manifest_bytes(commit: str, prefix: str, entries: Sequence[SourceEntry]) -> bytes:
    manifest = {
        "commit": commit,
        "file_count": len(entries),
        "files": [
            {
                "mode": entry.mode,
                "path": entry.path,
                "sha256": entry.sha256,
                "size": len(entry.data),
            }
            for entry in entries
        ],
        "prefix": prefix,
        "schema": SCHEMA,
    }
    return _canonical_json(manifest)


def _checksums_bytes(entries: Sequence[SourceEntry], manifest: bytes) -> bytes:
    lines = [f"{entry.sha256}  {entry.path}" for entry in entries]
    lines.append(f"{hashlib.sha256(manifest).hexdigest()}  {MANIFEST_NAME}")
    return ("\n".join(lines) + "\n").encode("utf-8")


def _zip_info(name: str, mode: str) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name, FIXED_ZIP_TIMESTAMP)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.create_system = 3
    info.external_attr = int(mode, 8) << 16
    info.flag_bits |= 0x800
    return info


def _zip_filename_bytes(info: zipfile.ZipInfo) -> bytes:
    encoding = "utf-8" if info.flag_bits & 0x800 else "cp437"
    return info.filename.encode(encoding, "strict")


def _validate_zip_member_metadata(
    info: zipfile.ZipInfo, mode: str, payload: bytes
) -> None:
    try:
        info.filename.encode("ascii", "strict")
        expected_flags = 0
    except UnicodeEncodeError:
        expected_flags = 0x800
    expected = {
        "date_time": FIXED_ZIP_TIMESTAMP,
        "compress_type": zipfile.ZIP_DEFLATED,
        "create_system": 3,
        "create_version": 20,
        "extract_version": 20,
        "reserved": 0,
        "flag_bits": expected_flags,
        "volume": 0,
        "internal_attr": 0,
        "external_attr": int(mode, 8) << 16,
        "extra": b"",
        "comment": b"",
        "file_size": len(payload),
        "CRC": zlib.crc32(payload) & 0xFFFFFFFF,
    }
    for field, value in expected.items():
        if getattr(info, field) != value:
            raise ReleaseError(
                f"non-canonical ZIP {field} for {info.filename}: "
                f"expected {value!r}, got {getattr(info, field)!r}"
            )


def _validate_zip_layout(
    release_path: Path,
    archive: zipfile.ZipFile,
    infos: Sequence[zipfile.ZipInfo],
) -> None:
    expected_offset = 0
    for info in infos:
        if info.header_offset != expected_offset:
            raise ReleaseError(
                f"non-canonical ZIP member offset for {info.filename}: "
                f"expected {expected_offset}, got {info.header_offset}"
            )
        expected_offset += 30 + len(_zip_filename_bytes(info)) + info.compress_size
    if archive.start_dir != expected_offset:
        raise ReleaseError("non-canonical ZIP local-header layout")

    central_size = sum(46 + len(_zip_filename_bytes(info)) for info in infos)
    expected_size = archive.start_dir + central_size + 22
    try:
        actual_size = release_path.stat().st_size
    except OSError as error:
        raise ReleaseError(f"cannot stat release ZIP {release_path}: {error}") from error
    if actual_size != expected_size:
        raise ReleaseError(
            f"non-canonical ZIP total size: expected {expected_size}, got {actual_size}"
        )


def _write_archive(
    output: Path,
    prefix: str,
    entries: Sequence[SourceEntry],
    manifest: bytes,
    checksums: bytes,
) -> None:
    with zipfile.ZipFile(
        output,
        mode="w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=9,
        strict_timestamps=True,
    ) as archive:
        archive.comment = b""
        for entry in entries:
            archive.writestr(
                _zip_info(f"{prefix}/{entry.path}", entry.mode),
                entry.data,
                compress_type=zipfile.ZIP_DEFLATED,
                compresslevel=9,
            )
        archive.writestr(
            _zip_info(f"{prefix}/{MANIFEST_NAME}", "100644"),
            manifest,
            compress_type=zipfile.ZIP_DEFLATED,
            compresslevel=9,
        )
        archive.writestr(
            _zip_info(f"{prefix}/{CHECKSUMS_NAME}", "100644"),
            checksums,
            compress_type=zipfile.ZIP_DEFLATED,
            compresslevel=9,
        )


def _archive_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as release:
        for chunk in iter(lambda: release.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _parse_checksums(data: bytes) -> dict[str, str]:
    try:
        text = data.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError("SHA256SUMS is not valid UTF-8") from error
    result: dict[str, str] = {}
    for line in text.splitlines():
        if "  " not in line:
            raise ReleaseError(f"malformed SHA256SUMS line: {line!r}")
        digest, path = line.split("  ", 1)
        _validate_release_path(path)
        if not HEX_DIGEST.fullmatch(digest):
            raise ReleaseError(f"invalid SHA-256 digest for {path}")
        if path in result:
            raise ReleaseError(f"duplicate SHA256SUMS path: {path}")
        result[path] = digest
    return result


def verify_archive(path: Path | str) -> dict[str, object]:
    release_path = Path(path)
    try:
        with zipfile.ZipFile(release_path, "r") as archive:
            if archive.comment:
                raise ReleaseError("release ZIP must not have an archive comment")
            infos = archive.infolist()
            names = [info.filename for info in infos]
            if len(names) != len(set(names)):
                raise ReleaseError("release ZIP contains duplicate member names")
            if not names:
                raise ReleaseError("release ZIP is empty")

            roots = {PurePosixPath(name).parts[0] for name in names}
            if len(roots) != 1:
                raise ReleaseError("release ZIP must contain exactly one top-level prefix")
            prefix = next(iter(roots))
            if not SAFE_PREFIX.fullmatch(prefix):
                raise ReleaseError(f"unsafe release prefix: {prefix!r}")

            manifest_name = f"{prefix}/{MANIFEST_NAME}"
            checksums_name = f"{prefix}/{CHECKSUMS_NAME}"
            if manifest_name not in names or checksums_name not in names:
                raise ReleaseError("release ZIP lacks its embedded manifest or SHA256SUMS")

            manifest_bytes = archive.read(manifest_name)
            try:
                manifest = json.loads(manifest_bytes.decode("ascii", "strict"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise ReleaseError("SOURCE-MANIFEST.json is not canonical JSON") from error
            if not isinstance(manifest, dict) or _canonical_json(manifest) != manifest_bytes:
                raise ReleaseError("SOURCE-MANIFEST.json is not canonical JSON")
            if manifest.get("schema") != SCHEMA:
                raise ReleaseError("unsupported source-release manifest schema")
            if manifest.get("prefix") != prefix:
                raise ReleaseError("manifest prefix does not match ZIP prefix")
            if not isinstance(manifest.get("commit"), str) or not HEX_COMMIT.fullmatch(
                manifest["commit"]
            ):
                raise ReleaseError("manifest contains an invalid commit identifier")

            raw_files = manifest.get("files")
            if not isinstance(raw_files, list):
                raise ReleaseError("manifest files field must be a list")
            if manifest.get("file_count") != len(raw_files):
                raise ReleaseError("manifest file_count does not match its files list")

            expected_names: list[str] = []
            expected_checksums: dict[str, str] = {}
            archive_entries: list[SourceEntry] = []
            previous_path: str | None = None
            info_by_name = {info.filename: info for info in infos}
            for item in raw_files:
                if not isinstance(item, dict) or set(item) != {
                    "mode",
                    "path",
                    "sha256",
                    "size",
                }:
                    raise ReleaseError("manifest contains a malformed file record")
                relative = item["path"]
                mode = item["mode"]
                digest = item["sha256"]
                size = item["size"]
                if not isinstance(relative, str) or not is_allowed_release_path(relative):
                    raise ReleaseError(f"manifest contains a non-allowlisted path: {relative!r}")
                if previous_path is not None and relative.encode("utf-8") <= previous_path.encode(
                    "utf-8"
                ):
                    raise ReleaseError("manifest file paths are not uniquely sorted")
                previous_path = relative
                if mode not in VALID_GIT_MODES:
                    raise ReleaseError(f"manifest contains an invalid mode for {relative}")
                if not isinstance(digest, str) or not HEX_DIGEST.fullmatch(digest):
                    raise ReleaseError(f"manifest contains an invalid digest for {relative}")
                if not isinstance(size, int) or isinstance(size, bool) or size < 0:
                    raise ReleaseError(f"manifest contains an invalid size for {relative}")

                member = f"{prefix}/{relative}"
                expected_names.append(member)
                expected_checksums[relative] = digest
                if member not in info_by_name:
                    raise ReleaseError(f"manifest member is absent from ZIP: {relative}")
                payload = archive.read(member)
                if len(payload) != size or hashlib.sha256(payload).hexdigest() != digest:
                    raise ReleaseError(f"payload does not match manifest: {relative}")
                archive_entries.append(
                    SourceEntry(path=relative, mode=mode, data=payload)
                )
                zip_mode = f"{(info_by_name[member].external_attr >> 16) & 0xFFFF:06o}"
                if zip_mode != mode:
                    raise ReleaseError(f"ZIP mode does not match manifest: {relative}")

            archived_paths = {entry.path for entry in archive_entries}
            missing_required = sorted(REQUIRED_RELEASE_PATHS - archived_paths)
            if missing_required:
                raise ReleaseError(
                    "release ZIP is missing required path(s): "
                    + ", ".join(missing_required)
                )
            validate_generated_runtime_source_closure(archive_entries)
            validate_document_links(archive_entries)
            validate_release_metadata(archive_entries)

            expected_order = expected_names + [manifest_name, checksums_name]
            if names != expected_order:
                raise ReleaseError("release ZIP member set or ordering does not match manifest")

            expected_checksums[MANIFEST_NAME] = hashlib.sha256(manifest_bytes).hexdigest()
            checksums_bytes = archive.read(checksums_name)
            actual_checksums = _parse_checksums(checksums_bytes)
            if actual_checksums != expected_checksums:
                raise ReleaseError("SHA256SUMS does not match the release payload")

            canonical_members = {
                f"{prefix}/{entry.path}": (entry.mode, entry.data)
                for entry in archive_entries
            }
            canonical_members[manifest_name] = ("100644", manifest_bytes)
            canonical_members[checksums_name] = ("100644", checksums_bytes)
            for info in infos:
                mode, payload = canonical_members[info.filename]
                _validate_zip_member_metadata(info, mode, payload)
            _validate_zip_layout(release_path, archive, infos)
            return manifest
    except (OSError, zipfile.BadZipFile, KeyError) as error:
        if isinstance(error, ReleaseError):
            raise
        raise ReleaseError(f"cannot verify release ZIP {release_path}: {error}") from error


def build_release(
    repo: Path | str,
    ref: str,
    output: Path | str,
    *,
    allow_dirty: bool = False,
    prefix: str | None = None,
) -> BuildResult:
    root = discover_repository(repo)
    assert_clean_worktree(root, allow_dirty=allow_dirty)
    commit = resolve_commit(root, ref)
    release_prefix = prefix or f"Ostadix-lang-source-{commit[:12]}"
    if not SAFE_PREFIX.fullmatch(release_prefix):
        raise ReleaseError(
            "release prefix must start with an alphanumeric character and contain "
            "only letters, digits, dots, underscores, or hyphens"
        )

    entries = collect_source_entries(root, commit)
    manifest = _manifest_bytes(commit, release_prefix, entries)
    checksums = _checksums_bytes(entries, manifest)
    destination = Path(output).expanduser().resolve()
    destination.parent.mkdir(parents=True, exist_ok=True)

    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{destination.name}.", suffix=".tmp", dir=destination.parent
    )
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        _write_archive(temporary, release_prefix, entries, manifest, checksums)
        verified = verify_archive(temporary)
        if verified["commit"] != commit:
            raise ReleaseError("self-verification returned the wrong commit")
        os.replace(temporary, destination)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass

    return BuildResult(
        output=destination,
        commit=commit,
        prefix=release_prefix,
        file_count=len(entries),
        archive_sha256=_archive_sha256(destination),
    )


def _argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Build or verify a deterministic allowlist-driven source release"
    )
    parser.add_argument("--repo", default=".", help="repository path (default: current directory)")
    parser.add_argument("--ref", default="HEAD", help="committed Git ref to archive")
    parser.add_argument("--output", help="output ZIP path (default: dist/<prefix>.zip)")
    parser.add_argument("--prefix", help="override the deterministic top-level ZIP prefix")
    parser.add_argument(
        "--allow-dirty",
        action="store_true",
        help="allow a dirty worktree; archive bytes still come only from the resolved commit",
    )
    parser.add_argument("--verify", metavar="ZIP", help="verify an existing release instead")
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    arguments = _argument_parser().parse_args(list(argv) if argv is not None else None)
    try:
        if arguments.verify:
            manifest = verify_archive(arguments.verify)
            digest = _archive_sha256(Path(arguments.verify))
            print(
                f"verified {arguments.verify}: {manifest['file_count']} files, "
                f"commit {manifest['commit']}, sha256 {digest}"
            )
            return 0

        root = discover_repository(arguments.repo)
        commit = resolve_commit(root, arguments.ref)
        prefix = arguments.prefix or f"Ostadix-lang-source-{commit[:12]}"
        output = arguments.output or os.fspath(root / "dist" / f"{prefix}.zip")
        result = build_release(
            root,
            arguments.ref,
            output,
            allow_dirty=arguments.allow_dirty,
            prefix=prefix,
        )
        print(
            f"built {result.output}: {result.file_count} files, commit {result.commit}, "
            f"sha256 {result.archive_sha256}"
        )
        return 0
    except ReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
