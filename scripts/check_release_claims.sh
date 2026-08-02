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

# The QEMU evidence manifest is the single source for the aggregate, CI, and
# public status/checklist projections. Validate its shape, gate scripts, and
# wiring before scanning prose; the aggregate separately enforces every marker
# against each gate's captured live transcript.
python3 scripts/release_evidence.py validate
python3 scripts/world_alpha_evidence.py --quiet

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

# The public bounded M3 gate now exists. Reject the older foundation-only
# wording while retaining the narrower fixed-capacity/single-CPU non-claims.
check_current_ocore_docs \
    'Milestone 3 has a verified foundation[^.]*not (a )?(completed|complete)|Milestone 3 is foundation-only|no CPL3 endpoint (ABI|operations)|cap_copy[^.]*(remain|still)[^.]*ERR_NOT_IMPLEMENTED' \
    "the bounded M3 gate now exposes CPL3 IPC, blocking/wake, transfer, and crash containment"
check_current_ocore_docs \
    '(transfer )?ticket[^.]*(bound to|for one validated)[^.]*(destination )?endpoint' \
    "M3 transfer tickets bind the exact creating process and destination CSpace, not the endpoint object"

# Mode 18 is the scalar M6A dependency slice, mode 19 is the bounded-copy M6B
# mechanism slice, and mode 24 composes one exact four-byte live bounded call.
# Full Milestone 6 still requires the wider foreign-ABI and lifecycle-race
# evidence; none of these bounded gates establishes that ABI.
check_current_ocore_docs \
    'Milestone 6 (is )?(complete|implemented)|M6 (complete|implemented|personality[^.]*PASS)' \
    "only bounded M6A/M6B mechanism and four-byte live slices are implemented; full Milestone 6 remains future work"
check_current_ocore_docs \
    '(M6A|Milestone 6A)[^.]*(shared|foreign|request-scoped)[^.]*memory view[^.]*(complete|implemented|PASS)' \
    "M6A has no shared or request-scoped foreign-process memory view"
check_current_ocore_docs \
    '(M6A|Milestone 6A)[^.]*(Linux|foreign operating-system) (ABI|personality)[^.]*(implemented|supported|PASS)' \
    "M6A is a native scalar test personality, not a foreign operating-system ABI"
check_current_ocore_docs \
    '(M6A|Milestone 6A)[^.]*(package-managed personality|installable personality|upgradeable personality)' \
    "M6A package-loads a pinned test personality but does not implement native personality package management"
check_current_ocore_docs \
    '(M6A|Milestone 6A)[^.]*supervisor[^.]*(performs|owns)[^.]*capability rebind' \
    "the M6A supervisor requests policy actions while O-core performs capability rebind"

# HostedSupervisor now rejects stale durable mutations with an active-set
# revision compare-and-swap. The CLI's wider transaction lock is not the only
# protection available to callers of the direct API.
check 'direct [`]*HostedSupervisor[`]* API[^.]*(do(es)? not|has no)[^.]*(revision|stale.writer)|direct supervisor API has no persisted-revision' \
    "the hosted supervisor API now rejects stale active-set writers by persisted revision"

# Ostadix World has one native constitution plus one explicitly non-qualifying
# hosted reference profile. Pin both truths so a hosted demo or bounded native
# mode cannot silently acquire G0-G13 or Alpha credit.
require_fixed() {
    local file="$1" text="$2" why="$3"
    if [ ! -f "$file" ]; then
        echo "MISSING WORLD CONTRACT SURFACE: $file ($why)"
        fail=1
        return
    fi
    if ! grep -Fq -- "$text" "$file"; then
        echo "WORLD CONTRACT DRIFT: $why"
        echo "  file: $file"
        echo "  required text: $text"
        echo
        fail=1
    fi
}

require_fixed docs/OSTADIX_WORLD.md \
    'normative native Alpha constitution and implementation program' \
    'the native constitutional status is missing'
require_fixed docs/OSTADIX_WORLD.md \
    'A computer is not a box. A computer is a governed structure of computational resources.' \
    'the governing statement is missing'
require_fixed docs/OSTADIX_WORLD.md \
    'They do not satisfy the native release gates in this roadmap.' \
    'the hosted-reference release exclusion is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '## 2.1 Ostadix World Alpha qualifying gate' \
    'the native Alpha qualification section is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '**At least three physical machines boot O-core as the sovereign kernel.**' \
    'the physical native-node minimum is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '**The Governor is logically singular and physically replicated.**' \
    'the replicated-Governor requirement is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '**A real foreign Linux kernel runs as a contained KernelWorld.**' \
    'the real-Linux KernelWorld requirement is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '**A real physical device is controlled through the foreign-kernel machinery.**' \
    'the physical-device requirement is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '## 3.2 The three crossing kinds remain constitutional' \
    'the OValue/capability/capsule partition is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '## 3.4 The Governor consistency model is fixed now' \
    'the replicated consistency model is missing'
