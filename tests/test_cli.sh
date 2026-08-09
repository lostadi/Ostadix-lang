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
OGIT_BIN="./target/release/ogit"
if [ -x ./target/release/olink ]; then
    OLINK_BIN="./target/release/olink"
else
    OLINK_BIN="./target/release/o-link"
fi
OUNLINK_BIN="./target/release/o-unlink"
O_CLI="./scripts/o-cli.sh"
O_KERNEL_CLI="./scripts/o-kernel.sh"
O_KERNEL_QEMU_RUNNER="./ocore/kernel/run-qemu.sh"

for bin in "$O_BIN" "$OLANGC_BIN" "$OCOREC_BIN" "$OGIT_BIN" "$OLINK_BIN" "$OUNLINK_BIN"; do
    if [ ! -x "$bin" ]; then
        echo "Missing executable: $bin" >&2
        exit 1
    fi
done
for script in "$O_CLI" "$O_KERNEL_CLI" "$O_KERNEL_QEMU_RUNNER"; do
    if [ ! -x "$script" ]; then
        echo "Missing executable: $script" >&2
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

check_olangc_capability_compile_and_run() {
    local desc="$1"
    local output_bin="$ARTIFACT_DIR/capability_compiled"

    run_command "$OLANGC_BIN" "$CAPABILITY_SOURCE" -o "$output_bin"
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "$desc" "(olangc capability compilation failed with exit $RUN_EXIT)"
        return
    fi
    run_command "$output_bin"
    if [ "$RUN_EXIT" -eq 0 ] && grep -Eq '^0$' "$STDOUT_FILE"; then
        pass "$desc"
    else
        fail "$desc" "(compiled capability program did not print 0)"
    fi
}

check_olangc_grounding_report() {
    local desc="$1"

    run_command "$OLANGC_BIN" examples/hello.O \
        --target ir \
        --grounding \
        --world-id desk \
        --world-epoch 4
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "$desc" "(grounding report failed with exit $RUN_EXIT)"
        return
    fi
    if ! grep -Fqx -- 'world desk@4' "$STDOUT_FILE"; then
        fail "$desc" "(grounding report omitted the caller-bound World epoch)"
        return
    fi
    if ! grep -Fqx -- 'governed-effects none' "$STDOUT_FILE"; then
        fail "$desc" "(grounding report did not preserve the empty governed-effect set)"
        return
    fi
    if ! grep -Eq -- '^ambient-effects P[0-9]+ reads=\[[^]]*HostWorld[^]]*\] writes=\[[^]]*HostWorld[^]]*\] hostworld=residual$' "$STDOUT_FILE"; then
        fail "$desc" "(grounding report did not expose residual HostWorld reads and writes)"
        return
    fi
    pass "$desc"
}

