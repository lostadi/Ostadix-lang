"""
Parser for .O source files.

Grammar (informally):

    document    := body_part*
    body_part   := text | expression
    expression  := OPENER body_part* CLOSER
    OPENER      := IDENT ( '[' DIGITS ']' )? '^('
    CLOSER      := ')_' IDENT ( '[' DIGITS ']' )?          (matching IDENT+env)
    IDENT       := [A-Za-z_][A-Za-z0-9_]*   AND   IDENT in registered-languages
    text        := (any char, or \\X escape for literal X in {opener, closer})

Key design decisions:

1. Only IDENTs that are REGISTERED LANGUAGES trigger expression parsing.
   This means '2 ^ (x+1)' in a Python body does NOT accidentally parse as
   a language expression, because '2' is not a registered language tag.

2. Backslash escape is SELECTIVE: '\)_python' and '\python^(' are the
   only forms that consume the backslash. A lone '\n' inside a Python body
   is left alone so Python string escapes keep working.

3. The inner body is only inspected for (a) the matching CLOSER, and
   (b) openings of OTHER typed expressions (for recursive parsing).
   Everything else is opaque to the O parser -- we never peek inside
   the inner language's syntax. This is what makes adding a new language
   a zero-parser-change operation.

4. Environment IDs via [N]. The opener 'python[0]^(...)_python[0]' matches
   strictly: the closer must include the [0] if the opener did. Omitting
   [N] in the opener means "default env 0" BUT the closer must also omit
   it. This makes parsing unambiguous without lookahead across languages.
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
}

# IDENT[N]?^(  -- the opening delimiter
OPEN_RE = re.compile(r"([A-Za-z_][A-Za-z0-9_]*)(?:\[(\d+)\])?\^\(")


# ---------------------------------------------------------------------------
# AST node types
# ---------------------------------------------------------------------------

@dataclass
class TextPart:
    """Raw text inside an expression's body (or at the top level)."""
    text: str


@dataclass
class ExpressionNode:
    """A typed expression: LANG[env]^( ... )_LANG[env]."""
    language: str
    env_id: int                # 0 when not explicitly written
    env_explicit: bool         # was [N] written in the source?
    body: List[Union["TextPart", "ExpressionNode"]] = field(default_factory=list)

    @property
    def closing_tag(self) -> str:
        if self.env_explicit:
            return f")_{self.language}[{self.env_id}]"
        return f")_{self.language}"

    @property
    def env_key(self) -> str:
        """Key used to look up persistent per-language environments."""
        return f"{self.canonical_language}[{self.env_id}]"

    @property
    def canonical_language(self) -> str:
        """Normalize aliases (py -> python, md -> markdown, tex -> latex)."""
        return _canonicalize(self.language)


@dataclass
class Document:
    """Top-level parsed .O file."""
    body: List[Union[TextPart, ExpressionNode]]


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
    body = p.parse_body(end_tag=None)
    if p.pos < len(src):
        raise ParseError(p.pos, "trailing content after document body", src)
    return Document(body=body)


class _ParserState:
    def __init__(self, src: str):
        self.src = src
        self.pos = 0

    def parse_body(self, end_tag: Optional[str]) -> List[Union[TextPart, ExpressionNode]]:
        """Parse text+expressions until end_tag is consumed (or EOF if None)."""
        out: List[Union[TextPart, ExpressionNode]] = []
        text_buf: List[str] = []

        def flush_text() -> None:
            if text_buf:
                out.append(TextPart("".join(text_buf)))
                text_buf.clear()

        while self.pos < len(self.src):
            # 1. Check for our closing tag (must come before opener check so that
            #    close-alike patterns don't get re-parsed).
            if end_tag is not None and self.src.startswith(end_tag, self.pos):
                flush_text()
                self.pos += len(end_tag)
                return out

            c = self.src[self.pos]

            # 2. Selective backslash escape: only eats the backslash when what
            #    follows is an actual opener or our matching closer.
            if c == "\\":
                # escaping the matching close tag?
                if end_tag is not None and self.src.startswith(end_tag, self.pos + 1):
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
                env_id = int(env_str) if env_str is not None else 0
                env_explicit = env_str is not None
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
    if isinstance(node, ExpressionNode):
        header = f"{pad}EXPR {node.language}[{node.env_id}]"
        children = "\n".join(pretty(c, indent + 1) for c in node.body)
        return header + ("\n" + children if children else "")
    return f"{pad}?? {node!r}"
