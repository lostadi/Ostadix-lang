#!/usr/bin/env python3
"""Fail the APK build if the explicit root PTY can inherit app-private state."""

from __future__ import annotations

import re
import sys
from pathlib import Path


class VerificationError(ValueError):
    """Raised when a security-sensitive source invariant is missing."""


def method_body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        raise VerificationError(f"method signature not found: {signature}")
    opening = source.find("{", start + len(signature))
    if opening < 0:
        raise VerificationError(f"method body not found: {signature}")
    depth = 0
    for index in range(opening, len(source)):
        character = source[index]
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return source[opening + 1 : index]
    raise VerificationError(f"unterminated method body: {signature}")


def verify(app_files: Path, main_activity: Path) -> None:
    app_source = app_files.read_text(encoding="utf-8")
    root_body = method_body(app_source, "public String[] rootEnvironment()")
    root_values = tuple(re.findall(r'"([^"\\]*)"', root_body))
    expected_values = (
        "HOME=/system",
        "USER=root",
        "LOGNAME=root",
        "ENV=",
        "PATH=/system/bin:/system/xbin",
        "SHELL=/system/bin/sh",
        "PWD=/system",
        "TERM=xterm-256color",
        "COLORTERM=truecolor",
        "LANG=C.UTF-8",
        "ANDROID_ROOT=/system",
        "ANDROID_DATA=/data",
    )
    if root_values != expected_values:
        raise VerificationError(
            "rootEnvironment must contain only the audited system-safe values; "
            f"found {root_values!r}"
        )

    forbidden = (
        "filesDir",
        "home.getAbsolutePath",
        "workspace",
        "OSTADIX_HOME",
        "O_BACKENDS_DIR",
        "LD_LIBRARY_PATH",
        "TERMINFO",
        "INPUTRC",
        "BASH_ENV",
        "LD_PRELOAD",
    )
    for token in forbidden:
        if token in root_body:
            raise VerificationError(f"rootEnvironment contains forbidden token: {token}")

    termux_body = method_body(app_source, "public String termuxLoginCommand()")
    for token in (
        'TERMUX_CANONICAL_HOME',
        'LD_PRELOAD=" + TERMUX_PREFIX',
        'exec " + TERMUX_PREFIX + "/bin/zsh -l',
    ):
        if token not in termux_body:
            raise VerificationError(
                f"Termux global-namespace login invariant missing: {token}"
            )

    main_source = main_activity.read_text(encoding="utf-8")
    shell_body = method_body(
        main_source, "private void startShell(boolean root, boolean termux)"
    )
    compact = " ".join(shell_body.split())
    required_routes = (
        'final boolean bundledBash = !root && files.isBashAvailable();',
        '? "/system/bin/su" : bundledBash',
        '? termux ? new String[] {"su", "-M", "-p", "-c", files.termuxLoginCommand()} '
        ': new String[] {"su", "-p"} : bundledBash',
        'final String workingDirectory = root ? "/system" '
        ': files.workspace().getAbsolutePath();',
        "final String[] sessionEnvironment = root ? termux ? files.termuxEnvironment() "
        ": files.rootEnvironment() : files.nonRootEnvironment(bundledBash);",
        "PtySession.start( executable, argv, workingDirectory, sessionEnvironment,",
    )
    for route in required_routes:
        if route not in compact:
            raise VerificationError(f"root PTY routing invariant missing: {route}")


def main() -> int:
    if len(sys.argv) != 3:
        print(
            "usage: verify_root_environment.py AppFiles.java MainActivity.java",
            file=sys.stderr,
        )
        return 2
    try:
        verify(Path(sys.argv[1]), Path(sys.argv[2]))
    except (OSError, VerificationError) as error:
        print(f"verify_root_environment: {error}", file=sys.stderr)
        return 1
    print("verify_root_environment: explicit root PTY environment is system-only")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