check_olangc_schedule_explanation() {
    local desc="$1"
    local source="$ARTIFACT_DIR/explain-do-not-run.O"
    local marker="$ARTIFACT_DIR/explain-executed"

    cat >"$source" <<EOF
python^(
from pathlib import Path
Path(r"$marker").write_text("executed")
__oval_result__ = 2
)_python
EOF

    run_command "$OLANGC_BIN" "$source" --target ir --explain-schedule
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "$desc" "(schedule explanation failed with exit $RUN_EXIT)"
        return
    fi
    for pattern in \
        '^; ExecutionAdmission oexec\.admission/v3$' \
        '^binding lowered-oir-sha256=' \
        '^runtime-snapshot kind=inspection dispatch-context=inspection-only$' \
        '^; ScheduleRealizability oexec\.realizability/v1$' \
        '^realizability status=inspection-only execution-realizable=unknown dispatch=not-run scope=local-worker-static-wave worker-count-covers-static-wave=not-applicable runtime-readiness=unknown placement-lease=none observed-overlap=not-run source=machine-default available-parallelism=[1-9][0-9]* admitted-static-max-wave-width=[0-9]+ admitted-max-local-worker-wave-width=0 selected-workers=1$' \
        '^; SchedulePrediction oexec\.schedule-prediction/v1$' \
        '^schedule-prediction schema=oexec\.schedule-prediction/v1 status=admitted-static provenance=evidence-bound-admission model=unit-cost-shim-hosted-tasks admission-sha256=[0-9a-f]{64} task-count=1 predicted-width=1 predicted-span=1 span-unit=hosted-task-layers$' \
        '^schedule-prediction-layer index=1 operations=\[P[0-9]+\]$' \
        '^operation P[0-9]+ admitted=yes ' \
        '^  dispatch lane=coordinator adapter=coordinator/v1 semantics=strict-equivalent preparation=coordinator-owned$' \
        '^wave 0 \[' \
        '^admission-note waves describe the legal static frontier, not observed dispatch$' \
        '^admission-note admitted maximum local-worker wave width is a static Kahn-wave capacity heuristic, not a bound on the completion-driven dynamic frontier$' \
        '^admission-note dispatch adapter IDs are evidence-bound; runtime preparation may validate but cannot reclassify an operation$' \
        '^admission-note local-worker runtime uses a fixed-size per-run pool with per-completion wakeups; static waves are not pool batches or capacity promises$' \
        '^admission-note verified-pure infallible local-worker outputs may provisionally unlock only equally safe worker dependents; dependent NodeStarted may precede producer NodeFinished, durable settlement remains serial-topological, and any earlier failure revokes provisionally published outputs$'; do
        if ! grep -Eq -- "$pattern" "$STDOUT_FILE"; then
            fail "$desc" "(schedule explanation omitted pattern: $pattern)"
            return
        fi
    done
    if [ -e "$marker" ]; then
        fail "$desc" "(--explain-schedule executed the inspected backend)"
        return
    fi

    cat >"$source" <<EOF
autonomous(batch(
    python^(
from pathlib import Path
Path(r"$marker").write_text("executed")
__oval_result__ = 1
    )_python,
    python^(
from pathlib import Path
Path(r"$marker").write_text("executed")
__oval_result__ = 2
    )_python
))
EOF
    run_command "$OLANGC_BIN" "$source" --target ir --explain-schedule --workers 1
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "$desc" "(autonomous realizability explanation failed with exit $RUN_EXIT)"
        return
    fi
    if ! grep -Eq -- '^realizability status=inspection-only execution-realizable=unknown dispatch=not-run scope=local-worker-static-wave worker-count-covers-static-wave=no runtime-readiness=unknown placement-lease=none observed-overlap=not-run source=cli-override available-parallelism=[1-9][0-9]* admitted-static-max-wave-width=[0-9]+ admitted-max-local-worker-wave-width=2 selected-workers=1$' "$STDOUT_FILE"; then
        fail "$desc" "(schedule explanation omitted the two-worker capacity marker)"
        return
    fi
    if ! grep -Eq -- '^schedule-prediction schema=oexec\.schedule-prediction/v1 status=admitted-static provenance=evidence-bound-admission model=unit-cost-shim-hosted-tasks admission-sha256=[0-9a-f]{64} task-count=2 predicted-width=2 predicted-span=1 span-unit=hosted-task-layers$' "$STDOUT_FILE"; then
        fail "$desc" "(schedule explanation omitted the admitted hosted-task prediction)"
        return
    fi
    if [ "$(grep -Ec '^  dispatch lane=local-worker adapter=autonomous-ephemeral-shim/v1 semantics=explicit-autonomous-unordered preparation=deferred-materialized-input-check$' "$STDOUT_FILE")" -ne 2 ]; then
        fail "$desc" "(schedule explanation did not admit both autonomous members to the local-worker lane)"
        return
    fi
    if [ -e "$marker" ]; then
        fail "$desc" "(--explain-schedule executed an autonomous backend)"
        return
    fi
    pass "$desc"
}

check_olink_hardened_round_trip() {
    local source="$ARTIFACT_DIR/link-source"
    local expected="$ARTIFACT_DIR/link-expected"
    local restored="$ARTIFACT_DIR/link-restored"
    local combined="$ARTIFACT_DIR/linked.O"

    mkdir -p "$source/src" "$expected/src"
    printf '%s' 'value = "$HOME )_python[0] python^("' >"$source/src/main.py"
    cp "$source/src/main.py" "$expected/src/main.py"
    printf '%s\n' 'ignored.py' >"$source/.olinkignore"
    printf '%s\n' 'ignored = true' >"$source/ignored.py"
    printf '%s\n' 'extensionless' >"$source/README"
    cp "$source/README" "$expected/README"
    printf '\377\376\000' >"$source/binary.py"

    run_command "$OLINK_BIN" --literal "$source" -o "$combined"
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "o-link hardening round-trip" "(o-link failed with exit $RUN_EXIT)"
        return
    fi
    if ! grep -Eq 'warning: skipped [0-9]+ path[s]? .*\.olinkignore' "$STDERR_FILE" \
        || ! grep -Eq 'warning: skipped [0-9]+ path[s]? \(not UTF-8 text\)' "$STDERR_FILE" \
        || ! grep -Eq 'o-link scan: 2 selected, [0-9]+ skipped' "$STDERR_FILE"; then
        fail "o-link hardening round-trip" "(skip warnings or summary are incomplete)"
        return
    fi
    if grep -Fq "$source" "$combined"; then
        fail "o-link hardening round-trip" "(combined markers contain an absolute source path)"
        return
    fi

    run_command "$OUNLINK_BIN" "$combined" -o "$restored"
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "o-link hardening round-trip" "(o-unlink failed with exit $RUN_EXIT)"
        return
    fi
    if diff -r "$expected" "$restored" >"$STDOUT_FILE" 2>"$STDERR_FILE"; then
        pass "o-link hardening round-trip"
    else
        fail "o-link hardening round-trip" "(restored tree differs from selected input tree)"
    fi
}

