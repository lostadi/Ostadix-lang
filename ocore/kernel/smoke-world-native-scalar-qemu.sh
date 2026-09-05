#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-native-scalar}"
mkdir -p "$BUILD_DIR"
cd "$ROOT"

O_NATIVE_SCALAR_FIXTURE_OUT="$BUILD_DIR/fixture.json" \
  cargo test --locked --package o-lang --test world_native_scalar \
    native_scalar_fixture_and_returned_receipts_use_exact_world_identity -- --exact
OCORE_PROBE_MODE=35 OCORE_BUILD_DIR="$BUILD_DIR" "$ROOT/ocore/kernel/build.sh"
python3 "$ROOT/ocore/kernel/verify-native-scalar.py" \
  "$BUILD_DIR/kernel.elf" "$BUILD_DIR/fixture.json" "$BUILD_DIR/results.json" \
  "$BUILD_DIR/transcripts"
O_NATIVE_SCALAR_RESULTS_IN="$BUILD_DIR/results.json" \
  cargo test --locked --package o-lang --test world_native_scalar
echo "World native scalar supplemental gate: PASS (unsigned observations; G1 remains defined)"
