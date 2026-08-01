#!/usr/bin/env python3
"""Shared example-manifest validation and edition smoke runner.

The manifest is intentionally consumed by the Rust-hosted shell sweep, the
C17 Make/CMake smoke tests, and the Python reference tests.  Keeping selection
and expectation handling here prevents an unsupported typed block from being
mistaken for successful literal-text evaluation in one edition.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import signal
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "examples" / "manifest.json"
EDITIONS = {"rust", "c17", "python"}
CLASSIFICATIONS = {"unit", "integration", "manual"}
MODES = {"interpreter", "aot"}
HOST_AUTHORITIES = {
    "elevated",
    "fs_read",
    "fs_write",
    "network",
    "process",
    "virtualization",
}
FATAL_DIAGNOSTICS = (
    "shim error:",
    "process: bad CBOR frame from shim",
    "process: shim closed stdout",
    "Traceback (most recent call last):",
    "SyntaxError:",
)
PAYLOAD_FIELDS = {"schema_version", "examples"}
ENTRY_FIELDS = {
    "path",
    "editions",
    "classification",
    "requirements",
    "expected",
    "timeout_seconds",
}
PROGRAM_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]*\Z")
PYTHON_PACKAGE_RE = re.compile(
    r"[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*\Z"
)
OPT_IN_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*=[^=\x00\r\n]+\Z")

# This deliberately recognises an opener independently of an edition's parser.
# In particular, it sees ubuntu_vm^(...) even when the Python parser would treat
# that unknown tag as ordinary text.
_TYPED_OPEN_RE = re.compile(
    r"(?<![\\A-Za-z0-9_])"
    r"([A-Za-z_][A-Za-z0-9_]*)"
    r"(?:\[[0-9]+\])?"
    r"(?:\{[^{}\n]*\})?"
    r"\^\("
)


class ManifestError(AssertionError):
    """The checked-in example manifest is incomplete or malformed."""


class CommandTimeout(RuntimeError):
    """A manifest subprocess and its descendants exceeded its deadline."""

    def __init__(self, command: list[str], timeout: int, stdout: str, stderr: str):
        super().__init__(f"{command[0]} exceeded {timeout}s")
        self.command = command
        self.timeout = timeout
        self.stdout = stdout
        self.stderr = stderr


def typed_backends(source: str) -> set[str]:
    """Return every unescaped typed-expression tag visible in ``source``."""

    return {match.group(1) for match in _TYPED_OPEN_RE.finditer(source)}


def _require_string_list(owner: str, field: str, value: object) -> list[str]:
    if not isinstance(value, list) or any(not isinstance(item, str) or not item for item in value):
        raise ManifestError(f"{owner}.{field} must be a list of non-empty strings")
    if len(value) != len(set(value)):
        raise ManifestError(f"{owner}.{field} contains duplicates")
    return value


def load_manifest(root: Path | None = None) -> list[dict]:
    """Load and fully validate the repository's single example manifest."""

    root = Path(root) if root is not None else ROOT
    manifest_path = root / "examples" / "manifest.json"
    try:
        payload = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ManifestError(f"cannot load {manifest_path}: {exc}") from exc

    if not isinstance(payload, dict) or payload.get("schema_version") != 1:
        raise ManifestError("examples/manifest.json must have schema_version = 1")
    unknown_payload_fields = set(payload) - PAYLOAD_FIELDS
    if unknown_payload_fields:
        raise ManifestError(
            "examples/manifest.json has unknown fields "
            f"{sorted(unknown_payload_fields)}"
        )
    examples = payload.get("examples")
    if not isinstance(examples, list):
        raise ManifestError("examples/manifest.json examples must be a list")

    declared_paths: list[str] = []
    for index, entry in enumerate(examples):
        owner = f"examples[{index}]"
        if not isinstance(entry, dict):
            raise ManifestError(f"{owner} must be an object")
        unknown_entry_fields = set(entry) - ENTRY_FIELDS
        if unknown_entry_fields:
            raise ManifestError(
                f"{owner} has unknown fields {sorted(unknown_entry_fields)}"
            )

        for field in ("path", "editions", "classification", "requirements", "expected"):
            if field not in entry:
                raise ManifestError(f"{owner} is missing required field {field!r}")

        path = entry["path"]
        if not isinstance(path, str) or not path.endswith(".O"):
            raise ManifestError(f"{owner}.path must name a relative .O file")
        relative = Path(path)
        if relative.is_absolute() or ".." in relative.parts or relative.as_posix() != path:
            raise ManifestError(f"{owner}.path must be a normalized path below examples/")
        declared_paths.append(path)

        editions = _require_string_list(owner, "editions", entry["editions"])
        if not editions or not set(editions) <= EDITIONS:
            raise ManifestError(f"{owner}.editions must be a non-empty subset of {sorted(EDITIONS)}")

        classification = entry["classification"]
        if classification not in CLASSIFICATIONS:
            raise ManifestError(
                f"{owner}.classification must be one of {sorted(CLASSIFICATIONS)}"
            )

        requirements = entry["requirements"]
        if not isinstance(requirements, dict):
            raise ManifestError(f"{owner}.requirements must be an object")
        allowed_requirement_fields = {
            "backends",
            "programs",
            "guest_programs",
            "python_packages",
            "authorities",
            "opt_in",
            "files",
        }
        unknown_requirement_fields = set(requirements) - allowed_requirement_fields
        if unknown_requirement_fields:
            raise ManifestError(
                f"{owner}.requirements has unknown fields {sorted(unknown_requirement_fields)}"
            )
        for field in requirements:
            _require_string_list(owner + ".requirements", field, requirements[field])
        for field in ("backends", "programs", "authorities"):
            if field not in requirements:
                raise ManifestError(f"{owner}.requirements is missing {field!r}")
        unknown_authorities = set(requirements["authorities"]) - HOST_AUTHORITIES
        if unknown_authorities:
            raise ManifestError(
                f"{owner}.requirements.authorities contains unknown host requirements "
                f"{sorted(unknown_authorities)}"
            )
        for field in ("programs", "guest_programs"):
            for program in requirements.get(field, []):
                if not PROGRAM_RE.fullmatch(program):
                    raise ManifestError(
                        f"{owner}.requirements.{field} contains invalid executable "
                        f"name {program!r}"
                    )
        for package in requirements.get("python_packages", []):
            if not PYTHON_PACKAGE_RE.fullmatch(package):
                raise ManifestError(
                    f"{owner}.requirements.python_packages contains invalid import "
                    f"name {package!r}"
                )
        for assignment in requirements.get("opt_in", []):
            if not OPT_IN_RE.fullmatch(assignment):
                raise ManifestError(
                    f"{owner}.requirements.opt_in contains malformed assignment "
                    f"{assignment!r}"
                )
        for required_file in requirements.get("files", []):
            required_path = Path(required_file)
            if (
                required_path.is_absolute()
                or ".." in required_path.parts
                or required_path.as_posix() != required_file
            ):
                raise ManifestError(
                    f"{owner}.requirements.files must contain normalized paths below "
                    f"the repository root; got {required_file!r}"
                )
            if not (root / required_path).is_file():
                raise ManifestError(
                    f"{owner}.requirements.files does not exist: {required_file}"
                )

        source_path = root / "examples" / path
        if not source_path.is_file():
            raise ManifestError(f"{owner}.path does not exist: examples/{path}")
        discovered = typed_backends(source_path.read_text(encoding="utf-8"))
        declared_backends = set(requirements["backends"])
        if not discovered <= declared_backends:
            raise ManifestError(
                f"{owner}.requirements.backends omits typed tags "
                f"{sorted(discovered - declared_backends)}"
            )

        expected = entry["expected"]
        if not isinstance(expected, dict) or set(expected) != set(editions):
            raise ManifestError(
                f"{owner}.expected keys must exactly match editions {sorted(editions)}"
            )
        for edition, expectation in expected.items():
            expectation_owner = f"{owner}.expected.{edition}"
            if not isinstance(expectation, dict):
                raise ManifestError(f"{expectation_owner} must be an object")
            unknown_expectation_fields = set(expectation) - {"patterns", "result", "modes"}
            if unknown_expectation_fields:
                raise ManifestError(
                    f"{expectation_owner} has unknown fields "
                    f"{sorted(unknown_expectation_fields)}"
                )
            patterns = expectation.get("patterns")
            if patterns is not None:
                _require_string_list(expectation_owner, "patterns", patterns)
                if not patterns:
                    raise ManifestError(f"{expectation_owner}.patterns must not be empty")
            if patterns is None and "result" not in expectation:
                raise ManifestError(
                    f"{expectation_owner} needs an exact result or output patterns"
                )
            if "result" in expectation and edition != "python":
                raise ManifestError(
                    f"{expectation_owner}.result is only supported by the Python "
                    "semantic OValue runner; Rust and C17 require observable patterns"
                )
            if "result" in expectation:
                result = expectation["result"]
                if not isinstance(result, dict) or not isinstance(result.get("tag"), str):
                    raise ManifestError(
                        f"{expectation_owner}.result must be an OValue JSON object with a tag"
                    )
            modes = expectation.get("modes", ["interpreter"])
            _require_string_list(expectation_owner, "modes", modes)
            if not modes or not set(modes) <= MODES:
                raise ManifestError(f"{expectation_owner}.modes contains an unknown mode")
            if edition != "c17" and "aot" in modes:
                raise ManifestError(f"{expectation_owner}: only c17 examples use the AOT smoke")

        timeout = entry.get("timeout_seconds", 10)
        if not isinstance(timeout, int) or timeout <= 0:
            raise ManifestError(f"{owner}.timeout_seconds must be a positive integer")

    if declared_paths != sorted(declared_paths):
        raise ManifestError("examples must be sorted by path")
    if len(declared_paths) != len(set(declared_paths)):
        raise ManifestError("examples/manifest.json declares a path more than once")

    actual_paths = sorted(
        path.relative_to(root / "examples").as_posix()
        for path in (root / "examples").rglob("*.O")
    )
    if declared_paths != actual_paths:
        missing = sorted(set(actual_paths) - set(declared_paths))
        extra = sorted(set(declared_paths) - set(actual_paths))
        raise ManifestError(
            f"manifest coverage differs from examples tree; missing={missing}, extra={extra}"
        )

    return examples