setup_kernel_qemu_runner_fixture() {
    KERNEL_RUNNER_ROOT="$ARTIFACT_DIR/kernel-runner-root"
    KERNEL_RUNNER="$KERNEL_RUNNER_ROOT/ocore/kernel/run-qemu.sh"
    KERNEL_BUILD_LOG="$ARTIFACT_DIR/kernel-build.log"
    KERNEL_QEMU_ARGS_LOG="$ARTIFACT_DIR/kernel-qemu-args.log"
    KERNEL_RUNNER_HOME="$KERNEL_RUNNER_ROOT/operator-home"
    KERNEL_QEMU_STUB="$KERNEL_RUNNER_ROOT/bin/qemu-system-x86_64"

    mkdir -p \
        "$KERNEL_RUNNER_ROOT/ocore/kernel" \
        "$KERNEL_RUNNER_ROOT/bin" \
        "$KERNEL_RUNNER_HOME"
    cp "$O_KERNEL_QEMU_RUNNER" "$KERNEL_RUNNER"

    cat >"$KERNEL_RUNNER_ROOT/ocore/kernel/build.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: "${KERNEL_BUILD_LOG:?}"
: "${OCORE_BUILD_DIR:?}"
: "${OCORE_PROBE_MODE:?}"
printf 'mode=%s\nbuild-dir=%s\n' \
    "$OCORE_PROBE_MODE" "$OCORE_BUILD_DIR" >"$KERNEL_BUILD_LOG"
mkdir -p "$OCORE_BUILD_DIR"
: >"$OCORE_BUILD_DIR/kernel.elf"
echo "stub kernel build"
if [[ "$OCORE_PROBE_MODE" == "16" ]]; then
    echo "m5-sha256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
fi
EOF

    cat >"$KERNEL_QEMU_STUB" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: "${KERNEL_QEMU_ARGS_LOG:?}"
printf '%s\n' "$@" >"$KERNEL_QEMU_ARGS_LOG"
EOF
    chmod +x \
        "$KERNEL_RUNNER" \
        "$KERNEL_RUNNER_ROOT/ocore/kernel/build.sh" \
        "$KERNEL_QEMU_STUB"
}

check_kernel_qemu_mode_and_build_dir_propagation() {
    local mode
    local build_dir

    for mode in 0 16; do
        build_dir="$KERNEL_RUNNER_ROOT/build-mode-$mode"
        rm -f "$KERNEL_BUILD_LOG" "$KERNEL_QEMU_ARGS_LOG"
        run_command env \
            HOME="$KERNEL_RUNNER_HOME" \
            KERNEL_BUILD_LOG="$KERNEL_BUILD_LOG" \
            KERNEL_QEMU_ARGS_LOG="$KERNEL_QEMU_ARGS_LOG" \
            OCORE_BUILD_DIR="$build_dir" \
            OCORE_PROBE_MODE="$mode" \
            OCORE_QEMU_BIN="$KERNEL_QEMU_STUB" \
            "$KERNEL_RUNNER"
        if [[ "$RUN_EXIT" -ne 0 ]]; then
            fail "kernel QEMU runner propagates interactive modes and build directories" \
                "(mode $mode exited $RUN_EXIT)"
            return
        fi
        if ! grep -Fqx -- "mode=$mode" "$KERNEL_BUILD_LOG" \
            || ! grep -Fqx -- "build-dir=$build_dir" "$KERNEL_BUILD_LOG"; then
            fail "kernel QEMU runner propagates interactive modes and build directories" \
                "(mode or build directory was not propagated to build.sh)"
            return
        fi
        if [[ ! -s "$KERNEL_QEMU_ARGS_LOG" ]]; then
            fail "kernel QEMU runner propagates interactive modes and build directories" \
                "(QEMU stub was not launched for mode $mode)"
            return
        fi
    done
    pass "kernel QEMU runner propagates interactive modes and build directories"
}

