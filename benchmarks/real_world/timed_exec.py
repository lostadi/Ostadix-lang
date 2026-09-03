#!/usr/bin/env python3
"""Run one command without a shell and report its complete wall time as JSON."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


SCHEMA = "ostadix.timed-exec/v1"
MAX_ENV_OVERRIDES = 32
ENV_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Run a command directly (without a shell), capture its output, and emit "
            "compact JSON wall-time metadata on this process's stdout."
        ),
        epilog=(
            "Example: timed_exec.py --stdout run.json --stderr run.err "
            "--env O_ASSET_OUT=/tmp/assets -- command --json"
        ),
    )
    parser.add_argument(
        "--stdout",
        type=Path,
        required=True,
        metavar="PATH",
        help="file that receives the child process's stdout",
    )
    parser.add_argument(
        "--stderr",
        type=Path,
        required=True,
        metavar="PATH",
        help="file that receives the child process's stderr",
    )
    parser.add_argument(
        "--env",
        action="append",
        default=[],
        metavar="NAME=VALUE",
        help=f"child-only environment override; repeat at most {MAX_ENV_OVERRIDES} times",
    )
    parser.add_argument(
        "--unset-env",
        action="append",
        default=[],
        metavar="NAME",
        help=f"remove a child environment variable; repeat at most {MAX_ENV_OVERRIDES} times",
    )
    parser.add_argument(
        "command",
        nargs=argparse.REMAINDER,
        help="command and arguments; place them after --",
    )
    return parser


def parse_env_overrides(parser: argparse.ArgumentParser, entries: list[str]) -> dict[str, str]:
    if len(entries) > MAX_ENV_OVERRIDES:
        parser.error(f"at most {MAX_ENV_OVERRIDES} --env overrides are allowed")

    overrides: dict[str, str] = {}
    for entry in entries:
        name, separator, value = entry.partition("=")
        if not separator or not ENV_NAME.fullmatch(name):
            parser.error(f"invalid --env value {entry!r}; expected NAME=VALUE")
        if name in overrides:
            parser.error(f"duplicate --env override for {name!r}")
        if "\x00" in value:
            parser.error(f"environment value for {name!r} contains a NUL byte")
        overrides[name] = value
    return overrides


def parse_env_unsets(parser: argparse.ArgumentParser, entries: list[str]) -> set[str]:
    if len(entries) > MAX_ENV_OVERRIDES:
        parser.error(f"at most {MAX_ENV_OVERRIDES} --unset-env options are allowed")
    names: set[str] = set()
    for name in entries:
        if not ENV_NAME.fullmatch(name):
            parser.error(f"invalid --unset-env value {name!r}; expected NAME")
        if name in names:
            parser.error(f"duplicate --unset-env option for {name!r}")
        names.add(name)
    return names


def normalized_exit_code(returncode: int) -> tuple[int, int | None]:
    if returncode >= 0:
        return returncode, None
    signal_number = -returncode
    return min(255, 128 + signal_number), signal_number


def emit_metadata(
    *,
    argv: list[str],
    elapsed_ns: int,
    env_names: list[str],
    env_unset_names: list[str],
    returncode: int,
    stdout_path: Path,
    stderr_path: Path,
) -> int:
    exit_code, signal_number = normalized_exit_code(returncode)
    payload = {
        "schema": SCHEMA,
        "argv": argv,
        "env_override_names": env_names,
        "env_unset_names": env_unset_names,
        "wall_time_ns": elapsed_ns,
        "wall_time_ms": elapsed_ns / 1_000_000,
        "returncode": returncode,
        "exit_code": exit_code,
        "signal": signal_number,
        "stdout_path": str(stdout_path),
        "stderr_path": str(stderr_path),
    }
    json.dump(payload, sys.stdout, sort_keys=True, separators=(",", ":"))
    sys.stdout.write("\n")
    sys.stdout.flush()
    return exit_code


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    command = list(args.command)
    if command[:1] == ["--"]:
        command.pop(0)
    if not command:
        parser.error("a command is required after --")

    overrides = parse_env_overrides(parser, args.env)
    unsets = parse_env_unsets(parser, args.unset_env)
    overlap = sorted(set(overrides) & unsets)
    if overlap:
        parser.error(f"environment variables cannot be both set and removed: {overlap!r}")
    if args.stdout.resolve() == args.stderr.resolve():
        parser.error("--stdout and --stderr must name different files")

    environment = os.environ.copy()
    for name in unsets:
        environment.pop(name, None)
    environment.update(overrides)

    returncode: int
    with args.stdout.open("wb") as child_stdout, args.stderr.open("wb") as child_stderr:
        started_ns = time.perf_counter_ns()
        try:
            completed = subprocess.run(
                command,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=child_stdout,
                stderr=child_stderr,
                check=False,
                shell=False,
            )
            returncode = completed.returncode
        except FileNotFoundError as error:
            returncode = 127
            child_stderr.write(f"timed_exec: command not found: {error}\n".encode())
        except PermissionError as error:
            returncode = 126
            child_stderr.write(f"timed_exec: cannot execute command: {error}\n".encode())
        elapsed_ns = time.perf_counter_ns() - started_ns
        child_stdout.flush()
        child_stderr.flush()

    return emit_metadata(
        argv=command,
        elapsed_ns=elapsed_ns,
        env_names=sorted(overrides),
        env_unset_names=sorted(unsets),
        returncode=returncode,
        stdout_path=args.stdout,
        stderr_path=args.stderr,
    )


if __name__ == "__main__":
    raise SystemExit(main())
