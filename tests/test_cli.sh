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

RELEASE_DIR="${O_LANG_RELEASE_DIR:-$ROOT/target/release}"
O_BIN="$RELEASE_DIR/O"
OLANGC_BIN="$RELEASE_DIR/olangc"
OCOREC_BIN="$RELEASE_DIR/ocorec"
OGIT_BIN="$RELEASE_DIR/ogit"
OINFO_BIN="$RELEASE_DIR/o-info"
if [ -x "$RELEASE_DIR/olink" ]; then
    OLINK_BIN="$RELEASE_DIR/olink"
else
    OLINK_BIN="$RELEASE_DIR/o-link"
fi
OUNLINK_BIN="$RELEASE_DIR/o-unlink"
O_CLI="./scripts/o-cli.sh"
O_KERNEL_CLI="./scripts/o-kernel.sh"
O_KERNEL_QEMU_RUNNER="./ocore/kernel/run-qemu.sh"

for bin in "$O_BIN" "$OLANGC_BIN" "$OCOREC_BIN" "$OGIT_BIN" "$OINFO_BIN" "$OLINK_BIN" "$OUNLINK_BIN"; do
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
        '^; ExecutionAdmission oexec\.admission/v6$' \
        '^binding analyzer-sha256=[0-9a-f]{64} evidence-sha256=[0-9a-f]{64} admitted-graph-sha256=[0-9a-f]{64} placement-admission-sha256=[0-9a-f]{64} admission-sha256=[0-9a-f]{64}$' \
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

check_olangc_schedule_explanation_json() {
    local desc="$1"
    local source="$ARTIFACT_DIR/explain-json-do-not-run.O"
    local marker="$ARTIFACT_DIR/explain-json-executed"

    cat >"$source" <<EOF
python^(
from pathlib import Path
Path(r"$marker").write_text("executed")
__oval_result__ = 2
)_python
EOF

    run_command "$OLANGC_BIN" "$source" \
        --target ir --explain-schedule --format json --workers 1
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "$desc" "(JSON schedule explanation failed with exit $RUN_EXIT)"
        return
    fi
    if ! python3 - "$STDOUT_FILE" <<'PY'
import json
from pathlib import Path
import sys

document = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
assert set(document) == {"schema", "admission", "realizability", "prediction"}
assert document["schema"] == "oexec.schedule-explanation/v2"
admission = document["admission"]
assert set(admission) == {
    "schema", "analyzer", "runtime_snapshot_kind", "base_policy", "bindings"
}
assert admission["schema"] == "oexec.admission/v6"
assert admission["runtime_snapshot_kind"] == "inspection"
bindings = admission["bindings"]
assert set(bindings) == {
    "lowered_oir_sha256", "plan_sha256", "analyzed_graph_sha256",
    "backend_catalog_projection_sha256", "backend_set_sha256",
    "direct_executable_manifest_sha256", "launch_context_sha256",
    "environment_sha256", "ambient_world_sha256", "analyzer_sha256",
    "evidence_sha256", "admitted_graph_sha256",
    "placement_admission_sha256", "admission_sha256",
}
assert all(
    isinstance(value, str)
    and len(value) == 64
    and all(character in "0123456789abcdef" for character in value)
    for value in bindings.values()
)
realizability = document["realizability"]
assert realizability["schema"] == "oexec.realizability/v1"
assert realizability["source"] == "cli-override"
assert realizability["selected_workers"] == 1
prediction = document["prediction"]
assert prediction["schema"] == "oexec.schedule-prediction/v1"
assert prediction["admission_sha256"] == bindings["admission_sha256"]
assert prediction["task_count"] == 1
assert prediction["predicted_width"] == 1
assert prediction["predicted_span"] == 1
assert len(prediction["layers"]) == 1
layer = prediction["layers"][0]
assert set(layer) == {"index", "operations"}
assert layer["index"] == 1
assert len(layer["operations"]) == 1
assert layer["operations"][0].startswith("P")
PY
    then
        fail "$desc" "(schedule explanation was not strict v2 JSON)"
        return
    fi
    if [ -e "$marker" ]; then
        fail "$desc" "(--format json executed the inspected backend)"
        return
    fi
    pass "$desc"
}