check_kernel_qemu_runner_rejects_direct_args() {
    local build_dir="$KERNEL_RUNNER_ROOT/direct-arg-build"

    rm -f "$KERNEL_BUILD_LOG" "$KERNEL_QEMU_ARGS_LOG"
    run_command env \
        HOME="$KERNEL_RUNNER_HOME" \
        KERNEL_BUILD_LOG="$KERNEL_BUILD_LOG" \
        KERNEL_QEMU_ARGS_LOG="$KERNEL_QEMU_ARGS_LOG" \
        OCORE_BUILD_DIR="$build_dir" \
        OCORE_PROBE_MODE=0 \
        OCORE_QEMU_BIN="$KERNEL_QEMU_STUB" \
        "$KERNEL_RUNNER" unexpected
    if [[ "$RUN_EXIT" -eq 2 ]] \
        && grep -Fq -- 'run-qemu.sh does not accept arguments' "$STDERR_FILE" \
        && [[ ! -e "$KERNEL_BUILD_LOG" ]] \
        && [[ ! -e "$KERNEL_QEMU_ARGS_LOG" ]]; then
        pass "kernel QEMU runner rejects direct arguments before side effects"
    else
        fail "kernel QEMU runner rejects direct arguments before side effects" \
            "(expected exit 2 without invoking build or QEMU)"
    fi
}

check_kernel_qemu_runner_preflights_before_build() {
    local build_dir="$KERNEL_RUNNER_ROOT/preflight-build"
    local missing_qemu="$KERNEL_RUNNER_ROOT/bin/missing-qemu"

    rm -f "$KERNEL_BUILD_LOG" "$KERNEL_QEMU_ARGS_LOG" "$missing_qemu"
    run_command env \
        HOME="$KERNEL_RUNNER_HOME" \
        KERNEL_BUILD_LOG="$KERNEL_BUILD_LOG" \
        KERNEL_QEMU_ARGS_LOG="$KERNEL_QEMU_ARGS_LOG" \
        OCORE_BUILD_DIR="$build_dir" \
        OCORE_PROBE_MODE=0 \
        OCORE_QEMU_BIN="$missing_qemu" \
        "$KERNEL_RUNNER"
    if [[ "$RUN_EXIT" -eq 127 ]] \
        && grep -Fq -- 'QEMU executable is not installed' "$STDERR_FILE" \
        && [[ ! -e "$KERNEL_BUILD_LOG" ]] \
        && [[ ! -e "$build_dir" ]]; then
        pass "kernel QEMU runner preflights QEMU before building"
    else
        fail "kernel QEMU runner preflights QEMU before building" \
            "(expected exit 127 without invoking build.sh)"
    fi
}

check_kernel_qemu_runner_restricts_modes() {
    local build_dir="$KERNEL_RUNNER_ROOT/rejected-mode-build"

    rm -f "$KERNEL_BUILD_LOG" "$KERNEL_QEMU_ARGS_LOG"
    run_command env \
        HOME="$KERNEL_RUNNER_HOME" \
        KERNEL_BUILD_LOG="$KERNEL_BUILD_LOG" \
        KERNEL_QEMU_ARGS_LOG="$KERNEL_QEMU_ARGS_LOG" \
        OCORE_BUILD_DIR="$build_dir" \
        OCORE_PROBE_MODE=32 \
        OCORE_QEMU_BIN="$KERNEL_QEMU_STUB" \
        "$KERNEL_RUNNER"
    if [[ "$RUN_EXIT" -eq 2 ]] \
        && grep -Fq -- 'supports only OCORE_PROBE_MODE=0 or 16' "$STDERR_FILE" \
        && [[ ! -e "$KERNEL_BUILD_LOG" ]] \
        && [[ ! -e "$KERNEL_QEMU_ARGS_LOG" ]]; then
        pass "kernel QEMU runner rejects non-interactive probe modes"
    else
        fail "kernel QEMU runner rejects non-interactive probe modes" \
            "(expected mode 32 to fail before build or QEMU)"
    fi
}

