#!/usr/bin/env bash

# Resolve one x86_64 OVMF/edk2 code image without weakening an explicit caller
# choice. This file is sourced by the UEFI runners so discovery and diagnostics
# remain one implementation rather than four drifting candidate lists.

_ostadix_ovmf_code_candidate() {
  case "${1##*/}" in
    OVMF_CODE.fd|OVMF_CODE_4M.fd|edk2-x86_64-code.fd) return 0 ;;
    *) return 1 ;;
  esac
}

resolve_ostadix_x86_64_ovmf_code() {
  local qemu_bin="${1:-${OCORE_QEMU_BIN:-qemu-system-x86_64}}"
  local explicit="${OSTADIX_OVMF_CODE:-}"
  local candidate_status

  if [[ -n "$explicit" ]]; then
    candidate_status=missing
    if [[ -f "$explicit" ]]; then
      candidate_status=selected
    fi
    printf 'ovmf-discovery source=explicit candidate=%q status=%s\n' \
      "$explicit" "$candidate_status" >&2
    if [[ "$candidate_status" != selected ]]; then
      printf 'error: explicit OSTADIX_OVMF_CODE is not a file: %s\n' "$explicit" >&2
      return 127
    fi
    printf 'ovmf-discovery result=resolved source=explicit path=%q searched=1\n' \
      "$explicit" >&2
    printf '%s\n' "$explicit"
    return 0
  fi

  local -a candidates=()
  local -a sources=()
  local candidate package package_path
  local qemu_path=""
  local qemu_prefix=""
  local brew_prefix=""

  # Stable layouts remain first so discovery is deterministic across hosts.
  for candidate in \
    /opt/homebrew/opt/qemu/share/qemu/edk2-x86_64-code.fd \
    /usr/local/opt/qemu/share/qemu/edk2-x86_64-code.fd \
    /usr/share/OVMF/OVMF_CODE_4M.fd \
    /usr/share/OVMF/OVMF_CODE.fd \
    /usr/share/qemu/edk2-x86_64-code.fd \
    /usr/share/qemu/OVMF_CODE.fd \
    /usr/share/edk2/ovmf/OVMF_CODE_4M.fd \
    /usr/share/edk2/ovmf/OVMF_CODE.fd \
    /usr/share/edk2/x64/OVMF_CODE_4M.fd \
    /usr/share/edk2/x64/OVMF_CODE.fd; do
    candidates+=("$candidate")
    sources+=(known-layout)
  done

  qemu_path="$(command -v "$qemu_bin" 2>/dev/null || true)"
  if [[ -n "$qemu_path" && "$qemu_path" == */bin/* ]]; then
    qemu_prefix="${qemu_path%/bin/*}"
    candidates+=(
      "$qemu_prefix/share/qemu/edk2-x86_64-code.fd"
      "$qemu_prefix/share/qemu/OVMF_CODE.fd"
    )
    sources+=(qemu-prefix qemu-prefix)
  fi

  if command -v brew >/dev/null 2>&1; then
    brew_prefix="$(brew --prefix qemu 2>/dev/null || true)"
    if [[ -n "$brew_prefix" ]]; then
      candidates+=(
        "$brew_prefix/share/qemu/edk2-x86_64-code.fd"
        "$brew_prefix/share/qemu/OVMF_CODE.fd"
      )
      sources+=(homebrew-package homebrew-package)
    fi
  fi

  # Package manifests locate distro-specific directories without an unbounded
  # filesystem search. Only plain x86_64 code images are accepted; variable and
  # secure-boot-specific images are not substituted for this proof implicitly.
  if command -v dpkg-query >/dev/null 2>&1; then
    while IFS= read -r package_path; do
      if _ostadix_ovmf_code_candidate "$package_path"; then
        candidates+=("$package_path")
        sources+=(dpkg:ovmf)
      fi
    done < <(dpkg-query -L ovmf 2>/dev/null | LC_ALL=C sort || true)
  fi
  if command -v rpm >/dev/null 2>&1; then
    for package in edk2-ovmf edk2-ovmf-x64; do
      while IFS= read -r package_path; do
        if _ostadix_ovmf_code_candidate "$package_path"; then
          candidates+=("$package_path")
          sources+=("rpm:$package")
        fi
      done < <(rpm -ql "$package" 2>/dev/null | LC_ALL=C sort || true)
    done
  fi
  if command -v pacman >/dev/null 2>&1; then
    while IFS= read -r package_path; do
      package_path="${package_path#* }"
      if _ostadix_ovmf_code_candidate "$package_path"; then
        candidates+=("$package_path")
        sources+=(pacman:edk2-ovmf)
      fi
    done < <(pacman -Ql edk2-ovmf 2>/dev/null | LC_ALL=C sort || true)
  fi

  local -a unique_candidates=()
  local -a unique_sources=()
  local source previous
  local duplicate
  local index previous_index
  for ((index = 0; index < ${#candidates[@]}; index++)); do
    candidate="${candidates[index]}"
    source="${sources[index]}"
    duplicate=false
    # Bash 3.2 (the system Bash on macOS) treats `"${empty[@]}"` as an
    # unbound variable under `set -u`. Index iteration preserves nounset while
    # keeping the empty first pass well-defined.
    for ((previous_index = 0; previous_index < ${#unique_candidates[@]}; previous_index++)); do
      previous="${unique_candidates[previous_index]}"
      if [[ "$previous" == "$candidate" ]]; then
        duplicate=true
        break
      fi
    done
    if [[ "$duplicate" == false ]]; then
      unique_candidates+=("$candidate")
      unique_sources+=("$source")
    fi
  done

  local selected=""
  local selected_source=""
  for ((index = 0; index < ${#unique_candidates[@]}; index++)); do
    candidate="${unique_candidates[index]}"
    source="${unique_sources[index]}"
    if [[ -f "$candidate" ]]; then
      if [[ -z "$selected" ]]; then
        selected="$candidate"
        selected_source="$source"
        candidate_status=selected
      else
        candidate_status=available-not-selected
      fi
    else
      candidate_status=missing
    fi
    printf 'ovmf-discovery source=%s candidate=%q status=%s\n' \
      "$source" "$candidate" "$candidate_status" >&2
  done

  if [[ -z "$selected" ]]; then
    printf 'ovmf-discovery result=not-found searched=%d\n' \
      "${#unique_candidates[@]}" >&2
    echo 'error: UEFI firmware not found; set OSTADIX_OVMF_CODE to an OVMF/edk2 x86_64 code image' >&2
    return 127
  fi

  printf 'ovmf-discovery result=resolved source=%s path=%q searched=%d\n' \
    "$selected_source" "$selected" "${#unique_candidates[@]}" >&2
  printf '%s\n' "$selected"
}
