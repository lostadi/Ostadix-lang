#!/bin/sh
# Runtime launcher embedded in the OSTADIX hosted-live workstation initramfs.
set -eu

emit_serial() {
  if [ -c /dev/ttyS0 ]; then
    printf '%s\n' "$*" >/dev/ttyS0 2>/dev/null || true
  fi
}

terminal_session() {
  cd /usr/src/ostadix
  export HOME=/root
  export O_LANG_ROOT=/usr/src/ostadix
  export CARGO_HOME=/root/.cargo
  export CARGO_TARGET_DIR=/workspace/target
  export CARGO_NET_OFFLINE=true
  export CARGO_PROFILE_RELEASE_LTO=false
  export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
  export O_BACKENDS_DIR=/usr/src/ostadix/backends
  export OSTADIX_NOTEBOOK_BROWSER=/usr/bin/firefox-esr
  export PATH=/usr/local/bin:/root/.cargo/bin:/sbin:/bin:/usr/sbin:/usr/bin
  export PS1='ostadix-workstation:\w# '
  clear 2>/dev/null || true
  # The six ANSI families are both a useful command legend and an intentional
  # visual contract for the QEMU desktop gate. A grayscale text VT cannot pass
  # the corresponding chromatic-hue checks.
  printf '\033[1;38;5;81mOSTADIX Workstation\033[0m\n'
  printf '\033[38;5;203m  objects  o object verify              verify the complete boot object store\033[0m\n'
  printf '\033[38;5;220m  rust     cargo --version              inspect the embedded Rust toolchain\033[0m\n'
  printf '\033[38;5;221m  builds   /workspace/target             writable offline Cargo output\033[0m\n'
  printf '\033[38;5;114m  packages apk --version                inspect the Alpine package manager\033[0m\n'
  printf '\033[38;5;81m  o-lang   O examples/hello.O backends  run O from the exact staged source tree\033[0m\n'
  printf '\033[38;5;117m  wasm     O examples/webassembly_hello.O backends  run O through Wasmtime\033[0m\n'
  printf '\033[38;5;111m  source   pwd                          inspect /usr/src/ostadix\033[0m\n'
  printf '\033[38;5;215m  systems  reboot, then choose a/g/b/p/r for Alpine/Guix/OpenBSD/9front/Redox\033[0m\n'
  printf '\033[38;5;218m  desktop  openbox + xterm              active local GUI session\033[0m\n'
  printf '\033[38;5;183m  notebook o-notebook                    open the local O notebook GUI\033[0m\n\n'
  exec /bin/bash --noprofile --norc -i
}

x_session() {
  export HOME=/root
  export DISPLAY=${DISPLAY:-:0}
  export XDG_RUNTIME_DIR=/run/user/0
  mkdir -p "$XDG_RUNTIME_DIR"
  chmod 0700 "$XDG_RUNTIME_DIR"

  if ! xsetroot -solid '#181825'; then
    emit_serial 'OSTADIX HOSTED DESKTOP: FAIL: xsetroot palette failed'
    return 1
  fi
  openbox --sm-disable >/tmp/ostadix-openbox.log 2>&1 &
  window_manager=$!
  O_BACKENDS_DIR=/usr/src/ostadix/backends \
    OSTADIX_NOTEBOOK_BROWSER=/usr/bin/firefox-esr \
    o-notebook >/tmp/ostadix-notebook-gui.out \
    2>/tmp/ostadix-notebook-gui.err &
  notebook=$!
  notebook_window=
  for attempt in $(seq 1 60); do
    if ! kill -0 "$window_manager" 2>/dev/null \
        || ! kill -0 "$notebook" 2>/dev/null; then
      break
    fi
    for window in $(xprop -root _NET_CLIENT_LIST 2>/dev/null \
        | sed 's/.*#//' | tr ',' ' '); do
      case "$window" in
        0x*)
          if xprop -id "$window" WM_CLASS 2>/dev/null \
              | grep -Eiq 'firefox|navigator'; then
            notebook_window=$window
            break
          fi
          ;;
      esac
    done
    [ -z "$notebook_window" ] || break
    sleep 1
  done
  if [ -z "$notebook_window" ]; then
    emit_serial 'OSTADIX HOSTED NOTEBOOK GUI: FAIL: Firefox window did not appear'
    return 1
  fi
  emit_serial 'OSTADIX HOSTED NOTEBOOK GUI READY: PASS'
  xterm -geometry 90x28+24+24 -title 'OSTADIX Workstation' \
    -bg '#1e1e2e' -fg '#cdd6f4' -cr '#f5e0dc' \
    -e /usr/local/bin/ostadix-desktop terminal \
    >/tmp/ostadix-xterm.log 2>&1 &
  terminal=$!
  sleep 2
  if ! kill -0 "$window_manager" 2>/dev/null \
      || ! kill -0 "$notebook" 2>/dev/null \
      || ! kill -0 "$terminal" 2>/dev/null; then
    emit_serial 'OSTADIX HOSTED DESKTOP: FAIL: window manager, notebook, or terminal exited'
    return 1
  fi
  emit_serial 'OSTADIX HOSTED DESKTOP READY: PASS'

  while kill -0 "$window_manager" 2>/dev/null; do
    if ! kill -0 "$notebook" 2>/dev/null; then
      emit_serial 'OSTADIX HOSTED NOTEBOOK GUI: FAIL: notebook server exited'
      return 1
    fi
    if ! kill -0 "$terminal" 2>/dev/null; then
      xterm -geometry 90x28+24+24 -title 'OSTADIX Workstation' \
        -bg '#1e1e2e' -fg '#cdd6f4' -cr '#f5e0dc' \
        -e /usr/local/bin/ostadix-desktop terminal \
        >/tmp/ostadix-xterm.log 2>&1 &
      terminal=$!
    fi
    sleep 1
  done
  emit_serial 'OSTADIX HOSTED DESKTOP: FAIL: openbox exited'
  return 1
}