check_kernel_qemu_runner_rejects_unsafe_build_dirs() {
    local build_dir

    for build_dir in "/" "$KERNEL_RUNNER_ROOT" "$KERNEL_RUNNER_HOME"; do
        rm -f "$KERNEL_BUILD_LOG" "$KERNEL_QEMU_ARGS_LOG"
        run_command env \
            HOME="$KERNEL_RUNNER_HOME" \
            KERNEL_BUILD_LOG="$KERNEL_BUILD_LOG" \
            KERNEL_QEMU_ARGS_LOG="$KERNEL_QEMU_ARGS_LOG" \
            OCORE_BUILD_DIR="$build_dir" \
            OCORE_PROBE_MODE=0 \
            OCORE_QEMU_BIN="$KERNEL_QEMU_STUB" \
            "$KERNEL_RUNNER"
        if [[ "$RUN_EXIT" -ne 2 ]] \
            || ! grep -Fq -- 'unsafe OCORE_BUILD_DIR' "$STDERR_FILE" \
            || [[ -e "$KERNEL_BUILD_LOG" ]] \
            || [[ -e "$KERNEL_QEMU_ARGS_LOG" ]]; then
            fail "kernel QEMU runner rejects unsafe build directories" \
                "(unsafe build directory was not rejected: $build_dir)"
            return
        fi
    done
    pass "kernel QEMU runner rejects filesystem, repository, and home roots"
}

check_kernel_qemu_runner_safe_flags() {
    local build_dir="$KERNEL_RUNNER_ROOT/safe-flags-build"
    local expected_args="$ARTIFACT_DIR/kernel-qemu-args.expected"

    rm -f "$KERNEL_BUILD_LOG" "$KERNEL_QEMU_ARGS_LOG"
    run_command env \
        HOME="$KERNEL_RUNNER_HOME" \
        KERNEL_BUILD_LOG="$KERNEL_BUILD_LOG" \
        KERNEL_QEMU_ARGS_LOG="$KERNEL_QEMU_ARGS_LOG" \
        OCORE_BUILD_DIR="$build_dir" \
        OCORE_PROBE_MODE=0 \
        OCORE_QEMU_BIN="$KERNEL_QEMU_STUB" \
        "$KERNEL_RUNNER"
    if [[ "$RUN_EXIT" -ne 0 ]]; then
        fail "kernel QEMU runner uses the exact minimal safe device set" \
            "(runner exited $RUN_EXIT)"
        return
    fi

    cat >"$expected_args" <<EOF
-machine
q35
-m
128M
-nodefaults
-nic
none
-kernel
$build_dir/kernel.elf
-display
none
-serial
mon:stdio
-no-reboot
-no-shutdown
EOF
    if diff -u "$expected_args" "$KERNEL_QEMU_ARGS_LOG" \
        >"$STDOUT_FILE" 2>"$STDERR_FILE"; then
        pass "kernel QEMU runner uses the exact minimal safe device set"
    else
        fail "kernel QEMU runner uses the exact minimal safe device set" \
            "(QEMU arguments differed from the safe allowlist)"
    fi
}

setup_kernel_media_cli_fixture() {
    KERNEL_MEDIA_STUB_DIR="$ARTIFACT_DIR/kernel-media-stubs"
    KERNEL_MEDIA_LOG="$ARTIFACT_DIR/kernel-media.log"
    KERNEL_MEDIA_BUILD_STUB="$KERNEL_MEDIA_STUB_DIR/media-build"
    KERNEL_MEDIA_SETUP_STUB="$KERNEL_MEDIA_STUB_DIR/media-setup"
    KERNEL_MEDIA_INSPECT_STUB="$KERNEL_MEDIA_STUB_DIR/media-inspect"
    KERNEL_MEDIA_BOOT_STUB="$KERNEL_MEDIA_STUB_DIR/media-boot"
    KERNEL_MEDIA_SMOKE_STUB="$KERNEL_MEDIA_STUB_DIR/media-smoke"
    KERNEL_MEDIA_WRITER_STUB="$KERNEL_MEDIA_STUB_DIR/media-writer"

    mkdir -p "$KERNEL_MEDIA_STUB_DIR"
    cat >"$KERNEL_MEDIA_BUILD_STUB" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: "${KERNEL_MEDIA_LOG:?}"
{
    printf 'script=%s\n' "${0##*/}"
    printf 'argc=%s\n' "$#"
    for arg in "$@"; do
        printf 'arg=%s\n' "$arg"
    done
} >"$KERNEL_MEDIA_LOG"
EOF
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_MEDIA_INSPECT_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_MEDIA_SETUP_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_MEDIA_BOOT_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_MEDIA_SMOKE_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_MEDIA_WRITER_STUB"
    chmod +x \
        "$KERNEL_MEDIA_BUILD_STUB" \
        "$KERNEL_MEDIA_SETUP_STUB" \
        "$KERNEL_MEDIA_INSPECT_STUB" \
        "$KERNEL_MEDIA_BOOT_STUB" \
        "$KERNEL_MEDIA_SMOKE_STUB" \
        "$KERNEL_MEDIA_WRITER_STUB"
}