def examples_for(
    edition: str,
    classifications: set[str] | None = None,
    root: Path | None = None,
) -> list[dict]:
    if edition not in EDITIONS:
        raise ManifestError(f"unknown edition {edition!r}")
    selected = [entry for entry in load_manifest(root) if edition in entry["editions"]]
    if classifications is not None:
        selected = [
            entry for entry in selected if entry["classification"] in classifications
        ]
    return selected


def unavailable_requirements(entry: dict, root: Path | None = None) -> list[str]:
    """Return unmet executable/package/file/opt-in requirements."""

    root = Path(root) if root is not None else ROOT
    requirements = entry["requirements"]
    missing = []
    for program in requirements.get("programs", []):
        if shutil.which(program) is None:
            missing.append(f"program:{program}")
    for package in requirements.get("python_packages", []):
        try:
            available = importlib.util.find_spec(package) is not None
        except (ImportError, ModuleNotFoundError, ValueError):
            available = False
        if not available:
            missing.append(f"python-package:{package}")
    for relative in requirements.get("files", []):
        if not (root / relative).exists():
            missing.append(f"file:{relative}")
    for assignment in requirements.get("opt_in", []):
        name, separator, value = assignment.partition("=")
        if not separator or os.environ.get(name) != value:
            missing.append(f"opt-in:{assignment}")
    return missing


