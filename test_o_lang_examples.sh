#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
O_BIN="$ROOT_DIR/target/release/O"
BACKENDS_DIR="$ROOT_DIR/backends"
EXAMPLES_DIR="$ROOT_DIR/examples"
TIMEOUT_SECONDS="${TIMEOUT_SECONDS:-10}"

passed=0
failed=0
skipped=0

run_example() {
  local file="$1"

  python3 - "$O_BIN" "$file" "$BACKENDS_DIR" "$TIMEOUT_SECONDS" <<'PY'
import subprocess
import sys

o_bin, example, backends, timeout_s = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])

try:
    result = subprocess.run(
        [
            o_bin,
            "--backend-grant",
            "backend=*:fs_read,fs_write,network,process",
            example,
            backends,
        ],
        capture_output=True,
        text=True,
        timeout=timeout_s,
    )
except subprocess.TimeoutExpired as exc:
    stdout = exc.stdout.decode() if isinstance(exc.stdout, bytes) else (exc.stdout or "")
    stderr = exc.stderr.decode() if isinstance(exc.stderr, bytes) else (exc.stderr or "")
    sys.stdout.write(stdout)
    sys.stderr.write(stderr)
    sys.stderr.write(f"[timeout] example exceeded {timeout_s:g}s\n")
    raise SystemExit(124)

sys.stdout.write(result.stdout)
sys.stderr.write(result.stderr)
raise SystemExit(result.returncode)
PY
}

run_test() {
  local name="$1"
  local file="$2"
  shift 2

  local output=""
  local status=0
  local expected=""

  if ! output="$(run_example "$file" 2>&1)"; then
    status=$?
    printf '[FAIL] %s (%s): exit %s\n%s\n' "$name" "$(basename "$file")" "$status" "$output"
    failed=$((failed + 1))
    return 0
  fi

  for expected in "$@"; do
    if ! grep -Fq -- "$expected" <<<"$output"; then
      printf '[FAIL] %s (%s): missing %q\n%s\n' "$name" "$(basename "$file")" "$expected" "$output"
      failed=$((failed + 1))
      return 0
    fi
  done

  printf '[PASS] %s\n' "$name"
  passed=$((passed + 1))
}

skip_test() {
  local name="$1"
  local reason="$2"
  printf '[SKIP] %s: %s\n' "$name" "$reason"
  skipped=$((skipped + 1))
}

run_registered_test() {
  local name="$1"
  local file="$2"

  case "$name" in
    bash_binding)
      run_test "$name" "$file" 'hello world'
      ;;
    bash_exit_code)
      run_test "$name" "$file" 'ok'
      ;;
    bash_hello)
      run_test "$name" "$file" 'hello from bash'
      ;;
    bash_multiline)
      run_test "$name" "$file" '10 + 20 = 30'
      ;;
    bindings)
      run_test "$name" "$file" '43'
      ;;
    computed_plot)
      run_test "$name" "$file" \
        '<title>O computed plot</title>' \
        '<figcaption>Figure 1. Computed at render time, embedded via OBlob(mime="image/png").</figcaption>' \
        'math is NOT defined in python[1]'
      ;;
    coordination_groups)
      run_test "$name" "$file" \
        '# Step-4 coordination primitives' \
        '<group:batch n=3' \
        'pkgs.hello'
      ;;
    env_split)
      run_test "$name" "$file" '100'
      ;;
    ephemeral)
      run_test "$name" "$file" 'EPHEMERAL: x from first bare block is NOT visible here (each bare is a fresh env)'
      ;;
    hello)
      run_test "$name" "$file" '2'
      ;;
    html_basic)
      run_test "$name" "$file" '<h1>O-lang is rendering HTML</h1>' '<p>The answer is 42.</p>'
      ;;
    html_escape)
      run_test "$name" "$file" '<p>&lt;O-lang &amp; friends&gt;</p>'
      ;;
    html_python_html)
      run_test "$name" "$file" '<h1>Outer HTML</h1>' '<strong>Hello, Lee.</strong>'
      ;;
    html_raw_roundtrip)
      run_test "$name" "$file" '<section>' '<strong>Hello, Lee.</strong>'
      ;;
    instantiate_realise_basic)
      skip_test "$name" 'requires Nix'
      ;;
    js_binding)
      run_test "$name" "$file" '42'
      ;;
    js_hello)
      run_test "$name" "$file" 'hello from js'
      ;;
    js_json)
      run_test "$name" "$file" '{"x":1,"y":2}'
      ;;
    js_multiline)
      run_test "$name" "$file" '42'
      ;;
    lazy_defer_attrs_basic)
      run_test "$name" "$file" '[request] <request eval|python|'
      ;;
    lazy_request_basic)
      run_test "$name" "$file" '# Step-3 lazy region (call form)' 'unresolved Request — no Nix call made'
      ;;
    literate_report)
      run_test "$name" "$file" \
        '# A Literate Report in .O' \
        'The answer is <strong style="color: teal">285</strong>.' \
        'Running it with `--as json` would dump the raw OValue tree.'
      ;;
    meta_eval)
      run_test "$name" "$file" '# Quote / Eval: homoiconic .O' 'OExprValue' '333'
      ;;
    nested_splice)
      run_test "$name" "$file" '42'
      ;;
    nix_*|nixos_*)
      skip_test "$name" 'requires Nix'
      ;;
    os_as_participant_basic)
      run_test "$name" "$file" \
        '# Step-4 OS-as-participant' \
        'real switch requires a live capability' \
        'current system reference:  (current_system() builtin)'
      ;;
    persist)
      run_test "$name" "$file" '42'
      ;;
    python_html_python)
      run_test "$name" "$file" 'PAGE START' '<p>The computed number is 42.</p>' 'PAGE END'
      ;;
    script)
      run_test "$name" "$file" '<h1>Hello from executable O-lang</h1>'
      ;;
    shell_hello)
      run_test "$name" "$file" 'hello from sh'
      ;;
    sql_aggregation)
      run_test "$name" "$file" $'total_rows\ttotal_points' $'3\t60'
      ;;
    sql_create_insert_select)
      run_test "$name" "$file" $'name\tage' $'Ollie\t3'
      ;;
    sql_python_sql)
      run_test "$name" "$file" 'doubled' '200'
      ;;
    sql_select)
      run_test "$name" "$file" 'result' '2'
      ;;
    trailing_expr)
      run_test "$name" "$file" '42'
      ;;
    *)
      printf '[FAIL] %s: no expectation registered\n' "$name"
      failed=$((failed + 1))
      ;;
  esac
}

if [[ ! -x "$O_BIN" ]]; then
  printf '[FAIL] missing O binary at %s\n' "$O_BIN"
  exit 1
fi

for file in "$EXAMPLES_DIR"/*.O; do
  name="$(basename "$file" .O)"
  run_registered_test "$name" "$file"
done

printf '\n%d passed, %d failed, %d skipped\n' "$passed" "$failed" "$skipped"

if (( failed > 0 )); then
  exit 1
fi
