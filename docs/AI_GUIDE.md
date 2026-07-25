# Ostadix-lang for AI Agents

This guide is written for AI coding agents (LLMs, autonomous agents, and
tooling that generates or executes code) that want to use Ostadix-lang as a
primary language for solving tasks. It is intentionally concise, exact, and
machine-oriented. The authoritative semantics live in [SPEC.md](../SPEC.md).

## Why O is a good agent language

- **One program, many runtimes.** Each `LANG^( ... )_LANG` block is executed by
  that language's real backend (CPython, Bash, SQLite, rustc, Node.js, ...).
  An agent can pick the best tool per subtask without glue scripts.
- **Deterministic value boundary.** Every block returns an OValue, a typed,
  JSON-serializable canonical value. No stringly-typed parsing between steps.
- **Machine-readable CLI.** `--json`, `--check`, and `--eval` give agents
  structured results, structured errors, and a no-execution validation path.
- **Homoiconicity.** `quote^` captures code as data; `O.eval` evaluates it,
  so agents can generate, inspect, and run O programs from within O.

## The agent workflow

1. **Generate** a `.O` program (or an inline expression).
2. **Validate** it without executing: `O --check --json program.O`.
   - Success: exit 0, stdout `{"ok":true,"stage":"parse",...}`.
   - Failure: exit 1, stdout `{"ok":false,"stage":"parse","error":"Line N: ..."}`.
   - Error messages include line numbers; fix and re-check.
3. **Run** it with structured output: `O --json program.O`.
   - Success: exit 0, stdout `{"ok":true,"value":<OValue>,"type":"<type>","elapsed_ms":N}`.
   - Failure: exit 1, stdout `{"ok":false,"stage":"eval","error":"..."}`
     (a human-readable copy also goes to stderr).
4. For quick one-shot computations, skip the file: `O --json -e '<source>'`.

The JSON object is always a single line on stdout, so it can be parsed with
any JSON parser without scraping logs. Program-level prints from blocks (for
example Bash stdout) become the block's *value*, not loose terminal noise.

## Core syntax rules (the parts agents get wrong)

1. **Opener and closer must match exactly.**
   `python^( ... )_python`, `python[0]^( ... )_python[0]`,
   `bash^( ... )_bash`. A mismatched or missing closer is a parse error.
2. **Python blocks return via `__oval_result__`.**
   ```O
   python^(
   __oval_result__ = sum(x*x for x in range(10))
   )_python
   ```
3. **`$name` is an O binding reference everywhere**, including inside Python
   and Bash bodies. Declare with `let`:
   ```O
   let answer = python^(
   __oval_result__ = 40 + 2
   )_python

   bash^(
   echo "the answer is $answer"
   )_bash
   ```
   To pass a literal `$` to the target language (for example shell `$PATH`),
   escape it: `\$PATH`.
4. **Persistent environments are explicit.** `python[0]^` blocks share one
   long-lived interpreter (imports and variables survive across blocks);
   bare `python^` blocks each get a fresh process.
5. **Only registered language tags open a block.** `2 ^ (x+1)` inside a
   Python body is ordinary Python, not an O expression. See SPEC.md section 6
   for the full tag table (`python`/`py`, `bash`, `shell`, `sql`, `rust`,
   `javascript`, `java`, `cpp`, `csharp`, `haskell`, `ruby`, `ocaml`, `racket`,
   `lisp`, `common_lisp`, `matlab`, `mathematica`, `webassembly`, `nix`,
   `html`, `markdown`/`md`, `latex`/`tex`, `text`/`plain`, `O`, `quote`).
6. **Sequencing:** wrap multiple steps in `O^( ... )_O`; it evaluates children
   in order and returns the last non-null value.
7. **Escapes:** `\LANG^(` and `\)_LANG` produce literal text instead of
   opening/closing an expression.

## Recipes

Compute in Python, format in Markdown:

```O
let total = python^(
__oval_result__ = 6 * 7
)_python

markdown^(The **total** is $total.)_markdown
```

Persistent SQL session across blocks:

```O
O^(
sql[0]^(CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1),(2),(3);)_sql[0]
sql[0]^(SELECT SUM(x) FROM t;)_sql[0]
)_O
```

Generate code as data, then evaluate it (use a persistent `[n]` Python
environment for `O.eval`):

```O
let q = quote^(python^(2 + 2)_python)_quote

python[0]^(
__oval_result__ = O.eval($q)
)_python[0]
```

## CLI reference for agents

| Command | Purpose | Exit code |
|---|---|---|
| `O program.O` | Run; human-readable result on stdout | 0 / non-zero |
| `O --json program.O` | Run; one-line JSON result or error on stdout | 0 / 1 |
| `O --check program.O` | Parse-only validation; prints `ok` | 0 / non-zero |
| `O --json --check program.O` | Parse-only validation with JSON verdict | 0 / 1 |
| `O --eval '<src>'` / `O -e '<src>'` | Evaluate inline source (combinable with `--json`) | 0 / 1 |
| `O --executor serial\|graph program.O` | Select execution engine (default: graph) | 0 / non-zero |

Flags may be combined and must precede the input file or inline source.

## OValue in JSON output

`--json` serializes the result using the OValue wire format, a tagged object
under the `"value"` key, e.g. `{"t":"number","v":{"kind":"int","v":"4"}}`,
`{"t":"text","v":{...}}`, `{"t":"bool","v":true}`, or `null`. The top-level
`"type"` field carries the short type name (`number`, `text`, `list`, `map`,
`html`, ...). See SPEC.md section 3 for the complete OValue catalogue.

## Failure handling guidance

- Treat `"stage":"parse"` errors as syntax bugs in the generated program:
  the message includes a line number; regenerate or patch and re-run
  `--check` before executing.
- Treat `"stage":"eval"` errors as runtime failures (backend missing, target
  language exception, non-zero exit). The error string embeds the backend's
  diagnostic output.
- Missing toolchains: hosted backends shell out to real interpreters and
  compilers. If `rust^` fails because `rustc` is absent, either install the
  toolchain or solve the subtask in an available language such as `python^`
  or `bash^`.
