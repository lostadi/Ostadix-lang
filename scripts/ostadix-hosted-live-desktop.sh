#!/bin/sh
# Runtime launcher embedded in the OSTADIX hosted-live workstation initramfs.
set -eu

emit_serial() {
  if [ -c /dev/ttyS0 ]; then
    printf '%s\n' "$*" >/dev/ttyS0 2>/dev/null || true
  fi
}

notebook_page_ready() {
  python3 - <<'PY'
import time
import urllib.request

deadline = time.monotonic() + 30
last_error = None
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
while time.monotonic() < deadline:
    try:
        with opener.open("http://127.0.0.1:8888/", timeout=0.5) as response:
            page = response.read(1024 * 1024)
        if b"<title>O \xc2\xb7 Notebook</title>" not in page:
            raise RuntimeError("notebook title marker is absent")
        raise SystemExit(0)
    except Exception as error:
        last_error = error
        time.sleep(0.1)
raise SystemExit(f"notebook root did not become ready: {last_error!r}")
PY
}

focus_x11_window() {
  python3 - "$1" <<'PY'
import ctypes
import re
import sys

Display = ctypes.c_void_p
Window = ctypes.c_ulong
Atom = ctypes.c_ulong


class ClientMessageData(ctypes.Union):
    _fields_ = [
        ("b", ctypes.c_char * 20),
        ("s", ctypes.c_short * 10),
        ("l", ctypes.c_long * 5),
    ]


class XClientMessageEvent(ctypes.Structure):
    _fields_ = [
        ("type", ctypes.c_int),
        ("serial", ctypes.c_ulong),
        ("send_event", ctypes.c_int),
        ("display", Display),
        ("window", Window),
        ("message_type", Atom),
        ("format", ctypes.c_int),
        ("data", ClientMessageData),
    ]


class XEvent(ctypes.Union):
    _fields_ = [
        ("xclient", XClientMessageEvent),
        ("pad", ctypes.c_long * 24),
    ]


x11 = ctypes.CDLL("libX11.so.6")
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XOpenDisplay.restype = Display
x11.XDefaultRootWindow.argtypes = [Display]
x11.XDefaultRootWindow.restype = Window
x11.XInternAtom.argtypes = [Display, ctypes.c_char_p, ctypes.c_int]
x11.XInternAtom.restype = Atom
x11.XRaiseWindow.argtypes = [Display, Window]
x11.XRaiseWindow.restype = ctypes.c_int
x11.XSendEvent.argtypes = [
    Display,
    Window,
    ctypes.c_int,
    ctypes.c_long,
    ctypes.POINTER(XEvent),
]
x11.XSendEvent.restype = ctypes.c_int
x11.XSync.argtypes = [Display, ctypes.c_int]
x11.XSync.restype = ctypes.c_int
x11.XCloseDisplay.argtypes = [Display]
x11.XCloseDisplay.restype = ctypes.c_int

raw_window = sys.argv[1]
if not re.fullmatch(r"0x[0-9A-Fa-f]+", raw_window):
    raise SystemExit("invalid X11 window id")
window = int(raw_window, 16)
if not 0 < window <= (1 << (8 * ctypes.sizeof(Window))) - 1:
    raise SystemExit("invalid X11 window id")
display = x11.XOpenDisplay(None)
if not display:
    raise SystemExit("could not open the X11 display")
try:
    root = x11.XDefaultRootWindow(display)
    active_window = x11.XInternAtom(display, b"_NET_ACTIVE_WINDOW", 0)
    if not active_window:
        raise SystemExit("could not intern _NET_ACTIVE_WINDOW")
    event = XEvent()
    event.xclient.type = 33  # ClientMessage
    event.xclient.send_event = 1
    event.xclient.display = display
    event.xclient.window = window
    event.xclient.message_type = active_window
    event.xclient.format = 32
    # This launcher is the local session controller, so the EWMH source is a
    # pager/controller rather than an arbitrary application focus-steal.
    event.xclient.data.l[0] = 2
    event.xclient.data.l[1] = 0  # CurrentTime
    x11.XRaiseWindow(display, window)
    event_mask = (1 << 19) | (1 << 20)
    if x11.XSendEvent(display, root, 0, event_mask, ctypes.byref(event)) == 0:
        raise SystemExit("Openbox activation request was not sent")
    x11.XSync(display, 0)
finally:
    x11.XCloseDisplay(display)
PY
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
  printf '\033[38;5;215m  systems  reboot; choose o/a/g/b/p/r     O-core/Alpine/Guix/OpenBSD/9front/Redox\033[0m\n'
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

  # Keep the Ostadix terminal visible and focused after Firefox finishes
  # mapping the local notebook.  The visual gate must observe the actual
  # workstation palette and deliver its input proof to this Xterm, not merely
  # find three live client processes behind an uncovered root window.
  mkdir -p "$HOME/.config/openbox"
  cat >"$HOME/.config/openbox/rc.xml" <<'EOF'
<openbox_config xmlns="http://openbox.org/3.4/rc">
  <applications>
    <application name="ostadix-workstation">
      <focus>yes</focus>
      <layer>above</layer>
    </application>
  </applications>
</openbox_config>
EOF

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
  if ! notebook_page_ready \
      >/tmp/ostadix-notebook-page.out \
      2>/tmp/ostadix-notebook-page.err; then
    emit_serial 'OSTADIX HOSTED NOTEBOOK GUI: FAIL: embedded notebook page did not become ready'
    return 1
  fi
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
  xterm -name ostadix-workstation \
    -geometry 90x28+24+24 -title 'OSTADIX Workstation' \
    -bg '#1e1e2e' -fg '#cdd6f4' -cr '#f5e0dc' \
    -e /usr/local/bin/ostadix-desktop terminal \
    >/tmp/ostadix-xterm.log 2>&1 &
  terminal=$!
  terminal_window=
  for attempt in $(seq 1 60); do
    if ! kill -0 "$window_manager" 2>/dev/null \
        || ! kill -0 "$notebook" 2>/dev/null \
        || ! kill -0 "$terminal" 2>/dev/null; then
      break
    fi
    for window in $(xprop -root _NET_CLIENT_LIST 2>/dev/null \
        | sed 's/.*#//' | tr ',' ' '); do
      case "$window" in
        0x*)
          if xprop -id "$window" WM_CLASS 2>/dev/null \
              | grep -Fqi 'ostadix-workstation'; then
            terminal_window=$window
            break
          fi
          ;;
      esac
    done
    [ -z "$terminal_window" ] || break
    sleep 1
  done
  if [ -z "$terminal_window" ]; then
    emit_serial 'OSTADIX HOSTED DESKTOP: FAIL: Ostadix Xterm did not become a mapped window'
    return 1
  fi
  active_window=
  active_confirmations=0
  focus_status=1
  for attempt in $(seq 1 60); do
    if ! kill -0 "$window_manager" 2>/dev/null \
        || ! kill -0 "$notebook" 2>/dev/null \
        || ! kill -0 "$terminal" 2>/dev/null; then
      break
    fi
    if focus_x11_window "$terminal_window" \
        >>/tmp/ostadix-x11-focus.out \
        2>>/tmp/ostadix-x11-focus.err; then
      focus_status=0
    else
      focus_status=$?
    fi
    active_window=$(xprop -root _NET_ACTIVE_WINDOW 2>/dev/null \
      | sed 's/.*# *//')
    if [ "$terminal_window" = "$active_window" ]; then
      active_confirmations=$((active_confirmations + 1))
    else
      active_confirmations=0
    fi
    # Three consecutive confirmations provide a two-second stable interval in
    # which Xterm can paint the palette before the VGA capture follows READY.
    [ "$active_confirmations" -lt 3 ] || break
    sleep 1
  done
  if [ "$active_confirmations" -lt 3 ]; then
    emit_serial "OSTADIX HOSTED DESKTOP: FAIL: Ostadix Xterm did not remain active terminal=$terminal_window active=${active_window:-none} confirmations=$active_confirmations focus_status=$focus_status"
    return 1
  fi
  emit_serial 'OSTADIX HOSTED DESKTOP READY: PASS'

  while kill -0 "$window_manager" 2>/dev/null; do
    if ! kill -0 "$notebook" 2>/dev/null; then
      emit_serial 'OSTADIX HOSTED NOTEBOOK GUI: FAIL: notebook server exited'
      return 1
    fi
    if ! kill -0 "$terminal" 2>/dev/null; then
      xterm -name ostadix-workstation \
        -geometry 90x28+24+24 -title 'OSTADIX Workstation' \
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
