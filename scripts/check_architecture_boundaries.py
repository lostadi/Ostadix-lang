#!/usr/bin/env python3
"""Reject the first frozen wrong-way Rust dependency edges.

This is intentionally a token-aware lexical guard, not a full Rust dependency
analyzer. It protects boundaries that have already been made explicit while a
broader workspace extraction remains future work. Comments, literals, and
top-level items that are definitely disabled when ``test = false`` cannot hide
or manufacture dependencies.
"""

from __future__ import annotations

import argparse
import dataclasses
import enum
from pathlib import Path
import re
import sys


@dataclasses.dataclass(frozen=True)
class Rule:
    paths: tuple[str, ...]
    forbidden_modules: tuple[str, ...]
    reason: str


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

# `src/lib.rs` loads this physical tree once through
# `#[path = "placement/protocol/mod.rs"] mod placement_protocol;`. Relative
# `super` paths must therefore be resolved from that declared module root, not
# from the compatibility facade implied by the on-disk directory names.
EXPLICIT_PATH_MODULE_ROOTS = (
    (("src", "placement", "protocol"), ("placement_protocol",)),
)


RULES = (
    Rule(
        ("src/parser.rs",),
        ("ir", "registry"),
        "syntax must depend only on its narrow dialect projection, not IR or the executable registry",
    ),
    Rule(
        ("src/syntax_dialect.rs",),
        ("ir", "registry", "runtime_exec"),
        "the syntax-dialect contract must remain a capability-free model boundary",
    ),
    Rule(
        ("src/ir.rs",),
        ("hgraph", "placement", "placement_protocol"),
        "IR must not depend on its HGraph or placement projections",
    ),
    Rule(
        ("src/effects.rs",),
        ("world",),
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
        ("executor",),
        "evidence must bind a dispatch model rather than import its executor",
    ),
    Rule(
        ("src/dispatch_model.rs",),
        ("evidence", "executor", "hgraph"),
        "the shared dispatch model must remain independent of HGraph and executor consumers",
    ),
    Rule(
        PLACEMENT_PROTOCOL_PATHS,
        (
            "backend",
            "dispatch_model",
            "effects",
            "eval",
            "evidence",
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
            "src/registry/bundle/mod.rs",
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


def _top_level_disabled_item_ranges(
    tokens: list[Token], matches: dict[int, int]
) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    delimiters: list[str] = []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if not delimiters and token.text == "#" and index + 1 < len(tokens) and tokens[index + 1].text == "[":
            attribute_end = matches[index + 1]
            cfg_value = _cfg_attribute_value(tokens, index, attribute_end, matches)
            if cfg_value is CfgValue.FALSE:
                item_end = _disabled_item_end(tokens, attribute_end + 1, matches)
                ranges.append((token.start, tokens[item_end].end))
                index = item_end + 1
                continue
        if token.text in OPEN_TO_CLOSE:
            delimiters.append(token.text)
        elif token.text in CLOSE_TO_OPEN:
            if not delimiters or delimiters[-1] != CLOSE_TO_OPEN[token.text]:
                raise ValueError(f"unbalanced delimiter `{token.text}` at source offset {token.start}")
            delimiters.pop()
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
    for start, end in _top_level_disabled_item_ranges(tokens, matches):
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


def _file_module_path(relative: str) -> tuple[str, ...]:
    """Derive the declared library-module path for one governed Rust file."""

    parts = Path(relative).parts
    if len(parts) < 2 or parts[0] != "src" or not parts[-1].endswith(".rs"):
        raise ValueError(f"`{relative}` is not a conventional Rust source path beneath src/")
    if len(parts) > 2 and parts[1] == "bin":
        raise ValueError(f"`{relative}` is a binary-crate path, not a library-module path")

    explicit_root: tuple[str, ...] | None = None
    explicit_suffix: tuple[str, ...] = ()
    for source_prefix, module_root in EXPLICIT_PATH_MODULE_ROOTS:
        if parts[: len(source_prefix)] == source_prefix:
            explicit_root = module_root
            explicit_suffix = parts[len(source_prefix) :]
            break

    if explicit_root is not None:
        if not explicit_suffix:
            raise ValueError(f"`{relative}` resolves to an explicit module directory, not a file")
        if explicit_suffix[-1] == "mod.rs":
            modules = (*explicit_root, *explicit_suffix[:-1])
        else:
            modules = (*explicit_root, *explicit_suffix[:-1], explicit_suffix[-1][:-3])
    elif parts[-1] in {"lib.rs", "main.rs"}:
        modules: tuple[str, ...] = ()
    elif parts[-1] == "mod.rs":
        modules = tuple(parts[1:-1])
    else:
        modules = (*parts[1:-1], parts[-1][:-3])
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


def _macro_token_ranges(
    source: str, tokens: list[Token], matches: dict[int, int]
) -> list[tuple[int, int]]:
    """Locate macro token trees, whose contents are not parsed as Rust items here."""

    ranges: list[tuple[int, int]] = []
    for opening, token in enumerate(tokens):
        if token.text not in OPEN_TO_CLOSE:
            continue
        direct_invocation = opening > 0 and tokens[opening - 1].text == "!"
        macro_rules_definition = (
            opening >= 3
            and tokens[opening - 2].text == "!"
            and _is_keyword(source, tokens[opening - 3], "macro_rules")
        )
        if direct_invocation or macro_rules_definition:
            ranges.append((opening, matches[opening]))
    return ranges


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
            if any(opening < index < closing for opening, closing in use_trees):
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


def findings(root: Path) -> list[str]:
    failures: list[str] = []
    for rule in RULES:
        for relative in rule.paths:
            path = root / relative
            if not path.is_file():
                failures.append(f"{relative}: required architecture surface is missing")
                continue
            try:
                file_module_path = _file_module_path(relative)
                source = production_source(path)
            except ValueError as error:
                failures.append(f"{relative}: could not analyze Rust tokens: {error}")
                continue
            dependencies, path_violations = dependency_paths(source, file_module_path)
            for violation in path_violations:
                line = source.count("\n", 0, violation.offset) + 1
                failures.append(
                    f"{relative}:{line}: {violation.message}; governed architecture surfaces require explicit root paths"
                )
            for dependency in dependencies:
                if dependency.module not in rule.forbidden_modules:
                    continue
                line = source.count("\n", 0, dependency.offset) + 1
                failures.append(
                    f"{relative}:{line}: forbidden dependency `{dependency.display}`; {rule.reason}"
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