start_x() {
  exec /usr/bin/startx /usr/local/bin/ostadix-desktop x-session \
    -- :0 vt1 -nolisten tcp
}

launch_desktop() {
  for command in openvt startx Xorg openbox xprop xsetroot xterm firefox-esr \
      o-notebook python3; do
    if ! command -v "$command" >/dev/null 2>&1; then
      emit_serial "OSTADIX HOSTED DESKTOP: FAIL: missing $command"
      return 1
    fi
  done
  if [ ! -c /dev/tty1 ]; then
    emit_serial 'OSTADIX HOSTED DESKTOP: FAIL: /dev/tty1 unavailable'
    return 1
  fi
  if [ ! -s /usr/share/fonts/misc/fonts.dir ] \
      || [ ! -s /usr/share/fonts/misc/fonts.alias ] \
      || ! grep -Eq '[[:space:]]-misc-fixed-' /usr/share/fonts/misc/fonts.dir \
      || ! grep -Eq '^fixed[[:space:]]' /usr/share/fonts/misc/fonts.alias; then
    emit_serial 'OSTADIX HOSTED X11 FONT: FAIL: fixed font is not indexed'
    return 1
  fi
  emit_serial 'OSTADIX HOSTED X11 FONT: PASS'
  if ! python3 - <<'PY'
import os

master, slave = os.openpty()
os.close(master)
os.close(slave)
PY
  then
    emit_serial 'OSTADIX HOSTED PTY: FAIL: Unix98 PTY allocation failed'
    return 1
  fi
  emit_serial 'OSTADIX HOSTED PTY: PASS'
  if command -v udevd >/dev/null 2>&1; then
    udevd --daemon >/tmp/ostadix-udevd.log 2>&1 || true
    udevadm trigger --action=add >/tmp/ostadix-udevadm.log 2>&1 || true
    udevadm settle --timeout=5 >>/tmp/ostadix-udevadm.log 2>&1 || true
  fi
  event_device=
  for candidate in /dev/input/event*; do
    if [ -c "$candidate" ]; then
      event_device=$candidate
      break
    fi
  done
  if [ -z "$event_device" ]; then
    emit_serial 'OSTADIX HOSTED EVDEV: FAIL: no /dev/input/event device'
    return 1
  fi
  emit_serial 'OSTADIX HOSTED EVDEV: PASS'
  if ! openvt -c 1 -s -w /usr/local/bin/ostadix-desktop start-x; then
    emit_serial 'OSTADIX HOSTED DESKTOP: FAIL: openvt/startx failed'
    return 1
  fi
  emit_serial 'OSTADIX HOSTED DESKTOP: FAIL: graphical session exited'
  return 1
}

case "${1:-launch}" in
  launch) launch_desktop ;;
  start-x) start_x ;;
  x-session) x_session ;;
  terminal) terminal_session ;;
  *)
    printf 'usage: ostadix-desktop [launch|start-x|x-session|terminal]\n' >&2
    exit 2
    ;;
esac
