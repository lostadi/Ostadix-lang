# Ostadix Terminal for Android

Ostadix Terminal is a standalone ARM64 Android app with its own terminal UI,
PTY-backed Android shell, embedded Ostadix evaluator, and device-aware CPU
controls. It supports Android 9 (API 28) and newer and does not depend on the
Termux app or Termux private storage.

## Included in the first standalone build

- Native PTY sessions backed by a bundled GNU Bash 5.3.15 runtime. A damaged
  Bash extraction falls back explicitly to `/system/bin/sh`.
- Standalone Bash is the default startup surface; O Console remains one tap away.
- An in-process JNI build of `ostadix-api` for `.O` evaluation.
- A real `O` command available in the app shell without Termux. Try
  `command -v O`, `O --help`, or `O --eval '2'`.
- A real `bash` command available without Termux. Try `command -v bash`,
  `bash --version`, or `bash -lc 'printf "hello\n"'`.
- O Console and bundled offline example actions.
- ANSI/VT rendering, 256/true-color support, scrollback, software keyboard,
  hardware-key input, and an extra-key row.
- Long-press text selection with draggable endpoints, theme-visible
  highlighting, Copy, and Select all actions.
- Obsidian, Solarized, Graphite, and Light palettes; adjustable text size,
  cursor shape, scrollback, haptics, wake behavior, startup mode, and O-session
  CPU policy.
- Balanced and CPU-7-priority modes. Balanced is the safe default because it
  leaves the full Android cpuset available to Ostadix's graph workers. Prime
  CPU7 remains an explicit setting for known-serial work: the O Console pins
  only its evaluator worker, while the non-root app shell pins its PTY child so
  commands inherit CPU 7. Android's UI thread and explicit root shells remain
  unpinned. Changing the O Console policy restarts its runtime before the next
  evaluation, clearing persistent backend state so actors and graph workers
  cannot retain the previous affinity. An active shell keeps its launch policy
  until a new shell session is opened. Upgrading from the former CPU7-default
  settings schema performs a one-time safety migration to Balanced; users who
  intentionally want Prime can select it again afterward.
- A root shell action that is always explicit. The app never probes for root
  during startup, and a new KernelSU grant is required for this package.
- An explicit **TERMUX** action that uses KernelSU to enter the installed
  Termux prefix and home, then starts the native Termux zsh. It exposes all
  installed Termux packages plus user-local, Cargo, and Ostadix commands,
  including codex, without copying or repackaging them.

## Deliberate boundary

Android gives every package a separate UID and private filesystem. Arbitrary
Termux packages cannot be reused by the ordinary app-sandbox shell because they
target Termux's hard-coded private prefix. That shell deliberately carries only
a pinned Bash dependency closure;
its executable, dependency names, SONAMEs, and RUNPATHs are normalized and
validated for Android APK extraction. Hosted O backends still need their
language runtimes; a future bundled userland and package repository remains a
separate distribution project.

The `O` and Bash executables and all Bash shared libraries remain in Android's
package-manager-owned native library directory. The writable private home
contains only `bin/O` and `bin/bash` symbolic links to read-only APK code. This
satisfies Android's W^X rule for apps targeting Android 10 and newer; the app
never copies or marks downloaded native code executable.

The non-root Bash child receives a sanitized library path containing only the
app's `nativeLibraryDir`, plus private `HOME`, `TMPDIR`, `TERMINFO`, and
`INPUTRC` paths. The explicit `/system/bin/su` session instead starts in the
root-owned, read-only `/system` directory with `HOME=/system`, `PWD=/system`, a
system-only `PATH`, `USER=root`, `LOGNAME=root`, an explicitly empty `ENV`, and
no preset temporary directory. It receives no app-private paths, Ostadix
backend variables, Bash startup hooks, or native loader overrides. Empty `ENV`
also prevents KernelSU from injecting its shell startup hook. The app invokes
KernelSU as `su -p`, preserving this exact audited allowlist instead of letting
`su` replace `HOME=/system` with the root passwd entry. Root remains opt-in and
never automatically sources an app-writable Bash startup script.

Typing `su` manually inside the non-root Bash session is a separate, explicitly
unsafe path: KernelSU preserves that shell's environment, and the command does
not use the ROOT action's large-or-multiline paste confirmation. Use the app's
ROOT action when a hardened privileged session is required.

The TERMUX action is the deliberate superset escape hatch. Android package
isolation makes KernelSU necessary to traverse Termux's private data tree.
It requests KernelSU's global mount namespace so Android's per-app filtered
view cannot hide Termux, then enters the canonical package-data tree at
/data/user/0/com.termux. After elevation, it restores the native prefix,
command search path, certificates, Ostadix paths, and termux-exec preload, and
then executes the installed zsh login shell. It has root authority and is
intentionally separate from the hardened ROOT action.

The app does not include a root daemon, change SELinux, write thermal or
governor nodes, install boot scripts, or attempt root hiding. Root access is
confined to a manually opened PTY.

The APK carries the repository's LGPL-2.1 license and notice plus the complete
Bash, Readline, libiconv, ncurses, and libandroid-support license texts under
`assets/licenses/`. `Bundled-Bash-SOURCES.txt` records exact versions, input
hashes, pinned Termux recipes and patches, upstream source hashes, and the
Ostadix ELF transformations required to reproduce the bundled runtime.

## Build on this Android/Termux machine

The local build uses the installed Java 21, `aapt2`, `d8`, `apksigner`, Android
API 34 platform jar, Rust, Clang, and the pinned local Termux Bash 5.3.15
runtime inputs—Gradle is not required. Input hashes are checked before use so a
package upgrade cannot silently change the APK's licensed native payload.
The default `portable` profile uses Rust's generic AArch64 target and
`-march=armv8-a` for the PTY bridge, preserving the Android 9+ compatibility
claim. Clang/LLD, fat LTO, and explicit 16 KiB ELF page alignment remain on in
both profiles. If installed, `sccache` is used for Rust and `ccache` for the
compiled PTY object; cache selection is printed at the start of the build.

```sh
cd "$HOME/Ostadix-lang/apps/android-terminal"
./build.sh
```

For a device-local build tuned to this machine (and not intended for arbitrary
ARM64 Android devices), select the explicit native profile:

```sh
OSTADIX_ANDROID_CPU_PROFILE=native ./build.sh
```

The signed debug APK is written to:

```text
build/outputs/apk/debug/OstadixTerminal-debug.apk
```

For third-party installer front ends that do not handle v3 signatures
correctly, the build also writes a v2-signed compatibility APK:

```text
build/outputs/apk/debug/OstadixTerminal-universal.apk
```

The native profile uses distinct
`OstadixTerminal-device-native-{debug,universal}.apk` filenames so it cannot be
mistaken for the portable artifact.

Install it from Android's package installer or with `adb install -r` from a
connected development host. A release build should use a private release key
instead of the Android debug key.

The build-time Bash, CLI, PTY, and JNI smokes run under the Termux build UID.
They are pre-install ABI checks, not proof of the standalone package's separate
app sandbox. Before treating package-UID independence as device evidence,
install the APK and exercise Bash, `O --eval 2`, and the O Console from the app.