check_olangc_schedule_why() {
    local desc="$1"
    local source="$ARTIFACT_DIR/why-do-not-run.O"
    local marker="$ARTIFACT_DIR/why-executed"
    local why_direct="$ARTIFACT_DIR/why-direct"
    local why_dispatch="$ARTIFACT_DIR/why-dispatch"
    local why_direct_normalized="$ARTIFACT_DIR/why-direct-normalized"
    local why_dispatch_normalized="$ARTIFACT_DIR/why-dispatch-normalized"

    cat >"$source" <<EOF
autonomous(batch(
python^(
from pathlib import Path
Path(r"$marker").write_text("executed")
__oval_result__ = 1
)_python,
python^(
__oval_result__ = 2
)_python
))
EOF

    run_command "$OLANGC_BIN" "$source" --target ir --explain-schedule
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "$desc" "(could not discover an admitted operation with exit $RUN_EXIT)"
        return
    fi
    local target
    target=$(sed -n 's/^operation \(P[0-9][0-9]*\) admitted=yes .*/\1/p' "$STDOUT_FILE" | head -n 1)
    if [[ ! "$target" =~ ^P[0-9]+$ ]]; then
        fail "$desc" "(schedule explanation did not identify an admitted operation)"
        return
    fi

    run_command env O_LANG_OLANGC_BIN="$OLANGC_BIN" \
        "$OLANGC_BIN" "$source" --target ir --why "$target"
    if [ "$RUN_EXIT" -ne 0 ]; then
        fail "$desc" "(focused schedule explanation failed with exit $RUN_EXIT)"
        return
    fi
    cp "$STDOUT_FILE" "$why_direct"
    for pattern in \
        '^; ExecutionAdmissionWhy oexec\.admission-why/v2$' \
        "^why operation=$target status=admitted-static inspection-only=yes dispatch=not-run admission-sha256=[0-9a-f]{64}$" \
        '^binding lowered-oir-sha256=' \
        "^plan-node $target kind=" \
        "^operation $target admitted=yes ordinal=" \
        '^wave index=[0-9]+ operations=\[' \
        '^; SourceOrigin oexec\.source-origin/v1$' \
        '^source-binding sha256=[0-9a-f]{64} bytes=[0-9]+ path=' \
        "^source-origin operation=$target bytes=[0-9]+\.\.[0-9]+ start=[0-9]+:[0-9]+ end=[0-9]+:[0-9]+$" \
        '^source-origin-note coordinates and source SHA-256 are descriptive sidecar provenance;'; do
        if ! grep -Eq -- "$pattern" "$STDOUT_FILE"; then
            fail "$desc" "(focused schedule explanation omitted pattern: $pattern)"
            return
        fi
    done
    if grep -Eq -- '^; (OIrProgram|HGraph)$' "$STDOUT_FILE"; then
        fail "$desc" "(--why emitted the unrelated whole-program IR/HGraph view)"
        return
    fi
    if [ -e "$marker" ]; then
        fail "$desc" "(--why executed the inspected backend)"
        return
    fi

    RUN_EXIT=0
    O_LANG_OLANGC_BIN="$OLANGC_BIN" "$O_CLI" why "$source" "$target" \
        >"$why_dispatch" 2>"$STDERR_FILE" || RUN_EXIT=$?
    sed -E 's/[0-9a-f]{64}/<digest>/g' "$why_direct" >"$why_direct_normalized"
    sed -E 's/[0-9a-f]{64}/<digest>/g' "$why_dispatch" >"$why_dispatch_normalized"
    if [ "$RUN_EXIT" -ne 0 ] || ! cmp -s "$why_direct_normalized" "$why_dispatch_normalized"; then
        fail "$desc" "(o why did not preserve the typed olangc query after normalizing per-process digests)"
        return
    fi
    if [ -e "$marker" ]; then
        fail "$desc" "(o why executed the inspected backend)"
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
    KERNEL_ISO_BUILD_STUB="$KERNEL_MEDIA_STUB_DIR/iso-build"
    KERNEL_ISO_INSPECT_STUB="$KERNEL_MEDIA_STUB_DIR/iso-inspect"
    KERNEL_ISO_BOOT_STUB="$KERNEL_MEDIA_STUB_DIR/iso-boot"
    KERNEL_ISO_SMOKE_STUB="$KERNEL_MEDIA_STUB_DIR/iso-smoke"
    KERNEL_CAPACITY_ISO_BUILD_STUB="$KERNEL_MEDIA_STUB_DIR/capacity-iso-build"
    KERNEL_CAPACITY_ISO_INSPECT_STUB="$KERNEL_MEDIA_STUB_DIR/capacity-iso-inspect"
    KERNEL_CAPACITY_ISO_BOOT_STUB="$KERNEL_MEDIA_STUB_DIR/capacity-iso-boot"
    KERNEL_HOSTED_LIVE_RELEASE_STUB="$KERNEL_MEDIA_STUB_DIR/hosted-live-release"
    KERNEL_HOSTED_LIVE_SMOKE_STUB="$KERNEL_MEDIA_STUB_DIR/hosted-live-smoke"
    KERNEL_VENTOY_INSTALLER_STUB="$KERNEL_MEDIA_STUB_DIR/ventoy-installer"
    KERNEL_BOOT_INFO_SMOKE_STUB="$KERNEL_MEDIA_STUB_DIR/boot-info-smoke"
    KERNEL_SMP_SMOKE_STUB="$KERNEL_MEDIA_STUB_DIR/smp-smoke"
    KERNEL_MEDIA_WRITER_STUB="$KERNEL_MEDIA_STUB_DIR/media-writer"
    KERNEL_PHYSICAL_EVIDENCE_STUB="$KERNEL_MEDIA_STUB_DIR/physical-evidence"

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
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_ISO_BUILD_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_ISO_INSPECT_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_ISO_BOOT_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_ISO_SMOKE_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_CAPACITY_ISO_BUILD_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_CAPACITY_ISO_INSPECT_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_CAPACITY_ISO_BOOT_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_HOSTED_LIVE_RELEASE_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_HOSTED_LIVE_SMOKE_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_VENTOY_INSTALLER_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_BOOT_INFO_SMOKE_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_SMP_SMOKE_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_MEDIA_WRITER_STUB"
    cp "$KERNEL_MEDIA_BUILD_STUB" "$KERNEL_PHYSICAL_EVIDENCE_STUB"
    chmod +x \
        "$KERNEL_MEDIA_BUILD_STUB" \
        "$KERNEL_MEDIA_SETUP_STUB" \
        "$KERNEL_MEDIA_INSPECT_STUB" \
        "$KERNEL_MEDIA_BOOT_STUB" \
        "$KERNEL_MEDIA_SMOKE_STUB" \
        "$KERNEL_ISO_BUILD_STUB" \
        "$KERNEL_ISO_INSPECT_STUB" \
        "$KERNEL_ISO_BOOT_STUB" \
        "$KERNEL_ISO_SMOKE_STUB" \
        "$KERNEL_CAPACITY_ISO_BUILD_STUB" \
        "$KERNEL_CAPACITY_ISO_INSPECT_STUB" \
        "$KERNEL_CAPACITY_ISO_BOOT_STUB" \
        "$KERNEL_HOSTED_LIVE_RELEASE_STUB" \
        "$KERNEL_HOSTED_LIVE_SMOKE_STUB" \
        "$KERNEL_VENTOY_INSTALLER_STUB" \
        "$KERNEL_BOOT_INFO_SMOKE_STUB" \
        "$KERNEL_SMP_SMOKE_STUB" \
        "$KERNEL_MEDIA_WRITER_STUB" \
        "$KERNEL_PHYSICAL_EVIDENCE_STUB"
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
        O_KERNEL_ISO_BUILD_SCRIPT="$KERNEL_ISO_BUILD_STUB" \
        O_KERNEL_ISO_INSPECT_SCRIPT="$KERNEL_ISO_INSPECT_STUB" \
        O_KERNEL_ISO_BOOT_SCRIPT="$KERNEL_ISO_BOOT_STUB" \
        O_KERNEL_ISO_SMOKE_SCRIPT="$KERNEL_ISO_SMOKE_STUB" \
        O_KERNEL_CAPACITY_ISO_BUILD_SCRIPT="$KERNEL_CAPACITY_ISO_BUILD_STUB" \
        O_KERNEL_CAPACITY_ISO_INSPECT_SCRIPT="$KERNEL_CAPACITY_ISO_INSPECT_STUB" \
        O_KERNEL_CAPACITY_ISO_BOOT_SCRIPT="$KERNEL_CAPACITY_ISO_BOOT_STUB" \
        O_KERNEL_HOSTED_LIVE_RELEASE_SCRIPT="$KERNEL_HOSTED_LIVE_RELEASE_STUB" \
        O_KERNEL_HOSTED_LIVE_SMOKE_SCRIPT="$KERNEL_HOSTED_LIVE_SMOKE_STUB" \
        O_KERNEL_VENTOY_INSTALLER_SCRIPT="$KERNEL_VENTOY_INSTALLER_STUB" \
        O_KERNEL_BOOT_INFO_SMOKE_SCRIPT="$KERNEL_BOOT_INFO_SMOKE_STUB" \
        O_KERNEL_SMP_SMOKE_SCRIPT="$KERNEL_SMP_SMOKE_STUB" \
        O_KERNEL_MEDIA_WRITER_SCRIPT="$KERNEL_MEDIA_WRITER_STUB" \
        O_KERNEL_PHYSICAL_EVIDENCE_SCRIPT="$KERNEL_PHYSICAL_EVIDENCE_STUB" \
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
check_stdout_contains "olangc help advertises schedule explanation formats" 0 '--format <FORMAT>' "$OLANGC_BIN" --help
check_stdout_contains "olangc help advertises focused schedule why" 0 '--why <PLAN_NODE>' "$OLANGC_BIN" --help
check_stdout_contains "olangc help advertises schedule worker override" 0 '--workers <N>' "$OLANGC_BIN" --help
check_nonzero_stderr_contains "olangc rejects schedule workers without explanation" '--workers requires --explain-schedule --target ir' "$OLANGC_BIN" examples/hello.O --target ir --workers 2
check_nonzero_stderr_contains "olangc rejects a zero schedule worker override" '--workers must be at least 1' "$OLANGC_BIN" examples/hello.O --target ir --explain-schedule --workers 0
check_nonzero_stderr_contains "olangc rejects schedule formats without explanation" '--format requires --explain-schedule --target ir' "$OLANGC_BIN" examples/hello.O --target ir --format json
check_nonzero_stderr_contains "olangc rejects JSON schedule output combined with grounding" '--format json is a standalone schedule view' "$OLANGC_BIN" examples/hello.O --target ir --explain-schedule --format json --grounding
check_olangc_schedule_explanation "olangc explains digest-bound admission without execution"
check_olangc_schedule_explanation_json "olangc emits typed schedule JSON without execution"
check_olangc_schedule_why "olangc and o explain one admitted operation without execution"
check_nonzero_stderr_contains "olangc schedule why rejects a non-IR target" \
    '--why is available only with --target ir' \
    "$OLANGC_BIN" examples/hello.O --target script --why P0
check_nonzero_stderr_contains "olangc schedule why rejects a malformed plan identity" \
    'expected a canonical plan node such as P3' \
    "$OLANGC_BIN" examples/hello.O --target ir --why p0
check_nonzero_stderr_contains "olangc schedule why rejects the whole admission view" \
    '--why and --explain-schedule are distinct inspection views' \
    "$OLANGC_BIN" examples/hello.O --target ir --why P0 --explain-schedule
check_nonzero_stderr_contains "olangc schedule why rejects non-executable plan text" \
    'P1 exists in the ExecutionPlan as `text` but is not an admitted executable operation' \
    "$OLANGC_BIN" examples/hello.O --target ir --why P1
check_nonzero_stderr_contains "olangc schedule why rejects project HGraphs" \
    '--explain-schedule and --why currently admit ordinary .O HGraphs only' \
    "$OLANGC_BIN" examples/group_pipeline --target ir --why P0
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
check_stdout_contains "lowercase o help advertises local information" 0 \
    'info <command>' "$O_CLI" help
check_stdout_contains "lowercase o dispatches local information help" 0 \
    'authority-free Ostadix information store' env O_LANG_INFO_BIN="$OINFO_BIN" \
    "$O_CLI" info --help
check_stdout_contains "lowercase o dispatches kernel help" 0 \
    '^Usage: o kernel <command>' "$O_CLI" kernel help
check_stdout_contains "kernel help publishes the hosted-live release route" 0 \
    '^  hosted-live-release  ' "$O_KERNEL_CLI" help
check_stdout_contains "kernel help publishes the guarded Ventoy prepare route" 0 \
    '^  prepare-ventoy  ' "$O_KERNEL_CLI" help
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
check_kernel_media_dispatch "kernel iso accepts no output path" \
    $'script=iso-build\nargc=0' \
    iso
KERNEL_ISO_OUTPUT="$ARTIFACT_DIR/custom-ostadix.iso"
check_kernel_media_dispatch "kernel iso forwards one output path" \
    "$(printf 'script=iso-build\nargc=1\narg=%s' "$KERNEL_ISO_OUTPUT")" \
    iso "$KERNEL_ISO_OUTPUT"
check_kernel_media_rejection "kernel iso rejects extra output paths" \
    'command accepts at most one path argument' \
    iso "$KERNEL_ISO_OUTPUT" "$ARTIFACT_DIR/unexpected.iso"
KERNEL_ISO_DEFAULT="$ROOT/target/ostadix-iso/x86_64/ostadix-x86_64-uefi.iso"
check_kernel_media_dispatch "kernel inspect-iso forwards the default ISO path" \
    "$(printf 'script=iso-inspect\nargc=2\narg=inspect\narg=%s' "$KERNEL_ISO_DEFAULT")" \
    inspect-iso
check_kernel_media_dispatch "kernel inspect-iso forwards an explicit ISO path" \
    "$(printf 'script=iso-inspect\nargc=2\narg=inspect\narg=%s' "$KERNEL_ISO_OUTPUT")" \
    inspect-iso "$KERNEL_ISO_OUTPUT"
KERNEL_CAPACITY_ISO_DEFAULT="$ROOT/target/ostadix-capacity-iso/x86_64/ostadix-hosted-live-x86_64-uefi.iso"
KERNEL_CAPACITY_ISO_OUTPUT="$ARTIFACT_DIR/custom-capacity.iso"
check_kernel_media_dispatch "kernel capacity-iso accepts no output path" \
    $'script=capacity-iso-build\nargc=0' \
    capacity-iso
check_kernel_media_dispatch "kernel capacity-iso forwards one output path" \
    "$(printf 'script=capacity-iso-build\nargc=1\narg=%s' "$KERNEL_CAPACITY_ISO_OUTPUT")" \
    capacity-iso "$KERNEL_CAPACITY_ISO_OUTPUT"
check_kernel_media_rejection "kernel capacity-iso rejects extra output paths" \
    'command accepts at most one path argument' \
    capacity-iso "$KERNEL_CAPACITY_ISO_OUTPUT" unexpected.iso
check_kernel_media_dispatch "kernel inspect-capacity-iso forwards the default ISO path" \
    "$(printf 'script=capacity-iso-inspect\nargc=2\narg=inspect\narg=%s' "$KERNEL_CAPACITY_ISO_DEFAULT")" \
    inspect-capacity-iso
check_kernel_media_dispatch "kernel inspect-capacity-iso forwards an explicit ISO path" \
    "$(printf 'script=capacity-iso-inspect\nargc=2\narg=inspect\narg=%s' "$KERNEL_CAPACITY_ISO_OUTPUT")" \
    inspect-capacity-iso "$KERNEL_CAPACITY_ISO_OUTPUT"
KERNEL_HOSTED_LIVE_OUTPUT="$ARTIFACT_DIR/ostadix-hosted-live-x86_64-uefi-0123456789ab.iso"
check_kernel_media_dispatch "kernel hosted-live-release accepts the default workflow" \
    $'script=hosted-live-release\nargc=0' \
    hosted-live-release
check_kernel_media_dispatch "kernel hosted-live-release forwards arbitrary release options" \
    "$(printf 'script=hosted-live-release\nargc=4\narg=--vm\narg=moral-gaur\narg=--output\narg=%s' "$KERNEL_HOSTED_LIVE_OUTPUT")" \
    hosted-live-release --vm moral-gaur --output "$KERNEL_HOSTED_LIVE_OUTPUT"
check_kernel_media_dispatch "kernel smoke-hosted-live accepts its default ISO" \
    $'script=hosted-live-smoke\nargc=0' \
    smoke-hosted-live
check_kernel_media_dispatch "kernel smoke-hosted-live forwards one exact ISO path" \
    "$(printf 'script=hosted-live-smoke\nargc=1\narg=%s' "$KERNEL_HOSTED_LIVE_OUTPUT")" \
    smoke-hosted-live "$KERNEL_HOSTED_LIVE_OUTPUT"
check_kernel_media_rejection "kernel smoke-hosted-live rejects extra ISO paths" \
    'command accepts at most one path argument' \
    smoke-hosted-live "$KERNEL_HOSTED_LIVE_OUTPUT" unexpected.iso
KERNEL_VENTOY_DEVICE=/dev/disk4
KERNEL_VENTOY_NAME=OSTADIX-Hosted-Live-x86_64-UEFI.iso
check_kernel_media_dispatch "kernel prepare-ventoy forwards the complete preparation request" \
    "$(printf 'script=ventoy-installer\nargc=9\narg=prepare\narg=--iso\narg=%s\narg=--device\narg=%s\narg=--volume\narg=/Volumes/Ventoy\narg=--name\narg=%s' "$KERNEL_HOSTED_LIVE_OUTPUT" "$KERNEL_VENTOY_DEVICE" "$KERNEL_VENTOY_NAME")" \
    prepare-ventoy --iso "$KERNEL_HOSTED_LIVE_OUTPUT" \
    --device "$KERNEL_VENTOY_DEVICE" --volume /Volumes/Ventoy \
    --name "$KERNEL_VENTOY_NAME"
check_kernel_media_dispatch "kernel install-ventoy forwards the rebound target and confirmation" \
    "$(printf 'script=ventoy-installer\nargc=12\narg=install\narg=--iso\narg=%s\narg=--device\narg=%s\narg=--volume\narg=/Volumes/Ventoy\narg=--name\narg=%s\narg=--confirm\narg=exact-token\narg=--eject' "$KERNEL_HOSTED_LIVE_OUTPUT" "$KERNEL_VENTOY_DEVICE" "$KERNEL_VENTOY_NAME")" \
    install-ventoy --iso "$KERNEL_HOSTED_LIVE_OUTPUT" \
    --device "$KERNEL_VENTOY_DEVICE" --volume /Volumes/Ventoy \
    --name "$KERNEL_VENTOY_NAME" --confirm exact-token --eject
check_kernel_media_dispatch "kernel verify-ventoy forwards arbitrary verification options" \
    "$(printf 'script=ventoy-installer\nargc=9\narg=verify\narg=--iso\narg=%s\narg=--device\narg=%s\narg=--volume\narg=/Volumes/Ventoy\narg=--name\narg=%s' "$KERNEL_HOSTED_LIVE_OUTPUT" "$KERNEL_VENTOY_DEVICE" "$KERNEL_VENTOY_NAME")" \
    verify-ventoy --iso "$KERNEL_HOSTED_LIVE_OUTPUT" \
    --device "$KERNEL_VENTOY_DEVICE" --volume /Volumes/Ventoy \
    --name "$KERNEL_VENTOY_NAME"
check_kernel_media_dispatch "kernel boot-media dispatches its exact boot script" \
    $'script=media-boot\nargc=0' \
    boot-media
check_kernel_media_rejection "kernel boot-media rejects arguments" \
    'command does not accept arguments' \
    boot-media unexpected
check_kernel_media_dispatch "kernel boot-iso dispatches its exact boot script" \
    $'script=iso-boot\nargc=0' \
    boot-iso
check_kernel_media_rejection "kernel boot-iso rejects arguments" \
    'command does not accept arguments' \
    boot-iso unexpected
check_kernel_media_dispatch "kernel boot-capacity-iso dispatches its exact boot script" \
    $'script=capacity-iso-boot\nargc=0' \
    boot-capacity-iso
check_kernel_media_dispatch "kernel boot-capacity-iso forwards one ISO path" \
    "$(printf 'script=capacity-iso-boot\nargc=1\narg=%s' "$KERNEL_CAPACITY_ISO_OUTPUT")" \
    boot-capacity-iso "$KERNEL_CAPACITY_ISO_OUTPUT"
check_kernel_media_rejection "kernel boot-capacity-iso rejects extra ISO paths" \
    'command accepts at most one path argument' \
    boot-capacity-iso "$KERNEL_CAPACITY_ISO_OUTPUT" unexpected.iso
check_kernel_media_dispatch "kernel smoke-media dispatches its exact smoke script" \
    $'script=media-smoke\nargc=0' \
    smoke-media
check_kernel_media_rejection "kernel smoke-media rejects arguments" \
    'command does not accept arguments' \
    smoke-media unexpected
check_kernel_media_dispatch "kernel smoke-iso dispatches its exact smoke script" \
    $'script=iso-smoke\nargc=0' \
    smoke-iso
check_kernel_media_rejection "kernel smoke-iso rejects arguments" \
    'command does not accept arguments' \
    smoke-iso unexpected
check_kernel_media_dispatch "kernel smoke-boot-info dispatches its exact smoke script" \
    $'script=boot-info-smoke\nargc=0' \
    smoke-boot-info
check_kernel_media_rejection "kernel smoke-boot-info rejects arguments" \
    'command does not accept arguments' \
    smoke-boot-info unexpected
check_kernel_media_dispatch "kernel smoke-smp dispatches its exact smoke script" \
    $'script=smp-smoke\nargc=0' \
    smoke-smp
check_kernel_media_rejection "kernel smoke-smp rejects arguments" \
    'command does not accept arguments' \
    smoke-smp unexpected
check_kernel_media_dispatch "kernel prepare-write forwards exact writer arguments" \
    $'script=media-writer\nargc=4\narg=prepare\narg=--device\narg=/dev/disk9\narg=--json' \
    prepare-write --device /dev/disk9 --json
check_kernel_media_dispatch "kernel write-media forwards exact writer arguments" \
    "$(printf 'script=media-writer\nargc=7\narg=write\narg=--device\narg=/dev/disk9\narg=--image\narg=%s\narg=--confirm\narg=bound-token' "$KERNEL_MEDIA_OUTPUT")" \
    write-media --device /dev/disk9 --image "$KERNEL_MEDIA_OUTPUT" \
    --confirm bound-token
check_kernel_media_dispatch "kernel boot-challenge requests one raw challenge" \
    $'script=physical-evidence\nargc=2\narg=challenge\narg=--raw' \
    boot-challenge
check_kernel_media_rejection "kernel boot-challenge rejects arguments" \
    'command does not accept arguments' \
    boot-challenge unexpected
check_kernel_media_dispatch "kernel prepare-physical forwards the exact intent arguments" \
    "$(printf 'script=physical-evidence\nargc=11\narg=prepare\narg=--image\narg=%s\narg=--media-write\narg=write.json\narg=--machine\narg=machine.json\narg=--profile\narg=smp4\narg=--expected-cpus\narg=4' "$KERNEL_MEDIA_OUTPUT")" \
    prepare-physical --image "$KERNEL_MEDIA_OUTPUT" --media-write write.json \
    --machine machine.json --profile smp4 --expected-cpus 4
check_kernel_media_dispatch "kernel record-physical forwards the exact observation arguments" \
    $'script=physical-evidence\nargc=7\narg=verify\narg=--intent\narg=intent.json\narg=--transcript\narg=serial.log\narg=--assert-physical\narg=literal' \
    record-physical --intent intent.json --transcript serial.log \
    --assert-physical literal
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
