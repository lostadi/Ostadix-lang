#!/usr/bin/env python3
"""Enforce the production library's declared root dependency DAG.

This is intentionally a token-aware lexical guard, not a full Rust dependency
analyzer. It enumerates every production library source, resolves explicit
``crate`` and ``super`` paths under the repository's declared module geometry,
and checks them against ``ci/architecture-roots.toml``. Comments, literals, and
top-level items that are definitely disabled when ``test = false`` cannot hide
or manufacture dependencies. Existing semantic boundary rules remain as
narrower, human-explained constraints within that repository-wide contract.
"""

from __future__ import annotations

import argparse
import dataclasses
import enum
import json
from pathlib import Path
import posixpath
import re
import sys
import tomllib


@dataclasses.dataclass(frozen=True)
class Rule:
    paths: tuple[str, ...]
    forbidden_modules: tuple[str, ...]
    reason: str
    allowed_modules: tuple[str, ...] | None = None


PLACEMENT_PROTOCOL_PATHS = (
    "src/placement/protocol/candidate.rs",
    "src/placement/protocol/catalog.rs",
    "src/placement/protocol/digest.rs",
    "src/placement/protocol/error.rs",
    "src/placement/protocol/mod.rs",
    "src/placement/protocol/records.rs",
    "src/placement/protocol/requirement.rs",
    "src/placement/protocol/state.rs",
    "src/placement/protocol/target.rs",
    "src/placement/protocol/warrant.rs",
)

EXECUTOR_RUNTIME_PATHS = (
    "src/executor/actor.rs",
    "src/executor/cancellation.rs",
    "src/executor/coordinator.rs",
    "src/executor/driver.rs",
    "src/executor/effects.rs",
    "src/executor/mod.rs",
    "src/executor/parallel.rs",
    "src/executor/pool.rs",
    "src/executor/task.rs",
    "src/executor/trace.rs",
)

DEFAULT_MANIFEST_RELATIVE = Path("ci/architecture-roots.toml")


RULES = (
    Rule(
        ("src/parser.rs",),
        ("execution_contract", "ir", "registry"),
        "syntax must depend only on its narrow dialect projection, not the execution contract, IR, or the executable registry",
    ),
    Rule(
        ("src/syntax_dialect.rs",),
        ("execution_contract", "ir", "registry", "runtime_exec"),
        "the syntax-dialect contract must remain a capability-free model boundary",
    ),
    Rule(
        ("src/ir.rs",),
        (
            "eval_core",
            "execution_contract",
            "hgraph",
            "placement",
            "placement_protocol",
            "registry",
        ),
        "IR must not depend on its execution-contract projection, its HGraph or placement projections, and must depend directly on the canonical backend catalog rather than registry compatibility projections",
    ),
    Rule(
        ("src/backend_catalog.rs",),
        (
            "backend",
            "eval",
            "eval_core",
            "evidence",
            "execution_contract",
            "executor",
            "hgraph",
            "ir",
            "placement",
            "registry",
            "runtime_exec",
            "scheduler",
            "world",
        ),
        "the canonical backend catalog must remain below backend realization, IR, analysis, execution, scheduling, registry storage, public placement projections, and World",
    ),
    Rule(
        ("src/backend_state.rs",),
        ("backend", "process"),
        "the canonical backend-state protocol may depend directly only on environment, value, and wire, never backend or process realizations",
        allowed_modules=("environment", "value", "wire"),
    ),
    Rule(
        ("src/execution_contract.rs",),
        (
            "api",
            "backend",
            "backend_morphism",
            "backend_state",
            "canonical_cbor",
            "capability",
            "dispatch_model",
            "environment",
            "eval",
            "eval_core",
            "evidence",
            "executor",
            "hgraph",
            "hosted_remote",
            "information",
            "information_provenance",
            "kernel_world",
            "live_system",
            "nix_ops",
            "nixos_ops",
            "ocore",
            "parser",
            "placement",
            "placement_protocol",
            "process",
            "project",
            "registry",
            "resource_identity",
            "runtime_exec",
            "scheduler",
            "shims",
            "syntax_dialect",
            "version",
            "wire",
            "world",
        ),
        "the canonical execution contract may depend directly only on backend_catalog, effects, ir, and value",
        allowed_modules=("backend_catalog", "effects", "ir", "value"),
    ),
    Rule(
        ("src/effects.rs",),
        ("execution_contract", "world"),
        "the effect vocabulary must remain below the execution contract and depend on shared identities, not World",
    ),
    Rule(
        ("src/value.rs",),
        ("backend_state", "eval_core", "execution_contract"),
        "the runtime value vocabulary must remain below backend state, the execution contract, and graph-evaluation core",
    ),
    Rule(
        ("src/environment.rs", "src/wire.rs"),
        ("backend_state",),
        "backend-state dependencies must remain below the canonical backend-state protocol",
    ),
    Rule(
        (
            "src/evidence/admit.rs",
            "src/evidence/analyze.rs",
            "src/evidence/fact.rs",
            "src/evidence/intent.rs",
            "src/evidence/mod.rs",
            "src/evidence/profile.rs",
        ),
        ("eval", "eval_core", "executor"),
        "evidence must bind the canonical execution contract and dispatch model rather than import evaluator or executor realizations",
    ),
    Rule(
        ("src/world/grounding.rs",),
        ("eval",),
        "World grounding must consume the canonical execution contract rather than evaluator realization internals",
    ),
    Rule(
        ("src/dispatch_model.rs",),
        ("evidence", "executor", "hgraph"),
        "the shared dispatch model must remain independent of HGraph and executor consumers",
    ),
    Rule(
        ("src/capability.rs",),
        ("eval_core",),
        "capability vocabulary must remain below the graph-evaluation core that consumes its sandbox policy",
    ),
    Rule(
        ("src/eval_core.rs",),
        (
            "backend",
            "eval",
            "executor",
            "hgraph",
            "hosted_remote",
            "process",
            "project",
            "registry",
            "runtime_exec",
            "scheduler",
            "world",
        ),
        "the graph-evaluation contract must remain independent of evaluator and executor realizations",
        ("backend_catalog", "backend_morphism", "capability", "evidence", "execution_contract", "ir", "value"),
    ),
    Rule(
        EXECUTOR_RUNTIME_PATHS,
        ("eval", "registry"),
        "the graph executor must consume eval_core and canonical catalogs rather than evaluator or registry realizations",
    ),
    Rule(
        ("src/process.rs",),
        ("backend",),
        "process management must consume the canonical backend-state protocol without importing the backend realization facade",
    ),
    Rule(
        PLACEMENT_PROTOCOL_PATHS,
        (
            "backend",
            "dispatch_model",
            "effects",
            "eval",
            "evidence",
            "execution_contract",
            "executor",
            "hgraph",
            "hosted_remote",
            "ir",
            "placement",
            "project",
            "registry",
            "runtime_exec",
            "value",
            "world",
        ),
        "the canonical placement protocol may depend on resource_identity but not backend, analysis, runtime, facade, registry, project, value, or World layers",
    ),
    Rule(
        (
            "src/placement/mod.rs",
            "src/placement/projection.rs",
        ),
        ("registry",),
        "the public placement projection must remain registry-independent",
    ),
    Rule(
        (
            "src/backend_catalog.rs",
            "src/registry/bundle/mod.rs",
            "src/registry/model.rs",
            "src/registry/placement_compat.rs",
            "src/registry/store.rs",
        ),
        ("placement",),
        "registry must bind the canonical placement_protocol module, not its public compatibility facade",
    ),
    Rule(
        (
            "src/placement/protocol/records.rs",
            "src/placement/protocol/state.rs",
            "src/placement/protocol/target.rs",
            "src/placement/protocol/warrant.rs",
            "src/backend_catalog.rs",
            "src/eval.rs",
            "src/runtime_exec.rs",
        ),
        ("world",),
        "shared artifact identity consumers must depend on resource_identity, not the World compatibility facade",
    ),
)


@dataclasses.dataclass(frozen=True)
class Token:
    text: str
    start: int
    end: int


IDENTIFIER_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
TOKEN_RE = re.compile(r"r#[A-Za-z_][A-Za-z0-9_]*|[A-Za-z_][A-Za-z0-9_]*|::|[^\s]")
OPEN_TO_CLOSE = {"(": ")", "[": "]", "{": "}"}
CLOSE_TO_OPEN = {closing: opening for opening, closing in OPEN_TO_CLOSE.items()}
STRING_LITERAL_SENTINEL = "\0"
BYTE_STRING_LITERAL_SENTINEL = "\x01"
C_STRING_LITERAL_SENTINEL = "\x02"
CHARACTER_LITERAL_SENTINEL = "\x03"
BYTE_CHARACTER_LITERAL_SENTINEL = "\x04"
STRING_LITERAL_TOKEN = "<string-literal>"
LITERAL_TOKENS = {
    STRING_LITERAL_SENTINEL: STRING_LITERAL_TOKEN,
    BYTE_STRING_LITERAL_SENTINEL: "<byte-string-literal>",
    C_STRING_LITERAL_SENTINEL: "<c-string-literal>",
    CHARACTER_LITERAL_SENTINEL: "<character-literal>",
    BYTE_CHARACTER_LITERAL_SENTINEL: "<byte-character-literal>",
}
RESERVED_SENTINELS = frozenset(LITERAL_TOKENS)


class CfgValue(enum.Enum):
    FALSE = 0
    UNKNOWN = 1
    TRUE = 2


def _mask_span(buffer: list[str], start: int, end: int) -> None:
    """Blank a source span without changing source offsets or line numbers."""

    for index in range(start, end):
        if buffer[index] not in "\r\n":
            buffer[index] = " "


def _mask_literal(buffer: list[str], start: int, end: int, sentinel: str) -> None:
    """Retain one opaque literal token at the literal's original offset."""

    _mask_span(buffer, start, end)
    buffer[start] = sentinel


def _raw_string_end(source: str, start: int) -> tuple[int, str] | None:
    """Return the end and opaque kind of a Rust raw string, if any."""

    if start > 0 and (source[start - 1].isalnum() or source[start - 1] == "_"):
        return None
    marker = start
    if source.startswith("br", start):
        sentinel = BYTE_STRING_LITERAL_SENTINEL
        marker += 2
    elif source.startswith("cr", start):
        sentinel = C_STRING_LITERAL_SENTINEL
        marker += 2
    elif source.startswith("r", start):
        sentinel = STRING_LITERAL_SENTINEL
        marker += 1
    else:
        return None
    hashes = 0
    while marker + hashes < len(source) and source[marker + hashes] == "#":
        hashes += 1
    quote = marker + hashes
    if quote >= len(source) or source[quote] != '"':
        return None
    delimiter = '"' + ("#" * hashes)
    closing = source.find(delimiter, quote + 1)
    if closing < 0:
        raise ValueError(f"unclosed raw string at source offset {start}")
    return closing + len(delimiter), sentinel


def _quoted_string_end(source: str, quote: int) -> int:
    index = quote + 1
    while index < len(source):
        if source[index] == "\\":
            index += 2
            continue
        if source[index] == '"':
            return index + 1
        index += 1
    raise ValueError(f"unclosed string at source offset {quote}")


def _character_end(source: str, quote: int) -> int | None:
    """Recognize a Rust character literal without mistaking lifetimes for one."""

    index = quote + 1
    if index >= len(source) or source[index] in "\r\n'":
        return None
    if source[index] != "\\":
        index += 1
    elif index + 1 >= len(source):
        return None
    elif source[index + 1] == "u" and index + 2 < len(source) and source[index + 2] == "{":
        closing = source.find("}", index + 3)
        if closing < 0:
            return None
        index = closing + 1
    elif source[index + 1] == "x":
        index += 4
    else:
        index += 2
    return index + 1 if index < len(source) and source[index] == "'" else None


def _mask_comments_and_literals(source: str) -> str:
    """Mask Rust comments and literals while preserving source coordinates."""

    buffer = list(source)
    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = len(source) if end < 0 else end
            _mask_span(buffer, index, end)
            index = end
            continue
        if source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            if depth:
                raise ValueError(f"unclosed block comment at source offset {index}")
            _mask_span(buffer, index, end)
            index = end
            continue
        raw_literal = _raw_string_end(source, index)
        if raw_literal is not None:
            raw_end, sentinel = raw_literal
            _mask_literal(buffer, index, raw_end, sentinel)
            index = raw_end
            continue
        if source.startswith("b\"", index):
            prefix_width = 1
            sentinel = BYTE_STRING_LITERAL_SENTINEL
        elif source.startswith("c\"", index):
            prefix_width = 1
            sentinel = C_STRING_LITERAL_SENTINEL
        else:
            prefix_width = 0
            sentinel = STRING_LITERAL_SENTINEL
        if source[index + prefix_width : index + prefix_width + 1] == '"':
            end = _quoted_string_end(source, index + prefix_width)
            _mask_literal(buffer, index, end, sentinel)
            index = end
            continue
        character_quote = None
        if source[index : index + 2] == "b'":
            character_quote = index + 1
            sentinel = BYTE_CHARACTER_LITERAL_SENTINEL
        elif source[index] == "'":
            character_quote = index
            sentinel = CHARACTER_LITERAL_SENTINEL
        if character_quote is not None:
            end = _character_end(source, character_quote)
            if end is not None:
                _mask_literal(buffer, index, end, sentinel)
                index = end
                continue
        index += 1
    return "".join(buffer)


