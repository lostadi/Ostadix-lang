"""Parser unit tests. Run with: python -m tests.test_parser"""

import sys
from pathlib import Path

# Allow running this file standalone from the O-lang/ project root.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from o_lang.parser import (
    Document, ExpressionNode, TextPart,
    parse, ParseError, REGISTERED_LANGUAGES,
)


def test_plain_text_document():
    doc = parse("hello world")
    assert isinstance(doc, Document)
    assert len(doc.body) == 1
    assert isinstance(doc.body[0], TextPart)
    assert doc.body[0].text == "hello world"


def test_single_expression():
    doc = parse("python^(1 + 1)_python")
    assert len(doc.body) == 1
    node = doc.body[0]
    assert isinstance(node, ExpressionNode)
    assert node.language == "python"
    assert node.env_id == 0
    assert not node.env_explicit
    assert len(node.body) == 1
    assert node.body[0].text == "1 + 1"


def test_explicit_env_expression():
    doc = parse("python[3]^(x)_python[3]")
    node = doc.body[0]
    assert node.env_id == 3
    assert node.env_explicit
    assert node.closing_tag == ")_python[3]"


def test_mismatched_env_bracket_is_unterminated():
    # Opener python[0]^(  requires closer )_python[0], NOT )_python.
    try:
        parse("python[0]^(x)_python")
    except ParseError as e:
        assert "unterminated" in str(e) or "closing" in str(e)
        return
    raise AssertionError("expected ParseError for mismatched closer")


def test_text_wraps_expression():
    doc = parse("prefix html^(<p>x</p>)_html suffix")
    assert len(doc.body) == 3
    assert isinstance(doc.body[0], TextPart)
    assert doc.body[0].text == "prefix "
    assert isinstance(doc.body[1], ExpressionNode)
    assert doc.body[1].language == "html"
    assert isinstance(doc.body[2], TextPart)
    assert doc.body[2].text == " suffix"


def test_different_language_nesting():
    doc = parse("html^(<b>python^(2)_python</b>)_html")
    outer = doc.body[0]
    assert outer.language == "html"
    # HTML body: text "<b>", expr(python), text "</b>"
    assert len(outer.body) == 3
    assert outer.body[0].text == "<b>"
    assert outer.body[1].language == "python"
    assert outer.body[1].body[0].text == "2"
    assert outer.body[2].text == "</b>"


def test_same_language_nesting_with_distinct_envs():
    doc = parse("python[0]^(python[1]^(1)_python[1])_python[0]")
    outer = doc.body[0]
    assert outer.env_id == 0
    assert len(outer.body) == 1
    inner = outer.body[0]
    assert isinstance(inner, ExpressionNode)
    assert inner.language == "python"
    assert inner.env_id == 1


def test_non_registered_ident_is_literal():
    # '2' is not a language; 'foo' is not a language.
    # These must NOT be parsed as expressions.
    doc = parse("foo^(bar) and 2 ^ (x+1)")
    assert len(doc.body) == 1
    assert isinstance(doc.body[0], TextPart)
    assert doc.body[0].text == "foo^(bar) and 2 ^ (x+1)"


def test_backslash_escape_of_closer():
    # \)_python should become literal )_python inside a python body.
    doc = parse(r"python^(x = '\)_python' and 1)_python")
    node = doc.body[0]
    # Body should be "x = ')_python' and 1"
    joined = "".join(c.text for c in node.body if isinstance(c, TextPart))
    assert joined == "x = ')_python' and 1"


def test_backslash_on_nonspecial_is_literal():
    # \n inside a Python body is NOT an escape -- it's literal \n for Python.
    doc = parse(r"python^(print('a\nb'))_python")
    node = doc.body[0]
    assert "\\n" in "".join(c.text for c in node.body if isinstance(c, TextPart))


def test_unterminated_expression_raises():
    try:
        parse("python^(no closing tag here")
    except ParseError as e:
        assert "unterminated" in str(e)
        return
    raise AssertionError("expected ParseError for unterminated expression")


def test_registered_languages_includes_essentials():
    for tag in ("python", "html", "markdown", "latex", "text"):
        assert tag in REGISTERED_LANGUAGES


ALL_TESTS = [v for k, v in list(globals().items()) if k.startswith("test_")]


def main():
    failed = []
    for t in ALL_TESTS:
        try:
            t()
            print(f"  PASS  {t.__name__}")
        except Exception as e:
            print(f"  FAIL  {t.__name__}: {e}")
            failed.append(t.__name__)
    if failed:
        print(f"\n{len(failed)} failed: {failed}")
        sys.exit(1)
    print(f"\nAll {len(ALL_TESTS)} parser tests passed.")


if __name__ == "__main__":
    main()
