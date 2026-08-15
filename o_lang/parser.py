"""
Parser for .O source files.

Grammar (informally):

    document    := body_part*
    body_part   := text | expression
    expression  := OPENER body_part* CLOSER
    OPENER      := IDENT ( '[' (DIGITS | '*') ']' )? '^('
    CLOSER      := ')_' IDENT ( '[' (DIGITS | '*') ']' )?  (matching IDENT+env)
    IDENT       := [A-Za-z_][A-Za-z0-9_]*   AND   IDENT in registered-languages
    text        := (any char, or \\X escape for literal X in {opener, closer})

Key design decisions:

1. Only IDENTs that are REGISTERED LANGUAGES trigger expression parsing.
   This means '2 ^ (x+1)' in a Python body does NOT accidentally parse as
   a language expression, because '2' is not a registered language tag.

2. Backslash escape is SELECTIVE: '\\)_python' and '\\python^(' are the
   only forms that consume the backslash. A lone '\\n' inside a Python body
   is left alone so Python string escapes keep working.

3. The inner body is only inspected for (a) the matching CLOSER, and
   (b) openings of OTHER typed expressions (for recursive parsing).
   Everything else is opaque to the O parser -- we never peek inside
   the inner language's syntax. This is what makes adding a new language
   a zero-parser-change operation.

4. Numeric environment IDs via [N] are persistent. Bare blocks and explicit
   [*] blocks are fresh per evaluation attempt, with distinct reserved AST
   encodings so their source spelling round-trips. Openers and closers match
   strictly, including the environment marker.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import List, Optional, Union


# Registered language tags. Tags not on this list are treated as literal text.
# (Add a backend to O-lang/backends/__init__.py and add its tag here.)
REGISTERED_LANGUAGES = {
    "python", "py",
    "markdown", "md",
    "html",
    "latex", "tex",
    "text", "plain",
    "O", "o",
    # quote^(...)_quote captures its body as an unevaluated AST (OExpr),
    # mirroring Lisp's quote. The companion operator is `O.eval(expr)`
    # available inside Python blocks, which re-evaluates an OExpr.
    "quote",
    # Nix family: typed evaluator, store-path realizer, OS test runner.
    "nix",
    "nix_store",
    "nixos_test",
}

# Environment encodings shared with the Rust and C17 editions. Numeric source
# IDs may not claim either reserved fresh-environment sentinel.
EPHEMERAL_ENV_ID = (1 << 32) - 1
LINKER_ISOLATED_ENV_ID = EPHEMERAL_ENV_ID - 1
MAX_PERSISTENT_ENV_ID = EPHEMERAL_ENV_ID - 2

# IDENT[N]?^( or IDENT[*]^( -- the opening delimiter
OPEN_RE = re.compile(r"([A-Za-z_][A-Za-z0-9_]*)(?:\[(\d+|\*)\])?\^\(")

# let NAME = LANG[N]?^(  -- top-level let binding
LET_RE = re.compile(r"let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*")

# $NAME -- variable reference (ASCII ident)
VAR_RE = re.compile(r"\$([A-Za-z_][A-Za-z0-9_]*)")


# ---------------------------------------------------------------------------
# AST node types
# ---------------------------------------------------------------------------

@dataclass
class TextPart:
    """Raw text inside an expression's body (or at the top level)."""
    text: str


@dataclass
class VarRef:
    """A ``$NAME`` variable reference that substitutes a prior let-binding."""
    name: str


@dataclass
class LetBinding:
    """A top-level ``let NAME = LANG^(...)_LANG`` binding.

    After evaluation, ``name`` is bound in the document scope and available
    via ``$name`` VarRef nodes in subsequent expression bodies.
    """
    name: str
    expr: "ExpressionNode"


@dataclass
class ExpressionNode:
    """A typed expression: LANG[env]^( ... )_LANG[env]."""
    language: str
    env_id: int                # reserved sentinel for bare and [*] blocks
    env_explicit: bool         # was [N] or [*] written in the source?
    body: List[Union["TextPart", "ExpressionNode", "VarRef"]] = field(default_factory=list)

    @property
    def environment_marker(self) -> str:
        if not self.env_explicit:
            return ""
        if self.env_id == LINKER_ISOLATED_ENV_ID:
            return "[*]"
        return f"[{self.env_id}]"

    @property
    def opening_tag(self) -> str:
        return f"{self.language}{self.environment_marker}^("

    @property
    def closing_tag(self) -> str:
        return f")_{self.language}{self.environment_marker}"

    @property
    def is_fresh_environment(self) -> bool:
        return self.env_id in (EPHEMERAL_ENV_ID, LINKER_ISOLATED_ENV_ID)

    @property
    def env_key(self) -> str:
        """Key used to look up persistent per-language environments."""
        return f"{self.canonical_language}{self.environment_marker or '[fresh]'}"

    @property
    def canonical_language(self) -> str:
        """Normalize aliases (py -> python, md -> markdown, tex -> latex)."""
        return _canonicalize(self.language)