def assert_python_expectation(entry: dict, value: object) -> None:
    """Assert a Python OValue against its manifest-declared semantic oracle."""

    from o_lang.ovalue import render_plain

    expectation = entry["expected"]["python"]
    actual_json = value.to_json()
    if "result" in expectation and actual_json != expectation["result"]:
        raise AssertionError(
            f"{entry['path']}: expected result {expectation['result']!r}, "
            f"got {actual_json!r}"
        )
    observable = render_plain(value) + "\n" + json.dumps(actual_json, sort_keys=True)
    for pattern in expectation.get("patterns", []):
        if pattern not in observable:
            raise AssertionError(
                f"{entry['path']}: expected pattern {pattern!r} in {observable!r}"
            )


def _check_patterns(entry: dict, edition: str, output: str) -> list[str]:
    return [
        pattern
        for pattern in entry["expected"][edition].get("patterns", [])
        if pattern not in output
    ]


def fatal_diagnostics(output: str) -> list[str]:
    """Return diagnostics that can never be positive example evidence."""

    return [diagnostic for diagnostic in FATAL_DIAGNOSTICS if diagnostic in output]


def _subprocess_env(cache_dir: Path) -> dict[str, str]:
    env = os.environ.copy()
    env.setdefault("MPLCONFIGDIR", str(cache_dir / "matplotlib"))
    env.setdefault("XDG_CACHE_HOME", str(cache_dir / "xdg-cache"))
    return env