run_kernel_media_cli() {
    rm -f "$KERNEL_MEDIA_LOG"
    run_command env \
        KERNEL_MEDIA_LOG="$KERNEL_MEDIA_LOG" \
        O_KERNEL_MEDIA_BUILD_SCRIPT="$KERNEL_MEDIA_BUILD_STUB" \
        O_KERNEL_SETUP_SCRIPT="$KERNEL_MEDIA_SETUP_STUB" \
        O_KERNEL_MEDIA_INSPECT_SCRIPT="$KERNEL_MEDIA_INSPECT_STUB" \
        O_KERNEL_MEDIA_BOOT_SCRIPT="$KERNEL_MEDIA_BOOT_STUB" \
        O_KERNEL_MEDIA_SMOKE_SCRIPT="$KERNEL_MEDIA_SMOKE_STUB" \
        O_KERNEL_MEDIA_WRITER_SCRIPT="$KERNEL_MEDIA_WRITER_STUB" \
        "$O_KERNEL_CLI" "$@"
}

check_kernel_media_dispatch() {
    local desc="$1"
    local expected_log="$2"
    shift 2

    run_kernel_media_cli "$@"
    if [[ "$RUN_EXIT" -ne 0 ]]; then
        fail "$desc" "(kernel media command exited $RUN_EXIT)"
        return
    fi
    if [[ ! -f "$KERNEL_MEDIA_LOG" ]]; then
        fail "$desc" "(expected media delegate was not dispatched)"
        return
    fi
    if [[ "$(cat "$KERNEL_MEDIA_LOG")" == "$expected_log" ]]; then
        pass "$desc"
    else
        printf '%s\n' "expected:" >"$STDOUT_FILE"
        printf '%s\n' "$expected_log" >>"$STDOUT_FILE"
        printf '%s\n' "actual:" >>"$STDOUT_FILE"
        cat "$KERNEL_MEDIA_LOG" >>"$STDOUT_FILE"
        fail "$desc" "(media delegate or arguments differed from the exact expectation)"
    fi
}

check_kernel_media_rejection() {
    local desc="$1"
    local pattern="$2"
    shift 2

    run_kernel_media_cli "$@"
    if [[ "$RUN_EXIT" -ne 0 ]] \
        && grep -Fq -- "$pattern" "$STDERR_FILE" \
        && [[ ! -e "$KERNEL_MEDIA_LOG" ]]; then
        pass "$desc"
    else
        fail "$desc" "(expected rejection before dispatching a media delegate)"
    fi
}

# --- Test inputs --- #