def _tokens(source: str) -> list[Token]:
    tokens: list[Token] = []
    for match in TOKEN_RE.finditer(source):
        text = match.group(0)
        if text in LITERAL_TOKENS:
            text = LITERAL_TOKENS[text]
        elif text.startswith("r#") and IDENTIFIER_RE.fullmatch(text[2:]):
            text = text[2:]
        tokens.append(Token(text, match.start(), match.end()))
    return tokens


def _delimiter_matches(tokens: list[Token]) -> dict[int, int]:
    stack: list[tuple[str, int]] = []
    matches: dict[int, int] = {}
    for index, token in enumerate(tokens):
        if token.text in OPEN_TO_CLOSE:
            stack.append((token.text, index))
        elif token.text in CLOSE_TO_OPEN:
            if not stack or stack[-1][0] != CLOSE_TO_OPEN[token.text]:
                raise ValueError(f"unbalanced delimiter `{token.text}` at source offset {token.start}")
            _, opening = stack.pop()
            matches[opening] = index
            matches[index] = opening
    if stack:
        opening, index = stack[-1]
        raise ValueError(f"unclosed delimiter `{opening}` at source offset {tokens[index].start}")
    return matches


def _cfg_all(values: list[CfgValue]) -> CfgValue:
    if CfgValue.FALSE in values:
        return CfgValue.FALSE
    return CfgValue.TRUE if all(value is CfgValue.TRUE for value in values) else CfgValue.UNKNOWN


def _cfg_any(values: list[CfgValue]) -> CfgValue:
    if CfgValue.TRUE in values:
        return CfgValue.TRUE
    return CfgValue.FALSE if all(value is CfgValue.FALSE for value in values) else CfgValue.UNKNOWN


def _parse_cfg_predicate(
    tokens: list[Token], start: int, end: int, matches: dict[int, int]
) -> tuple[CfgValue, int]:
    if start >= end or not IDENTIFIER_RE.fullmatch(tokens[start].text):
        offset = tokens[start].start if start < len(tokens) else -1
        raise ValueError(f"malformed cfg predicate at source offset {offset}")

    name = tokens[start].text
    if start + 1 < end and tokens[start + 1].text == "(":
        closing = matches[start + 1]
        if closing >= end:
            raise ValueError(f"cfg predicate `{name}` crosses its attribute boundary")
        if name not in {"all", "any", "not"}:
            raise ValueError(f"unsupported cfg predicate function `{name}`")

        values: list[CfgValue] = []
        cursor = start + 2
        while cursor < closing:
            value, cursor = _parse_cfg_predicate(tokens, cursor, closing, matches)
            values.append(value)
            if cursor == closing:
                break
            if tokens[cursor].text != ",":
                raise ValueError(
                    f"cfg operator `{name}` expected a comma at source offset {tokens[cursor].start}"
                )
            cursor += 1
            if cursor == closing:
                break

        if name == "not":
            if len(values) != 1:
                raise ValueError("cfg operator `not` requires exactly one predicate")
            value = values[0]
            if value is CfgValue.TRUE:
                return CfgValue.FALSE, closing + 1
            if value is CfgValue.FALSE:
                return CfgValue.TRUE, closing + 1
            return CfgValue.UNKNOWN, closing + 1
        if name == "all":
            return _cfg_all(values), closing + 1
        return _cfg_any(values), closing + 1

    cursor = start + 1
    if cursor < end and tokens[cursor].text == "=":
        if cursor + 1 >= end or tokens[cursor + 1].text != STRING_LITERAL_TOKEN:
            raise ValueError(
                f"cfg name-value predicate `{name}` requires exactly one ordinary string literal value"
            )
        return CfgValue.UNKNOWN, cursor + 2
    if name == "test":
        return CfgValue.FALSE, cursor
    return CfgValue.UNKNOWN, cursor


def _cfg_attribute_value(
    tokens: list[Token], start: int, closing: int, matches: dict[int, int]
) -> CfgValue | None:
    content = start + 2
    if content >= closing or tokens[content].text != "cfg":
        return None
    opening = content + 1
    if opening >= closing or tokens[opening].text != "(":
        raise ValueError(f"malformed cfg attribute at source offset {tokens[start].start}")
    predicate_end = matches[opening]
    if predicate_end != closing - 1:
        raise ValueError(f"malformed cfg attribute at source offset {tokens[start].start}")
    value, cursor = _parse_cfg_predicate(tokens, opening + 1, predicate_end, matches)
    if cursor != predicate_end:
        raise ValueError("cfg attribute must contain exactly one predicate")
    return value


def _item_kind(tokens: list[Token], start: int, matches: dict[int, int]) -> str:
    index = start
    while index < len(tokens) and tokens[index].text == "#":
        if index + 1 >= len(tokens) or tokens[index + 1].text != "[":
            break
        index = matches[index + 1] + 1
    if index < len(tokens) and tokens[index].text == "pub":
        index += 1
        if index < len(tokens) and tokens[index].text == "(":
            index = matches[index] + 1
    saw_const = False
    saw_extern = False
    while index < len(tokens) and tokens[index].text in {
        "async",
        "auto",
        "const",
        "default",
        "extern",
        "unsafe",
    }:
        saw_const = saw_const or tokens[index].text == "const"
        saw_extern = saw_extern or tokens[index].text == "extern"
        index += 1
    if index < len(tokens) and tokens[index].text == "fn":
        return "fn"
    if saw_const:
        return "const"
    if saw_extern:
        return "extern_crate" if index < len(tokens) and tokens[index].text == "crate" else "extern"
    return tokens[index].text if index < len(tokens) else ""


def _disabled_item_end(tokens: list[Token], start: int, matches: dict[int, int]) -> int:
    kind = _item_kind(tokens, start, matches)
    semicolon_kind = kind in {"const", "extern_crate", "let", "static", "type", "use"}
    index = start
    while index < len(tokens):
        text = tokens[index].text
        if text == ";":
            return index
        if text in {"(", "["}:
            index = matches[index] + 1
            continue
        if text == "{":
            closing = matches[index]
            if semicolon_kind:
                index = closing + 1
                continue
            if closing + 1 < len(tokens) and tokens[closing + 1].text in {
                "!",
                "%",
                "&",
                ")",
                "*",
                "+",
                ",",
                "-",
                ".",
                "/",
                ":",
                "::",
                "<",
                "=",
                ">",
                "?",
                "[",
                "]",
                "^",
                "{",
                "as",
                "for",
                "where",
                "|",
            }:
                index = closing + 1
                continue
            return closing
        index += 1
    raise ValueError(
        f"production-disabled item at source offset {tokens[start].start} has no terminator"
    )


def _production_disabled_item_ranges(
    source: str, tokens: list[Token], matches: dict[int, int]
) -> list[tuple[int, int]]:
    """Find definitely test-only Rust items at every analyzable module depth."""

    ranges: list[tuple[int, int]] = []
    macro_ranges = _macro_token_ranges(source, tokens, matches)
    analyzable_item_kinds = {
        "const",
        "enum",
        "extern",
        "extern_crate",
        "fn",
        "impl",
        "macro",
        "macro_rules",
        "mod",
        "static",
        "struct",
        "trait",
        "type",
        "union",
        "use",
    }
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if (
            token.text == "#"
            and index + 1 < len(tokens)
            and tokens[index + 1].text == "["
            and not any(opening < index < closing for opening, closing in macro_ranges)
        ):
            attribute_end = matches[index + 1]
            if (
                _attribute_definitely_disables_item(
                    tokens, index, attribute_end, matches
                )
                and _item_kind(tokens, index, matches) in analyzable_item_kinds
            ):
                item_end = _disabled_item_end(tokens, attribute_end + 1, matches)
                ranges.append((token.start, tokens[item_end].end))
                index = item_end + 1
                continue
        index += 1
    return ranges


def production_source(path: Path) -> str:
    """Return token-visible source under the conservative ``test = false`` view."""

    source = path.read_text(encoding="utf-8")
    for offset, character in enumerate(source):
        if character in RESERVED_SENTINELS:
            raise ValueError(
                f"source contains reserved literal sentinel U+{ord(character):04X} at source offset {offset}"
            )
    production = _mask_comments_and_literals(source)
    tokens = _tokens(production)
    matches = _delimiter_matches(tokens)
    buffer = list(production)
    for start, end in _production_disabled_item_ranges(production, tokens, matches):
        _mask_span(buffer, start, end)
    return "".join(buffer)


@dataclasses.dataclass(frozen=True)
class Dependency:
    module: str
    offset: int
    display: str


@dataclasses.dataclass(frozen=True)
class PathViolation:
    offset: int
    message: str


@dataclasses.dataclass(frozen=True)
class ProductionSpec:
    package_manifest: str
    source_root: str
    crate_root: str
    excluded_files: frozenset[str]
    excluded_directories: tuple[str, ...]


@dataclasses.dataclass(frozen=True)
class PhysicalOverride:
    path: str
    kind: str
    module_path: tuple[str, ...]


@dataclasses.dataclass(frozen=True)
class CompiledFragment:
    path: str
    owner: str
    included_from: str


@dataclasses.dataclass(frozen=True)
class SourceInclusion:
    source: str
    target: str
    offset: int


@dataclasses.dataclass(frozen=True)
class CrateRootDeclaration:
    path: str | None
    public: bool


@dataclasses.dataclass(frozen=True)
class FacadeSpec:
    path: tuple[str, ...]
    kind: str
    source: str
    owner: str
    target: str


@dataclasses.dataclass(frozen=True)
class RootSpec:
    name: str
    layer: int
    allowed_dependencies: frozenset[str]


@dataclasses.dataclass(frozen=True)
class ArchitectureManifest:
    path: str
    production: ProductionSpec
    compiled_fragments: tuple[CompiledFragment, ...]
    physical_overrides: tuple[PhysicalOverride, ...]
    facades: tuple[FacadeSpec, ...]
    roots: dict[str, RootSpec]


@dataclasses.dataclass(frozen=True)
class SourceAnalysis:
    relative: str
    module_path: tuple[str, ...]
    source: str
    dependencies: tuple[Dependency, ...]
    path_violations: tuple[PathViolation, ...]

    @property
    def root(self) -> str:
        return self.module_path[0]


@dataclasses.dataclass(frozen=True)
class ArchitectureAudit:
    failures: tuple[str, ...]
    production_file_count: int
    root_count: int
    edge_count: int


def _file_module_path(
    relative: str,
    source_root: str = "src",
    physical_overrides: tuple[PhysicalOverride, ...] = (),
) -> tuple[str, ...]:
    """Derive the declared library-module path for one governed Rust file."""

    parts = Path(relative).parts
    source_parts = Path(source_root).parts
    if parts[: len(source_parts)] != source_parts:
        raise ValueError(
            f"`{relative}` is not a conventional Rust source path beneath "
            f"`{source_root}`"
        )
    local_parts = parts[len(source_parts) :]
    explicit_file_override = any(
        override.kind == "file" and parts == Path(override.path).parts
        for override in physical_overrides
    )
    if (
        not local_parts
        or (not local_parts[-1].endswith(".rs") and not explicit_file_override)
    ):
        raise ValueError(
            f"`{relative}` is not a conventional Rust source path beneath "
            f"`{source_root}`"
        )

    explicit_root: tuple[str, ...] | None = None
    explicit_suffix: tuple[str, ...] = ()
    for override in physical_overrides:
        source_prefix = Path(override.path).parts
        if override.kind == "file":
            matches_override = parts == source_prefix
            suffix: tuple[str, ...] = ()
        else:
            matches_override = parts[: len(source_prefix)] == source_prefix
            suffix = parts[len(source_prefix) :]
        if not matches_override:
            continue
        if explicit_root is not None:
            raise ValueError(f"`{relative}` matches multiple physical module overrides")
        explicit_root = override.module_path
        explicit_suffix = suffix

    if explicit_root is not None:
        if not explicit_suffix:
            modules = explicit_root
        elif explicit_suffix[-1] == "mod.rs":
            modules = (*explicit_root, *explicit_suffix[:-1])
        else:
            modules = (*explicit_root, *explicit_suffix[:-1], explicit_suffix[-1][:-3])
    elif local_parts[-1] in {"lib.rs", "main.rs"}:
        modules: tuple[str, ...] = ()
    elif local_parts[-1] == "mod.rs":
        modules = tuple(local_parts[:-1])
    else:
        modules = (*local_parts[:-1], local_parts[-1][:-3])
    if any(not IDENTIFIER_RE.fullmatch(module) for module in modules):
        raise ValueError(f"`{relative}` has an invalid declared module path")
    return modules


