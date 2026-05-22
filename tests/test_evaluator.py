"""End-to-end evaluator tests. Run with: python -m tests.test_evaluator"""

import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from o_lang import (
    EvalContext, OBlob, OBool, OExpr, OFloat, OInt, OList, OMap, ONull,
    OStorePath, OStr,
    evaluate_document, parse, run,
)

_NIX_FEATURE_TOKENS = (
    "nix^(",
    "nix_expr^(",
    "nix_store^(",
    "nixos_test^(",
    "instantiate(",
    "realise(",
    "activate(",
    "current_system(",
)

_RUST_ONLY_FEATURE_TOKENS = (
    "run_script(",
    "read_file(",
    "bash^(",
    "sh^(",
    "shell^(",
)


def _nix_available() -> bool:
    return shutil.which("nix") is not None


def _matplotlib_available() -> bool:
    try:
        import matplotlib  # noqa: F401
    except ImportError:
        return False
    return True


def _requires_nix(src: str) -> bool:
    return any(token in src for token in _NIX_FEATURE_TOKENS)


def _requires_rust_only_features(src: str) -> bool:
    return any(token in src for token in _RUST_ONLY_FEATURE_TOKENS)


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


def test_requires_nix_detects_nix_tokens():
    assert _requires_nix("let x = instantiate(nix_expr^(1)_nix_expr)")
    assert not _requires_nix("python^(1 + 1)_python")


def test_requires_rust_only_features_detects_tokens():
    assert _requires_rust_only_features("let r = run_script(\"examples/script_import.py\")")
    assert not _requires_rust_only_features("html^(<p>ok</p>)_html")


def test_example_files_parse_and_eval():
    root = Path(__file__).resolve().parents[1] / "examples"
    nix_available = _nix_available()
    matplotlib_available = _matplotlib_available()
    for p in sorted(root.glob("*.O")):
        src = p.read_text(encoding="utf-8")
        if not nix_available and _requires_nix(src):
            print(f"  (skipping {p.name} -- nix not installed)")
            continue
        if not matplotlib_available and "matplotlib" in src:
            print(f"  (skipping {p.name} -- matplotlib not installed)")
            continue
        if _requires_rust_only_features(src):
            print(f"  (skipping {p.name} -- Rust-only builtins/backends)")
            continue
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


# ---------------------------------------------------------------------------
# Nix backends (Milestones A-D)
# ---------------------------------------------------------------------------

def test_nix_backend_is_registered():
    # The parser must recognise nix^(...)_nix without crashing.
    from o_lang.parser import REGISTERED_LANGUAGES
    assert "nix" in REGISTERED_LANGUAGES
    assert "nix_store" in REGISTERED_LANGUAGES
    assert "nixos_test" in REGISTERED_LANGUAGES


def test_nix_backend_in_default_registry():
    from o_lang.backends import default_registry
    reg = default_registry()
    assert "nix" in reg
    assert "nix_store" in reg
    assert "nixos_test" in reg


def test_ostorepath_in_ovalue_module():
    from o_lang.ovalue import OStorePath
    sp = OStorePath("/nix/store/abc-hello")
    assert sp.path == "/nix/store/abc-hello"
    assert sp.tag == "store_path"
    assert sp.to_json() == {"tag": "store_path", "path": "/nix/store/abc-hello"}


def test_html_renders_ostorepath_as_code_tag():
    # OStorePath spliced into html^() must not render as plain text.
    from o_lang.backends.html_backend import HtmlBackend
    from o_lang.ovalue import OStorePath
    b = HtmlBackend()
    html = b.render_child(OStorePath("/nix/store/abc-hello"))
    assert '<code class="o-store-path">' in html
    assert "/nix/store/abc-hello" in html


def test_nix_render_child_produces_nix_syntax():
    from o_lang.backends.nix_backend import NixBackend
    from o_lang.ovalue import OBool, OInt, OList, OMap, ONull, OStr
    b = NixBackend()
    assert b.render_child(ONull()) == "null"
    assert b.render_child(OBool(True)) == "true"
    assert b.render_child(OBool(False)) == "false"
    assert b.render_child(OInt(42)) == "42"
    assert b.render_child(OStr("hello")) == '"hello"'
    lst = b.render_child(OList((OInt(1), OInt(2))))
    assert lst == "[ 1 2 ]"
    m = b.render_child(OMap((("x", OInt(1)),)))
    assert m == "{ x = 1; }"


def test_nix_eval_integer(skip_msg="  (skipping nix eval test -- nix not installed)"):
    if not _nix_available():
        print(skip_msg)
        return
    v = run("nix^(40 + 2)_nix")
    assert isinstance(v, OInt)
    assert v.value == 42


def test_nix_eval_attrset():
    if not _nix_available():
        print("  (skipping nix attrset test -- nix not installed)")
        return
    v = run('nix^({ answer = 40 + 2; name = "O-lang"; })_nix')
    assert isinstance(v, OMap)
    pairs = dict(v.pairs)
    assert isinstance(pairs["answer"], OInt) and pairs["answer"].value == 42
    assert isinstance(pairs["name"], OStr) and pairs["name"].value == "O-lang"


def test_nix_to_html():
    if not _nix_available():
        print("  (skipping nix->html test -- nix not installed)")
        return
    v = run('html^(<p>nix^(40 + 2)_nix</p>)_html')
    assert isinstance(v, OStr)
    assert "42" in v.value


def test_nix_value_spliced_into_python():
    if not _nix_available():
        print("  (skipping nix->python test -- nix not installed)")
        return
    src = (
        "python^("
        "x = nix^(40 + 2)_nix\n"
        "x * 2"
        ")_python"
    )
    v = run(src)
    assert isinstance(v, OInt)
    assert v.value == 84


def test_nix_store_returns_ostoreppath():
    if not _nix_available():
        print("  (skipping nix_store test -- nix not installed)")
        return
    v = run('nix_store^(builtins.toFile "hello.txt" "hi\n")_nix_store')
    assert isinstance(v, OStorePath)
    assert v.path.startswith("/nix/store/")


def test_nix_store_path_readable_from_python():
    if not _nix_available():
        print("  (skipping nix_store+python test -- nix not installed)")
        return
    src = (
        'python^('
        'p = nix_store^(builtins.toFile "hello.txt" "hi\\n")_nix_store\n'
        'with open(p) as fh: __oval_result__ = fh.read().strip()'
        ')_python'
    )
    v = run(src)
    assert isinstance(v, OStr)
    assert v.value == "hi"


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