@dataclass
class Document:
    """Top-level parsed .O file."""
    body: List[Union[TextPart, ExpressionNode, LetBinding]]


# ---------------------------------------------------------------------------
# Language tag canonicalization
# ---------------------------------------------------------------------------

_ALIASES = {
    "py": "python",
    "md": "markdown",
    "tex": "latex",
    "plain": "text",
    "o": "O",
}


def _canonicalize(lang: str) -> str:
    return _ALIASES.get(lang, lang)


def _decode_environment(marker: Optional[str], pos: int, src: str) -> tuple[int, bool]:
    if marker is None:
        return EPHEMERAL_ENV_ID, False
    if marker == "*":
        return LINKER_ISOLATED_ENV_ID, True
    env_id = int(marker)
    if env_id > MAX_PERSISTENT_ENV_ID:
        raise ParseError(
            pos,
            "persistent environment id is reserved or out of range "
            f"(maximum {MAX_PERSISTENT_ENV_ID})",
            src,
        )
    return env_id, True


# ---------------------------------------------------------------------------
# Parser
# ---------------------------------------------------------------------------

class ParseError(Exception):
    def __init__(self, pos: int, msg: str, src: str = ""):
        snippet = ""
        if src:
            line = src[:pos].count("\n") + 1
            col = pos - (src.rfind("\n", 0, pos) + 1) + 1 if pos else 1
            ctx = src[max(0, pos - 20):pos + 20]
            snippet = f" (line {line}, col {col}, near {ctx!r})"
        super().__init__(f"Parse error at {pos}: {msg}{snippet}")
        self.pos = pos


def parse(src: str) -> Document:
    """Parse a complete .O source string into a Document AST."""
    p = _ParserState(src)
    body = p.parse_body(end_tag=None, top_level=True)
    if p.pos < len(src):
        raise ParseError(p.pos, "trailing content after document body", src)
    return Document(body=body)