INVALID_SOURCE="$ARTIFACT_DIR/invalid.O"
cat >"$INVALID_SOURCE" <<'EOF'
python^(
__oval_result__ = 2
EOF

CAPABILITY_SOURCE="$ARTIFACT_DIR/capability.O"
cat >"$CAPABILITY_SOURCE" <<'EOF'
python{cap=runner,process}^(
import os
__oval_result__ = os.system("true")
)_python{cap=runner,process}
EOF

AMBIENT_AUTHORITY_SOURCE="$ARTIFACT_DIR/ambient-authority.O"
cat >"$AMBIENT_AUTHORITY_SOURCE" <<'EOF'
python^(
import os
__oval_result__ = os.system("true")
)_python
EOF

cat >"$ARTIFACT_DIR/smoke.oc" <<'EOF'
module smoke;
@export @no_mangle
unsafe fn kernel_main() -> never {
    unsafe { outb(0x3f8, b'O'); }
    loop { unsafe { halt(); } }
}
EOF

setup_kernel_qemu_runner_fixture
setup_kernel_media_cli_fixture

# --- CLI integration tests --- #

check_stderr_contains "O with no args shows usage error" 1 'Usage:|missing input file' "$O_BIN"
check_nonzero_stderr_contains "O missing file errors" 'failed to read input file|No such file' "$O_BIN" nonexistent.O backends/
check_stdout_contains "O runs hello.O" 0 '^(\[number\] )?2$' "$O_BIN" examples/hello.O backends/
check_stdout_contains "legacy backend cap attrs run without a host grant" 0 '^(\[number\] )?0$' "$O_BIN" "$CAPABILITY_SOURCE" backends/
check_stdout_contains "backend grants remain accepted but unnecessary" 0 '^(\[number\] )?0$' "$O_BIN" --backend-grant runner=python:process "$CAPABILITY_SOURCE" backends/
check_stdout_contains "plain Python has ambient process authority" 0 '^(\[number\] )?0$' "$O_BIN" "$AMBIENT_AUTHORITY_SOURCE" backends/
check_stdout_contains "O --help shows usage" 0 '^Usage:' "$O_BIN" --help
check_stdout_contains "O help defines graph worker pool capacity" 0 'local-worker pool capacity' "$O_BIN" --help
check_nonzero_stderr_contains "O rejects a zero graph worker bound" '--workers must be at least 1' "$O_BIN" --workers 0 examples/hello.O backends/
check_stdout_contains "olangc --help shows usage" 0 '^Usage: olangc' "$OLANGC_BIN" --help
check_stdout_contains "olangc help advertises schedule explanation" 0 '--explain-schedule' "$OLANGC_BIN" --help
check_stdout_contains "olangc help advertises schedule worker override" 0 '--workers <N>' "$OLANGC_BIN" --help
check_nonzero_stderr_contains "olangc rejects schedule workers without explanation" '--workers requires --explain-schedule --target ir' "$OLANGC_BIN" examples/hello.O --target ir --workers 2
check_nonzero_stderr_contains "olangc rejects a zero schedule worker override" '--workers must be at least 1' "$OLANGC_BIN" examples/hello.O --target ir --explain-schedule --workers 0
check_olangc_schedule_explanation "olangc explains digest-bound admission without execution"
check_nonzero_stderr_contains "olangc schedule explanation rejects a non-IR target" \
    '--explain-schedule is available only with --target ir' \
    "$OLANGC_BIN" examples/hello.O --target script --explain-schedule
check_olangc_grounding_report "olangc grounding reports exact World and residual HostWorld"
check_nonzero_stderr_contains "olangc grounding rejects a non-IR target" \
    '--grounding is available only with --target ir' \
    "$OLANGC_BIN" examples/hello.O --target dot --grounding
check_nonzero_stderr_contains "olangc World flags require grounding" \
    '--world-id and --world-epoch require --grounding --target ir' \
    "$OLANGC_BIN" examples/hello.O --target ir --world-id desk --world-epoch 4
check_nonzero_stderr_contains "olangc World identity requires an epoch" \
    '--world-id requires --world-epoch' \
    "$OLANGC_BIN" examples/hello.O --target ir --grounding --world-id desk
check_nonzero_stderr_contains "olangc World epoch requires an identity" \
    '--world-epoch requires --world-id' \
    "$OLANGC_BIN" examples/hello.O --target ir --grounding --world-epoch 4
check_nonzero_stderr_contains "olangc grounding rejects an invalid World identity" \
    'world identity .*unsupported character or path component' \
    "$OLANGC_BIN" examples/hello.O --target ir --grounding \
    --world-id desk/escape --world-epoch 4
check_nonzero_stderr_contains "olangc grounding rejects World epoch zero" \
    'world epoch must be nonzero' \
    "$OLANGC_BIN" examples/hello.O --target ir --grounding \
    --world-id desk --world-epoch 0
check_olangc_compile_and_run "olangc compiles hello.O and the output runs"
check_olangc_capability_compile_and_run "olangc compiles legacy backend cap attrs without a host grant"
check_stdout_contains "ocorec --help shows usage" 0 '^Usage: ocorec' "$OCOREC_BIN" --help
check_ocore_compile "ocorec emits x86-64 freestanding ELF object"
check_stdout_contains "lowercase o help advertises the kernel CLI" 0 \
    'kernel <command>' "$O_CLI" help
check_stdout_contains "lowercase o dispatches kernel help" 0 \
    '^Usage: o kernel <command>' "$O_CLI" kernel help
check_stdout_contains "kernel CLI with no command is non-booting help" 0 \
    '^Usage: o kernel <command>' "$O_KERNEL_CLI"
check_nonzero_stderr_contains "kernel CLI rejects an unknown command" \
    "unknown kernel command 'warp'" "$O_KERNEL_CLI" warp
check_nonzero_stderr_contains "kernel boot rejects arguments before launching QEMU" \
    'command does not accept arguments' "$O_KERNEL_CLI" boot unexpected
check_kernel_media_dispatch "kernel doctor-media checks the exact optional profile" \
    $'script=media-setup\nargc=3\narg=--with-ocore-media\narg=--check\narg=--no-env' \
    doctor-media
check_kernel_media_dispatch "kernel media accepts no output path" \
    $'script=media-build\nargc=0' \
    media
KERNEL_MEDIA_OUTPUT="$ARTIFACT_DIR/custom-ostadix.img"
check_kernel_media_dispatch "kernel media forwards one output path" \
    "$(printf 'script=media-build\nargc=1\narg=%s' "$KERNEL_MEDIA_OUTPUT")" \
    media "$KERNEL_MEDIA_OUTPUT"
check_kernel_media_rejection "kernel media rejects extra output paths" \
    'command accepts at most one path argument' \
    media "$KERNEL_MEDIA_OUTPUT" "$ARTIFACT_DIR/unexpected.img"
KERNEL_MEDIA_DEFAULT="$ROOT/target/ostadix-media/x86_64/ostadix-x86_64-uefi.img"
check_kernel_media_dispatch "kernel inspect-media forwards the default image path" \
    "$(printf 'script=media-inspect\nargc=2\narg=inspect\narg=%s' "$KERNEL_MEDIA_DEFAULT")" \
    inspect-media
check_kernel_media_dispatch "kernel inspect-media forwards an explicit image path" \
    "$(printf 'script=media-inspect\nargc=2\narg=inspect\narg=%s' "$KERNEL_MEDIA_OUTPUT")" \
    inspect-media "$KERNEL_MEDIA_OUTPUT"
check_kernel_media_dispatch "kernel boot-media dispatches its exact boot script" \
    $'script=media-boot\nargc=0' \
    boot-media
check_kernel_media_rejection "kernel boot-media rejects arguments" \
    'command does not accept arguments' \
    boot-media unexpected
check_kernel_media_dispatch "kernel smoke-media dispatches its exact smoke script" \
    $'script=media-smoke\nargc=0' \
    smoke-media
check_kernel_media_rejection "kernel smoke-media rejects arguments" \
    'command does not accept arguments' \
    smoke-media unexpected
check_kernel_media_dispatch "kernel prepare-write forwards exact writer arguments" \
    $'script=media-writer\nargc=4\narg=prepare\narg=--device\narg=/dev/disk9\narg=--json' \
    prepare-write --device /dev/disk9 --json
check_kernel_media_dispatch "kernel write-media forwards exact writer arguments" \
    "$(printf 'script=media-writer\nargc=7\narg=write\narg=--device\narg=/dev/disk9\narg=--image\narg=%s\narg=--confirm\narg=bound-token' "$KERNEL_MEDIA_OUTPUT")" \
    write-media --device /dev/disk9 --image "$KERNEL_MEDIA_OUTPUT" \
    --confirm bound-token
check_kernel_qemu_mode_and_build_dir_propagation
check_kernel_qemu_runner_rejects_direct_args
check_kernel_qemu_runner_preflights_before_build
check_kernel_qemu_runner_restricts_modes
check_kernel_qemu_runner_rejects_unsafe_build_dirs
check_kernel_qemu_runner_safe_flags
check_stdout_contains "ogit demo help lists semantic receipt" 0 'semantic-receipt' "$OGIT_BIN" demo --help
check_stdout_contains "ogit semantic diff detects policy change" 0 'runtime demand model changed' \
    "$OGIT_BIN" diff-semantic examples/group_pipeline/main.O examples/group_pipeline/main.eager.O
check_stdout_contains "olink help shows usage" 0 'Usage: (olink|o-link)' "$OLINK_BIN" --help
check_olink_hardened_round_trip
check_nonzero_stderr_contains "O invalid syntax exits with an error" 'failed to parse \.O source|error:' "$O_BIN" "$INVALID_SOURCE" backends/
check_stdout_contains "O --check validates a valid file" 0 '^ok$' "$O_BIN" --check examples/hello.O backends/
check_stdout_contains "O --json --check reports structured success" 0 '"ok":true' "$O_BIN" --json --check examples/hello.O backends/
check_stdout_contains "O --json --check reports structured parse errors" 1 '"ok":false.*"stage":"parse"' "$O_BIN" --json --check "$INVALID_SOURCE" backends/
check_stdout_contains "O --json runs hello.O with structured output" 0 '"ok":true.*"value".*"elapsed_ms"' "$O_BIN" --json examples/hello.O backends/
check_stdout_contains "O --eval evaluates an inline expression" 0 '^(\[number\] )?2$' "$O_BIN" --eval 'python^(
__oval_result__ = 1 + 1
)_python' backends/

echo ""
echo "Results: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
