#!/usr/bin/env bash
# check_release_claims.sh — release-claim regression guard.
#
# Fails when active documentation or source contains known stale or
# misleading phrases about the current implementation. Run from the
# repository root (CI runs it via `bash scripts/check_release_claims.sh`).
#
# Exclusions:
#   - c_cpp/legacy_cpp/  : explicitly marked historical C++ prototype; it
#     genuinely used the obsolete newline-JSON protocol, so those phrases
#     are correct there and deliberately excluded.
#   - docs/HGRAPH_EXECUTOR_PLAN.md : a future-work design document; it may
#     legitimately describe not-yet-implemented graph execution.
#   - This script itself and CI config (they name the phrases they ban).
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "error: check_release_claims.sh must run inside a Git worktree" >&2
    exit 2
}
cd "$ROOT" || exit 2

# Bash 3.2, shipped by macOS, has arrays but not mapfile/readarray.  Keep the
# NUL-delimited Git path list in a temporary file so spaces and glob characters
# in tracked paths remain safe on every supported shell.
FILES_FILE=$(mktemp "${TMPDIR:-/tmp}/ostadix-release-files.XXXXXX") || exit 2
HITS_FILE=$(mktemp "${TMPDIR:-/tmp}/ostadix-release-hits.XXXXXX") || {
    rm -f "$FILES_FILE"
    exit 2
}
trap 'rm -f "$FILES_FILE" "$HITS_FILE"' EXIT HUP INT TERM

if ! git ls-files -z -- \
    '*.md' '*.rs' '*.py' '*.c' '*.h' '*.O' '*.toml' '*.cff' '*.sh' '*.yml' '*.txt' \
    ':!:c_cpp/legacy_cpp/**' \
    ':!:docs/HGRAPH_EXECUTOR_PLAN.md' \
    ':!:scripts/check_release_claims.sh' \
    ':!:.github/workflows/**' \
    ':!:codebase_tape.md' \
    ':!:cvelist*' \
    >"$FILES_FILE"
then
    echo "error: could not enumerate active release-claim files" >&2
    exit 2
fi

if [ ! -s "$FILES_FILE" ]; then
    echo "error: release-claim scan selected no tracked files" >&2
    exit 2
fi

fail=0

check() {
    local pattern="$1" why="$2" file status scanned
    : >"$HITS_FILE" || exit 2
    scanned=0

    while IFS= read -r -d '' file; do
        # A tracked path deleted in the current worktree is not active content
        # and will disappear from the next commit, so it is safe to skip.
        [ -f "$file" ] || continue
        scanned=$((scanned + 1))
        if grep -nHEi -- "$pattern" "$file" >>"$HITS_FILE"; then
            :
        else
            status=$?
            if [ "$status" -ne 1 ]; then
                echo "error: grep failed while scanning $file" >&2
                exit 2
            fi
        fi
    done <"$FILES_FILE"

    if [ "$scanned" -eq 0 ]; then
        echo "error: release-claim scan found no existing tracked files" >&2
        exit 2
    fi

    if [ -s "$HITS_FILE" ]; then
        echo "STALE CLAIM: $why"
        echo "  pattern: $pattern"
        sed 's/^/  /' "$HITS_FILE"
        echo
        fail=1
    fi
}

# Current O-core status prose has a narrower scope than the general release
# scan. docs/ODOMAIN_PLAN.md intentionally contains historical milestone
# baselines and future acceptance text, so it must not be rejected for phrases
# that are stale only when used as a present-tense description of the kernel.
check_current_ocore_docs() {
    local pattern="$1" why="$2" file status
    : >"$HITS_FILE" || exit 2

    for file in README.md docs/CLAIMS.md docs/OCORE.md ocore/README.md; do
        if [ ! -f "$file" ]; then
            echo "error: required current O-core document is missing: $file" >&2
            exit 2
        fi
        if grep -nHEi -- "$pattern" "$file" >>"$HITS_FILE"; then
            :
        else
            status=$?
            if [ "$status" -ne 1 ]; then
                echo "error: grep failed while scanning $file" >&2
                exit 2
            fi
        fi
    done

    if [ -s "$HITS_FILE" ]; then
        echo "STALE O-CORE CLAIM: $why"
        echo "  pattern: $pattern"
        sed 's/^/  /' "$HITS_FILE"
        echo
        fail=1
    fi
}

