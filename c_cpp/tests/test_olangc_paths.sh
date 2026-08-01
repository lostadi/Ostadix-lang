#!/bin/sh
set -eu

script_dir=$(CDPATH= cd "$(dirname "$0")" && pwd)
c_root=$(CDPATH= cd "$script_dir/.." && pwd)
olangc=${1:-"$c_root/olangc"}
real_cc=${2:-${OLANGC_REAL_CC:-cc}}
runner=${3:-"$c_root/O"}

case $olangc in
    */*)
        olangc_dir=$(CDPATH= cd "$(dirname "$olangc")" && pwd) || exit 1
        olangc="$olangc_dir/$(basename "$olangc")"
        ;;
    *)
        olangc=$(command -v "$olangc") || {
            echo "test_olangc_paths: olangc not found: $olangc" >&2
            exit 1
        }
        ;;
esac

case $runner in
    */*)
        runner_dir=$(CDPATH= cd "$(dirname "$runner")" && pwd) || exit 1
        runner="$runner_dir/$(basename "$runner")"
        ;;
    *)
        runner=$(command -v "$runner") || {
            echo "test_olangc_paths: interpreter not found: $runner" >&2
            exit 1
        }
        ;;
esac

case $real_cc in
    */*)
        if [ ! -x "$real_cc" ]; then
            echo "test_olangc_paths: compiler is not executable: $real_cc" >&2
            exit 1
        fi
        cc_dir=$(CDPATH= cd "$(dirname "$real_cc")" && pwd) || exit 1
        real_cc="$cc_dir/$(basename "$real_cc")"
        ;;
    *)
        resolved_cc=$(command -v "$real_cc") || {
            echo "test_olangc_paths: compiler not found: $real_cc" >&2
            exit 1
        }
        real_cc=$resolved_cc
        ;;
esac

tmp_base=${TMPDIR:-/tmp}
case_dir=$(mktemp -d "$tmp_base/olangc-path-regression.XXXXXX")
OLANGC_TMPDIR_SENTINEL="/tmp/olangc-tmpdir-evaluated-$$"
OLANGC_SHIM_SENTINEL="/tmp/olangc-shim-evaluated-$$"
OLANGC_INPUT_SENTINEL="/tmp/olangc-input-evaluated-$$"
OLANGC_OUTPUT_DIR_SENTINEL="/tmp/olangc-output-dir-evaluated-$$"
OLANGC_RESULT_SENTINEL="/tmp/olangc-result-evaluated-$$"
OLANGC_CC_PATH_SENTINEL="/tmp/olangc-cc-path-evaluated-$$"
OLANGC_CC_TEXT_SENTINEL="/tmp/olangc-cc-text-evaluated-$$"
export OLANGC_TMPDIR_SENTINEL OLANGC_SHIM_SENTINEL OLANGC_INPUT_SENTINEL
export OLANGC_OUTPUT_DIR_SENTINEL OLANGC_RESULT_SENTINEL OLANGC_CC_PATH_SENTINEL

cleanup() {
    rm -rf "$case_dir"
    rm -f "$OLANGC_TMPDIR_SENTINEL" "$OLANGC_SHIM_SENTINEL" \
        "$OLANGC_INPUT_SENTINEL" "$OLANGC_OUTPUT_DIR_SENTINEL" \
        "$OLANGC_RESULT_SENTINEL" "$OLANGC_CC_PATH_SENTINEL" \
        "$OLANGC_CC_TEXT_SENTINEL"
}
trap cleanup EXIT HUP INT TERM

assert_no_shell_evaluation() {
    for sentinel_path in \
        "$OLANGC_TMPDIR_SENTINEL" "$OLANGC_SHIM_SENTINEL" \
        "$OLANGC_INPUT_SENTINEL" "$OLANGC_OUTPUT_DIR_SENTINEL" \
        "$OLANGC_RESULT_SENTINEL" "$OLANGC_CC_PATH_SENTINEL" \
        "$OLANGC_CC_TEXT_SENTINEL"
    do
        if [ -e "$sentinel_path" ]; then
            echo "test_olangc_paths: shell evaluated path text: $sentinel_path" >&2
            exit 1
        fi
    done
}

build_tmp="$case_dir/"'tmp dir; $(touch${IFS}$OLANGC_TMPDIR_SENTINEL)'
shim_dir="$case_dir/"'shim dir; $(touch${IFS}$OLANGC_SHIM_SENTINEL)'
out_dir="$case_dir/"'output dir; $(touch${IFS}$OLANGC_OUTPUT_DIR_SENTINEL)'
input="$case_dir/"'program [x]; $(touch${IFS}$OLANGC_INPUT_SENTINEL).O'
output="$out_dir/"'result [x]; $(touch${IFS}$OLANGC_RESULT_SENTINEL)'
compiler_wrapper="$case_dir/"'cc wrapper; $(touch${IFS}$OLANGC_CC_PATH_SENTINEL)'
relative_wrapper="$case_dir/relative compiler"

mkdir -p "$build_tmp" "$out_dir"
ln -s "$c_root/../backends" "$shim_dir"
cp "$c_root/../examples/hello.O" "$input"

cat > "$compiler_wrapper" <<'EOF'
#!/bin/sh
exec "$OLANGC_REAL_CC" "$@"
EOF
chmod +x "$compiler_wrapper"
cp "$compiler_wrapper" "$relative_wrapper"

OLANGC_REAL_CC="$real_cc" \
CC="$compiler_wrapper" \
TMPDIR="$build_tmp" \
    "$olangc" "$input" -o "$output" --shim-dir "$shim_dir"

program_output=$("$output")
assert_no_shell_evaluation
case $program_output in
    *2*) ;;
    *)
        echo "test_olangc_paths: expected generated program output to contain 2" >&2
        exit 1
        ;;
esac

# Locating the per-executable shim directory is based on the running binary,
# not the caller's current directory or whether argv[0] contains a slash.
path_cwd="$case_dir/path invocation cwd"
mkdir -p "$path_cwd"
output_name=$(basename "$output")
path_output=$(
    cd "$path_cwd"
    PATH="$out_dir:$PATH" "$output_name"
)
case $path_output in
    *2*) ;;
    *)
        echo "test_olangc_paths: PATH invocation did not locate executable shims" >&2
        exit 1
        ;;
esac

# A slash-containing relative CC path is resolved before olangc changes the
# compiler child's working directory.
relative_out_dir="$case_dir/relative output"
mkdir -p "$relative_out_dir"
(
    cd "$case_dir"
    OLANGC_REAL_CC="$real_cc" \
    CC="./relative compiler" \
    TMPDIR="$build_tmp" \
        "$olangc" "$input" -o "$relative_out_dir/program" --shim-dir "$shim_dir"
)
relative_output=$("$relative_out_dir/program")
case $relative_output in
    *2*) ;;
    *)
        echo "test_olangc_paths: relative CC path produced wrong output" >&2
        exit 1
        ;;
esac

# Missing required shim assets are a compile-time error; olangc must never
# fabricate a protocol-incompatible placeholder or publish an executable.
missing_out="$case_dir/missing-output/program"
mkdir -p "$(dirname "$missing_out")"
missing_log="$case_dir/missing-shim.log"
if CC="$real_cc" TMPDIR="$build_tmp" \
    "$olangc" "$input" -o "$missing_out" --shim-dir "$case_dir/no such shims" \
    >"$missing_log" 2>&1; then
    echo "test_olangc_paths: missing shim directory unexpectedly compiled" >&2
    exit 1
fi
if [ -e "$missing_out" ]; then
    echo "test_olangc_paths: missing-shim compile published an executable" >&2
    exit 1
fi
if ! grep -q 'missing required shim asset' "$missing_log"; then
    echo "test_olangc_paths: missing-shim diagnostic was not explicit" >&2
    cat "$missing_log" >&2
    exit 1
fi

# Bundle publication stages every shim before touching the live pair. A fault
# after the first staged shim must preserve an existing executable and its
# complete prior shim tree byte-for-byte.
blocked_out_dir="$case_dir/blocked publication"
blocked_output="$blocked_out_dir/program"
blocked_log="$case_dir/blocked-publication.log"
mkdir -p "$blocked_out_dir"
CC="$real_cc" TMPDIR="$build_tmp" \
    "$olangc" "$input" -o "$blocked_output" --shim-dir "$shim_dir"
cp "$blocked_output" "$case_dir/previous-executable"
cp -R "${blocked_output}.shims" "$case_dir/previous-shims"
if OLANGC_TEST_FAIL_AFTER_FIRST_PUBLISHED_SHIM=1 \
    CC="$real_cc" TMPDIR="$build_tmp" \
    "$olangc" "$input" -o "$blocked_output" --shim-dir "$shim_dir" \
    >"$blocked_log" 2>&1; then
    echo "test_olangc_paths: partial-shim publication unexpectedly succeeded" >&2
    exit 1
fi
if ! cmp -s "$blocked_output" "$case_dir/previous-executable"; then
    echo "test_olangc_paths: partial-shim failure replaced prior executable" >&2
    exit 1
fi
if ! diff -r "${blocked_output}.shims" "$case_dir/previous-shims" >/dev/null; then
    echo "test_olangc_paths: partial-shim failure changed prior shim tree" >&2
    exit 1
fi
if ! grep -q 'failed to publish output bundle' "$blocked_log"; then
    echo "test_olangc_paths: bundle-publication diagnostic was not explicit" >&2
    cat "$blocked_log" >&2
    exit 1
fi

# A failure after the new shim tree becomes live but before the executable
# rename must roll the shim swap back and preserve the same old bundle.
if OLANGC_TEST_FAIL_EXEC_RENAME=1 \
    CC="$real_cc" TMPDIR="$build_tmp" \
    "$olangc" "$input" -o "$blocked_output" --shim-dir "$shim_dir" \
    >"$blocked_log" 2>&1; then
    echo "test_olangc_paths: pre-executable-commit fault unexpectedly succeeded" >&2
    exit 1
fi
if ! cmp -s "$blocked_output" "$case_dir/previous-executable" \
    || ! diff -r "${blocked_output}.shims" "$case_dir/previous-shims" >/dev/null; then
    echo "test_olangc_paths: executable-commit failure changed prior bundle" >&2
    exit 1
fi

# A malformed backend frame must make the generated executable exit nonzero.
bad_shims="$case_dir/bad shims"
bad_program="$out_dir/bad-neighbor"
mkdir -p "$bad_shims"
cp -R "$c_root/../backends/." "$bad_shims/"
cat > "$bad_shims/python_shim.py" <<'PY'
#!/usr/bin/env python3
import sys

header = sys.stdin.buffer.read(4)
if len(header) == 4:
    length = int.from_bytes(header, "big")
    sys.stdin.buffer.read(length)
sys.stdout.buffer.write(b"\x00\x00\x00\x01x")
sys.stdout.buffer.flush()
PY
chmod +x "$bad_shims/python_shim.py"
native_bad_log="$case_dir/native-bad-frame.log"
if "$runner" "$input" "$bad_shims" >"$native_bad_log" 2>&1; then
    echo "test_olangc_paths: interpreter accepted malformed backend frame" >&2
    exit 1
fi
if ! grep -q 'bad CBOR frame' "$native_bad_log"; then
    echo "test_olangc_paths: interpreter malformed-frame diagnostic was not explicit" >&2
    cat "$native_bad_log" >&2
    exit 1
fi
CC="$real_cc" TMPDIR="$build_tmp" \
    "$olangc" "$input" -o "$bad_program" --shim-dir "$bad_shims"
bad_log="$case_dir/bad-frame.log"
if "$bad_program" >"$bad_log" 2>&1; then
    echo "test_olangc_paths: malformed backend frame exited successfully" >&2
    exit 1
fi

# Two binaries in one directory retain independent backend assets. Publishing
# the deliberately broken neighbor above must not alter the first program.
program_output_after_neighbor=$("$output")
case $program_output_after_neighbor in
    *2*) ;;
    *)
        echo "test_olangc_paths: same-directory neighbor clobbered first binary shims" >&2
        exit 1
        ;;
esac
if [ ! -d "${output}.shims" ] || [ ! -d "${bad_program}.shims" ]; then
    echo "test_olangc_paths: per-executable shim directories were not published" >&2
    exit 1
fi
if cmp -s "${output}.shims/python_shim.py" "${bad_program}.shims/python_shim.py"; then
    echo "test_olangc_paths: distinct binaries unexpectedly share shim content" >&2
    exit 1
fi
if ! grep -q 'bad CBOR frame' "$bad_log"; then
    echo "test_olangc_paths: malformed-frame diagnostic was not explicit" >&2
    cat "$bad_log" >&2
    exit 1
fi

# The source byte-array generator must preserve non-ASCII bytes and compile
# programs larger than ISO C's 4095-character string-literal minimum.
large_input="$case_dir/large utf8.O"
large_out_dir="$case_dir/large output"
mkdir -p "$large_out_dir"
python3 - "$large_input" <<'PY'
from pathlib import Path
import sys

source = 'python^(\n__oval_result__ = "§3"\n# ' + ('x' * 5000) + '\n)_python\n'
Path(sys.argv[1]).write_text(source, encoding='utf-8')
PY
OLANGC_WARNINGS_AS_ERRORS=1 CC="$real_cc" TMPDIR="$build_tmp" \
    "$olangc" "$large_input" -o "$large_out_dir/program" --shim-dir "$shim_dir"
large_output=$("$large_out_dir/program")
case $large_output in
    *§3*) ;;
    *)
        echo "test_olangc_paths: large UTF-8 source was not byte-exact" >&2
        exit 1
        ;;
esac

# The executable and matching <executable>.shims directory are one bundle.
# Copying only the executable must fail when it first needs an external backend.
alone_dir="$case_dir/executable only"
mkdir -p "$alone_dir"
cp "$output" "$alone_dir/program"
if "$alone_dir/program" >"$case_dir/executable-only.log" 2>&1; then
    echo "test_olangc_paths: executable without matching shims unexpectedly ran" >&2
    exit 1
fi

# A CC value that is shell text must be treated as one (invalid) executable
# name. The command must fail without running the injected touch command.
bad_cc="cc; touch $OLANGC_CC_TEXT_SENTINEL; false"
if CC="$bad_cc" TMPDIR="$build_tmp" \
    "$olangc" "$input" -o "$out_dir/rejected" --shim-dir "$shim_dir" \
    >/dev/null 2>&1; then
    echo "test_olangc_paths: shell-text CC unexpectedly succeeded" >&2
    exit 1
fi
assert_no_shell_evaluation

echo "olangc argv/path, bundle, source-byte, and backend-error safety: OK"
