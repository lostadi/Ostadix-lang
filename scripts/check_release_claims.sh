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
set -u

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

# Active files to scan: tracked text files, minus deliberate exclusions.
mapfile -t FILES < <(git ls-files -- \
    '*.md' '*.rs' '*.py' '*.c' '*.h' '*.O' '*.toml' '*.cff' '*.sh' '*.yml' '*.txt' \
    ':!:c_cpp/legacy_cpp/**' \
    ':!:docs/HGRAPH_EXECUTOR_PLAN.md' \
    ':!:scripts/check_release_claims.sh' \
    ':!:.github/workflows/**' \
    ':!:codebase_tape.md' \
    ':!:cvelist*' \
)

fail=0

check() {
    local pattern="$1" why="$2"
    local hits
    hits=$(grep -nEi -- "$pattern" "${FILES[@]}" 2>/dev/null)
    if [ -n "$hits" ]; then
        echo "STALE CLAIM: $why"
        echo "  pattern: $pattern"
        echo "$hits" | sed 's/^/  /'
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

exit $fail
