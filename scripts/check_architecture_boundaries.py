#!/usr/bin/env python3
"""Reject the first frozen wrong-way Rust dependency edges.

This is intentionally a small lexical guard, not a full Rust dependency
analyzer. It protects boundaries that have already been made explicit while a
broader workspace extraction remains future work.
"""

from __future__ import annotations

import argparse
import dataclasses
from pathlib import Path
import re
import sys


@dataclasses.dataclass(frozen=True)
class Rule:
    paths: tuple[str, ...]
    forbidden: tuple[str, ...]
    reason: str


RULES = (
    Rule(
        ("src/parser.rs",),
        (r"\bcrate::ir\b", r"\bcrate::registry\b"),
        "syntax must depend only on its narrow dialect projection, not IR or the executable registry",
    ),
    Rule(
        ("src/syntax_dialect.rs",),
        (r"\bcrate::ir\b", r"\bcrate::registry\b", r"\bcrate::runtime_exec\b"),
        "the syntax-dialect contract must remain a capability-free model boundary",
    ),
    Rule(
        ("src/ir.rs",),
        (r"\bcrate::hgraph\b",),
        "IR must not depend on its HGraph projection",
    ),
    Rule(
        ("src/effects.rs",),
        (r"\bcrate::world\b",),
        "the effect vocabulary must depend on shared identities, not World",
    ),
    Rule(
        (
            "src/evidence/admit.rs",
            "src/evidence/analyze.rs",
            "src/evidence/fact.rs",
            "src/evidence/intent.rs",
            "src/evidence/profile.rs",
        ),
        (r"\bcrate::executor\b",),
        "evidence must bind a dispatch model rather than import its executor",
    ),
    Rule(
        ("src/dispatch_model.rs",),
        (r"\bcrate::evidence\b", r"\bcrate::executor\b", r"\bcrate::hgraph\b"),
        "the shared dispatch model must remain independent of HGraph and executor consumers",
    ),
    Rule(
        (
            "src/placement/mod.rs",
            "src/placement/projection.rs",
            "src/placement/protocol/mod.rs",
        ),
        (r"\bcrate::registry\b",),
        "placement protocol and projection must remain registry-independent",
    ),
)


def production_source(path: Path) -> str:
    """Return the production prefix, excluding this file's unit-test module."""

    source = path.read_text(encoding="utf-8")
    marker = "#[cfg(test)]"
    return source.split(marker, 1)[0]


def findings(root: Path) -> list[str]:
    failures: list[str] = []
    for rule in RULES:
        for relative in rule.paths:
            path = root / relative
            if not path.is_file():
                failures.append(f"{relative}: required architecture surface is missing")
                continue
            source = production_source(path)
            for pattern in rule.forbidden:
                match = re.search(pattern, source)
                if match is None:
                    continue
                line = source.count("\n", 0, match.start()) + 1
                failures.append(
                    f"{relative}:{line}: forbidden dependency `{match.group(0)}`; {rule.reason}"
                )
    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="repository root (default: parent of scripts/)",
    )
    args = parser.parse_args(argv)
    failures = findings(args.root.resolve())
    if failures:
        for failure in failures:
            print(f"architecture boundary: FAIL: {failure}", file=sys.stderr)
        return 1
    print("architecture dependency boundaries: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