def _inline_module_ranges(tokens: list[Token], matches: dict[int, int]) -> list[tuple[int, int]]:
    """Locate lexical inline-module bodies whose `super` depth is file-path ambiguous."""

    ranges: list[tuple[int, int]] = []
    for index in range(len(tokens) - 2):
        if (
            tokens[index].text == "mod"
            and IDENTIFIER_RE.fullmatch(tokens[index + 1].text)
            and tokens[index + 2].text == "{"
        ):
            ranges.append((index + 2, matches[index + 2]))
    return ranges


def _group_entries(
    tokens: list[Token], opening: int, matches: dict[int, int]
) -> list[tuple[int, int]]:
    closing = matches[opening]
    entries: list[tuple[int, int]] = []
    entry_start = opening + 1
    cursor = entry_start
    while cursor < closing:
        if tokens[cursor].text in OPEN_TO_CLOSE:
            cursor = matches[cursor] + 1
            continue
        if tokens[cursor].text == ",":
            if entry_start < cursor:
                entries.append((entry_start, cursor))
            entry_start = cursor + 1
        cursor += 1
    if entry_start < closing:
        entries.append((entry_start, closing))
    return entries


def _analyze_root_group(
    tokens: list[Token],
    opening: int,
    matches: dict[int, int],
    root_display: str,
    root_offset: int,
    resolved_prefix: tuple[str, ...],
) -> tuple[list[Dependency], list[PathViolation]]:
    dependencies: list[Dependency] = []
    violations: list[PathViolation] = []
    for start, _end in _group_entries(tokens, opening, matches):
        first = tokens[start].text
        if first == "*":
            violations.append(
                PathViolation(root_offset, f"{root_display} root glob is not analyzable")
            )
        elif first == "self":
            violations.append(
                PathViolation(root_offset, f"{root_display} root self-import is not analyzable")
            )
        elif IDENTIFIER_RE.fullmatch(first):
            resolved_module = resolved_prefix[0] if resolved_prefix else first
            dependencies.append(
                Dependency(resolved_module, root_offset, f"{root_display}::{{{first}::...}}")
            )
        else:
            violations.append(
                PathViolation(
                    root_offset,
                    f"{root_display} root import entry at source offset {tokens[start].start} is not analyzable",
                )
            )
    return dependencies, violations


def _is_keyword(source: str, token: Token, keyword: str) -> bool:
    """Distinguish a Rust keyword from a normalized raw identifier."""

    return token.text == keyword and source[token.start : token.end] == keyword


def _is_macro_rules_body_opening(
    source: str, tokens: list[Token], opening: int
) -> bool:
    ordinary_name = (
        opening >= 3
        and tokens[opening - 2].text == "!"
        and _is_keyword(source, tokens[opening - 3], "macro_rules")
    )
    transcribed_name = (
        opening >= 4
        and tokens[opening - 2].text == "$"
        and tokens[opening - 3].text == "!"
        and _is_keyword(source, tokens[opening - 4], "macro_rules")
    )
    return ordinary_name or transcribed_name


def _macro_token_ranges(
    source: str, tokens: list[Token], matches: dict[int, int]
) -> list[tuple[int, int]]:
    """Locate macro token trees, whose contents are not parsed as Rust items here."""

    ranges: list[tuple[int, int]] = []
    for opening, token in enumerate(tokens):
        if token.text not in OPEN_TO_CLOSE:
            continue
        direct_invocation = opening > 0 and tokens[opening - 1].text == "!"
        macro_rules_definition = _is_macro_rules_body_opening(
            source, tokens, opening
        )
        if direct_invocation or macro_rules_definition:
            ranges.append((opening, matches[opening]))
    return ranges


def _macro_rules_regions(
    source: str, tokens: list[Token], matches: dict[int, int]
) -> tuple[set[int], list[tuple[int, int]], list[tuple[int, int]]]:
    """Return macro_rules body openings plus matcher/transcriber token trees."""

    definitions: set[int] = set()
    matchers: list[tuple[int, int]] = []
    transcribers: list[tuple[int, int]] = []
    invocation_ranges = [
        (opening, matches[opening])
        for opening, token in enumerate(tokens)
        if token.text in OPEN_TO_CLOSE
        and opening > 0
        and tokens[opening - 1].text == "!"
    ]

    def inside(index: int, ranges: list[tuple[int, int]]) -> bool:
        return any(start < index < end for start, end in ranges)

    for opening, token in enumerate(tokens):
        if token.text not in OPEN_TO_CLOSE:
            continue
        if not _is_macro_rules_body_opening(source, tokens, opening):
            continue
        # A macro_rules-looking token sequence inside another matcher's or a
        # direct invocation's token tree is data for that macro, not a live
        # nested definition in this source geometry.
        if inside(opening, matchers) or inside(opening, invocation_ranges):
            continue
        definitions.add(opening)
        closing = matches[opening]
        cursor = opening + 1
        while cursor < closing:
            if tokens[cursor].text not in OPEN_TO_CLOSE:
                raise ValueError(
                    f"macro_rules matcher at source offset {tokens[cursor].start} "
                    "must be delimiter-bounded"
                )
            matcher_closing = matches[cursor]
            if matcher_closing >= closing:
                raise ValueError("macro_rules matcher crosses its definition boundary")
            matchers.append((cursor, matcher_closing))
            cursor = matcher_closing + 1
            if not (
                cursor + 2 < closing
                and tokens[cursor].text == "="
                and tokens[cursor + 1].text == ">"
                and tokens[cursor + 2].text in OPEN_TO_CLOSE
            ):
                raise ValueError("macro_rules arm requires a delimiter-bounded transcriber")
            transcriber_opening = cursor + 2
            transcriber_closing = matches[transcriber_opening]
            if transcriber_closing >= closing:
                raise ValueError("macro_rules transcriber crosses its definition boundary")
            transcribers.append((transcriber_opening, transcriber_closing))
            cursor = transcriber_closing + 1
            if cursor < closing:
                if tokens[cursor].text not in {";", ","}:
                    raise ValueError("macro_rules arms require a semicolon or comma separator")
                cursor += 1
    return definitions, matchers, transcribers


def _macro_invocation_ranges(
    tokens: list[Token], matches: dict[int, int]
) -> list[tuple[int, int]]:
    return [
        (opening, matches[opening])
        for opening, token in enumerate(tokens)
        if token.text in OPEN_TO_CLOSE
        and opening > 0
        and tokens[opening - 1].text == "!"
    ]


def _use_is_at_item_start(
    source: str,
    tokens: list[Token],
    index: int,
    matches: dict[int, int],
) -> bool:
    """Recognize the bounded prefixes allowed immediately before a use item."""

    cursor = index - 1
    if cursor >= 0 and _is_keyword(source, tokens[cursor], "pub"):
        cursor -= 1
    elif cursor >= 0 and tokens[cursor].text == ")":
        opening = matches[cursor]
        if opening == 0 or not _is_keyword(source, tokens[opening - 1], "pub"):
            return False
        cursor = opening - 2
    elif cursor >= 0 and _is_keyword(source, tokens[cursor], "crate"):
        cursor -= 1

    while cursor >= 0 and tokens[cursor].text == "]":
        opening = matches[cursor]
        marker = opening - 1
        if marker >= 0 and tokens[marker].text == "!":
            marker -= 1
        if marker < 0 or tokens[marker].text != "#":
            return False
        cursor = marker - 1

    return cursor < 0 or tokens[cursor].text in {";", "{", "}"}


def _use_tree_ranges(
    source: str, tokens: list[Token], matches: dict[int, int]
) -> list[tuple[int, int]]:
    """Locate actual use items through their matching top-level semicolon."""

    macro_ranges = _macro_token_ranges(source, tokens, matches)
    ranges: list[tuple[int, int]] = []
    for index, token in enumerate(tokens):
        if not _is_keyword(source, token, "use"):
            continue
        if any(opening < index < closing for opening, closing in macro_ranges):
            continue
        if not _use_is_at_item_start(source, tokens, index, matches):
            continue
        cursor = index + 1
        while cursor < len(tokens):
            current = tokens[cursor].text
            if current in OPEN_TO_CLOSE:
                cursor = matches[cursor] + 1
                continue
            if current in CLOSE_TO_OPEN:
                break
            if current == ";":
                ranges.append((index, cursor))
                break
            cursor += 1
    return ranges


def dependency_paths(
    source: str, file_module_path: tuple[str, ...]
) -> tuple[list[Dependency], list[PathViolation]]:
    """Resolve explicit root dependencies using conventional file-module geometry.

    A lexical file path does not safely determine the depth of `super` inside
    an inline production module, especially when macro expansion is involved.
    Such paths therefore fail closed instead of being silently resolved against
    the outer file module.
    """

    tokens = _tokens(source)
    matches = _delimiter_matches(tokens)
    inline_modules = _inline_module_ranges(tokens, matches)
    macro_invocations = _macro_invocation_ranges(tokens, matches)
    use_trees = _use_tree_ranges(source, tokens, matches)
    dependencies: list[Dependency] = []
    violations: list[PathViolation] = []
    index = 0
    while index < len(tokens):
        if (
            index + 2 < len(tokens)
            and tokens[index].text == "extern"
            and tokens[index + 1].text == "crate"
            and tokens[index + 2].text == "self"
        ):
            violations.append(
                PathViolation(
                    tokens[index].start,
                    "crate root alias via `extern crate self` is not analyzable",
                )
            )
            index += 3
            continue

        root = tokens[index].text
        if root not in {"crate", "super"}:
            index += 1
            continue
        if (
            root == "super"
            and index >= 2
            and tokens[index - 1].text == "::"
            and tokens[index - 2].text == "super"
        ):
            index += 1
            continue

        root_offset = tokens[index].start
        root_segments = 1
        cursor = index
        if root == "super":
            while (
                cursor + 2 < len(tokens)
                and tokens[cursor + 1].text == "::"
                and tokens[cursor + 2].text == "super"
            ):
                root_segments += 1
                cursor += 2
        root_display = "crate" if root == "crate" else "::".join(["super"] * root_segments)
        if any(
            opening < index < closing
            for opening, closing in macro_invocations
        ):
            violations.append(
                PathViolation(
                    root_offset,
                    f"{root_display} token inside a macro invocation is not analyzable",
                )
            )
            index = cursor + 1
            continue
        containing_use = next(
            (
                (use_index, semicolon)
                for use_index, semicolon in use_trees
                if use_index < index < semicolon
            ),
            None,
        )
        if root == "super" and containing_use is not None and index != containing_use[0] + 1:
            violations.append(
                PathViolation(
                    root_offset,
                    "nested `super` root inside a grouped use is not analyzable",
                )
            )
            index = cursor + 1
            continue
        if root == "super" and any(
            opening < index < closing for opening, closing in inline_modules
        ):
            violations.append(
                PathViolation(
                    root_offset,
                    f"{root_display} path inside an inline production module has ambiguous module depth",
                )
            )
            index = cursor + 1
            continue
        if root == "super" and root_segments > len(file_module_path):
            violations.append(
                PathViolation(
                    root_offset,
                    f"{root_display} exceeds file module depth {len(file_module_path)}",
                )
            )
            index = cursor + 1
            continue
        resolved_prefix = () if root == "crate" else file_module_path[:-root_segments]

        if cursor + 1 < len(tokens) and tokens[cursor + 1].text == "as":
            violations.append(
                PathViolation(root_offset, f"{root_display} root alias is not analyzable")
            )
            index = cursor + 2
            continue
        if cursor + 1 >= len(tokens) or tokens[cursor + 1].text != "::":
            if containing_use is not None:
                violations.append(
                    PathViolation(
                        root_offset,
                        f"bare {root_display} root path is not analyzable",
                    )
                )
            index += 1
            continue
        if cursor + 2 >= len(tokens):
            violations.append(
                PathViolation(root_offset, f"incomplete {root_display} path is not analyzable")
            )
            index = cursor + 2
            continue

        target_index = cursor + 2
        target = tokens[target_index].text
        if target == "*":
            violations.append(
                PathViolation(root_offset, f"{root_display} root glob is not analyzable")
            )
        elif target == "{":
            grouped_dependencies, grouped_violations = _analyze_root_group(
                tokens,
                target_index,
                matches,
                root_display,
                root_offset,
                resolved_prefix,
            )
            dependencies.extend(grouped_dependencies)
            violations.extend(grouped_violations)
        elif target in {"self", "super"}:
            violations.append(
                PathViolation(root_offset, f"{root_display} root escape is not analyzable")
            )
        elif IDENTIFIER_RE.fullmatch(target):
            resolved_module = resolved_prefix[0] if resolved_prefix else target
            dependencies.append(
                Dependency(resolved_module, root_offset, f"{root_display}::{target}")
            )
        else:
            violations.append(
                PathViolation(root_offset, f"{root_display} path is not analyzable")
            )
        index = target_index + 1
    return dependencies, violations