require_fixed docs/OSTADIX_WORLD.md \
    'A minority partition enters **island mode**.' \
    'the minority-partition fencing rule is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '## 3.6 The memory model is aggregate and explicit, not transparent DSM' \
    'the honest aggregate-memory model is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '# 21. Integration gate ladder' \
    'the G0-G13 convergence ladder is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '**G0 -- constitutional baseline**' \
    'the G0 definition is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '**G13 -- eight-node World Alpha**' \
    'the G13 definition is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '# 28. Alpha non-claims' \
    'the Alpha non-claim section is missing'
require_fixed docs/OSTADIX_WORLD.md \
    '- uniform coherent RAM across nodes;' \
    'the coherent-RAM non-claim is missing'
require_fixed docs/OSTADIX_WORLD.md \
    'gate is not implemented merely because it is defined here or in the registry.' \
    'the definition-is-not-evidence rule is missing'

require_fixed docs/HOSTED_WORLD_REFERENCE_PROFILE.md \
    'non-qualifying for native Ostadix' \
    'the hosted profile is not explicitly non-qualifying'
require_fixed docs/HOSTED_WORLD_REFERENCE_PROFILE.md \
    'cannot satisfy G0 through G13' \
    'the hosted profile could be misread as gate evidence'
require_fixed docs/HOSTED_WORLD_REFERENCE_PROFILE.md \
    'G12, G13, or the name **Ostadix World Alpha**' \
    'the hosted profile Alpha non-claim is missing'

require_fixed docs/CLAIMS.md \
    'gate is `defined`; zero gates are `passed`, including G0 and G13.' \
    'the current zero-passed-gate boundary is missing'
require_fixed docs/CLAIMS.md \
    "Mode 23's synthetic guest is not G7 or G8" \
    'the bounded KernelWorld substitution guard is missing'
require_fixed docs/CLAIMS.md \
    'All 20 identity atoms named by' \
    'the complete shared World identity vocabulary is missing'
require_fixed docs/CLAIMS.md \
    'A serialized `CapabilityId` is descriptive data' \
    'serialized identity must not be promoted to capability authority'
require_fixed docs/CLAIMS.md \
    'Mode 28 adds the bounded canonical World wire-codec PR3 gate' \
    'the implemented PR3 codec boundary is missing'
require_fixed docs/CLAIMS.md \
    'fixed 20-record, 1254-byte corpus' \
    'the exact cross-language World protocol corpus is missing'
require_fixed docs/CLAIMS.md \
    'not a stream or network transport' \
    'the World protocol codec could be misread as a live transport'
require_fixed docs/CLAIMS.md \
    'decode and negotiation grant no bearer' \
    'the World protocol codec could be misread as authority'
require_fixed docs/CLAIMS.md \
    'Mode 29 adds the bounded canonical World-value PR4 gate' \
    'the implemented PR4 portable-value boundary is missing'
require_fixed docs/CLAIMS.md \
    'separate self-framed `OWVALUE`' \
    'OWVALUE must remain separate from frozen OWPROTO v1'
require_fixed docs/CLAIMS.md \
    'root-only inert versioned' \
    'the bounded extension-envelope rule is missing'
require_fixed docs/CLAIMS.md \
    'same SHA-256 over each complete record' \
    'the canonical full-record hash claim is missing'
require_fixed docs/CLAIMS.md \
    'fixed 19-record,' \
    'the exact World-value corpus record count is missing'
require_fixed docs/CLAIMS.md \
    '928-byte corpus is 1856 lowercase hex digits' \
    'the exact World-value corpus byte count is missing'
require_fixed docs/CLAIMS.md \
    '264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc' \
    'the exact World-value corpus digest is missing'
require_fixed docs/CLAIMS.md \
    'does not make the full hosted `OValue` enum portable' \
    'the bounded portable allowlist could be promoted to the full hosted value enum'
require_fixed docs/CLAIMS.md \
    'passes no G0--G13 gate' \
    'the Mode 29 value slice could be misread as Alpha qualification'
require_fixed docs/CLAIMS.md \
    'Mode 30 adds the bounded canonical World-receipt PR5 gate' \
    'the implemented PR5 canonical-receipt boundary is missing'
require_fixed docs/CLAIMS.md \
    '`OWRECEIPT` v1 record binds' \
    'OWRECEIPT must remain separate from OWPROTO and OWVALUE'
require_fixed docs/CLAIMS.md \
    'pinned, explicitly non-secret conformance key' \
    'the receipt conformance key could be misread as production key material'
require_fixed docs/CLAIMS.md \
    'does not implement or prove a general' \
    'the native signature envelope could be misread as an Ed25519 verifier'
require_fixed docs/CLAIMS.md \
    'do not yet emit or consume it in live execution' \
    'the offline receipt corpus could be misread as live receipt integration'
require_fixed docs/CLAIMS.md \
    'typed World Alpha attestation' \
    'the PR5 receipt could be misread as qualifying World Alpha evidence'
