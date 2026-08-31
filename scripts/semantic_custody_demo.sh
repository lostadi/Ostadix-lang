#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
OUT=${1:-"$ROOT/target/semantic-custody"}
PROGRAM=${O_PROGRAM:-"$ROOT/examples/semantic_custody.O"}
O_BIN=${O_BIN:-"$ROOT/target/release/O"}
OLANGC_BIN=${OLANGC_BIN:-"$ROOT/target/release/olangc"}
O_CLI_BIN=${O_CLI_BIN:-"$ROOT/target/release/o-cli"}
SHIMS=${O_BACKENDS_DIR:-"$ROOT/backends"}

for executable in "$O_BIN" "$OLANGC_BIN" "$O_CLI_BIN"; do
  if [[ ! -x "$executable" ]]; then
    echo "semantic-custody: missing executable: $executable" >&2
    exit 1
  fi
done
if [[ ! -f "$PROGRAM" ]]; then
  echo "semantic-custody: missing program: $PROGRAM" >&2
  exit 1
fi
if [[ ! -d "$SHIMS" ]]; then
  echo "semantic-custody: missing shim directory: $SHIMS" >&2
  exit 1
fi

mkdir -p "$OUT"
LOCK="$OUT/.semantic-custody.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
  echo "semantic-custody: output is locked by another or interrupted invocation: $LOCK" >&2
  exit 1
fi
STAGE=
cleanup() {
  if [[ -n "$STAGE" && -d "$STAGE" ]]; then
    find "$STAGE" -depth -delete
  fi
  rmdir "$LOCK" 2>/dev/null || true
}
trap cleanup EXIT

for artifact in execution-intent.json schedule.txt hgraph.dot result.json computation.cbor computation.json manifest.json; do
  destination="$OUT/$artifact"
  if [[ ( -e "$destination" || -L "$destination" ) && ( ! -f "$destination" || -L "$destination" ) ]]; then
    echo "semantic-custody: refusing non-regular output entry: $destination" >&2
    exit 1
  fi
done
find "$OUT" -maxdepth 1 -type f -name manifest.json -delete
STAGE=$(mktemp -d "$OUT/.semantic-custody.XXXXXX")

"$OLANGC_BIN" "$PROGRAM" --target ir --execution-intent-json --shim-dir "$SHIMS" \
  >"$STAGE/execution-intent.json"
"$OLANGC_BIN" "$PROGRAM" --target ir --explain-schedule --shim-dir "$SHIMS" \
  >"$STAGE/schedule.txt"
"$OLANGC_BIN" "$PROGRAM" --target dot --shim-dir "$SHIMS" \
  >"$STAGE/hgraph.dot"

read -r SOURCE_SHA256 INTENT_SHA256 < <(
  python3 - "$STAGE/execution-intent.json" <<'PY'
import json
import pathlib
import sys

intent = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
print(intent["source_sha256"], intent["execution_intent_sha256"])
PY
)

"$O_BIN" --json --executor graph \
  --require-source-sha256 "$SOURCE_SHA256" \
  --require-execution-intent-sha256 "$INTENT_SHA256" \
  "$PROGRAM" "$SHIMS" >"$STAGE/result.json"

COMPUTATION_REVISION_SHA256=$("$O_CLI_BIN" computation \
  --source "$PROGRAM" \
  --execution-intent "$STAGE/execution-intent.json" \
  --schedule "$STAGE/schedule.txt" \
  --hgraph-dot "$STAGE/hgraph.dot" \
  --result "$STAGE/result.json" \
  --o-bin "$O_BIN" \
  --olangc-bin "$OLANGC_BIN" \
  --cbor-out "$STAGE/computation.cbor" \
  --json-out "$STAGE/computation.json")

python3 - "$STAGE" "$COMPUTATION_REVISION_SHA256" <<'PY'
import hashlib
import json
import pathlib
import sys

out = pathlib.Path(sys.argv[1])
computation_revision = sys.argv[2]
if (
    len(computation_revision) != 64
    or any(character not in "0123456789abcdef" for character in computation_revision)
):
    raise SystemExit("semantic-custody: computation adapter returned an invalid revision")
intent = json.loads((out / "execution-intent.json").read_text(encoding="utf-8"))
result = json.loads((out / "result.json").read_text(encoding="utf-8"))
if result.get("ok") is not True:
    raise SystemExit("semantic-custody: gated execution did not settle successfully")
value = result.get("value", {})
if (
    value.get("t") != "text"
    or value.get("v", {}).get("utf8") != "semantic-custody answer=42"
):
    raise SystemExit("semantic-custody: gated execution returned an unexpected value")

names = (
    "execution-intent.json",
    "schedule.txt",
    "hgraph.dot",
    "result.json",
    "computation.cbor",
    "computation.json",
)
artifacts = {
    name: hashlib.sha256((out / name).read_bytes()).hexdigest()
    for name in names
}
manifest = {
    "schema": "ostadix.semantic-custody-artifact/v2",
    "source_sha256": intent["source_sha256"],
    "execution_intent_sha256": intent["execution_intent_sha256"],
    "computation_revision_sha256": computation_revision,
    "artifacts": artifacts,
    "claim_scope": [
        "exact source bytes are bound to one stable analyzed execution intent",
        "the gated run recomputed that same intent before fresh local V6 admission",
        "result.json records the observed local terminal value",
        "computation.cbor is the canonical authority-free body and computation.json is its matching manifest projection",
        "one locked staged workflow attests the derivation edges and exact O and olangc paths it invoked",
    ],
    "nonclaims": [
        "the execution intent is authority or a reusable admission",
        "the schedule view proves simultaneous dispatch or physical placement",
        "the local result is a signed World or Hosted V2 receipt",
        "the computation manifest grants admission, placement, dispatch, or reusable runtime authority",
        "canonical decoding verifies content identities and graph structure, not historical transform execution",
        "the unsigned workflow attestation does not independently authenticate shim, ambient-world, Python runtime, or process identity",
    ],
}
(out / "manifest.json").write_text(
    json.dumps(manifest, sort_keys=True, indent=2) + "\n",
    encoding="utf-8",
)
PY

for artifact in execution-intent.json schedule.txt hgraph.dot result.json computation.cbor computation.json; do
  mv -f "$STAGE/$artifact" "$OUT/$artifact"
done
# Publish the manifest last: its presence means every artifact it hashes was
# generated and installed by this invocation.
mv -f "$STAGE/manifest.json" "$OUT/manifest.json"

echo "semantic-custody: artifact=$OUT/manifest.json"
