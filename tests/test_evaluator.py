"""End-to-end evaluator tests. Run with: python -m tests.test_evaluator"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from o_lang import (
    EvalContext, OBlob, OBool, OExpr, OFloat, OInt, OList, OMap, ONull, OStr,
    evaluate_document, parse, run,
)


def test_plain_text_evaluates_to_string():
    v = run("just a string")
    assert isinstance(v, OStr)
    assert v.value == "just a string"


def test_python_arithmetic():
    v = run("python^(2 + 2)_python")
    assert isinstance(v, OInt)
    assert v.value == 4


def test_python_last_expression_wins():
    v = run("python^(x = 10; y = 20; x + y)_python")
    assert isinstance(v, OInt)
    assert v.value == 30


def test_python_no_value_with_stdout_returns_ostr():
    v = run("python^(print('hi there'))_python")
    assert isinstance(v, OStr)
    assert v.value == "hi there"


def test_python_no_value_no_stdout_returns_onull():
    v = run("python^(x = 1)_python")
    assert isinstance(v, ONull)


def test_python_list_to_olist():
    v = run("python^([1, 2, 3])_python")
    assert isinstance(v, OList)
    assert [x.value for x in v.items] == [1, 2, 3]


def test_python_dict_to_omap():
    v = run('python^({"a": 1, "b": 2})_python')
    assert isinstance(v, OMap)
    assert dict(((k, x.value) for k, x in v.pairs)) == {"a": 1, "b": 2}


def test_html_embeds_python_number():
    v = run("html^(<p>python^(3 + 4)_python</p>)_html")
    assert isinstance(v, OStr)
    assert v.value == "<p>7</p>"


def test_html_embeds_python_list_as_ul():
    v = run("html^(python^([1, 2, 3])_python)_html")
    assert v.value == "<ul><li>1</li><li>2</li><li>3</li></ul>"


def test_markdown_embeds_python_inline():
    v = run("markdown^(Answer: python^(6 * 7)_python)_markdown")
    assert v.value == "Answer: 42"


def test_env_0_persists_across_python_blocks():
    src = (
        "html^("
        "python[0]^(x = 100)_python[0]"
        "python[0]^(x + 1)_python[0]"
        ")_html"
    )
    v = run(src)
    assert "101" in v.value


def test_env_0_and_env_1_are_isolated():
    src = (
        "html^("
        "python[0]^(shared = 'zero')_python[0]"
        "python[1]^(shared = 'one')_python[1]"
        "python[0]^(shared)_python[0]"
        ")_html"
    )
    v = run(src)
    # python[0]'s 'shared' should still be 'zero' after python[1] set its own.
    assert "zero" in v.value
    assert "one" not in v.value


def test_python_matplotlib_lifts_to_oblob():
    # Skip gracefully if matplotlib isn't installed.
    try:
        import matplotlib  # noqa: F401
    except ImportError:
        print("  (skipping matplotlib test -- not installed)")
        return
    src = """python^(
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
fig, ax = plt.subplots()
ax.plot([1,2,3], [1,4,9])
fig
)_python"""
    v = run(src)
    assert isinstance(v, OBlob)
    assert v.mime == "image/png"
    assert v.data[:8] == b"\x89PNG\r\n\x1a\n"


def test_html_renders_png_blob_as_data_uri():
    src = """html^(python^(
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
fig, _ = plt.subplots()
fig
)_python)_html"""
    try:
        import matplotlib  # noqa: F401
    except ImportError:
        print("  (skipping matplotlib HTML test -- not installed)")
        return
    v = run(src)
    assert "<img src=\"data:image/png;base64," in v.value


def test_backslash_escaped_closer_is_literal_in_python():
    v = run(r"python^(s = '\)_python'; s)_python")
    assert isinstance(v, OStr)
    assert v.value == ")_python"


def test_example_files_parse_and_eval():
    root = Path(__file__).resolve().parents[1] / "examples"
    for p in sorted(root.glob("*.O")):
        src = p.read_text(encoding="utf-8")
        v = run(src)
        assert v is not None, f"example {p.name} returned None"


# ---------------------------------------------------------------------------
# O^ sequencing backend
# ---------------------------------------------------------------------------

def test_o_root_single_child_returns_child_value():
    v = run("O^(python^(2 + 2)_python)_O")
    assert isinstance(v, OInt) and v.value == 4


def test_o_root_multiple_children_returns_olist():
    v = run("O^(python^(1)_python python^(2)_python python^(3)_python)_O")
    assert isinstance(v, OList)
    assert [x.value for x in v.items] == [1, 2, 3]


def test_o_root_whitespace_only_is_onull():
    v = run("O^(   \n   )_O")
    assert isinstance(v, ONull)


def test_o_root_sequences_side_effects_in_order():
    # python[0] env mutations in the first child are visible in the second.
    src = (
        "O^("
        "python[0]^(x = 21)_python[0]"
        "python[0]^(x * 2)_python[0]"
        ")_O"
    )
    v = run(src)
    assert isinstance(v, OInt) and v.value == 42


def test_o_root_preserves_nonwhitespace_text():
    v = run("O^(hello python^(5)_python)_O")
    assert isinstance(v, OList)
    assert isinstance(v.items[0], OStr) and v.items[0].value == "hello "
    assert isinstance(v.items[1], OInt) and v.items[1].value == 5


# ---------------------------------------------------------------------------
# quote^ and O.eval
# ---------------------------------------------------------------------------

def test_quote_returns_oexpr_without_evaluating():
    # A division by zero would crash if evaluated -- quote^ must not.
    v = run("quote^(python^(1/0)_python)_quote")
    assert isinstance(v, OExpr)


def test_o_eval_on_quoted_expression():
    src = (
        "O^("
        "python[0]^(q = quote^(python^(6 * 7)_python)_quote)_python[0]"
        "python[0]^(O.eval(q))_python[0]"
        ")_O"
    )
    v = run(src)
    assert isinstance(v, OInt) and v.value == 42


def test_quote_of_multi_child_body_wraps_in_synthetic_o_node():
    # The body of quote^ contains several ExpressionNodes -> OExpr of an O node.
    src = (
        "python[0]^("
        "q = quote^("
        "python[0]^(x = 10)_python[0]"
        "python[0]^(x * 5)_python[0]"
        ")_quote\n"
        "O.eval(q)"
        ")_python[0]"
    )
    v = run(src)
    assert isinstance(v, OInt) and v.value == 50


def test_o_quote_from_python_source_string():
    # O.quote parses a raw source string (with inner openers backslash-escaped).
    src = (
        "python[0]^(\n"
        "q = O.quote(\"\\python^(111 + 222)_python\")\n"
        "O.eval(q)\n"
        ")_python[0]"
    )
    v = run(src)
    assert isinstance(v, OInt) and v.value == 333


def test_lift_preserves_ovalues_inside_python_lists():
    # Python code returning a list of OValues should keep them intact,
    # not stringify each item.
    src = (
        "O^("
        "python[0]^(variants = [quote^(python^(1)_python)_quote, "
        "quote^(python^(2)_python)_quote, quote^(python^(3)_python)_quote])_python[0]"
        "python[0]^([O.eval(v) for v in variants])_python[0]"
        ")_O"
    )
    v = run(src)
    assert isinstance(v, OList)
    assert [x.value for x in v.items] == [1, 2, 3]


ALL_TESTS = [v for k, v in list(globals().items()) if k.startswith("test_")]


def main():
    failed = []
    for t in ALL_TESTS:
        try:
            t()
            print(f"  PASS  {t.__name__}")
        except Exception as e:
            print(f"  FAIL  {t.__name__}: {type(e).__name__}: {e}")
            import traceback
            traceback.print_exc()
            failed.append(t.__name__)
    if failed:
        print(f"\n{len(failed)} failed: {failed}")
        sys.exit(1)
    print(f"\nAll {len(ALL_TESTS)} evaluator tests passed.")


if __name__ == "__main__":
    main()