def _inside_brace(tokens: list[Token], matches: dict[int, int], index: int) -> bool:
    return any(
        token.text == "{" and opening < index < matches[opening]
        for opening, token in enumerate(tokens)
    )


def _item_production_presence(
    tokens: list[Token], matches: dict[int, int], item_keyword: int
) -> CfgValue:
    values: list[CfgValue] = []
    cursor = item_keyword - 1
    if cursor >= 0 and tokens[cursor].text == "pub":
        cursor -= 1
    while cursor >= 0 and tokens[cursor].text == "]":
        opening = matches[cursor]
        marker = opening - 1
        if marker >= 0 and tokens[marker].text == "!":
            marker -= 1
        if marker < 0 or tokens[marker].text != "#":
            break
        values.append(
            _attribute_production_presence(tokens, marker, cursor, matches)
        )
        cursor = marker - 1
    return _cfg_all(values)


def _has_public_use_of(
    source: str,
    tokens: list[Token],
    matches: dict[int, int],
    target: str,
    *,
    alias: str | None = None,
    lower: int = 0,
    upper: int | None = None,
    top_level: bool = False,
    direct_container: int | None = None,
) -> bool:
    upper = len(tokens) if upper is None else upper
    for use_index, semicolon in _use_tree_ranges(source, tokens, matches):
        if not (lower < use_index < upper) or semicolon >= upper:
            continue
        if use_index == 0 or not _is_keyword(source, tokens[use_index - 1], "pub"):
            continue
        if _item_production_presence(tokens, matches, use_index) is not CfgValue.TRUE:
            continue
        if top_level and _inside_brace(tokens, matches, use_index):
            continue
        if direct_container is not None:
            containing_braces = [
                opening
                for opening, token in enumerate(tokens)
                if token.text == "{" and opening < use_index < matches[opening]
            ]
            if containing_braces != [direct_container]:
                continue
        if use_index + 3 >= semicolon:
            continue
        if not (
            _is_keyword(source, tokens[use_index + 1], "crate")
            and tokens[use_index + 2].text == "::"
            and tokens[use_index + 3].text == target
        ):
            continue
        if alias is None:
            if (
                use_index + 6 == semicolon
                and tokens[use_index + 4].text == "::"
                and tokens[use_index + 5].text == "*"
            ):
                return True
            continue
        if (
            use_index + 6 == semicolon
            and _is_keyword(source, tokens[use_index + 4], "as")
            and tokens[use_index + 5].text == alias
        ):
            return True
    return False


def _has_public_external_module(source: str, module_name: str) -> bool:
    tokens = _tokens(source)
    matches = _delimiter_matches(tokens)
    for index in range(len(tokens) - 3):
        if not (
            _is_keyword(source, tokens[index], "pub")
            and _is_keyword(source, tokens[index + 1], "mod")
            and tokens[index + 2].text == module_name
            and tokens[index + 3].text == ";"
        ):
            continue
        if _inside_brace(tokens, matches, index):
            continue
        if _item_production_presence(tokens, matches, index + 1) is not CfgValue.TRUE:
            continue
        return True
    return False


def _facade_projection_error(
    facade: FacadeSpec, analysis: SourceAnalysis
) -> str | None:
    display = "::".join(facade.path)
    if facade.kind == "module":
        if analysis.module_path != facade.path:
            return (
                f"facade `{display}` module source resolves to "
                f"`{'::'.join(analysis.module_path)}`"
            )
    elif analysis.module_path != facade.path[:-1]:
        return (
            f"facade `{display}` source resolves to `{'::'.join(analysis.module_path)}`, "
            f"not its declared parent `{'::'.join(facade.path[:-1])}`"
        )

    tokens = _tokens(analysis.source)
    matches = _delimiter_matches(tokens)
    if facade.kind == "alias":
        if _has_public_use_of(
            analysis.source,
            tokens,
            matches,
            facade.target,
            alias=facade.path[-1],
            top_level=True,
        ):
            return None
        return (
            f"facade `{display}` source does not contain a top-level public alias "
            f"of `crate::{facade.target}`"
        )
    if facade.kind == "module":
        if _has_public_use_of(
            analysis.source,
            tokens,
            matches,
            facade.target,
            top_level=True,
        ):
            return None
        return (
            f"facade `{display}` source does not publicly project "
            f"`crate::{facade.target}`"
        )

    module_name = facade.path[-1]
    for index in range(len(tokens) - 3):
        if not (
            _is_keyword(analysis.source, tokens[index], "pub")
            and _is_keyword(analysis.source, tokens[index + 1], "mod")
            and tokens[index + 2].text == module_name
            and tokens[index + 3].text == "{"
        ):
            continue
        opening = index + 3
        if _inside_brace(tokens, matches, index):
            continue
        if _item_production_presence(tokens, matches, index + 1) is not CfgValue.TRUE:
            continue
        if _has_public_use_of(
            analysis.source,
            tokens,
            matches,
            facade.target,
            lower=opening,
            upper=matches[opening],
            direct_container=opening,
        ):
            return None
    return (
        f"facade `{display}` source does not contain a public inline module "
        f"projecting `crate::{facade.target}`"
    )


def _exact_keys(
    table: dict[str, object], expected: frozenset[str], label: str
) -> None:
    missing = sorted(expected - table.keys())
    unknown = sorted(table.keys() - expected)
    if missing:
        raise ValueError(f"{label} is missing field(s): {', '.join(missing)}")
    if unknown:
        raise ValueError(f"{label} has unknown field(s): {', '.join(unknown)}")


def _relative_manifest_path(value: object, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{label} must be a non-empty relative path")
    candidate = Path(value)
    if (
        candidate.is_absolute()
        or "\\" in value
        or any(part in {"", ".", ".."} for part in candidate.parts)
        or candidate.as_posix() != value
    ):
        raise ValueError(f"{label} must be a normalized repository-relative path")
    return value


def _package_target_path(package_manifest: str, value: object, label: str) -> str:
    """Resolve a Cargo target path relative to its package manifest."""

    if not isinstance(value, str) or not value:
        raise ValueError(f"{label} must be a non-empty relative path")
    candidate = Path(value)
    if candidate.is_absolute() or "\\" in value or candidate.as_posix() != value:
        raise ValueError(f"{label} must remain repository-relative")
    manifest_parent = Path(package_manifest).parent.as_posix()
    joined = posixpath.normpath(
        value if manifest_parent == "." else f"{manifest_parent}/{value}"
    )
    if joined in {"", ".", ".."} or joined.startswith("../"):
        raise ValueError(f"{label} escapes the repository")
    return _relative_manifest_path(joined, label)


def _string_list(value: object, label: str) -> tuple[str, ...]:
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise ValueError(f"{label} must be an array of strings")
    items = tuple(value)
    if len(set(items)) != len(items):
        raise ValueError(f"{label} must not contain duplicates")
    return items


def _manifest_tables(data: dict[str, object], key: str) -> list[dict[str, object]]:
    value = data.get(key)
    if not isinstance(value, list) or any(not isinstance(item, dict) for item in value):
        raise ValueError(f"manifest `{key}` must be an array of tables")
    return value


def load_manifest(root: Path, manifest_path: Path | None = None) -> ArchitectureManifest:
    root = root.resolve()
    if manifest_path is None:
        path = root / DEFAULT_MANIFEST_RELATIVE
    else:
        path = manifest_path if manifest_path.is_absolute() else root / manifest_path
    try:
        relative_manifest = path.resolve().relative_to(root.resolve()).as_posix()
    except ValueError as error:
        raise ValueError("architecture manifest must remain inside the repository root") from error
    if not path.is_file():
        raise ValueError(f"{relative_manifest}: required architecture manifest is missing")
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"{relative_manifest}: could not parse manifest: {error}") from error

    _exact_keys(
        data,
        frozenset(
            {
                "schema_version",
                "production",
                "compiled_fragment",
                "physical_override",
                "facade",
                "root",
            }
        ),
        "architecture manifest",
    )
    if data["schema_version"] != 1 or isinstance(data["schema_version"], bool):
        raise ValueError("architecture manifest schema_version must be integer 1")

    production_table = data["production"]
    if not isinstance(production_table, dict):
        raise ValueError("manifest `production` must be a table")
    _exact_keys(
        production_table,
        frozenset(
            {
                "package_manifest",
                "source_root",
                "crate_root",
                "excluded_files",
                "excluded_directories",
            }
        ),
        "manifest `production`",
    )
    package_manifest = _relative_manifest_path(
        production_table["package_manifest"],
        "manifest production.package_manifest",
    )
    source_root = _relative_manifest_path(
        production_table["source_root"], "manifest production.source_root"
    )
    crate_root = _relative_manifest_path(
        production_table["crate_root"], "manifest production.crate_root"
    )
    if crate_root != source_root and not crate_root.startswith(f"{source_root}/"):
        raise ValueError(f"manifest production.crate_root is outside `{source_root}`")
    excluded_files = frozenset(
        _relative_manifest_path(value, "manifest production.excluded_files entry")
        for value in _string_list(
            production_table["excluded_files"], "manifest production.excluded_files"
        )
    )
    excluded_directories = tuple(
        _relative_manifest_path(value, "manifest production.excluded_directories entry")
        for value in _string_list(
            production_table["excluded_directories"],
            "manifest production.excluded_directories",
        )
    )
    for excluded in (*excluded_files, *excluded_directories):
        if excluded != source_root and not excluded.startswith(f"{source_root}/"):
            raise ValueError(
                f"manifest production exclusion `{excluded}` is outside `{source_root}`"
            )
    if crate_root not in excluded_files:
        raise ValueError("manifest production.crate_root must be an excluded module entrypoint")
    production = ProductionSpec(
        package_manifest, source_root, crate_root, excluded_files, excluded_directories
    )

    physical_overrides: list[PhysicalOverride] = []
    override_paths: set[str] = set()
    for index, table in enumerate(_manifest_tables(data, "physical_override")):
        label = f"manifest physical_override[{index}]"
        _exact_keys(table, frozenset({"path", "kind", "module_path"}), label)
        override_path = _relative_manifest_path(table["path"], f"{label}.path")
        if override_path != source_root and not override_path.startswith(f"{source_root}/"):
            raise ValueError(f"{label}.path is outside `{source_root}`")
        if override_path in excluded_files or any(
            override_path == directory or override_path.startswith(f"{directory}/")
            for directory in excluded_directories
        ):
            raise ValueError(
                f"{label}.path overlaps an excluded production source"
            )
        if override_path in override_paths:
            raise ValueError(f"duplicate physical override path `{override_path}`")
        override_paths.add(override_path)
        kind = table["kind"]
        if kind not in {"file", "directory"}:
            raise ValueError(f"{label}.kind must be `file` or `directory`")
        module_path = _string_list(table["module_path"], f"{label}.module_path")
        if len(module_path) != 1 or not IDENTIFIER_RE.fullmatch(module_path[0]):
            raise ValueError(
                f"{label}.module_path must contain exactly one Rust root identifier"
            )
        physical_overrides.append(PhysicalOverride(override_path, kind, module_path))

    roots: dict[str, RootSpec] = {}
    root_tables = _manifest_tables(data, "root")
    if not root_tables:
        raise ValueError("architecture manifest must declare at least one root")
    for index, table in enumerate(root_tables):
        label = f"manifest root[{index}]"
        _exact_keys(table, frozenset({"name", "layer", "allowed_dependencies"}), label)
        name = table["name"]
        if not isinstance(name, str) or not IDENTIFIER_RE.fullmatch(name):
            raise ValueError(f"{label}.name must be a Rust identifier")
        if name in roots:
            raise ValueError(f"duplicate architecture root `{name}`")
        layer = table["layer"]
        if not isinstance(layer, int) or isinstance(layer, bool) or layer < 0:
            raise ValueError(f"{label}.layer must be a non-negative integer")
        allowed = _string_list(
            table["allowed_dependencies"], f"{label}.allowed_dependencies"
        )
        if any(not IDENTIFIER_RE.fullmatch(dependency) for dependency in allowed):
            raise ValueError(f"{label}.allowed_dependencies must contain Rust identifiers")
        roots[name] = RootSpec(name, layer, frozenset(allowed))

    for root_spec in roots.values():
        for dependency in sorted(root_spec.allowed_dependencies):
            target = roots.get(dependency)
            if target is None:
                raise ValueError(
                    f"root `{root_spec.name}` allows unknown dependency root `{dependency}`"
                )
            if dependency == root_spec.name:
                raise ValueError(f"root `{root_spec.name}` cannot allow itself")
            if target.layer >= root_spec.layer:
                raise ValueError(
                    f"root `{root_spec.name}` layer {root_spec.layer} must be above "
                    f"dependency `{dependency}` layer {target.layer}"
                )

    compiled_fragments: list[CompiledFragment] = []
    fragment_paths: set[str] = set()
    for index, table in enumerate(_manifest_tables(data, "compiled_fragment")):
        label = f"manifest compiled_fragment[{index}]"
        _exact_keys(table, frozenset({"path", "owner", "included_from"}), label)
        fragment_path = _relative_manifest_path(table["path"], f"{label}.path")
        if fragment_path in fragment_paths:
            raise ValueError(f"duplicate compiled fragment path `{fragment_path}`")
        fragment_paths.add(fragment_path)
        if fragment_path not in excluded_files:
            raise ValueError(
                f"compiled fragment `{fragment_path}` must be excluded from standalone modules"
            )
        if not fragment_path.endswith(".rs") or not (root / fragment_path).is_file():
            raise ValueError(
                f"compiled fragment `{fragment_path}` is not an existing Rust source file"
            )
        owner = table["owner"]
        if not isinstance(owner, str) or owner not in roots:
            raise ValueError(f"{label}.owner must name a known architecture root")
        included_from = _relative_manifest_path(
            table["included_from"], f"{label}.included_from"
        )
        if included_from in excluded_files or not (root / included_from).is_file():
            raise ValueError(
                f"{label}.included_from must name an enumerated production source"
            )
        compiled_fragments.append(
            CompiledFragment(fragment_path, owner, included_from)
        )

    expected_excluded_files = {
        crate_root,
        *(fragment.path for fragment in compiled_fragments),
    }
    if excluded_files != expected_excluded_files:
        missing = sorted(expected_excluded_files - excluded_files)
        unexpected = sorted(excluded_files - expected_excluded_files)
        details: list[str] = []
        if missing:
            details.append(f"missing {', '.join(missing)}")
        if unexpected:
            details.append(f"unexpected {', '.join(unexpected)}")
        raise ValueError(
            "manifest production.excluded_files must contain exactly the crate "
            "entrypoint and declared compiled fragments (" + "; ".join(details) + ")"
        )
    expected_excluded_directories: set[str] = set()
    if set(excluded_directories) != expected_excluded_directories:
        missing = sorted(expected_excluded_directories - set(excluded_directories))
        unexpected = sorted(set(excluded_directories) - expected_excluded_directories)
        details = []
        if missing:
            details.append(f"missing {', '.join(missing)}")
        if unexpected:
            details.append(f"unexpected {', '.join(unexpected)}")
        raise ValueError(
            "manifest production.excluded_directories must be empty; the governed "
            "library package cannot hide a source directory (" + "; ".join(details) + ")"
        )

    facades: list[FacadeSpec] = []
    facade_paths: set[tuple[str, ...]] = set()
    for index, table in enumerate(_manifest_tables(data, "facade")):
        label = f"manifest facade[{index}]"
        _exact_keys(
            table,
            frozenset({"path", "kind", "source", "owner", "target"}),
            label,
        )
        raw_path = table["path"]
        if not isinstance(raw_path, str):
            raise ValueError(f"{label}.path must be a `::`-separated module path")
        facade_path = tuple(raw_path.split("::"))
        if len(facade_path) < 2 or any(
            not IDENTIFIER_RE.fullmatch(part) for part in facade_path
        ):
            raise ValueError(f"{label}.path must contain at least two Rust identifiers")
        if facade_path in facade_paths:
            raise ValueError(f"duplicate facade path `{raw_path}`")
        facade_paths.add(facade_path)
        kind = table["kind"]
        if kind not in {"alias", "inline_module", "module"}:
            raise ValueError(
                f"{label}.kind must be `alias`, `inline_module`, or `module`"
            )
        source = _relative_manifest_path(table["source"], f"{label}.source")
        owner = table["owner"]
        target = table["target"]
        if owner not in roots or target not in roots:
            raise ValueError(f"{label} must name known owner and target roots")
        if facade_path[0] != owner:
            raise ValueError(f"{label}.owner must match the first facade path component")
        if target not in roots[owner].allowed_dependencies:
            raise ValueError(
                f"facade `{raw_path}` target `{target}` is not an allowed dependency of `{owner}`"
            )
        facades.append(FacadeSpec(facade_path, kind, source, owner, target))

    for override in physical_overrides:
        target = root / override.path
        exists_as_kind = target.is_file() if override.kind == "file" else target.is_dir()
        if not exists_as_kind:
            raise ValueError(
                f"physical override `{override.path}` is not an existing {override.kind}"
            )
        if override.kind == "directory":
            module_file = target / "mod.rs"
            if module_file.is_symlink() or not module_file.is_file():
                raise ValueError(
                    f"physical directory override `{override.path}` has no regular "
                    "non-symlink mod.rs entrypoint"
                )
        if override.module_path[0] not in roots:
            raise ValueError(
                f"physical override `{override.path}` maps to unknown root "
                f"`{override.module_path[0]}`"
            )
    for excluded in sorted(excluded_files):
        if not (root / excluded).is_file():
            raise ValueError(f"excluded production file `{excluded}` does not exist")
    for excluded in excluded_directories:
        if not (root / excluded).is_dir():
            raise ValueError(f"excluded production directory `{excluded}` does not exist")

    return ArchitectureManifest(
        relative_manifest,
        production,
        tuple(compiled_fragments),
        tuple(physical_overrides),
        tuple(facades),
        roots,
    )


