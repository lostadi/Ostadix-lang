#!/usr/bin/env bash
# Build and exercise the minimal hosted Docker profile without writing into the
# source checkout. CI may set OSTADIX_DOCKER_IMAGE or reuse a previously built
# image with OSTADIX_DOCKER_SKIP_BUILD=1.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "error: smoke-docker.sh must run inside a Git worktree" >&2
    exit 2
}
cd "$repo_root"

docker_bin="${OSTADIX_DOCKER_BIN:-docker}"
image="${OSTADIX_DOCKER_IMAGE:-o-lang:docker-smoke}"
skip_build="${OSTADIX_DOCKER_SKIP_BUILD:-0}"

case "$skip_build" in
    0|1) ;;
    *)
        echo "error: OSTADIX_DOCKER_SKIP_BUILD must be 0 or 1" >&2
        exit 2
        ;;
esac

if ! command -v "$docker_bin" >/dev/null 2>&1; then
    echo "error: Docker CLI not found: $docker_bin" >&2
    exit 2
fi
if ! "$docker_bin" info >/dev/null 2>&1; then
    echo "error: Docker engine is unavailable" >&2
    exit 2
fi

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ostadix-docker-smoke.XXXXXX")"
cleanup() {
    rm -rf -- "$work_dir"
}
trap cleanup EXIT HUP INT TERM

require_exact() {
    local label=$1 expected=$2 actual=$3
    if [[ "$actual" != "$expected" ]]; then
        printf 'error: %s expected %q, got %q\n' "$label" "$expected" "$actual" >&2
        exit 1
    fi
}

if [[ "$skip_build" == 0 ]]; then
    "$docker_bin" build \
        --file "$repo_root/Dockerfile" \
        --tag "$image" \
        "$repo_root"
fi
printf 'Docker minimal image build: PASS\n'

"$docker_bin" run --rm \
    --entrypoint /bin/sh \
    "$image" -eu -c '
        test "$O_BACKENDS_DIR" = /opt/o-lang/backends
        test -f "$O_BACKENDS_DIR/python_shim.py"
        command -v python3 >/dev/null
        if command -v rustc >/dev/null 2>&1; then exit 1; fi
    '
printf 'Docker minimal runtime profile: PASS\n'

hello_output="$(
    "$docker_bin" run --rm \
        --mount "type=bind,src=$repo_root,dst=/work,readonly" \
        "$image" examples/hello.O
)"
require_exact "hello.O output" "[number] 2" "$hello_output"
printf 'Docker read-only hello.O: PASS\n'

project_output="$work_dir/project.O"
"$docker_bin" run --rm \
    --mount "type=bind,src=$repo_root,dst=/work,readonly" \
    --entrypoint o-link \
    "$image" --project . --stdout >"$project_output"
grep -Fq '# O-PROJECT-BUNDLE-V1 BEGIN' "$project_output"
grep -Fq 'No project route was executed.' "$project_output"
grep -Fq 'rust-build' "$project_output"
printf 'Docker inert repository project lift: PASS\n'

literal_fixture="$repo_root/examples/docker_literal"
literal_output="$(
    "$docker_bin" run --rm \
        --mount "type=bind,src=$literal_fixture,dst=/work,readonly" \
        --entrypoint o-link \
        "$image" . -o /tmp/docker-literal.O
)"
require_exact "Python literal fixture output" "42" "$literal_output"
printf 'Docker overridden-entrypoint shim discovery: PASS\n'
printf 'Docker Python literal execution: PASS\n'
