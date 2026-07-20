#!/usr/bin/env bash
# Compile every binary declared by the root Cargo package and prove that Cargo
# reported an artifact for each declaration. `--all-features` includes targets
# guarded by `required-features`, such as o-notebook.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "error: check_declared_bins.sh must run inside a Git worktree" >&2
    exit 1
}
cd "$repo_root"

metadata_file="$(mktemp "${TMPDIR:-/tmp}/ostadix-cargo-metadata.XXXXXX")"
messages_file="$(mktemp "${TMPDIR:-/tmp}/ostadix-cargo-messages.XXXXXX")"
cleanup() {
    rm -f "$metadata_file" "$messages_file"
}
trap cleanup EXIT

cargo metadata --no-deps --format-version 1 >"$metadata_file"
cargo check --bins --all-features --message-format=json-render-diagnostics >"$messages_file"

python3 - "$metadata_file" "$messages_file" "$repo_root/Cargo.toml" <<'PY'
import json
import os
import sys

metadata_path, messages_path, root_manifest = sys.argv[1:]
with open(metadata_path, encoding="utf-8") as stream:
    metadata = json.load(stream)

root_manifest = os.path.realpath(root_manifest)
packages = [
    package
    for package in metadata["packages"]
    if os.path.realpath(package["manifest_path"]) == root_manifest
]
if len(packages) != 1:
    raise SystemExit(
        f"error: expected one root Cargo package at {root_manifest}, found {len(packages)}"
    )

declared = sorted(
    target["name"]
    for target in packages[0]["targets"]
    if "bin" in target.get("kind", [])
)
if not declared:
    raise SystemExit("error: root Cargo package declares no binary targets")

reported = set()
with open(messages_path, encoding="utf-8") as stream:
    for line_number, line in enumerate(stream, 1):
        if not line.strip():
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError as error:
            raise SystemExit(
                f"error: Cargo emitted malformed JSON on line {line_number}: {error}"
            ) from error
        target = message.get("target", {})
        if message.get("reason") == "compiler-artifact" and "bin" in target.get("kind", []):
            reported.add(target.get("name"))

missing = sorted(set(declared) - reported)
if missing:
    raise SystemExit(
        "error: cargo check --bins did not report declared target(s): "
        + ", ".join(missing)
    )

print(f"declared Cargo binaries ({len(declared)}): {', '.join(declared)}")
print("cargo check --bins reported every declared binary: PASS")
PY
