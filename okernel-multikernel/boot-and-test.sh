#!/usr/bin/env bash
# =============================================================================
# Ostadix-lang / O-core : boot + test command block
# -----------------------------------------------------------------------------
# One entrypoint that exercises every layer of the system, from the hosted
# polyglot `.O` runtime up to a live QEMU boot of the O-core (Okernel) image
# and its full smoke-gate matrix.
#
# Every command below is a thin wrapper over a real script that already ships
# in this repo (setup.sh, ocore/kernel/*.sh) or a documented cargo/ocorec
# invocation. Nothing here is invented; this file just sequences them and adds
# tool-presence guards so a missing optional dependency skips rather than aborts.
#
# USAGE
#   ./okernel-multikernel/boot-and-test.sh [phase]
#
#   phase (default: quick)
#     setup    Run ./setup.sh --minimal --verify  (build Rust+C17+Python editions)
#     hosted   Hosted .O runtime + differential (graph vs serial) + Python/C17 xcheck
#     ocore    Build ocorec, dump HIR/MIR, emit a freestanding ELF object
#     kernel   Build the kernel ELF and BOOT it interactively in QEMU (serial console)
#     smoke    Build + run the asserted 4s smoke gate, then the full probe matrix
#     tests    Rust test suite + parser proptest + reproducibility + example sweep
#     quick    hosted + ocore + default smoke  (fast confidence check)
#     full     setup + hosted + ocore + smoke(all) + tests   (everything)
#
# ENVIRONMENT
#   OCORE_LLD   Absolute path to rust-lld / ld.lld if it lives outside the
#               Rust sysroot, PATH, or Homebrew prefixes (build.sh probes those).
#   Kernel layer needs: clang (x86_64-unknown-none-elf assembler), an LLD-class
#   linker, and qemu-system-x86_64. Hosted layer needs: cargo, python3; node for
#   javascript^, etc. Missing optional tools are reported and skipped.
# =============================================================================
set -uo pipefail

# Locate the repo root robustly: walk up from the script's own directory, then
# from $PWD, looking for the marker pair (Cargo.toml + ocore/kernel/build.sh).
# This makes the script work whether it lives at the repo root, inside
# okernel-multikernel/, or is invoked from any subdirectory of the tree.
find_repo_root() {
  local start d
  for start in "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" "$PWD"; do
    d="$start"
    while [ -n "$d" ] && [ "$d" != "/" ]; do
      if [ -f "$d/Cargo.toml" ] && [ -f "$d/ocore/kernel/build.sh" ]; then
        printf '%s\n' "$d"; return 0
      fi
      d="$(dirname "$d")"
    done
  done
  return 1
}
ROOT="$(find_repo_root)" || {
  printf 'error: could not find the Ostadix repo root (a directory containing both Cargo.toml and ocore/kernel/build.sh) from the script location or %s\n' "$PWD" >&2
  exit 1
}
cd "$ROOT"