# Obsolete protocol claims: the hosted protocol is 4-byte length-prefixed
# canonical CBOR.
check 'newline[- ]?delimited[- ]JSON' \
    "hosted protocol is canonical CBOR, not newline-delimited JSON"
check 'newline-JSON|newline JSON' \
    "hosted protocol is canonical CBOR, not newline-JSON"
check 'JSON IPC' \
    "hosted protocol is canonical CBOR, not JSON IPC"
check 'OValue \+ JSON wire' \
    "the wire format is canonical CBOR, not JSON"

# The graph executor is now the runtime default: `eval_ir_program_with_scope`
# drives a readiness-based operation-hypergraph coordinator, with the serial
# reference executor available behind `O_EXECUTOR=serial`. The former guard
# against claiming general graph execution is therefore no longer applicable and
# has been removed.

# Overstated openness claims: the evaluator registry is compile-time/static
# (registry-extensible), not a runtime-open universe.
check 'open-world recursive evaluator composition|open-world evaluator' \
    "evaluator registration is compile-time/static (registry-extensible), not open-world"
check 'An open evaluator set' \
    "the evaluator set is registry-extensible at compile time, not open"

# {lazy} is rejected for unrestricted shim backends; nix{lazy} must not be
# recommended anywhere as a valid pattern.
check 'nix\{lazy\}\^' \
    "nix{lazy} is rejected; use nix{defer}^ or bare nix_expr^"

# O-core Milestone 0.3 implements capability-returning anonymous/shared page
# allocation. Historical baseline text may still mention the old bump
# allocator, but active ABI text must not call syscall 3 reserved.
check 'reserved [`]*page_alloc|page_alloc.*returns `ERR_NOT_IMPLEMENTED`' \
    "O-core page_alloc is implemented for typed anonymous/shared memory objects"
check 'cap_copy and page_alloc remain reserved' \
    "only cap_copy remains reserved in the current native ABI"

# Milestones 1 and 2 are executable current gates. These checks are scoped to
# current-facing documents so historical Milestone 0 text in ODOMAIN_PLAN can
# continue to state the limits that applied at that earlier gate.
check_current_ocore_docs \
    'there is no scheduler yet|yield remains an accounting hook, not a scheduler|preemptive schedul(er|ing)[^.]*remain(s)? follow-on work' \
    "the bounded single-CPU M2 scheduler gate is implemented"
check_current_ocore_docs \
    'independent per-process page tables[^.]*(absent|remain[^.]*follow-on)|with no sibling process or second CR3|still one statically linked process in one bootstrap CR3' \
    "the bounded M1 gate proves independent CR3s and sibling survival"
check_current_ocore_docs \
    'no process teardown|process teardown[^.]*(absent|not implemented)|no mapping operation calls it yet|memory objects are real owned kernel objects but are not yet user mappings' \
    "M1 teardown and the bounded M3 shared-mapping primitive are implemented"
check_current_ocore_docs \
    'syscall numbers? (are |remain )?0 through 5|yield[^.]*(only|merely)[^.]*(request|hook) counter|cap_copy and page_alloc remain reserved' \
    "native ABI v1 includes gated exit and sleep, and page_alloc is implemented"

# The inverse claim boundary matters too: only the M3 foundation is verified.
# Keep this list aligned with the forbidden success markers in the foundation
# QEMU harness.
check_current_ocore_docs \
    'Milestone 3 (is )?(complete|implemented)|M3 (complete|implemented|IPC: PASS|full IPC: PASS|blocking IPC: PASS|ping-pong: PASS|personality crash containment: PASS)' \
    "Milestone 3 has a verified foundation but its full IPC gate is incomplete"

exit $fail
