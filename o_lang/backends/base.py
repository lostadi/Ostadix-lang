"""
Backend protocol.

A Backend is the language-specific plug-in that tells the O runtime:

  1. How to start a fresh persistent environment for [env_id]
  2. How to render a child expression's OValue as a string in MY language
  3. How to evaluate MY language's source code (with children already spliced
     in) into an OValue

The default evaluation flow (splice children then evaluate body string)
suffices for most languages. For structural backends that need control over
child evaluation -- `O` (sequencing) and `quote` (capture the AST without
evaluating) being the motivating examples -- a backend can OPTIONALLY
implement `eval_ast(node, ctx)`. The evaluator checks for this method and
calls it if present, otherwise falls back to the splice-then-evaluate flow.

Every new language you want to embed in .O implements this protocol.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Protocol, runtime_checkable

from ..ovalue import OValue

if TYPE_CHECKING:
    # Imported only for type hints; avoid a circular import at runtime.
    from ..evaluator import EvalContext
    from ..parser import ExpressionNode


@runtime_checkable
class Backend(Protocol):
    """Interface that every language backend must implement."""

    #: Canonical language name ("python", "html", ...). Alias lookup is
    #: handled by the parser; by the time we hit the backend the name is
    #: already canonical.
    name: str

    def make_env(self) -> Any:
        """Create a fresh persistent environment for this language.

        Called once per unique (language, env_id) pair that the program
        references. Subsequent expressions in the same env share this state.
        For Python this is a globals dict; for Markdown/HTML it can be
        anything (or None) since those backends are stateless.
        """
        ...

    def render_child(self, child_value: OValue) -> str:
        """Render a child expression's OValue as a string INSIDE my language.

        Example: the HTML backend's render_child(OBlob(png_bytes, 'image/png'))
        returns a <img src="data:image/png;base64,..."> tag. Python's
        render_child on the same value returns the repr of the bytes object.
        """
        ...

    def evaluate(self, body: str, env: Any, ctx: "EvalContext" = None) -> OValue:
        """Evaluate my language's source code and return an OValue.

        By the time we get here, all child expressions have already been
        recursively evaluated and their values have been rendered into
        `body` as strings via render_child. So `body` is a pure
        my-language source string.

        The ctx parameter provides access to the EvalContext (envs, backend
        registry) for backends that need to evaluate OExpr values at runtime
        (e.g., Python's O.eval helper).
        """
        ...

    # ---- Optional structural hook ---------------------------------------
    # Backends that implement eval_ast take FULL CONTROL of their children:
    # the default splice-then-evaluate flow is skipped entirely. This is how
    # O^(...)_O sequences children and how quote^(...)_quote captures its
    # body without evaluating. If eval_ast is not defined, the default flow
    # is used.
    #
    #   def eval_ast(self, node: ExpressionNode, ctx: EvalContext) -> OValue:
    #       ...