class _ParserState:
    def __init__(self, src: str):
        self.src = src
        self.pos = 0

    def parse_body(
        self,
        end_tag: Optional[str],
        top_level: bool = False,
    ) -> List[Union[TextPart, ExpressionNode, LetBinding, VarRef]]:
        """Parse text+expressions until end_tag is consumed (or EOF if None).

        When ``top_level`` is True, ``let NAME = LANG^(...)`` patterns are
        parsed as :class:`LetBinding` nodes.  At all levels, ``$NAME`` tokens
        that match a known word boundary are emitted as :class:`VarRef` nodes
        so the evaluator can substitute them with scoped values.
        """
        out: List[Union[TextPart, ExpressionNode, LetBinding, VarRef]] = []
        text_buf: List[str] = []

        def flush_text() -> None:
            if text_buf:
                out.append(TextPart("".join(text_buf)))
                text_buf.clear()

        while self.pos < len(self.src):
            # 1. Check for our closing tag (must come before opener check so that
            #    close-alike patterns don't get re-parsed).
            if end_tag is not None and self._matches_end_tag(end_tag, self.pos):
                flush_text()
                self.pos += len(end_tag)
                return out

            c = self.src[self.pos]

            # 2. Selective backslash escape: only eats the backslash when what
            #    follows is an actual opener or our matching closer.
            if c == "\\":
                # escaping the matching close tag?
                if end_tag is not None and self._matches_end_tag(end_tag, self.pos + 1):
                    text_buf.append(end_tag)
                    self.pos += 1 + len(end_tag)
                    continue
                # escaping a registered opener?
                m = OPEN_RE.match(self.src, self.pos + 1)
                if m and m.group(1) in REGISTERED_LANGUAGES:
                    text_buf.append(self.src[self.pos + 1:m.end()])
                    self.pos = m.end()
                    continue
                # Not escaping anything structural -- keep the backslash as-is.
                text_buf.append(c)
                self.pos += 1
                continue

            # 2b. Top-level let binding: `let NAME = LANG^(...)`
            if top_level and self.src.startswith("let ", self.pos):
                let_m = LET_RE.match(self.src, self.pos)
                if let_m:
                    # Peek ahead for a typed-expression opener.
                    rest_pos = let_m.end()
                    prev = self.src[rest_pos - 1] if rest_pos > 0 else ""
                    prev_is_word_here = prev.isalnum() or prev == "_"
                    open_m = (
                        None
                        if prev_is_word_here
                        else OPEN_RE.match(self.src, rest_pos)
                    )
                    if open_m and open_m.group(1) in REGISTERED_LANGUAGES:
                        flush_text()
                        binding_name = let_m.group(1)
                        lang = open_m.group(1)
                        env_str = open_m.group(2)
                        env_id, env_explicit = _decode_environment(
                            env_str, rest_pos, self.src
                        )
                        self.pos = open_m.end()
                        expr_node = ExpressionNode(
                            language=lang,
                            env_id=env_id,
                            env_explicit=env_explicit,
                            body=[],
                        )
                        expr_node.body = self.parse_body(end_tag=expr_node.closing_tag)
                        out.append(LetBinding(name=binding_name, expr=expr_node))
                        continue

            # 2c. $NAME variable reference.
            if c == "$":
                var_m = VAR_RE.match(self.src, self.pos)
                if var_m:
                    flush_text()
                    out.append(VarRef(name=var_m.group(1)))
                    self.pos = var_m.end()
                    continue

            # 3. Look for a typed expression opener. But ONLY at a word
            #    boundary -- otherwise `foo^(` would match `o^(` starting
            #    at position 2, because `o` is a registered language alias.
            prev_is_word = (
                self.pos > 0
                and (self.src[self.pos - 1].isalnum() or self.src[self.pos - 1] == "_")
            )
            m = None if prev_is_word else OPEN_RE.match(self.src, self.pos)
            if m and m.group(1) in REGISTERED_LANGUAGES:
                flush_text()
                lang = m.group(1)
                env_str = m.group(2)
                env_id, env_explicit = _decode_environment(
                    env_str, self.pos, self.src
                )
                self.pos = m.end()

                node = ExpressionNode(
                    language=lang,
                    env_id=env_id,
                    env_explicit=env_explicit,
                    body=[],
                )
                node.body = self.parse_body(end_tag=node.closing_tag)
                out.append(node)
                continue

            # 4. Otherwise plain text.
            text_buf.append(c)
            self.pos += 1

        # End-of-input handling.
        if end_tag is not None:
            raise ParseError(
                self.pos,
                f"unterminated expression, expected closing tag {end_tag!r}",
                self.src,
            )
        flush_text()
        return out

    def _matches_end_tag(self, end_tag: str, pos: int) -> bool:
        if not self.src.startswith(end_tag, pos):
            return False
        after = pos + len(end_tag)
        # Do not let a bare closer consume the prefix of `)_lang[N]` or
        # `)_lang[*]`; the source marker is part of the delimiter identity.
        return after >= len(self.src) or self.src[after] != "["


# ---------------------------------------------------------------------------
# Debug pretty-printer
# ---------------------------------------------------------------------------

def pretty(node, indent: int = 0) -> str:
    pad = "  " * indent
    if isinstance(node, Document):
        return "\n".join(pretty(child, indent) for child in node.body)
    if isinstance(node, TextPart):
        t = node.text.replace("\n", "\\n")
        if len(t) > 60:
            t = t[:60] + "..."
        return f"{pad}TEXT {t!r}"
    if isinstance(node, VarRef):
        return f"{pad}VAR ${node.name}"
    if isinstance(node, LetBinding):
        return f"{pad}LET {node.name} = {pretty(node.expr, indent)}"
    if isinstance(node, ExpressionNode):
        marker = node.environment_marker or "[fresh]"
        header = f"{pad}EXPR {node.language}{marker}"
        children = "\n".join(pretty(c, indent + 1) for c in node.body)
        return header + ("\n" + children if children else "")
    return f"{pad}?? {node!r}"


def reconstruct_source(node) -> str:
    """Reconstruct parsed O source while preserving each environment spelling."""
    if isinstance(node, Document):
        return "".join(reconstruct_source(child) for child in node.body)
    if isinstance(node, TextPart):
        return node.text
    if isinstance(node, VarRef):
        return f"${node.name}"
    if isinstance(node, LetBinding):
        return f"let {node.name} = {reconstruct_source(node.expr)}"
    if isinstance(node, ExpressionNode):
        body = "".join(reconstruct_source(child) for child in node.body)
        return f"{node.opening_tag}{body}{node.closing_tag}"
    raise TypeError(f"Unknown AST node: {node!r}")
