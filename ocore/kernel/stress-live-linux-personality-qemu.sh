#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SMOKE="$ROOT/ocore/kernel/smoke-live-linux-personality-qemu.sh"
RUNS="${OCORE_M6_LINUX_STRESS_RUNS:-2}"
PRESSURE_WORKERS="${OCORE_M6_LINUX_STRESS_PRESSURE_WORKERS:-0}"

if [[ ! "$RUNS" =~ ^([2-9]|[1-9][0-9]{1,2})$ ]]; then
  echo "error: OCORE_M6_LINUX_STRESS_RUNS must be an integer from 2 through 999 without leading zeros" >&2
  exit 2
fi
if [[ ! "$PRESSURE_WORKERS" =~ ^[0-8]$ ]]; then
  echo "error: OCORE_M6_LINUX_STRESS_PRESSURE_WORKERS must be an integer from 0 through 8" >&2
  exit 2
fi
if (( PRESSURE_WORKERS > 0 )) && ! command -v yes >/dev/null 2>&1; then
  echo "error: yes is required when Mode 25 host-pressure workers are requested" >&2
  exit 127
fi

cleanup_root=0
pressure_pids=()
if [[ -n "${OCORE_M6_LINUX_STRESS_ROOT:-}" ]]; then
  mkdir -p -- "$OCORE_M6_LINUX_STRESS_ROOT"
  STRESS_ROOT="$(mktemp -d "$OCORE_M6_LINUX_STRESS_ROOT/mode25-stress.XXXXXX")"
else
  STRESS_ROOT="$(mktemp -d "/tmp/ostadix-mode25-stress.XXXXXX")"
  cleanup_root=1
fi

cleanup() {
  local pid
  for pid in ${pressure_pids[@]+"${pressure_pids[@]}"}; do
    kill "$pid" 2>/dev/null || true
  done
  for pid in ${pressure_pids[@]+"${pressure_pids[@]}"}; do
    wait "$pid" 2>/dev/null || true
  done
  pressure_pids=()
  if (( cleanup_root == 1 )); then
    if [[ -z "$STRESS_ROOT" \
      || ! -d "$STRESS_ROOT" \
      || "$STRESS_ROOT" != /tmp/ostadix-mode25-stress.* ]]; then
      echo "warning: refusing to clean unexpected stress root: ${STRESS_ROOT:-<empty>}" >&2
      return
    fi
    find "$STRESS_ROOT" -depth -delete
  fi
}
trap cleanup EXIT

for ((worker = 1; worker <= PRESSURE_WORKERS; worker += 1)); do
  yes >/dev/null &
  pressure_pids+=("$!")
done
if (( PRESSURE_WORKERS > 0 )); then
  echo "Mode 25 non-admissible host pressure: $PRESSURE_WORKERS tracked workers"
fi

for ((iteration = 1; iteration <= RUNS; iteration += 1)); do
  run_dir="$STRESS_ROOT/run-$iteration"
  log="$STRESS_ROOT/run-$iteration.log"
  mkdir -- "$run_dir"
  if ! OCORE_BUILD_DIR="$run_dir" "$SMOKE" >"$log" 2>&1; then
    cat "$log" >&2
    echo \
      "Mode 25 non-admissible repeat stress: FAIL at iteration $iteration/$RUNS" \
      >&2
    exit 1
  fi
  cat "$log"
  if [[ "$(grep -Fxc 'M6 live Linux personality smoke: PASS' "$log")" != 1 ]]; then
    echo \
      "Mode 25 non-admissible repeat stress: invalid PASS count at iteration $iteration/$RUNS" \
      >&2
    exit 1
  fi
  echo "Mode 25 non-admissible stress iteration $iteration/$RUNS: PASS"
done

echo "Mode 25 non-admissible repeat stress: PASS ($RUNS/$RUNS)"