def _run_command(
    command: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout: int,
) -> subprocess.CompletedProcess[str]:
    """Run one evidence command and kill its full process group on timeout."""

    process = subprocess.Popen(
        command,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=os.name == "posix",
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        if os.name == "posix":
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        else:  # pragma: no cover - CI and supported release hosts are POSIX
            process.kill()
        stdout, stderr = process.communicate()
        raise CommandTimeout(
            command,
            timeout,
            stdout or "",
            stderr or "",
        ) from error
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def run_interpreter_suite(
    edition: str,
    runner: Path,
    backends: Path,
    classifications: set[str],
) -> int:
    examples = examples_for(edition)
    passed = failed = skipped = 0
    with tempfile.TemporaryDirectory(prefix=f"o-{edition}-examples-") as temp:
        env = _subprocess_env(Path(temp))
        for entry in examples:
            path = entry["path"]
            if entry["classification"] not in classifications:
                print(f"[SKIP] {path}: classified {entry['classification']}")
                skipped += 1
                continue
            if "interpreter" not in entry["expected"][edition].get(
                "modes", ["interpreter"]
            ):
                print(f"[SKIP] {path}: interpreter mode not declared")
                skipped += 1
                continue
            missing = unavailable_requirements(entry)
            if missing:
                print(f"[SKIP] {path}: requires {', '.join(missing)}")
                skipped += 1
                continue

            command = [str(runner)]
            command.extend([str(ROOT / "examples" / path), str(backends)])
            try:
                result = _run_command(
                    command,
                    cwd=ROOT,
                    env=env,
                    timeout=entry.get("timeout_seconds", 10),
                )
            except CommandTimeout as exc:
                print(
                    f"[FAIL] {path}: exceeded {entry.get('timeout_seconds', 10)}s\n"
                    f"{exc.stdout}{exc.stderr}"
                )
                failed += 1
                continue

            output = result.stdout + result.stderr
            missing_patterns = _check_patterns(entry, edition, output)
            fatal = fatal_diagnostics(output)
            if result.returncode != 0 or missing_patterns or fatal:
                detail = []
                if result.returncode != 0:
                    detail.append(f"exit {result.returncode}")
                if missing_patterns:
                    detail.append(f"missing {missing_patterns!r}")
                if fatal:
                    detail.append(f"fatal diagnostics {fatal!r}")
                print(f"[FAIL] {path}: {', '.join(detail)}\n{output}")
                failed += 1
                continue

            authorities = ",".join(entry["requirements"]["authorities"]) or "none"
            print(f"[PASS] {path} (host-authority requirements: {authorities})")
            passed += 1

    print(f"\n{passed} passed, {failed} failed, {skipped} skipped")
    if passed == 0:
        print("[FAIL] no selected example executed; an all-skipped suite is not evidence")
        return 1
    return 1 if failed else 0


def run_c17_aot_suite(
    compiler: Path,
    backends: Path,
    classifications: set[str],
) -> int:
    examples = examples_for("c17")
    passed = failed = skipped = 0
    with tempfile.TemporaryDirectory(prefix="o-c17-aot-") as temp:
        temp_path = Path(temp)
        env = _subprocess_env(temp_path)
        for index, entry in enumerate(examples):
            path = entry["path"]
            modes = entry["expected"]["c17"].get("modes", ["interpreter"])
            if entry["classification"] not in classifications or "aot" not in modes:
                skipped += 1
                continue
            missing = unavailable_requirements(entry)
            if missing:
                print(f"[SKIP] {path}: requires {', '.join(missing)}")
                skipped += 1
                continue

            output_path = temp_path / f"example-{index}"
            try:
                compile_result = _run_command(
                    [
                        str(compiler),
                        str(ROOT / "examples" / path),
                        "-o",
                        str(output_path),
                        "--shim-dir",
                        str(backends),
                    ],
                    cwd=ROOT,
                    env=env,
                    timeout=entry.get("timeout_seconds", 10),
                )
            except CommandTimeout as exc:
                print(f"[FAIL] {path} AOT compile: timed out\n{exc.stdout}{exc.stderr}")
                failed += 1
                continue
            if compile_result.returncode != 0:
                print(
                    f"[FAIL] {path} AOT compile: exit {compile_result.returncode}\n"
                    f"{compile_result.stdout}{compile_result.stderr}"
                )
                failed += 1
                continue
            try:
                run_result = _run_command(
                    [str(output_path)],
                    cwd=ROOT,
                    env=env,
                    timeout=entry.get("timeout_seconds", 10),
                )
            except CommandTimeout as exc:
                print(f"[FAIL] {path} AOT: timed out\n{exc.stdout}{exc.stderr}")
                failed += 1
                continue
            output = run_result.stdout + run_result.stderr
            missing_patterns = _check_patterns(entry, "c17", output)
            fatal = fatal_diagnostics(output)
            if run_result.returncode != 0 or missing_patterns or fatal:
                print(
                    f"[FAIL] {path} AOT: exit {run_result.returncode}, "
                    f"missing {missing_patterns!r}, fatal diagnostics {fatal!r}\n{output}"
                )
                failed += 1
                continue
            print(f"[PASS] {path} (AOT)")
            passed += 1

    print(f"\n{passed} passed, {failed} failed, {skipped} skipped")
    if passed == 0:
        print("[FAIL] no selected AOT example executed; an all-skipped suite is not evidence")
        return 1
    return 1 if failed else 0


def _parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    subparsers.add_parser("validate", help="validate schema and complete .O coverage")

    run_parser = subparsers.add_parser("run", help="run one edition's interpreter examples")
    run_parser.add_argument("--edition", choices=sorted(EDITIONS), required=True)
    run_parser.add_argument("--runner", type=Path, required=True)
    run_parser.add_argument("--backends", type=Path, required=True)
    run_parser.add_argument(
        "--classification",
        action="append",
        choices=sorted(CLASSIFICATIONS),
        dest="classifications",
    )

    aot_parser = subparsers.add_parser("run-c17-aot", help="run manifest-selected C17 AOT smokes")
    aot_parser.add_argument("--compiler", type=Path, required=True)
    aot_parser.add_argument("--backends", type=Path, required=True)
    aot_parser.add_argument(
        "--classification",
        action="append",
        choices=sorted(CLASSIFICATIONS),
        dest="classifications",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(sys.argv[1:] if argv is None else argv)
    try:
        examples = load_manifest()
    except ManifestError as exc:
        print(f"manifest error: {exc}", file=sys.stderr)
        return 1

    if args.command == "validate":
        print(f"example manifest: PASS ({len(examples)} files)")
        return 0

    classifications = set(args.classifications or ["unit", "integration"])
    if args.command == "run":
        return run_interpreter_suite(
            args.edition,
            args.runner.resolve(),
            args.backends.resolve(),
            classifications,
        )
    if args.command == "run-c17-aot":
        return run_c17_aot_suite(
            args.compiler.resolve(),
            args.backends.resolve(),
            classifications,
        )
    raise AssertionError(f"unhandled command {args.command}")


if __name__ == "__main__":
    raise SystemExit(main())
