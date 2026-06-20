#!/usr/bin/env bash
set -euo pipefail

PASS=0
FAIL=0
RUN_EXIT=0

# --- Paths and scratch space --- #

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ARTIFACT_DIR="$ROOT/tests/.cli_test_artifacts"
STDOUT_FILE="$ARTIFACT_DIR/stdout"
STDERR_FILE="$ARTIFACT_DIR/stderr"

cleanup() {
    rm -rf "$ARTIFACT_DIR"
}
trap cleanup EXIT

mkdir -p "$ARTIFACT_DIR"

O_BIN="./target/release/O"
OLANGC_BIN="./target/release/olangc"
OCOREC_BIN="./target/release/ocorec"
if [ -x ./target/release/olink ]; then
    OLINK_BIN="./target/release/olink"
else
    OLINK_BIN="./target/release/o-link"
fi

for bin in "$O_BIN" "$OLANGC_BIN" "$OCOREC_BIN" "$OLINK_BIN"; do
    if [ ! -x "$bin" ]; then
        echo "Missing executable: $bin" >&2
        exit 1
    fi
done

# --- Test runner helpers --- #

show_last_output() {
    if [ -s "$STDOUT_FILE" ]; then
        echo "--- stdout ---"
        cat "$STDOUT_FILE"
    fi
    if [ -s "$STDERR_FILE" ]; then
        echo "--- stderr ---"
        cat "$STDERR_FILE"
    fi
}

check_ocore_compile() {
    local desc="$1"
    local source="$ARTIFACT_DIR/smoke.oc"
    local object="$ARTIFACT_DIR/smoke.o"

    run_command "$OCOREC_BIN" "$source" --emit obj -o "$object"
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "$desc" "(ocorec compilation failed with exit $RUN_EXIT)"
        return
    fi
    if [ ! -f "$object" ]; then
        fail "$desc" "(object was not created)"
        return
    fi
    if file "$object" | grep -Eq 'ELF 64-bit LSB relocatable, x86-64'; then
        pass "$desc"
    else
        fail "$desc" "(output is not an x86-64 ELF relocatable object)"
    fi
}

pass() {
    local desc="$1"
    echo "PASS: $desc"
    ((PASS++)) || true
}

fail() {
    local desc="$1"
    shift || true
    echo "FAIL: $desc"
    if [ "$#" -gt 0 ]; then
        printf '%s\n' "$@"
    fi
    show_last_output
    ((FAIL++)) || true
}

run_command() {
    : >"$STDOUT_FILE"
    : >"$STDERR_FILE"
    RUN_EXIT=0
    "$@" >"$STDOUT_FILE" 2>"$STDERR_FILE" || RUN_EXIT=$?
}

check() {
    local desc="$1"
    local expected_exit="$2"
    shift 2

    run_command "$@"
    if [ "$RUN_EXIT" -eq "$expected_exit" ]; then
        pass "$desc"
    else
        fail "$desc" "(expected exit $expected_exit, got $RUN_EXIT)"
    fi
}

check_stdout_contains() {
    local desc="$1"
    local expected_exit="$2"
    local pattern="$3"
    shift 3

    run_command "$@"
    if [ "$RUN_EXIT" -ne "$expected_exit" ]; then
        fail "$desc" "(expected exit $expected_exit, got $RUN_EXIT)"
        return
    fi
    if grep -Eq -- "$pattern" "$STDOUT_FILE"; then
        pass "$desc"
    else
        fail "$desc" "(stdout missing pattern: $pattern)"
    fi
}

check_stderr_contains() {
    local desc="$1"
    local expected_exit="$2"
    local pattern="$3"
    shift 3

    run_command "$@"
    if [ "$RUN_EXIT" -ne "$expected_exit" ]; then
        fail "$desc" "(expected exit $expected_exit, got $RUN_EXIT)"
        return
    fi
    if grep -Eq -- "$pattern" "$STDERR_FILE"; then
        pass "$desc"
    else
        fail "$desc" "(stderr missing pattern: $pattern)"
    fi
}

check_nonzero_stderr_contains() {
    local desc="$1"
    local pattern="$2"
    shift 2

    run_command "$@"
    if [ "$RUN_EXIT" -eq 0 ]; then
        fail "$desc" "(expected non-zero exit, got 0)"
        return
    fi
    if grep -Eq -- "$pattern" "$STDERR_FILE"; then
        pass "$desc"
    else
        fail "$desc" "(stderr missing pattern: $pattern; exit $RUN_EXIT)"
    fi
}

check_olangc_compile_and_run() {
    local desc="$1"
    local output_bin="$ARTIFACT_DIR/hello_compiled"

    run_command "$OLANGC_BIN" examples/hello.O -o "$output_bin"
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "$desc" "(olangc compilation failed with exit $RUN_EXIT)"
        return
    fi
    if [ ! -x "$output_bin" ]; then
        fail "$desc" "(compiled binary was not created at $output_bin)"
        return
    fi

    run_command "$output_bin"
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "$desc" "(compiled binary failed with exit $RUN_EXIT)"
        return
    fi
    if grep -Eq '^2$' "$STDOUT_FILE"; then
        pass "$desc"
    else
        fail "$desc" "(compiled binary stdout missing expected output)"
    fi
}

# --- Test inputs --- #

INVALID_SOURCE="$ARTIFACT_DIR/invalid.O"
cat >"$INVALID_SOURCE" <<'EOF'
python^(
__oval_result__ = 2
EOF

cat >"$ARTIFACT_DIR/smoke.oc" <<'EOF'
module smoke;
@export @no_mangle
unsafe fn kernel_main() -> never {
    unsafe { outb(0x3f8, b'O'); }
    loop { unsafe { halt(); } }
}
EOF

# --- CLI integration tests --- #

check_stderr_contains "O with no args shows usage error" 1 'Usage:|missing input file' "$O_BIN"
check_nonzero_stderr_contains "O missing file errors" 'failed to read input file|No such file' "$O_BIN" nonexistent.O backends/
check_stdout_contains "O runs hello.O" 0 '^2$' "$O_BIN" examples/hello.O backends/
check_stdout_contains "O --help shows usage" 0 '^Usage:' "$O_BIN" --help
check_stdout_contains "olangc --help shows usage" 0 '^Usage: olangc' "$OLANGC_BIN" --help
check_olangc_compile_and_run "olangc compiles hello.O and the output runs"
check_stdout_contains "ocorec --help shows usage" 0 '^Usage: ocorec' "$OCOREC_BIN" --help
check_ocore_compile "ocorec emits x86-64 freestanding ELF object"
check_stdout_contains "olink help shows usage" 0 'Usage: (olink|o-link)' "$OLINK_BIN" --help
check_nonzero_stderr_contains "O invalid syntax exits with an error" 'failed to parse \.O source|error:' "$O_BIN" "$INVALID_SOURCE" backends/

echo ""
echo "Results: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
