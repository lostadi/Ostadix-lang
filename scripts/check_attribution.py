#!/usr/bin/env python3
"""Reject Claude or Codex attribution in project metadata and new commits.

Tooling references are allowed. This guard is deliberately limited to fields
that Git, GitHub, Cargo, or CFF interpret as authorship or contribution credit.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import re
import subprocess
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
POLICY_PATH = "scripts/check_attribution.py"
LEGACY_POLICY_BASELINE = "5581877ee5a4120d53d3a1d4ab14bc59ef60c67f"
CODEX_IDENTITY = re.compile(r"(?i)(?<![a-z0-9])codex(?![a-z0-9])")
CLAUDE_PRODUCT_IDENTITY = re.compile(
    r"(?i)(?<![a-z0-9])claude[- ]+(?:code|haiku|opus|sonnet)(?![a-z0-9])"
)
CLAUDE_ACCOUNT_IDENTITY = re.compile(
    r"(?i)^\s*@?claude(?:\s*<[^>]+>)?\s*$"
)
CLAUDE_NOREPLY_IDENTITY = re.compile(r"(?i)\bnoreply@anthropic\.com\b")
VENDOR_TOOL_IDENTITY = re.compile(
    r"(?i)^\s*(?:anthropic|openai)(?:\s*<[^>]+>)?\s*$"
)
ATTRIBUTION_TRAILER = re.compile(
    r"(?im)^(?P<key>[a-z][a-z0-9-]*):[ \t]*(?P<value>[^\n]+)$"
)
ATTRIBUTION_TRAILER_KEYS = frozenset(
    {
        "author",
        "authored-by",
        "co-author",
        "co-authored-by",
        "contributed-by",
        "contributor",
        "contributors",
        "assisted-by",
        "helped-by",
        "pair-programmed-by",
        "reviewed-by",
        "signed-off-by",
        "tested-by",
    }
)
ZERO_OBJECT_ID = re.compile(r"^0+$")
AUTHOR_SURFACES = (
    "NOTICE",
    "ORIGIN.md",
    "crates/ostadix-api/NOTICE",
)
AUTHOR_LINE_PATTERNS = {
    "README.md": (re.compile(r"(?m)^\*By .+\*$"),),
    "SPEC.md": (re.compile(r"(?m)^Author:\s*.+$"),),
    "ostadix-lang-info.md": (re.compile(r"(?m)^- \*\*Author:\*\*\s*.+$"),),
}


@dataclass(frozen=True)
class CommitMetadata:
    oid: str
    author_name: str
    author_email: str
    committer_name: str
    committer_email: str
    message: str


@dataclass(frozen=True)
class Violation:
    location: str
    field: str
    value: str


def contains_forbidden_identity(value: str) -> bool:
    return any(
        pattern.search(value) is not None
        for pattern in (
            CODEX_IDENTITY,
            CLAUDE_PRODUCT_IDENTITY,
            CLAUDE_ACCOUNT_IDENTITY,
            CLAUDE_NOREPLY_IDENTITY,
            VENDOR_TOOL_IDENTITY,
        )
    )


def commit_violations(commit: CommitMetadata) -> list[Violation]:
    violations: list[Violation] = []
    identity_fields = (
        ("author name", commit.author_name),
        ("author email", commit.author_email),
        ("committer name", commit.committer_name),
        ("committer email", commit.committer_email),
    )
    for field, value in identity_fields:
        if contains_forbidden_identity(value):
            violations.append(Violation(commit.oid, field, value))

    for match in ATTRIBUTION_TRAILER.finditer(commit.message):
        key = match.group("key").lower()
        value = match.group("value").strip()
        if key in ATTRIBUTION_TRAILER_KEYS and contains_forbidden_identity(value):
            violations.append(Violation(commit.oid, f"{key} trailer", value))
    return violations


def cff_author_values(text: str) -> list[tuple[int, str]]:
    authors: list[tuple[int, str]] = []
    author_indent: int | None = None
    for line_number, line in enumerate(text.splitlines(), 1):
        stripped = line.lstrip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(stripped)
        if stripped.startswith("authors:"):
            author_indent = indent
            inline_value = stripped.partition(":")[2].strip()
            if inline_value:
                authors.append((line_number, inline_value))
            continue
        if author_indent is not None and indent <= author_indent:
            author_indent = None
        if author_indent is not None:
            authors.append((line_number, stripped))
    return authors


def cff_author_violations(path: Path) -> list[Violation]:
    violations: list[Violation] = []
    text = path.read_text(encoding="utf-8")
    for line_number, value in cff_author_values(text):
        if contains_forbidden_identity(value):
            violations.append(
                Violation(
                    str(path.relative_to(ROOT)),
                    f"authors line {line_number}",
                    value,
                )
            )
    return violations


def string_leaves(value: object) -> list[str]:
    if isinstance(value, str):
        return [value]
    if isinstance(value, list):
        return [item for nested in value for item in string_leaves(nested)]
    if isinstance(value, dict):
        return [item for nested in value.values() for item in string_leaves(nested)]
    return []


def cargo_author_values(manifest: dict[str, object]) -> list[tuple[str, str]]:
    authors: list[tuple[str, str]] = []
    package = manifest.get("package")
    if isinstance(package, dict):
        authors.extend(
            ("package.authors", value)
            for value in string_leaves(package.get("authors"))
        )
    workspace = manifest.get("workspace")
    if isinstance(workspace, dict):
        workspace_package = workspace.get("package")
        if isinstance(workspace_package, dict):
            authors.extend(
                ("workspace.package.authors", value)
                for value in string_leaves(workspace_package.get("authors"))
            )
    return authors


def forbidden_lines(path: Path, text: str, *, line_offset: int = 0) -> list[Violation]:
    violations: list[Violation] = []
    for line_number, line in enumerate(text.splitlines(), 1):
        if contains_forbidden_identity(line):
            violations.append(
                Violation(
                    str(path.relative_to(ROOT)),
                    f"line {line_number + line_offset}",
                    line.strip(),
                )
            )
    return violations


def codeowner_violations(path: Path) -> list[Violation]:
    violations: list[Violation] = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        for owner in stripped.split()[1:]:
            if contains_forbidden_identity(owner):
                violations.append(
                    Violation(
                        str(path.relative_to(ROOT)),
                        f"owner line {line_number}",
                        owner,
                    )
                )
    return violations


def markdown_section(path: Path, heading: str) -> tuple[str, int]:
    lines = path.read_text(encoding="utf-8").splitlines()
    try:
        heading_index = lines.index(heading)
    except ValueError:
        raise RuntimeError(f"missing authorship section {heading!r} in {path.relative_to(ROOT)}")
    start = heading_index + 1
    end = next(
        (index for index in range(start, len(lines)) if lines[index].startswith("## ")),
        len(lines),
    )
    return "\n".join(lines[start:end]), start


def tracked_files(pattern: str) -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files", "-z", "--", pattern],
        cwd=ROOT,
        check=True,
        capture_output=True,
    )
    return [ROOT / entry.decode() for entry in result.stdout.split(b"\0") if entry]


def project_metadata_violations() -> list[Violation]:
    violations: list[Violation] = []
    for path in tracked_files("*Cargo.toml"):
        manifest = tomllib.loads(path.read_text(encoding="utf-8"))
        for field, author in cargo_author_values(manifest):
            if contains_forbidden_identity(author):
                violations.append(
                    Violation(str(path.relative_to(ROOT)), field, author)
                )
    for path in tracked_files("*.cff"):
        violations.extend(cff_author_violations(path))
    for relative in AUTHOR_SURFACES:
        path = ROOT / relative
        violations.extend(forbidden_lines(path, path.read_text(encoding="utf-8")))
    violations.extend(codeowner_violations(ROOT / ".github/CODEOWNERS"))
    readme = ROOT / "README.md"
    authorship_section, line_offset = markdown_section(
        readme, "## Citation and authorship"
    )
    violations.extend(
        forbidden_lines(readme, authorship_section, line_offset=line_offset)
    )
    for relative, patterns in AUTHOR_LINE_PATTERNS.items():
        path = ROOT / relative
        text = path.read_text(encoding="utf-8")
        for pattern in patterns:
            for match in pattern.finditer(text):
                if contains_forbidden_identity(match.group(0)):
                    line_number = text.count("\n", 0, match.start()) + 1
                    violations.append(
                        Violation(relative, f"author line {line_number}", match.group(0))
                    )
    for name in ("AUTHORS", "AUTHORS.md", "CONTRIBUTORS", "CONTRIBUTORS.md"):
        for path in tracked_files(name):
            violations.extend(forbidden_lines(path, path.read_text(encoding="utf-8")))
    return violations


def policy_boundary(head: str) -> tuple[str | None, str]:
    """Return the policy-introduction commit and its exclusive scan base.

    Discovering this boundary from Git history keeps the guard valid when all
    descendant commit IDs change during a provenance-only history rewrite.
    Before the policy is committed, use the audited legacy tip so the working
    tree implementation remains testable.
    """
    result = subprocess.run(
        [
            "git",
            "log",
            "--diff-filter=A",
            "--reverse",
            "--format=%H",
            head,
            "--",
            POLICY_PATH,
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    introductions = result.stdout.split()
    if not introductions:
        return None, LEGACY_POLICY_BASELINE
    if len(introductions) != 1:
        raise RuntimeError(
            f"expected one attribution-policy introduction, found {len(introductions)}"
        )
    policy_commit = introductions[0]
    parent_result = subprocess.run(
        ["git", "show", "-s", "--format=%P", policy_commit],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    parents = parent_result.stdout.split()
    if len(parents) != 1:
        raise RuntimeError(
            "the attribution policy must be introduced by a single-parent commit"
        )
    return policy_commit, parents[0]


def revision_spec(
    base: str | None,
    head: str,
    *,
    fallback_base: str | None = None,
) -> str:
    if fallback_base is None:
        _, fallback_base = policy_boundary(head)
    effective_base = (
        base if base and not ZERO_OBJECT_ID.fullmatch(base) else fallback_base
    )
    return f"{effective_base}..{head}"


def select_scan_base(
    base: str | None,
    fallback_base: str,
    *,
    base_is_ancestor: bool,
) -> str:
    if not base or ZERO_OBJECT_ID.fullmatch(base) or not base_is_ancestor:
        return fallback_base
    return base


def is_ancestor(base: str, head: str) -> bool:
    result = subprocess.run(
        ["git", "merge-base", "--is-ancestor", base, head],
        cwd=ROOT,
        check=False,
        capture_output=True,
    )
    if result.returncode not in (0, 1):
        raise RuntimeError(f"could not compare attribution base {base} with {head}")
    return result.returncode == 0


def commit_exists(revision: str) -> bool:
    result = subprocess.run(
        ["git", "cat-file", "-e", f"{revision}^{{commit}}"],
        cwd=ROOT,
        check=False,
        capture_output=True,
    )
    if result.returncode not in (0, 1, 128):
        raise RuntimeError(f"could not resolve attribution revision {revision}")
    return result.returncode == 0


def commits_in(
    base: str | None,
    head: str,
    *,
    fallback_base: str | None = None,
) -> list[str]:
    if fallback_base is None:
        _, fallback_base = policy_boundary(head)
    base_is_ancestor = bool(
        base
        and not ZERO_OBJECT_ID.fullmatch(base)
        and commit_exists(base)
        and is_ancestor(base, head)
    )
    effective_base = select_scan_base(
        base,
        fallback_base,
        base_is_ancestor=base_is_ancestor,
    )
    result = subprocess.run(
        [
            "git",
            "rev-list",
            "--reverse",
            revision_spec(effective_base, head, fallback_base=fallback_base),
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.split()


def read_commit(oid: str) -> CommitMetadata:
    result = subprocess.run(
        [
            "git",
            "show",
            "-s",
            "--format=%an%x00%ae%x00%cn%x00%ce%x00%B",
            oid,
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
    )
    fields = result.stdout.decode("utf-8", errors="replace").split("\0", 4)
    if len(fields) != 5:
        raise RuntimeError(f"could not parse attribution fields for commit {oid}")
    return CommitMetadata(oid, *fields)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base",
        help="exclusive base commit; empty/all-zero uses the discovered policy boundary",
    )
    parser.add_argument("--head", default="HEAD", help="inclusive head commit")
    parser.add_argument(
        "--ref-type",
        choices=("branch", "tag"),
        default="branch",
        help="tags must contain the attribution-policy introduction commit",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    policy_commit, policy_base = policy_boundary(args.head)
    base = policy_base if args.ref_type == "tag" else args.base
    if args.ref_type == "tag" and (
        policy_commit is None or not is_ancestor(policy_commit, args.head)
    ):
        boundary = policy_commit or "<not present>"
        print(
            f"release tag {args.head} does not contain attribution-policy "
            f"introduction {boundary}",
            file=sys.stderr,
        )
        return 1
    violations = project_metadata_violations()
    for oid in commits_in(base, args.head, fallback_base=policy_base):
        violations.extend(commit_violations(read_commit(oid)))

    if violations:
        print("Claude/Codex attribution is forbidden:", file=sys.stderr)
        for violation in violations:
            print(
                f"  {violation.location}: {violation.field}: {violation.value}",
                file=sys.stderr,
            )
        return 1

    print("attribution policy: pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
