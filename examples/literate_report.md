
# A Literate Report in .O

_This entire file is a single Markdown expression with embedded Python.
There is no "code cell vs. prose cell" distinction — there is one expression
tree whose leaves are text and typed sub-expressions._

## A small statistical experiment

We draw samples from a normal distribution and compute some summary statistics.
All of the numbers below are computed _at render time_ by Python code living
in the middle of this sentence:



- **Sample mean**: 0.0412
- **Sample stddev**: 0.9988
- **Min / max**: [-2.62, 3.33]

Because `python[0]` is a _persistent_ environment, the `samples` list defined
in the first block is still in scope in every subsequent `python[0]` block in
this document. State flows naturally along the reading order of the text.

## What about a different language?

Here is some raw HTML embedded inline:

<span style="color: crimson; font-weight: bold">I am HTML inside Markdown inside O.</span>

And a small computation combining multiple languages — Python produces the
number, HTML wraps it for styling, Markdown links it into the prose:

The answer is <strong style="color: teal">285</strong>.

## The collapse

What you just read _is_ the program that produced it. The document's layout
and its computational content are the same S-expression tree. Running it
produces this Markdown. Running it with `--as html` would produce HTML.
Running it with `--as json` would dump the raw OValue tree. The source is
neutral to the output format — the invocation chooses.

