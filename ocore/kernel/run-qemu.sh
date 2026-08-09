#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${OCORE_BUILD_DIR:-$ROOT/target/ocore-kernel}"
PROBE_MODE="${OCORE_PROBE_MODE:-0}"
QEMU_BIN="${OCORE_QEMU_BIN:-qemu-system-x86_64}"

if [[ $# -ne 0 ]]; then
  echo "error: run-qemu.sh does not accept arguments" >&2
  exit 2
fi

case "$PROBE_MODE" in
  0 | 16) ;;
  *)
    echo "error: interactive QEMU supports only OCORE_PROBE_MODE=0 or 16" >&2
    exit 2
    ;;
esac

# Resolve an existing prefix physically, then normalize any missing suffix.
# This lets the guard reject aliases of sensitive directories without creating
# the requested build directory as part of validation.
resolve_guard_path() {
  local path="$1"
  local name
  local parent
  local resolved_parent

  if [[ "$path" != /* ]]; then
    path="$PWD/$path"
  fi
  while [[ "$path" != "/" && "$path" == */ ]]; do
    path="${path%/}"
  done

  if [[ -d "$path" ]]; then
    (cd "$path" && pwd -P)
    return
  fi

  name="${path##*/}"
  parent="${path%/*}"
  if [[ -z "$parent" ]]; then
    parent="/"
  fi
  case "$name" in
    '' | .)
      resolve_guard_path "$parent"
      ;;
    ..)
      resolved_parent="$(resolve_guard_path "$parent")"
      if [[ "$resolved_parent" == "/" ]]; then
        printf '/\n'
      else
        parent="${resolved_parent%/*}"
        printf '%s\n' "${parent:-/}"
      fi
      ;;
    *)
      resolved_parent="$(resolve_guard_path "$parent")"
      if [[ "$resolved_parent" == "/" ]]; then
        printf '/%s\n' "$name"
      else
        printf '%s/%s\n' "$resolved_parent" "$name"
      fi
      ;;
  esac
}

ROOT_GUARD="$(resolve_guard_path "$ROOT")"
BUILD_DIR_GUARD="$(resolve_guard_path "$BUILD_DIR")"
HOME_GUARD=""
if [[ -n "${HOME:-}" ]]; then
  HOME_GUARD="$(resolve_guard_path "$HOME")"
fi

case "$BUILD_DIR_GUARD" in
  /)
    echo "error: unsafe OCORE_BUILD_DIR resolves to the filesystem root" >&2
    exit 2
    ;;
  "$ROOT_GUARD")
    echo "error: unsafe OCORE_BUILD_DIR resolves to the repository root" >&2
    exit 2
    ;;
  "$HOME_GUARD")
    if [[ -n "$HOME_GUARD" ]]; then
      echo "error: unsafe OCORE_BUILD_DIR resolves to the home directory" >&2
      exit 2
    fi
    ;;
esac

if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
  printf 'error: QEMU executable is not installed: %s\n' "$QEMU_BIN" >&2
  if [[ "$QEMU_BIN" == "qemu-system-x86_64" ]]; then
    echo "install locally with: brew install qemu" >&2
  fi
  exit 127
fi

BUILD_OUTPUT="$(
  OCORE_PROBE_MODE="$PROBE_MODE" OCORE_BUILD_DIR="$BUILD_DIR" \
    "$ROOT/ocore/kernel/build.sh"
)"
printf '%s\n' "$BUILD_OUTPUT"

if [[ "$PROBE_MODE" == "16" ]]; then
  M5_DIGEST="$(
    printf '%s\n' "$BUILD_OUTPUT" | sed -n 's/^m5-sha256: //p' | tail -n 1
  )"
  if [[ ! "$M5_DIGEST" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: mode-16 build did not report the embedded M5 image digest" >&2
    exit 1
  fi
  cat >&2 <<EOF

O-core native console commands after the 'o> ' prompt:
  status
  install $M5_DIGEST 5 1
  activate $M5_DIGEST

The install/activate sequence runs the bounded M5 lifecycle and then parks the
console while the kernel proves fault containment, restart, and reclamation.
Exit QEMU at any time with Ctrl-A X.

EOF
else
  printf '\nO-core baseline boot. Exit QEMU with Ctrl-A X.\n\n' >&2
fi

exec "$QEMU_BIN" \
  -machine q35 \
  -m 128M \
  -nodefaults \
  -nic none \
  -kernel "$BUILD_DIR/kernel.elf" \
  -display none \
  -serial mon:stdio \
  -no-reboot \
  -no-shutdown
