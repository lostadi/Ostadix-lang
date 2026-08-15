#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
OUT=${1:-"$ROOT/target/semantic-custody"}
PROGRAM=${O_PROGRAM:-"$ROOT/examples/semantic_custody.O"}
O_BIN=${O_BIN:-"$ROOT/target/release/O"}
OLANGC_BIN=${OLANGC_BIN:-"$ROOT/target/release/olangc"}
SHIMS=${O_BACKENDS_DIR:-"$ROOT/backends"}

for executable in "$O_BIN" "$OLANGC_BIN"; do
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
find "$OUT" -maxdepth 1 -type f -name manifest.json -delete
STAGE=$(mktemp -d "$OUT/.semantic-custody.XXXXXX")
cleanup() {
  if [[ -d "$STAGE" ]]; then
    find "$STAGE" -depth -delete
  fi
}
trap cleanup EXIT

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

python3 - "$STAGE" <<'PY'
import hashlib
import json
import pathlib
import sys

out = pathlib.Path(sys.argv[1])
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

names = ("execution-intent.json", "schedule.txt", "hgraph.dot", "result.json")
artifacts = {
    name: hashlib.sha256((out / name).read_bytes()).hexdigest()
    for name in names
}
manifest = {
    "schema": "ostadix.semantic-custody-artifact/v1",
    "source_sha256": intent["source_sha256"],
    "execution_intent_sha256": intent["execution_intent_sha256"],
    "artifacts": artifacts,
    "claim_scope": [
        "exact source bytes are bound to one stable analyzed execution intent",
        "the gated run recomputed that same intent before fresh local V5 admission",
        "result.json records the observed local terminal value",
    ],
    "nonclaims": [
        "the execution intent is authority or a reusable admission",
        "the schedule view proves simultaneous dispatch or physical placement",
        "the local result is a signed World or Hosted V2 receipt",
    ],
}
(out / "manifest.json").write_text(
    json.dumps(manifest, sort_keys=True, indent=2) + "\n",
    encoding="utf-8",
)
PY

for artifact in execution-intent.json schedule.txt hgraph.dot result.json; do
  mv -f "$STAGE/$artifact" "$OUT/$artifact"
done
# Publish the manifest last: its presence means every artifact it hashes was
# generated and installed by this invocation.
mv -f "$STAGE/manifest.json" "$OUT/manifest.json"

echo "semantic-custody: artifact=$OUT/manifest.json"
