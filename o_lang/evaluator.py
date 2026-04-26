"""
Evaluator: walks the AST produced by parser.py and evaluates each
ExpressionNode via its language backend, threading a registry of persistent
per-(language, env_id) environments.

Evaluation order: LEAVES UP (applicative order, like standard Lisp).
A node's children are evaluated first; their OValues are then rendered
into the parent's language via the parent backend's render_child(); the
parent backend then runs its evaluate() on the fully-spliced body string.

Persistent environments: every unique (canonical_language, env_id) pair
gets its own env object created exactly once via backend.make_env() and
reused for every expression that references it. This gives you a REPL-
like shell per bracket-labeled env, surviving across evaluations.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Dict, List, Tuple, Union

from .backends import default_registry
from .backends.base import Backend
from .ovalue import OStr, OValue
from .parser import Document, ExpressionNode, TextPart


# (canonical_language, env_id) -> persistent env object
EnvRegistry = Dict[Tuple[str, int], object]


@dataclass
class EvalContext:
    backends: Dict[str, Backend] = field(default_factory=default_registry)
    envs: EnvRegistry = field(default_factory=dict)

    def backend_for(self, canonical_language: str) -> Backend:
        if canonical_language not in self.backends:
            raise KeyError(
                f"No backend registered for language {canonical_language!r}. "
                f"Known: {sorted(self.backends)}"
            )
        return self.backends[canonical_language]

    def env_for(self, canonical_language: str, env_id: int) -> object:
        key = (canonical_language, env_id)
        if key not in self.envs:
            self.envs[key] = self.backend_for(canonical_language).make_env()
        return self.envs[key]


def evaluate_document(doc: Document, ctx: EvalContext = None) -> OValue:
    """Evaluate a parsed .O document and return the root OValue.

    If the document contains a single top-level expression, its OValue is
    the result. If it contains multiple top-level expressions (or mixed
    text/expressions), they are concatenated as a Text root -- we synthesize
    an implicit text[0] wrapper so that every document has a single root.
    """
    ctx = ctx or EvalContext()

    top_body = doc.body

    # Ignore whitespace-only TextParts at the top level so a trailing
    # newline (ubiquitous in source files) doesn't prevent us from
    # treating a single expression as the root.
    meaningful = [
        c for c in top_body
        if not (isinstance(c, TextPart) and not c.text.strip())
    ]

    # If the document is exactly one ExpressionNode (ignoring stray
    # whitespace), that expression IS the root.
    if len(meaningful) == 1 and isinstance(meaningful[0], ExpressionNode):
        return _eval_expression(meaningful[0], ctx)

    # Otherwise synthesize an implicit text root so we always return a single
    # OValue. The text backend renders children using render_plain.
    synthetic_root = ExpressionNode(
        language="text",
        env_id=0,
        env_explicit=False,
        body=top_body,
    )
    return _eval_expression(synthetic_root, ctx)


def _eval_expression(node: ExpressionNode, ctx: EvalContext) -> OValue:
    """Evaluate one ExpressionNode.

    If the backend implements `eval_ast(node, ctx)`, the backend takes
    full control of child evaluation (used by O^, which sequences
    children, and quote^, which captures the AST without evaluating).

    Otherwise we use the default flow:
      1. Evaluate every child ExpressionNode recursively.
      2. Build the final body string by concatenating TextParts verbatim
         with child render_child() results.
      3. Call the backend's evaluate() with the spliced body.
    """
    backend = ctx.backend_for(node.canonical_language)

    eval_ast = getattr(backend, "eval_ast", None)
    if callable(eval_ast):
        return eval_ast(node, ctx)

    env = ctx.env_for(node.canonical_language, node.env_id)

    buf: List[str] = []
    for child in node.body:
        if isinstance(child, TextPart):
            buf.append(child.text)
        elif isinstance(child, ExpressionNode):
            child_value = _eval_expression(child, ctx)
            buf.append(backend.render_child(child_value))
        else:
            raise TypeError(f"Unknown AST child node: {child!r}")

    body_str = "".join(buf)
    # Pass ctx to evaluate when the backend accepts it (Python uses this
    # for O.eval; others don't care).
    try:
        return backend.evaluate(body_str, env, ctx)
    except TypeError:
        # Backward compat with backends that still have 2-arg evaluate.
        return backend.evaluate(body_str, env)


# ---------------------------------------------------------------------------
# Convenience top-level driver
# ---------------------------------------------------------------------------

def run(src: str, ctx: EvalContext = None) -> OValue:
    """Parse + evaluate a .O source string. Returns the root OValue."""
    from .parser import parse
    return evaluate_document(parse(src), ctx)