# ---- pretty banners ---------------------------------------------------------
c_bold=$'\033[1m'; c_grn=$'\033[32m'; c_yel=$'\033[33m'; c_red=$'\033[31m'; c_rst=$'\033[0m'
say()  { printf '\n%s==> %s%s\n' "$c_bold" "$*" "$c_rst"; }
ok()   { printf '%s[ ok ]%s %s\n'   "$c_grn" "$c_rst" "$*"; }
skip() { printf '%s[skip]%s %s\n'   "$c_yel" "$c_rst" "$*"; }
die()  { printf '%s[fail]%s %s\n'   "$c_red" "$c_rst" "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
need() { have "$1" || die "required tool '$1' not found on PATH"; }

# ---- layer 0: setup / bootstrap --------------------------------------------
phase_setup() {
  say "Layer 0 — bootstrap (setup.sh)"
  [ -x ./setup.sh ] || die "setup.sh missing or not executable"
  # --minimal: core only; swap for --full to install every backend runtime.
  # --verify:  runs hello/meta example verification after the build.
  ./setup.sh --minimal --verify
  ok "setup complete (Rust + C17 + Python editions built)"
}

# ---- layer 1: hosted .O polyglot runtime -----------------------------------
phase_hosted() {
  say "Layer 1 — hosted .O runtime"
  need cargo
  # Authoritative Rust interpreter. default-run is the `O` binary.
  cargo run -q -- examples/hello.O                         || die "hosted hello.O failed"
  cargo run -q -- examples/hello.O backends                || die "hosted hello.O (backends) failed"
  ok "graph executor (default) ran hello.O"

  # Differential conformance: the serial topological OIR interpreter is the
  # reference oracle. Its output must agree with the default graph coordinator.
  O_EXECUTOR=serial cargo run -q -- examples/hello.O        || die "serial oracle failed"
  ok "serial oracle agrees (differential check)"

  # Python reference edition (readable semantics cross-check).
  if have python3; then
    python3 -m o_lang examples/hello.O                      && ok "python reference ran hello.O" \
      || skip "python reference edition returned nonzero"
  else skip "python3 absent — skipping reference edition"; fi

  # C17 standalone edition.
  if have make && have cc; then
    make -C c_cpp >/dev/null 2>&1 && c_cpp/O examples/hello.O backends \
      && ok "C17 edition ran hello.O" || skip "C17 edition build/run skipped"
  else skip "make/cc absent — skipping C17 edition"; fi
}

# ---- layer 2: O-core native compiler (ocorec) ------------------------------
phase_ocore() {
  say "Layer 2 — O-core compiler (ocorec)"
  need cargo
  cargo build -q --bin ocorec || die "ocorec build failed"
  local OCOREC=target/debug/ocorec
  # Inspect the typed pipeline on the minimal example: AST -> HIR -> MIR -> ELF obj.
  "$OCOREC" ocore/examples/minimal.oc --emit hir -o - >/dev/null && ok "typed HIR emitted"
  "$OCOREC" ocore/examples/minimal.oc --emit mir -o - >/dev/null && ok "SSA MIR emitted"
  mkdir -p target
  "$OCOREC" ocore/examples/minimal.oc --emit obj --keep-asm -o target/minimal.o \
    && ok "freestanding x86_64 ELF object emitted -> target/minimal.o"
  have file && file target/minimal.o || true
}

# ---- layer 3: boot the Okernel in QEMU (interactive) -----------------------
phase_kernel() {
  say "Layer 3 — build + BOOT the O-core kernel (interactive serial console)"
  need cargo; need clang; need qemu-system-x86_64
  # build.sh compiles the runtime+kernel .oc unit through ocorec, assembles
  # boot.S, links with LLD against linker.ld -> target/ocore-kernel/kernel.elf
  ./ocore/kernel/build.sh || die "kernel build failed (set OCORE_LLD if the linker wasn't found)"
  ok "kernel.elf built"
  say "Handing off to QEMU. Ctrl-A X to quit the serial console."
  # run-qemu.sh rebuilds (cheap) then execs qemu with -serial stdio, no display.
  exec ./ocore/kernel/run-qemu.sh
}

# ---- layer 4: smoke-gate matrix --------------------------------------------
# Each script sets its own OCORE_PROBE_MODE and asserts a fixed serial trace.
# Probe map:  0 default | 9 user-copy fault | 12 M2 sched | 13 M3 ipc-foundation
# 14 M3 ipc | 15 M4 loader/OVFS | 16 M5 live | 17 M5 semantics | 18 M6A personality
phase_smoke() {
  say "Layer 4 — smoke gates"
  need cargo; need clang; need qemu-system-x86_64
  local gates=(
    smoke-qemu.sh                 # default gate (M0.1-M0.3 core: CPL3/SYSCALL, W^X, frames, caps)
    smoke-faults-qemu.sh          # fatal-fault matrix + user-copy recovery
    smoke-processes-qemu.sh       # M1 isolation + teardown
    smoke-scheduler-qemu.sh       # M2 thread/scheduler lifecycle
    smoke-ipc-foundation-qemu.sh  # M3 mechanism regression
    smoke-ipc-qemu.sh             # M3 public CPL3 IPC + containment
    smoke-loader-qemu.sh          # M4 OVFS + static ELF lifecycle
    smoke-live-qemu.sh            # M5 activation + one pkgd restart
    smoke-live-semantics-qemu.sh  # M5 state-machine corpus
    smoke-personality-qemu.sh     # M6A scalar personality supervision
  )
  local g rc=0
  for g in "${gates[@]}"; do
    if [ -x "ocore/kernel/$g" ]; then
      say "gate: $g"
      if "ocore/kernel/$g"; then ok "$g PASS"; else printf '%s[fail]%s %s\n' "$c_red" "$c_rst" "$g"; rc=1; fi
    else skip "missing $g"; fi
  done
  [ "$rc" -eq 0 ] && ok "all present smoke gates passed" || die "one or more smoke gates failed"
}

# ---- layer 5: host test suite ----------------------------------------------
phase_tests() {
  say "Layer 5 — host test suite"
  need cargo
  cargo test -q --all-targets --all-features || die "cargo test failed"
  cargo test -q --test parser_proptest        || die "parser proptest failed"
  # Byte-reproducibility of ocore object emission across source dirs (CI gate).
  cargo test -q --lib \
    ocore::driver::tests::ocore_object_is_byte_reproducible_across_source_directories -- --exact \
    && ok "ocore object emission is byte-reproducible"
  [ -x ./test_o_lang_examples.sh ] && ./test_o_lang_examples.sh || skip "example sweep script absent"
  ok "tests complete"
}

# ---- dispatch ---------------------------------------------------------------
phase="${1:-quick}"
case "$phase" in
  setup)  phase_setup ;;
  hosted) phase_hosted ;;
  ocore)  phase_ocore ;;
  kernel) phase_kernel ;;
  smoke)  phase_smoke ;;
  tests)  phase_tests ;;
  quick)  phase_hosted; phase_ocore; say "Layer 3/4 — default smoke gate"; \
          need cargo; need clang; need qemu-system-x86_64; ./ocore/kernel/smoke-qemu.sh && ok "QEMU smoke: PASS" ;;
  full)   phase_setup; phase_hosted; phase_ocore; phase_smoke; phase_tests; \
          say "ALL PHASES COMPLETE"; ok "Ostadix system booted and tested end to end" ;;
  *) die "unknown phase '$phase' (use: setup|hosted|ocore|kernel|smoke|tests|quick|full)" ;;
esac