def _path_attribute_value(
    raw_source: str,
    tokens: list[Token],
    attribute_start: int,
    attribute_end: int,
) -> str | None:
    content = attribute_start + 2
    if content >= attribute_end or tokens[content].text != "path":
        return None
    if not (
        content + 3 == attribute_end
        and tokens[content + 1].text == "="
        and tokens[content + 2].text == STRING_LITERAL_TOKEN
    ):
        raise ValueError(
            f"malformed path attribute at source offset {tokens[attribute_start].start}"
        )
    literal_offset = tokens[content + 2].start
    if raw_source[literal_offset : literal_offset + 1] != '"':
        raise ValueError("module path attributes require ordinary string literals")
    literal_end = _quoted_string_end(raw_source, literal_offset)
    try:
        value = json.loads(raw_source[literal_offset:literal_end])
    except (TypeError, ValueError) as error:
        raise ValueError("module path attribute is not a valid string") from error
    if not isinstance(value, str) or not value:
        raise ValueError("module path attribute must be a non-empty string")
    return value


def _crate_root_declarations(path: Path) -> dict[str, CrateRootDeclaration]:
    raw_source = path.read_text(encoding="utf-8")
    source = production_source(path)
    tokens = _tokens(source)
    matches = _delimiter_matches(tokens)
    declarations: dict[str, CrateRootDeclaration] = {}
    pending_path: str | None = None
    index = 0
    while index < len(tokens):
        if (
            tokens[index].text == "#"
            and index + 1 < len(tokens)
            and tokens[index + 1].text == "["
        ):
            attribute_end = matches[index + 1]
            attribute_path = _path_attribute_value(
                raw_source, tokens, index, attribute_end
            )
            if attribute_path is not None:
                if pending_path is not None:
                    raise ValueError("module declaration has multiple path attributes")
                pending_path = attribute_path
            conditional_paths = _cfg_attr_path_values(
                raw_source, tokens, index, attribute_end, matches
            )
            if conditional_paths:
                raise ValueError(
                    "crate-root modules cannot use production-active "
                    "cfg_attr path attributes"
                )
            if attribute_path is None:
                raise ValueError(
                    "crate-root module attributes are forbidden except exact #[path] "
                    "physical overrides"
                )
            index = attribute_end + 1
            continue

        if (
            tokens[index].text == "#"
            and index + 2 < len(tokens)
            and tokens[index + 1].text == "!"
            and tokens[index + 2].text == "["
        ):
            raise ValueError("crate-root inner attributes are not permitted")

        cursor = index
        public = False
        if _is_keyword(source, tokens[cursor], "pub"):
            public = True
            cursor += 1
            if cursor < len(tokens) and tokens[cursor].text == "(":
                public = False
                cursor = matches[cursor] + 1
        if public and cursor + 3 < len(tokens) and _is_keyword(
            source, tokens[cursor], "use"
        ):
            if pending_path is not None:
                raise ValueError("path attribute is not attached to a module declaration")
            if not (
                tokens[cursor + 1].text == "api"
                and tokens[cursor + 2].text == "::"
                and tokens[cursor + 3].text == "{"
            ):
                raise ValueError(
                    "crate-root public uses are limited to an explicit `api::{...}` "
                    "value projection"
                )
            closing = matches[cursor + 3]
            projection_tokens = tokens[cursor + 4 : closing]
            expect_name = True
            names: set[str] = set()
            for projection in projection_tokens:
                if expect_name:
                    if not IDENTIFIER_RE.fullmatch(projection.text):
                        raise ValueError(
                            "crate-root `api` value projection must contain plain identifiers"
                        )
                    if projection.text in names:
                        raise ValueError(
                            "crate-root `api` value projection contains a duplicate"
                        )
                    names.add(projection.text)
                    expect_name = False
                else:
                    if projection.text != ",":
                        raise ValueError(
                            "crate-root `api` value projection must be comma separated"
                        )
                    expect_name = True
            if not names:
                raise ValueError("crate-root `api` value projection is empty")
            terminator = closing + 1
            if terminator >= len(tokens) or tokens[terminator].text != ";":
                raise ValueError("crate-root `api` value projection must end with `;`")
            index = terminator + 1
            continue
        if cursor + 1 < len(tokens) and _is_keyword(source, tokens[cursor], "mod"):
            name = tokens[cursor + 1].text
            if not IDENTIFIER_RE.fullmatch(name):
                raise ValueError(
                    f"module declaration at source offset {tokens[cursor].start} "
                    "has no analyzable identifier"
                )
            if name in declarations:
                raise ValueError(f"crate root declares module `{name}` more than once")
            declarations[name] = CrateRootDeclaration(pending_path, public)
            pending_path = None
            terminator = cursor + 2
            if terminator >= len(tokens) or tokens[terminator].text != ";":
                raise ValueError(
                    f"crate-root module `{name}` must be an external semicolon declaration"
                )
            index = terminator + 1
            continue

        if pending_path is not None:
            raise ValueError("path attribute is not attached to a module declaration")
        raise ValueError(
            "crate root must contain only attributes and external module declarations; "
            f"found `{tokens[index].text}` at source offset {tokens[index].start}"
        )
    if pending_path is not None:
        raise ValueError("path attribute is not attached to a module declaration")
    return declarations


def _declared_module_path(source_root: str, value: str) -> str:
    candidate = Path(value)
    if (
        candidate.is_absolute()
        or "\\" in value
        or any(part in {"", ".", ".."} for part in candidate.parts)
        or candidate.as_posix() != value
    ):
        raise ValueError(
            f"crate-root module path `{value}` must remain normalized beneath `{source_root}`"
        )
    return f"{source_root}/{value}"


def _ordinary_string_value(raw_source: str, token: Token, label: str) -> str:
    if token.text != STRING_LITERAL_TOKEN or raw_source[token.start : token.start + 1] != '"':
        raise ValueError(f"{label} requires an ordinary string literal")
    literal_end = _quoted_string_end(raw_source, token.start)
    try:
        value = json.loads(raw_source[token.start:literal_end])
    except (TypeError, ValueError) as error:
        raise ValueError(f"{label} is not a valid string literal") from error
    if not isinstance(value, str) or not value:
        raise ValueError(f"{label} must be a non-empty string")
    return value


def _cfg_attr_payload(
    tokens: list[Token],
    start: int,
    end: int,
    matches: dict[int, int],
) -> tuple[CfgValue, list[tuple[int, int]]]:
    opening = start + 1
    if (
        opening >= end
        or tokens[start].text != "cfg_attr"
        or tokens[opening].text != "("
        or matches[opening] != end - 1
    ):
        raise ValueError(f"malformed cfg_attr at source offset {tokens[start].start}")
    closing = end - 1
    value, cursor = _parse_cfg_predicate(tokens, opening + 1, closing, matches)
    if cursor >= closing or tokens[cursor].text != ",":
        raise ValueError(
            f"cfg_attr at source offset {tokens[start].start} requires attributes"
        )
    cursor += 1
    if cursor >= closing:
        raise ValueError(
            f"cfg_attr at source offset {tokens[start].start} requires attributes"
        )

    payload: list[tuple[int, int]] = []
    while cursor < closing:
        segment_start = cursor
        while cursor < closing and tokens[cursor].text != ",":
            if tokens[cursor].text in OPEN_TO_CLOSE:
                nested_closing = matches[cursor]
                if nested_closing >= closing:
                    raise ValueError(
                        f"cfg_attr payload at source offset "
                        f"{tokens[segment_start].start} crosses its boundary"
                    )
                cursor = nested_closing + 1
                continue
            cursor += 1
        segment_end = cursor
        if segment_start == segment_end:
            raise ValueError(
                f"cfg_attr at source offset {tokens[start].start} "
                "contains an empty attribute"
            )
        payload.append((segment_start, segment_end))
        if cursor == closing:
            break
        cursor += 1
        if cursor == closing:
            break
    return value, payload


