import re
from dataclasses import dataclass, field
from typing import Optional, Union

# ── AST Nodes ──────────────────────────────────────────────────────────────────

@dataclass
class RawText:
    """Opaque inner-language content — passed verbatim to the backend."""
    text: str

@dataclass
class VarRef:
    """$name — splice the value of a previously bound variable."""
    name: str

@dataclass
class TypedExpr:
    """lang[n]^( body )_lang[n] — the core O construct."""
    lang: str
    env_id: Optional[int]          # None = ephemeral; int = persistent named env
    body: list                     # List[RawText | VarRef | TypedExpr]

ONode = Union[RawText, VarRef, TypedExpr]

# ── Patterns ───────────────────────────────────────────────────────────────────

# Matches: python[0]^(  or  html^(  or  O^(
_OPEN  = re.compile(r'([A-Za-z][A-Za-z0-9_]*)(?:\[(\d+)\])?\^\(')
# Matches: $varname
_VAR   = re.compile(r'\$([A-Za-z_][A-Za-z0-9_]*)')

# ── Parser ─────────────────────────────────────────────────────────────────────

class OParser:
    def __init__(self, source: str):
        self.src   = source
        self.pos   = 0
        self.line  = 1           # for error messages
    
    def parse(self) -> TypedExpr:
        """
        A .O file is implicitly an O^(...)_O expression.
        At the top level we don't require a closing tag — EOF closes it.
        """
        body = self._parse_body(enclosing_lang='O', enclosing_env=None, top_level=True)
        return TypedExpr(lang='O', env_id=None, body=body)
    
    def _close_tag(self, lang: str, env_id: Optional[int]) -> str:
        suffix = f'[{env_id}]' if env_id is not None else ''
        return f')_{lang}{suffix}'
    
    def _parse_body(self, enclosing_lang: str, enclosing_env: Optional[int],
                    top_level: bool = False) -> list[ONode]:
        close = self._close_tag(enclosing_lang, enclosing_env)
        nodes  = []
        start  = self.pos          # beginning of current raw-text accumulation

        while self.pos < len(self.src):
            rest = self.src[self.pos:]

            # ── 1. Check for closing tag ───────────────────────────────────────
            if rest.startswith(close):
                self._flush_raw(nodes, start)
                self.pos += len(close)
                return nodes

            # ── 2. Check for nested typed expression opening ───────────────────
            m = _OPEN.match(rest)
            if m:
                self._flush_raw(nodes, start)
                lang   = m.group(1)
                env_id = int(m.group(2)) if m.group(2) is not None else None
                self.pos += m.end()
                nested = self._parse_body(lang, env_id)
                nodes.append(TypedExpr(lang=lang, env_id=env_id, body=nested))
                start = self.pos
                continue

            # ── 3. Check for variable reference ────────────────────────────────
            m = _VAR.match(rest)
            if m:
                self._flush_raw(nodes, start)
                nodes.append(VarRef(m.group(1)))
                self.pos += m.end()
                start = self.pos
                continue

            # ── 4. Track newlines for error reporting, advance ─────────────────
            if self.src[self.pos] == '\n':
                self.line += 1
            self.pos += 1

        # EOF
        if top_level:
            self._flush_raw(nodes, start)
            return nodes
        raise SyntaxError(
            f"Line {self.line}: Unclosed expression — expected '{close}'"
        )
    
    def _flush_raw(self, nodes: list, start: int):
        text = self.src[start:self.pos]
        if text:
            nodes.append(RawText(text))
The important thing to notice: the parser never inspects the content of inner expressions. Between python^( and _python, every character is accumulated as RawText UNLESS it matches an O-level pattern (another typed expression or a $var reference). This is the right design — it means the parser is O(n) in source length and adding a new language requires zero parser changes.

The one vulnerability is the delimiter collision problem I mentioned before — what if Python code contains the string _python? The clean fix is an escape: \_python inside inner code is not treated as a closing tag. You add one line to step 1:

python
if rest.startswith('\\' + close):   # escaped closing tag
    self.pos += 1 + len(close)      # consume the backslash + tag as raw text
    continue
Part 2: The OValue Universal IR
This is your L* in runtime form — the mandatory serialization fiber that all inter-language data must pass through. Every backend must implement to_oval and from_oval.

python
