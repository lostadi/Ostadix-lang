#!/usr/bin/env bash
set -euo pipefail

run_example() {
  local example="$1"
  local output

  if ! output="$(./target/release/O "examples/${example}.O" backends/ 2>&1)"; then
    printf 'Example %s failed:\n%s\n' "$example" "$output" >&2
    return 1
  fi

  printf '%s\n' "$output"
}

assert_example_matches() {
  local example="$1"
  local pattern="$2"
  local output

  output="$(run_example "$example")"
  grep -q -- "$pattern" <<<"$output"
}

assert_example_matches bindings '^43$'
assert_example_matches nested_splice '^42$'
assert_example_matches html_escape '&lt;O-lang &amp; friends&gt;'

html_roundtrip_output="$(run_example html_raw_roundtrip)"
grep -q '<section>' <<<"$html_roundtrip_output"
grep -q '<strong>Hello, Lee.</strong>' <<<"$html_roundtrip_output"

sql_select_output="$(run_example sql_select)"
grep -q '^result$' <<<"$sql_select_output"
grep -q '^2$' <<<"$sql_select_output"

sql_create_insert_select_output="$(run_example sql_create_insert_select)"
grep -q '^name[[:space:]]age$' <<<"$sql_create_insert_select_output"
grep -q '^Ollie[[:space:]]3$' <<<"$sql_create_insert_select_output"

sql_python_sql_output="$(run_example sql_python_sql)"
grep -q '^doubled$' <<<"$sql_python_sql_output"
grep -q '^200$' <<<"$sql_python_sql_output"

sql_aggregation_output="$(run_example sql_aggregation)"
grep -q '^total_rows[[:space:]]total_points$' <<<"$sql_aggregation_output"
grep -q '^3[[:space:]]60$' <<<"$sql_aggregation_output"

if command -v nix >/dev/null 2>&1; then
  nix_basic_output="$(run_example nix_basic)"
  grep -q 'Nix inside O-lang' <<<"$nix_basic_output"
  grep -q 'O-lang Nix bridge' <<<"$nix_basic_output"
  grep -q '42' <<<"$nix_basic_output"

  nix_python_html_output="$(run_example nix_python_html)"
  grep -q 'Nix → Python → HTML' <<<"$nix_python_html_output"
  grep -q 'Nix-born value says answer=42' <<<"$nix_python_html_output"

  nix_storepath_output="$(run_example nix_storepath)"
  grep -q 'O-lang StorePath test' <<<"$nix_storepath_output"
  grep -q '/nix/store/' <<<"$nix_storepath_output"
  grep -q 'hello-from-o-lang.txt' <<<"$nix_storepath_output"
  grep -q 'class="o-store-path"' <<<"$nix_storepath_output"

  nix_storepath_python_output="$(run_example nix_storepath_python)"
  grep -q 'Python reads Nix StorePath' <<<"$nix_storepath_python_output"
  grep -q 'Hello from O-lang + Nix' <<<"$nix_storepath_python_output"

  coordination_output="$(run_example coordination_groups)"
  grep -q 'Step-4 coordination primitives' <<<"$coordination_output"
  grep -q '<group:batch n=3' <<<"$coordination_output"
  grep -q 'pkgs.hello' <<<"$coordination_output"
else
  echo "(skipping nix-backed examples -- nix not installed)"
fi

assert_example_matches js_hello 'hello from js'
assert_example_matches js_binding '^42$'
assert_example_matches js_json '{"x":1,"y":2}'
assert_example_matches js_multiline '^42$'

assert_example_matches bash_hello '^hello from bash$'
echo 'PASS: bash_hello'
assert_example_matches bash_binding '^hello world$'
echo 'PASS: bash_binding'
assert_example_matches bash_multiline '^10 + 20 = 30$'
echo 'PASS: bash_multiline'
assert_example_matches shell_hello '^hello from sh$'
echo 'PASS: shell_hello'
assert_example_matches bash_exit_code '^ok$'
echo 'PASS: bash_exit_code'

echo "All O-lang smoke tests passed."