def _cfg_meta_value(
    tokens: list[Token], start: int, end: int, matches: dict[int, int]
) -> CfgValue | None:
    if tokens[start].text != "cfg":
        return None
    opening = start + 1
    if (
        opening >= end
        or tokens[opening].text != "("
        or matches[opening] != end - 1
    ):
        raise ValueError(f"malformed cfg meta item at source offset {tokens[start].start}")
    closing = end - 1
    value, cursor = _parse_cfg_predicate(tokens, opening + 1, closing, matches)
    if cursor != closing:
        raise ValueError(
            f"cfg meta item at source offset {tokens[start].start} "
            "must contain exactly one predicate"
        )
    return value


def _attribute_definitely_disables_item(
    tokens: list[Token],
    attribute_start: int,
    attribute_end: int,
    matches: dict[int, int],
) -> bool:
    if (
        _cfg_attribute_value(tokens, attribute_start, attribute_end, matches)
        is CfgValue.FALSE
    ):
        return True

    content = attribute_start + 2
    if content >= attribute_end or tokens[content].text != "cfg_attr":
        return False

    def active_cfg_is_false(start: int, end: int) -> bool:
        predicate, payload = _cfg_attr_payload(tokens, start, end, matches)
        if predicate is not CfgValue.TRUE:
            return False
        for segment_start, segment_end in payload:
            cfg_value = _cfg_meta_value(
                tokens, segment_start, segment_end, matches
            )
            if cfg_value is CfgValue.FALSE:
                return True
            if (
                tokens[segment_start].text == "cfg_attr"
                and active_cfg_is_false(segment_start, segment_end)
            ):
                return True
        return False

    return active_cfg_is_false(content, attribute_end)


def _attribute_production_presence(
    tokens: list[Token],
    attribute_start: int,
    attribute_end: int,
    matches: dict[int, int],
) -> CfgValue:
    direct = _cfg_attribute_value(
        tokens, attribute_start, attribute_end, matches
    )
    if direct is not None:
        return direct

    content = attribute_start + 2
    if content >= attribute_end or tokens[content].text != "cfg_attr":
        return CfgValue.TRUE

    def cfg_attr_presence(start: int, end: int) -> CfgValue:
        predicate, payload = _cfg_attr_payload(tokens, start, end, matches)
        applied_values: list[CfgValue] = []
        for segment_start, segment_end in payload:
            cfg_value = _cfg_meta_value(
                tokens, segment_start, segment_end, matches
            )
            if cfg_value is not None:
                applied_values.append(cfg_value)
            elif tokens[segment_start].text == "cfg_attr":
                applied_values.append(
                    cfg_attr_presence(segment_start, segment_end)
                )
        applied = _cfg_all(applied_values)
        if predicate is CfgValue.FALSE or applied is CfgValue.TRUE:
            return CfgValue.TRUE
        if predicate is CfgValue.TRUE:
            return applied
        return CfgValue.UNKNOWN

    return cfg_attr_presence(content, attribute_end)


def _cfg_attr_path_values(
    raw_source: str,
    tokens: list[Token],
    attribute_start: int,
    attribute_end: int,
    matches: dict[int, int],
) -> list[str]:
    """Return path attributes whose surrounding cfg_attr can be active."""

    content = attribute_start + 2
    if content >= attribute_end or tokens[content].text != "cfg_attr":
        return []

    def active_paths(start: int, end: int) -> list[str]:
        value, payload = _cfg_attr_payload(tokens, start, end, matches)
        if value is CfgValue.FALSE:
            return []

        values: list[str] = []
        for segment_start, segment_end in payload:
            if tokens[segment_start].text == "path":
                if not (
                    segment_start + 3 == segment_end
                    and tokens[segment_start + 1].text == "="
                    and tokens[segment_start + 2].text == STRING_LITERAL_TOKEN
                ):
                    raise ValueError(
                        f"malformed cfg_attr path at source offset "
                        f"{tokens[segment_start].start}"
                    )
                values.append(
                    _ordinary_string_value(
                        raw_source,
                        tokens[segment_start + 2],
                        "cfg_attr module path",
                    )
                )
            elif tokens[segment_start].text == "cfg_attr":
                values.extend(active_paths(segment_start, segment_end))
        return values

    return active_paths(content, attribute_end)


def _source_inclusion_surface(
    path: Path, relative: str, production: str
) -> tuple[list[SourceInclusion], list[tuple[int, str]]]:
    raw_source = path.read_text(encoding="utf-8")
    tokens = _tokens(production)
    matches = _delimiter_matches(tokens)
    inclusions: list[SourceInclusion] = []
    path_attributes: list[tuple[int, str]] = []
    for index, token in enumerate(tokens):
        if (
            token.text == "#"
            and index + 1 < len(tokens)
            and tokens[index + 1].text == "["
        ):
            attribute_end = matches[index + 1]
            value = _path_attribute_value(raw_source, tokens, index, attribute_end)
            if value is not None:
                path_attributes.append((token.start, value))
            for conditional_path in _cfg_attr_path_values(
                raw_source, tokens, index, attribute_end, matches
            ):
                path_attributes.append((token.start, conditional_path))
        if token.text != "include":
            continue
        if not (
            index + 2 < len(tokens)
            and tokens[index + 1].text == "!"
            and tokens[index + 2].text in OPEN_TO_CLOSE
        ):
            raise ValueError(
                f"include macro reference at source offset {token.start} must be "
                "a directly analyzable invocation; aliases are forbidden"
            )
        closing = matches[index + 2]
        if closing != index + 4:
            raise ValueError(
                f"include! at source offset {token.start} must contain one literal path"
            )
        literal = _ordinary_string_value(
            raw_source, tokens[index + 3], "include! source path"
        )
        literal_path = Path(literal)
        if (
            literal_path.is_absolute()
            or "\\" in literal
            or literal_path.as_posix() != literal
        ):
            raise ValueError(
                f"include! path `{literal}` at source offset {token.start} "
                "must be a file-relative POSIX path"
            )
        target = posixpath.normpath(
            f"{Path(relative).parent.as_posix()}/{literal}"
        )
        if target == ".." or target.startswith("../"):
            raise ValueError(
                f"include! path `{literal}` at source offset {token.start} "
                "escapes the repository"
            )
        inclusions.append(SourceInclusion(relative, target, token.start))
    return inclusions, path_attributes