require_fixed docs/CLAIMS.md \
    'Hosted ResourceKey PR6 now supplies typed World, Governor, node,' \
    'the complete hosted PR6 ResourceKey vocabulary is missing'
require_fixed docs/CLAIMS.md \
    'This is not O-core Mode 31,' \
    'the hosted PR6 corpus could be misread as native evidence'
require_fixed README.md \
    'World ResourceKey hosted repository-conformance gate' \
    'the executable hosted PR6 gate is missing from the public status'
require_fixed README.md \
    'No production lowering currently emits these' \
    'the hosted PR6 vocabulary could be misread as production governed lowering'
require_fixed README.md \
    'Project HGraph hosted logical-planning gate' \
    'the executable hosted PR7 project-planning gate is missing from the public status'
require_fixed scripts/o-cli.sh \
    'exec "$OLANGC_BIN" "$1" --target ir "${@:2}"' \
    'the repository-owned o plan dispatcher no longer reaches project IR planning'
require_fixed setup.sh \
    '"$PROJECT_ROOT/scripts/install-o-cli-wrapper.sh" "$CARGO_BIN_DIR/o"' \
    'the cargo-bin lowercase o wrapper no longer delegates to the repository installer'
require_fixed setup.sh \
    '"$PROJECT_ROOT/scripts/install-o-cli-wrapper.sh" "$BIN_DIR/o"' \
    'the local-bin lowercase o wrapper no longer delegates to the repository installer'
require_fixed scripts/install-o-cli-wrapper.sh \
    'exec "$ROOT/scripts/o-cli.sh" "\$@"' \
    'the installed lowercase o wrapper no longer delegates to the repository dispatcher'
require_fixed AGENTS.md \
    'export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$O_LANG_ROOT/target/release:$PATH"' \
    'the canonical PATH order lets the case-insensitive raw O binary shadow lowercase o'
require_fixed scripts/smoke-project-hgraph.sh \
    'PATH="$installed_local_bin:$installed_cargo_bin:$ROOT/target/release:/usr/bin:/bin"' \
    'the installed o smoke no longer covers the documented target-release shadow path'
require_fixed docs/CLAIMS.md \
    'PR7 now provides a bounded hosted project logical planner.' \
    'the implemented PR7 project-plan boundary is missing'
require_fixed docs/CLAIMS.md \
    'logical-planning gate. It does not execute project commands through the' \
    'the hosted project graph could be misread as the command-execution path'
require_fixed docs/CLAIMS.md \
    'conservative fallible `HostWorld` effects' \
    'untrusted project purity could be misread as verified mediated execution'
require_fixed docs/CLAIMS.md \
    'Logical alternative branches may therefore be serialized and' \
    'logical branches could be misread as independently mediated or parallel execution'
require_fixed docs/OSTADIX_WORLD.md \
    'complete at the bounded hosted logical-planning' \
    'the PR7 roadmap status is missing'
require_fixed docs/CLAIMS.md \
    'Strict decoding rejects malformed' \
    'invalid-record rejection must remain a decoder claim'
require_fixed docs/CLAIMS.md \
    'current/reference checks reject stale generations' \
    'staleness must remain a current/reference comparison claim'
require_fixed docs/OSTADIX_WORLD.md \
    '20-atom Rust/`.oc` vocabulary' \
    'the shared-identity Move 2 status is missing'
require_fixed README.md \
    'passes no G0--G13 gate.' \
    'the Mode 27 identity slice could be misread as Alpha qualification'
require_fixed docs/OSTADIX_WORLD.md \
    'Schema v1 admits no evidence records; only a future versioned,' \
    'the definition-only evidence boundary is missing'

for file in README.md llms.txt docs/CLAIMS.md docs/ODOMAIN_PLAN.md \
    okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md
do
    require_fixed "$file" 'OSTADIX_WORLD.md' \
        "$file does not link the native World constitution"
done

for file in README.md ARCHITECTURE.md docs/CLAIMS.md docs/ODOMAIN_PLAN.md \
    docs/RELEASE_CHECKLIST.md
do
    require_fixed "$file" 'world_alpha_gates.toml' \
        "$file does not link the G0-G13 qualification registry"
done

for file in README.md llms.txt ARCHITECTURE.md docs/CLAIMS.md \
    okernel-multikernel/MULTIKERNEL_PERSONALITY_PROPOSAL.md
do
    require_fixed "$file" 'HOSTED_WORLD_REFERENCE_PROFILE.md' \
        "$file does not preserve the hosted reference as non-qualifying"
done

check 'M7.?M11[^.]*(are|remain)[^.]*(all )?planned|next PR[^.]*M7[^.]*slice.?1|Mode (23|24|25|26|27|28|29) is the next[^.]*slice' \
    "Modes 24-29 and KernelWorld Mode 23 are bounded implemented gates, not wholly planned next slices"

exit $fail