def _cargo_library_findings(
    root: Path, manifest: ArchitectureManifest
) -> list[str]:
    cargo_relative = manifest.production.package_manifest
    cargo_path = root / cargo_relative
    if not cargo_path.is_file() or cargo_path.is_symlink():
        return [f"{cargo_relative}: required regular package manifest is missing"]
    try:
        cargo = tomllib.loads(cargo_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        return [f"{cargo_relative}: could not parse package manifest: {error}"]

    package = cargo.get("package")
    if not isinstance(package, dict):
        return [f"{cargo_relative}: architecture root requires a package table"]
    autolib = package.get("autolib", True)
    if not isinstance(autolib, bool):
        return [f"{cargo_relative}: package.autolib must be boolean when present"]

    lib = cargo.get("lib")
    if lib is None:
        if not autolib:
            return [
                f"{cargo_relative}: package.autolib=false removes the governed "
                "library target"
            ]
        raw_path: object = "src/lib.rs"
    else:
        if not isinstance(lib, dict):
            return [f"{cargo_relative}: lib target must be a table"]
        raw_path = lib.get("path", "src/lib.rs")
    try:
        declared = _package_target_path(
            cargo_relative, raw_path, f"{cargo_relative} lib.path"
        )
    except ValueError as error:
        return [str(error)]

    if declared != manifest.production.crate_root:
        return [
            f"{cargo_relative}: library target `{declared}` does not match governed crate "
            f"root `{manifest.production.crate_root}`"
        ]
    return []


def _cargo_dependency_entries(
    cargo: dict[str, object], workspace_dependencies: dict[str, object] | None = None
) -> list[tuple[str, str, str, object]]:
    """Return (scope, key, package identity, specification) for direct deps."""

    entries: list[tuple[str, str, str, object]] = []

    def collect(scope: str, table: object) -> None:
        if not isinstance(table, dict):
            return
        for key, raw_spec in table.items():
            if not isinstance(key, str):
                continue
            resolved = raw_spec
            if (
                isinstance(raw_spec, dict)
                and raw_spec.get("workspace") is True
                and workspace_dependencies is not None
            ):
                resolved = workspace_dependencies.get(key, raw_spec)
            package = key
            if isinstance(resolved, dict) and isinstance(
                resolved.get("package"), str
            ):
                package = resolved["package"]
            entries.append((scope, key, package, resolved))

    for scope in ("dependencies", "dev-dependencies", "build-dependencies"):
        collect(scope, cargo.get(scope))
    targets = cargo.get("target")
    if isinstance(targets, dict):
        for target_name, target in targets.items():
            if not isinstance(target, dict):
                continue
            for scope in ("dependencies", "dev-dependencies", "build-dependencies"):
                collect(f"target.{target_name}.{scope}", target.get(scope))
    return entries


def _compatibility_shell_reexports(path: Path) -> tuple[set[str] | None, str | None]:
    """Parse the shell's one permitted explicit engine-module re-export."""

    try:
        source = production_source(path)
        tokens = _tokens(source)
        matches = _delimiter_matches(tokens)
    except (OSError, UnicodeError, ValueError) as error:
        return None, str(error)
    if len(tokens) < 7 or [token.text for token in tokens[:5]] != [
        "pub",
        "use",
        "ostadix_api",
        "::",
        "{",
    ]:
        return None, "must contain only one explicit `pub use ostadix_api::{...};`"
    closing = matches.get(4)
    if closing is None or closing + 2 != len(tokens) or tokens[closing + 1].text != ";":
        return None, "must contain only one explicit `pub use ostadix_api::{...};`"
    names: list[str] = []
    expect_name = True
    for token in tokens[5:closing]:
        if expect_name:
            if token.text == "," and not names:
                return None, "engine module re-export list starts with a comma"
            if not IDENTIFIER_RE.fullmatch(token.text):
                return None, "engine module re-export list must contain plain identifiers"
            names.append(token.text)
            expect_name = False
        else:
            if token.text != ",":
                return None, "engine module re-export list must be comma separated"
            expect_name = True
    if not names:
        return None, "engine module re-export list is empty"
    if len(names) != len(set(names)):
        return None, "engine module re-export list contains a duplicate"
    return set(names), None


def _engine_shell_direction_findings(
    root: Path, manifest: ArchitectureManifest
) -> list[str]:
    """Enforce one-way engine ownership and a non-implementing root shell."""

    failures: list[str] = []
    root_manifest_relative = "Cargo.toml"
    root_manifest_path = root / root_manifest_relative
    engine_manifest_relative = manifest.production.package_manifest
    engine_manifest_path = root / engine_manifest_relative
    cargos: dict[str, dict[str, object]] = {}
    for relative, path in (
        (root_manifest_relative, root_manifest_path),
        (engine_manifest_relative, engine_manifest_path),
    ):
        if not path.is_file() or path.is_symlink():
            failures.append(f"{relative}: required regular package manifest is missing")
            continue
        try:
            parsed = tomllib.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
            failures.append(f"{relative}: could not parse package manifest: {error}")
            continue
        cargos[relative] = parsed
    if len(cargos) != 2:
        return failures

    root_cargo = cargos[root_manifest_relative]
    engine_cargo = cargos[engine_manifest_relative]
    root_package = root_cargo.get("package")
    engine_package = engine_cargo.get("package")
    if not isinstance(root_package, dict) or not isinstance(engine_package, dict):
        failures.append("root and governed engine manifests must each declare a package")
        return failures
    if root_package.get("name") != "o-lang":
        failures.append("Cargo.toml: compatibility shell package must be named `o-lang`")
    if engine_package.get("name") != "ostadix-api":
        failures.append(
            f"{engine_manifest_relative}: governed engine package must be named `ostadix-api`"
        )
    root_version = root_package.get("version")
    engine_version = engine_package.get("version")
    if not isinstance(root_version, str) or root_version != engine_version:
        failures.append(
            "Cargo.toml: `o-lang` and `ostadix-api` package versions must match exactly"
        )

    root_autolib = root_package.get("autolib", True)
    root_lib = root_cargo.get("lib")
    if root_autolib is False:
        failures.append("Cargo.toml: compatibility shell library target is required")
    elif not isinstance(root_autolib, bool):
        failures.append("Cargo.toml: package.autolib must be boolean when present")
    else:
        root_lib_path: object = "src/lib.rs"
        if root_lib is not None:
            if not isinstance(root_lib, dict):
                failures.append("Cargo.toml: compatibility shell lib target must be a table")
                root_lib_path = None
            else:
                root_lib_path = root_lib.get("path", "src/lib.rs")
        if root_lib_path is not None:
            try:
                declared_root_lib = _package_target_path(
                    root_manifest_relative,
                    root_lib_path,
                    "Cargo.toml lib.path",
                )
            except ValueError as error:
                failures.append(str(error))
            else:
                if declared_root_lib != "src/lib.rs":
                    failures.append(
                        "Cargo.toml: compatibility shell library target must remain "
                        "`src/lib.rs`"
                    )

    workspace = root_cargo.get("workspace")
    workspace_dependencies = None
    if isinstance(workspace, dict) and isinstance(workspace.get("dependencies"), dict):
        workspace_dependencies = workspace["dependencies"]
    for scope, key, package, spec in _cargo_dependency_entries(
        engine_cargo, workspace_dependencies
    ):
        identity = package
        points_to_root = False
        if isinstance(spec, dict) and isinstance(spec.get("path"), str):
            workspace_owned = (
                workspace_dependencies is not None
                and spec is workspace_dependencies.get(key)
            )
            manifest_parent = (
                "."
                if workspace_owned
                else Path(engine_manifest_relative).parent.as_posix()
            )
            dependency_path = posixpath.normpath(
                f"{manifest_parent}/{spec['path']}"
            )
            points_to_root = dependency_path == "."
        if identity == "o-lang" or points_to_root:
            failures.append(
                f"{engine_manifest_relative}: {scope}.{key} must not depend on the "
                "`o-lang` compatibility shell"
            )

    dependencies = root_cargo.get("dependencies")
    engine_dependency = (
        dependencies.get("ostadix-api") if isinstance(dependencies, dict) else None
    )
    expected_path = Path(engine_manifest_relative).parent.as_posix()
    expected_version = f"={engine_version}" if isinstance(engine_version, str) else None
    if not isinstance(engine_dependency, dict):
        failures.append(
            "Cargo.toml: compatibility shell must directly depend on `ostadix-api`"
        )
    else:
        if engine_dependency.get("package", "ostadix-api") != "ostadix-api":
            failures.append(
                "Cargo.toml: canonical `ostadix-api` dependency must name package "
                "`ostadix-api`"
            )
        if engine_dependency.get("path") != expected_path:
            failures.append(
                "Cargo.toml: `ostadix-api` dependency path must exactly match "
                f"`{expected_path}`"
            )
        if engine_dependency.get("version") != expected_version:
            failures.append(
                "Cargo.toml: `ostadix-api` dependency must use exact same-version "
                f"requirement `{expected_version}`"
            )
        if engine_dependency.get("optional", False) is not False:
            failures.append(
                "Cargo.toml: `ostadix-api` dependency must be unconditional, not optional"
            )
    aliases = [
        key
        for _scope, key, package, _spec in _cargo_dependency_entries(
            root_cargo, workspace_dependencies
        )
        if package == "ostadix-api" and key != "ostadix-api"
    ]
    if aliases:
        failures.append(
            "Cargo.toml: compatibility shell must not add renamed `ostadix-api` "
            f"dependencies: {', '.join(sorted(aliases))}"
        )

    shell_source = root / "src"
    if not shell_source.is_dir() or shell_source.is_symlink():
        failures.append("src: compatibility shell source root is missing or symlinked")
        return failures
    shell_symlinks = sorted(
        path.relative_to(root).as_posix()
        for path in shell_source.rglob("*")
        if path.is_symlink()
    )
    if shell_symlinks:
        failures.append(
            "root compatibility shell source geometry must not contain symlinks: "
            + ", ".join(shell_symlinks)
        )
    duplicate_sources = sorted(
        path.relative_to(root).as_posix()
        for path in shell_source.rglob("*.rs")
        if path.is_file()
        and not path.is_symlink()
        and path.relative_to(shell_source).as_posix() not in {"lib.rs", "main.rs"}
        and not path.relative_to(shell_source).as_posix().startswith("bin/")
    )
    if duplicate_sources:
        failures.append(
            "root compatibility shell contains runtime implementation source outside "
            f"its entrypoints: {', '.join(duplicate_sources)}"
        )

    root_lib_path = root / "src/lib.rs"
    if not root_lib_path.is_file() or root_lib_path.is_symlink():
        failures.append("src/lib.rs: required regular compatibility library is missing")
        return failures
    reexports, error = _compatibility_shell_reexports(root_lib_path)
    if error is not None:
        failures.append(f"src/lib.rs: {error}")
        return failures
    try:
        declarations = _crate_root_declarations(root / manifest.production.crate_root)
    except (OSError, UnicodeError, ValueError) as declaration_error:
        failures.append(
            f"{manifest.production.crate_root}: could not determine public engine modules: "
            f"{declaration_error}"
        )
        return failures
    expected_reexports = {
        name for name, declaration in declarations.items() if declaration.public
    }
    if reexports != expected_reexports:
        missing = sorted(expected_reexports - (reexports or set()))
        extra = sorted((reexports or set()) - expected_reexports)
        details = []
        if missing:
            details.append(f"missing {', '.join(missing)}")
        if extra:
            details.append(f"unexpected {', '.join(extra)}")
        failures.append(
            "src/lib.rs: compatibility module reexports must exactly match the "
            "governed engine's public roots (" + "; ".join(details) + ")"
        )
    return failures


def _crate_root_findings(root: Path, manifest: ArchitectureManifest) -> list[str]:
    failures: list[str] = []
    crate_root = root / manifest.production.crate_root
    try:
        declarations = _crate_root_declarations(crate_root)
    except (OSError, UnicodeError, ValueError) as error:
        return [
            f"{manifest.production.crate_root}: could not analyze crate-root modules: {error}"
        ]

    expected_overrides: dict[str, str] = {}
    for override in manifest.physical_overrides:
        if len(override.module_path) != 1:
            continue
        module = override.module_path[0]
        expected = (
            override.path
            if override.kind == "file"
            else f"{override.path}/mod.rs"
        )
        if module in expected_overrides:
            failures.append(f"root `{module}` has multiple crate-root physical overrides")
        expected_overrides[module] = expected

    for module in sorted(declarations.keys() - manifest.roots.keys()):
        failures.append(f"crate root declares unknown architecture root `{module}`")
    for module in sorted(manifest.roots.keys() - declarations.keys()):
        failures.append(f"manifest root `{module}` is not declared by the crate root")
    for module in sorted(declarations.keys() & manifest.roots.keys()):
        raw_path = declarations[module].path
        try:
            declared_path = (
                None
                if raw_path is None
                else _declared_module_path(manifest.production.source_root, raw_path)
            )
        except ValueError as error:
            failures.append(f"crate-root module `{module}`: {error}")
            continue
        expected_path = expected_overrides.get(module)
        if declared_path != expected_path:
            failures.append(
                f"crate-root module `{module}` declares physical path "
                f"`{declared_path or '<conventional>'}`, expected "
                f"`{expected_path or '<conventional>'}`"
            )
    for owner in sorted({facade.owner for facade in manifest.facades}):
        declaration = declarations.get(owner)
        if declaration is not None and not declaration.public:
            failures.append(
                f"facade owner root `{owner}` must be a plain public crate-root module"
            )
    return failures


def _top_level_external_modules(path: Path) -> set[str]:
    """Return production-active `mod name;` declarations in one source file.

    Rust permits item declarations inside ordinary blocks as well as inline
    modules. An external child declaration in either location has a physical
    path derived from a lexical module stack that this manifest does not model,
    so it fails closed. Parentheses and brackets are traversed because they can
    contain brace blocks. Macro matchers remain opaque, while a literal module
    keyword in a transcriber or invocation is an explicit unsupported geometry
    even when that macro is not reached under the current build configuration.
    """

    source = production_source(path)
    tokens = _tokens(source)
    matches = _delimiter_matches(tokens)
    macro_ranges = _macro_token_ranges(source, tokens, matches)
    macro_openings = {opening for opening, _closing in macro_ranges}
    definition_openings, matcher_ranges, transcriber_ranges = _macro_rules_regions(
        source, tokens, matches
    )

    def inside(index: int, ranges: list[tuple[int, int]]) -> bool:
        return any(opening < index < closing for opening, closing in ranges)

    macro_geometry_ranges = list(transcriber_ranges)
    macro_geometry_ranges.extend(
        (opening, closing)
        for opening, closing in macro_ranges
        if opening not in definition_openings and not inside(opening, matcher_ranges)
    )
    for opening, closing in macro_geometry_ranges:
        for index in range(opening + 1, closing):
            token = tokens[index]
            if inside(index, matcher_ranges):
                continue
            if (
                _is_keyword(source, token, "mod")
                and (index == 0 or tokens[index - 1].text != "$")
            ):
                raise ValueError(
                    f"module keyword at source offset {token.start} appears inside a "
                    "macro transcriber or invocation; macro-generated physical "
                    "module geometry is unsupported"
                )
    modules: set[str] = set()

    def scan(start: int, end: int, inside_nested_scope: bool) -> None:
        index = start
        while index < end:
            token = tokens[index]
            if _is_keyword(source, token, "mod"):
                if index + 2 >= end:
                    raise ValueError(
                        f"module declaration at source offset {token.start} is incomplete"
                    )
                name = tokens[index + 1].text
                if not IDENTIFIER_RE.fullmatch(name):
                    raise ValueError(
                        f"module declaration at source offset {token.start} "
                        "has no analyzable identifier"
                    )
                terminator = tokens[index + 2]
                if terminator.text == ";":
                    if inside_nested_scope:
                        raise ValueError(
                            f"external module `{name}` at source offset {token.start} "
                            "is nested inside an inline module or block; this physical "
                            "source geometry is unsupported"
                        )
                    modules.add(name)
                    index += 3
                    continue
                if terminator.text == "{":
                    closing = matches[index + 2]
                    scan(index + 3, closing, True)
                    index = closing + 1
                    continue
            if token.text in OPEN_TO_CLOSE:
                closing = matches[index]
                if index not in macro_openings:
                    scan(
                        index + 1,
                        closing,
                        inside_nested_scope or token.text == "{",
                    )
                index = closing + 1
                continue
            index += 1

    scan(0, len(tokens), False)
    return modules


def _physical_override_ownership_findings(
    root: Path,
    manifest: ArchitectureManifest,
    production_paths: list[Path],
) -> list[str]:
    """Reject an override entrypoint that also has conventional ownership.

    Rust may compile the same bytes twice when a `#[path]` crate-root module
    points at a file that a parent module also reaches through `mod child;`.
    A pathname-keyed analysis would then certify only one of the two module
    identities. The supported overrides intentionally replace, rather than
    supplement, conventional ownership, so dual ownership is fail-closed.
    """

    failures: list[str] = []
    production_by_relative = {
        path.relative_to(root).as_posix(): path for path in production_paths
    }
    module_declarations: dict[str, set[str]] = {}
    for relative, path in production_by_relative.items():
        try:
            module_declarations[relative] = _top_level_external_modules(path)
        except (OSError, UnicodeError, ValueError) as error:
            failures.append(
                f"{relative}: could not analyze module ownership: {error}"
            )
    crate_root = root / manifest.production.crate_root
    try:
        crate_declarations = _crate_root_declarations(crate_root)
    except (OSError, UnicodeError, ValueError) as error:
        return [
            f"{manifest.production.crate_root}: could not analyze override ownership: "
            f"{error}"
        ]

    for override in manifest.physical_overrides:
        entrypoint = Path(override.path)
        if override.kind == "directory":
            entrypoint /= "mod.rs"
        else:
            declared_children = module_declarations.get(override.path, set())
            if declared_children:
                failures.append(
                    f"physical file override `{override.path}` declares external "
                    f"modules {sorted(declared_children)}; child module ownership "
                    "is unsupported"
                )

        if entrypoint.name == "mod.rs":
            child_name = entrypoint.parent.name
            parent_directory = entrypoint.parent.parent
        else:
            # A non-.rs #[path] target has no conventional `mod child;`
            # spelling; any second ownership would require another #[path],
            # which the source-inclusion closure rejects independently.
            if entrypoint.suffix != ".rs":
                continue
            child_name = entrypoint.stem
            parent_directory = entrypoint.parent
        if not IDENTIFIER_RE.fullmatch(child_name):
            continue

        parent_candidates: list[str] = []
        source_root = Path(manifest.production.source_root)
        if parent_directory == source_root:
            declaration = crate_declarations.get(child_name)
            if declaration is not None and declaration.path is None:
                parent_candidates.append(manifest.production.crate_root)
        else:
            parent_candidates.extend(
                (
                    parent_directory.with_suffix(".rs").as_posix(),
                    (parent_directory / "mod.rs").as_posix(),
                )
            )

        for parent_relative in parent_candidates:
            parent = production_by_relative.get(parent_relative)
            if parent is None and parent_relative == manifest.production.crate_root:
                parent = crate_root
            if parent is None:
                continue
            declared_children = module_declarations.get(parent_relative)
            if declared_children is None and parent_relative == manifest.production.crate_root:
                try:
                    declared_children = _top_level_external_modules(parent)
                except (OSError, UnicodeError, ValueError) as error:
                    failures.append(
                        f"{parent_relative}: could not analyze module ownership: {error}"
                    )
                    continue
            if declared_children is None:
                continue
            if child_name in declared_children:
                failures.append(
                    f"physical override `{override.path}` also has conventional module "
                    f"ownership through `{parent_relative}` (`mod {child_name};`)"
                )

    # An include! fragment is compiled in its owner's module identity. External
    # module declarations inside one have path-resolution semantics tied to the
    # include site rather than the fragment pathname, which this lexical root
    # graph intentionally does not model.
    for fragment in manifest.compiled_fragments:
        fragment_path = root / fragment.path
        try:
            declared_children = _top_level_external_modules(fragment_path)
        except (OSError, UnicodeError, ValueError) as error:
            failures.append(
                f"{fragment.path}: could not analyze fragment module ownership: {error}"
            )
            continue
        if declared_children:
            failures.append(
                f"compiled fragment `{fragment.path}` declares external modules "
                f"{sorted(declared_children)}; fragment module ownership is unsupported"
            )
    return failures


def _production_paths(root: Path, manifest: ArchitectureManifest) -> list[Path]:
    source_root = root / manifest.production.source_root
    if not source_root.is_dir():
        raise ValueError(
            f"production source root `{manifest.production.source_root}` is missing"
        )
    if source_root.is_symlink():
        raise ValueError(
            f"production source root `{manifest.production.source_root}` must not be a symlink"
        )
    for candidate in sorted(source_root.rglob("*")):
        if candidate.is_symlink():
            relative = candidate.relative_to(root).as_posix()
            raise ValueError(f"production source path `{relative}` must not be a symlink")
    paths: dict[str, Path] = {}
    for path in sorted(source_root.rglob("*.rs")):
        relative = path.relative_to(root).as_posix()
        if relative in manifest.production.excluded_files:
            continue
        if any(
            relative == directory or relative.startswith(f"{directory}/")
            for directory in manifest.production.excluded_directories
        ):
            continue
        if path.is_symlink():
            raise ValueError(f"production Rust path `{relative}` must not be a symlink")
        if not path.is_file():
            raise ValueError(f"production Rust path `{relative}` is not a regular file")
        paths[relative] = path

    # Rust accepts any filename in a #[path = "..."] module declaration. A
    # file override is therefore production source even when its name does not
    # end in `.rs`; the manifest declaration, not the conventional glob, is
    # authoritative for this closed source geometry.
    for override in manifest.physical_overrides:
        if override.kind != "file":
            continue
        path = root / override.path
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            raise ValueError(f"production Rust path `{relative}` must not be a symlink")
        if not path.is_file():
            raise ValueError(f"production Rust path `{relative}` is not a regular file")
        paths[relative] = path
    return [paths[relative] for relative in sorted(paths)]


def _source_inclusion_findings(
    root: Path,
    manifest: ArchitectureManifest,
    analyses: dict[str, SourceAnalysis],
) -> list[str]:
    failures: list[str] = []
    actual: list[SourceInclusion] = []
    for analysis in analyses.values():
        path = root / analysis.relative
        try:
            inclusions, path_attributes = _source_inclusion_surface(
                path, analysis.relative, analysis.source
            )
        except (OSError, UnicodeError, ValueError) as error:
            failures.append(
                f"{analysis.relative}: could not analyze source inclusion: {error}"
            )
            continue
        actual.extend(inclusions)
        for offset, declared_path in path_attributes:
            line = analysis.source.count("\n", 0, offset) + 1
            failures.append(
                f"{analysis.relative}:{line}: undeclared #[path = "
                f"{declared_path!r}] module source; physical source geometry "
                f"must be declared in {manifest.path} and the crate root"
            )

    crate_relative = manifest.production.crate_root
    crate_path = root / crate_relative
    try:
        crate_production = production_source(crate_path)
        crate_inclusions, _crate_path_attributes = _source_inclusion_surface(
            crate_path, crate_relative, crate_production
        )
        actual.extend(crate_inclusions)
    except (OSError, UnicodeError, ValueError) as error:
        failures.append(f"{crate_relative}: could not analyze source inclusion: {error}")

    declared = {
        (fragment.included_from, fragment.path): fragment
        for fragment in manifest.compiled_fragments
    }
    counts: dict[tuple[str, str], int] = {}
    for inclusion in actual:
        key = (inclusion.source, inclusion.target)
        counts[key] = counts.get(key, 0) + 1
        if key not in declared:
            source = analyses.get(inclusion.source)
            production = source.source if source is not None else production_source(
                root / inclusion.source
            )
            line = production.count("\n", 0, inclusion.offset) + 1
            failures.append(
                f"{inclusion.source}:{line}: include! source `{inclusion.target}` "
                f"is not declared in {manifest.path}"
            )
    for key, fragment in declared.items():
        count = counts.get(key, 0)
        if count != 1:
            failures.append(
                f"compiled fragment `{fragment.path}` must have exactly one include! "
                f"from `{fragment.included_from}`; observed {count}"
            )
        owner_source = analyses.get(fragment.included_from)
        if owner_source is None or owner_source.root != fragment.owner:
            observed = "missing" if owner_source is None else owner_source.root
            failures.append(
                f"compiled fragment `{fragment.path}` owner source resolves to "
                f"`{observed}`, expected `{fragment.owner}`"
            )
    return failures


def _tarjan_components(
    vertices: set[str], edges: set[tuple[str, str]]
) -> list[tuple[str, ...]]:
    adjacency = {
        vertex: sorted(target for source, target in edges if source == vertex)
        for vertex in vertices
    }
    index = 0
    indices: dict[str, int] = {}
    lowlinks: dict[str, int] = {}
    stack: list[str] = []
    on_stack: set[str] = set()
    components: list[tuple[str, ...]] = []

    def visit(vertex: str) -> None:
        nonlocal index
        indices[vertex] = index
        lowlinks[vertex] = index
        index += 1
        stack.append(vertex)
        on_stack.add(vertex)
        for target in adjacency[vertex]:
            if target not in indices:
                visit(target)
                lowlinks[vertex] = min(lowlinks[vertex], lowlinks[target])
            elif target in on_stack:
                lowlinks[vertex] = min(lowlinks[vertex], indices[target])
        if lowlinks[vertex] != indices[vertex]:
            return
        component: list[str] = []
        while True:
            member = stack.pop()
            on_stack.remove(member)
            component.append(member)
            if member == vertex:
                break
        components.append(tuple(sorted(component)))

    for vertex in sorted(vertices):
        if vertex not in indices:
            visit(vertex)
    return sorted(components)


def audit_architecture(
    root: Path, manifest_path: Path | None = None
) -> ArchitectureAudit:
    root = root.resolve()
    failures: list[str] = []
    try:
        manifest = load_manifest(root, manifest_path)
        paths = _production_paths(root, manifest)
    except ValueError as error:
        return ArchitectureAudit((str(error),), 0, 0, 0)

    failures.extend(_crate_root_findings(root, manifest))
    failures.extend(_cargo_library_findings(root, manifest))
    failures.extend(_engine_shell_direction_findings(root, manifest))
    failures.extend(_physical_override_ownership_findings(root, manifest, paths))

    source_entries: list[tuple[Path, tuple[str, ...] | None]] = [
        (path, None) for path in paths
    ]
    source_entries.extend(
        (root / fragment.path, (fragment.owner,))
        for fragment in manifest.compiled_fragments
    )

    analyses: dict[str, SourceAnalysis] = {}
    observed_roots: set[str] = set()
    for path, declared_module_path in source_entries:
        relative = path.relative_to(root).as_posix()
        try:
            module_path = declared_module_path or _file_module_path(
                relative,
                source_root=manifest.production.source_root,
                physical_overrides=manifest.physical_overrides,
            )
            source = production_source(path)
            dependencies, path_violations = dependency_paths(source, module_path)
        except (OSError, UnicodeError, ValueError) as error:
            failures.append(f"{relative}: could not analyze Rust tokens: {error}")
            continue
        analysis = SourceAnalysis(
            relative,
            module_path,
            source,
            tuple(dependencies),
            tuple(path_violations),
        )
        analyses[relative] = analysis
        observed_roots.add(analysis.root)
        if analysis.root not in manifest.roots:
            failures.append(
                f"{relative}: production path resolves to unknown root `{analysis.root}`"
            )
        for violation in path_violations:
            line = source.count("\n", 0, violation.offset) + 1
            failures.append(
                f"{relative}:{line}: {violation.message}; production architecture "
                "surfaces require explicit root paths"
            )

    failures.extend(_source_inclusion_findings(root, manifest, analyses))

    for root_name in sorted(manifest.roots.keys() - observed_roots):
        failures.append(f"manifest root `{root_name}` has no production Rust source")

    edges: set[tuple[str, str]] = set()
    for analysis in analyses.values():
        source_spec = manifest.roots.get(analysis.root)
        if source_spec is None:
            continue
        for dependency in analysis.dependencies:
            if dependency.module == analysis.root:
                continue
            edge = (analysis.root, dependency.module)
            edges.add(edge)
            line = analysis.source.count("\n", 0, dependency.offset) + 1
            target_spec = manifest.roots.get(dependency.module)
            if target_spec is None:
                failures.append(
                    f"{analysis.relative}:{line}: dependency `{dependency.display}` resolves "
                    f"to unknown root `{dependency.module}`"
                )
                continue
            if dependency.module not in source_spec.allowed_dependencies:
                failures.append(
                    f"{analysis.relative}:{line}: root edge `{analysis.root} -> "
                    f"{dependency.module}` is not declared in {manifest.path}"
                )
            if target_spec.layer >= source_spec.layer:
                failures.append(
                    f"{analysis.relative}:{line}: root edge `{analysis.root} -> "
                    f"{dependency.module}` does not descend from layer "
                    f"{source_spec.layer} to a lower layer"
                )

    known_edges = {
        (source, target)
        for source, target in edges
        if source in manifest.roots and target in manifest.roots
    }
    for component in _tarjan_components(set(manifest.roots), known_edges):
        if len(component) > 1:
            failures.append(
                "multi-root strongly connected component detected: "
                f"{', '.join(component)}; the declared root graph must remain a DAG"
            )

    for facade in manifest.facades:
        source = analyses.get(facade.source)
        display = "::".join(facade.path)
        if source is None:
            failures.append(f"facade `{display}` source `{facade.source}` is not production")
            continue
        if source.root != facade.owner:
            failures.append(
                f"facade `{display}` source resolves to `{source.root}`, not owner "
                f"`{facade.owner}`"
            )
        if facade.kind == "module":
            parents = [
                analysis
                for analysis in analyses.values()
                if analysis.module_path == facade.path[:-1]
            ]
            if len(parents) != 1:
                failures.append(
                    f"facade `{display}` requires exactly one production parent "
                    f"module `{'::'.join(facade.path[:-1])}`; observed {len(parents)}"
                )
            elif not _has_public_external_module(
                parents[0].source, facade.path[-1]
            ):
                failures.append(
                    f"facade `{display}` parent does not publicly declare external "
                    f"module `{facade.path[-1]}`"
                )
        projection_error = _facade_projection_error(facade, source)
        if projection_error is not None:
            failures.append(projection_error)
        if facade.target not in {
            dependency.module
            for dependency in source.dependencies
            if dependency.module != source.root
        }:
            failures.append(
                f"facade `{display}` source does not expose a direct "
                f"`{facade.owner} -> {facade.target}` production edge"
            )

    for rule in RULES:
        for crate_relative in rule.paths:
            if crate_relative == "src":
                relative = manifest.production.source_root
            elif crate_relative.startswith("src/"):
                relative = (
                    f"{manifest.production.source_root}/"
                    f"{crate_relative.removeprefix('src/')}"
                )
            else:
                relative = crate_relative
            analysis = analyses.get(relative)
            if analysis is None:
                failures.append(f"{relative}: required architecture surface is missing")
                continue
            for dependency in analysis.dependencies:
                explicitly_forbidden = dependency.module in rule.forbidden_modules
                outside_allowlist = (
                    rule.allowed_modules is not None
                    and dependency.module not in rule.allowed_modules
                )
                if not explicitly_forbidden and not outside_allowlist:
                    continue
                line = analysis.source.count("\n", 0, dependency.offset) + 1
                failures.append(
                    f"{relative}:{line}: forbidden dependency `{dependency.display}`; {rule.reason}"
                )

    return ArchitectureAudit(
        tuple(failures),
        len(paths),
        len(observed_roots),
        len(edges),
    )


def findings(root: Path, manifest_path: Path | None = None) -> list[str]:
    """Compatibility wrapper returning only audit failures."""

    return list(audit_architecture(root, manifest_path).failures)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="repository root (default: parent of scripts/)",
    )
    parser.add_argument(
        "--manifest",
        type=Path,
        help="manifest path, relative to --root (default: ci/architecture-roots.toml)",
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()
    audit = audit_architecture(root, args.manifest)
    if audit.failures:
        for failure in audit.failures:
            print(f"architecture boundary: FAIL: {failure}", file=sys.stderr)
        return 1
    print(
        "architecture dependency boundaries: PASS "
        f"({audit.production_file_count} production files, "
        f"{audit.root_count} roots, {audit.edge_count} cross-root edges)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
