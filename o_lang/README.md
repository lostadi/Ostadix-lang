# o_lang (Legacy Python Implementation)

This directory contains the **original Python implementation** of the O language.
It served as the prototype and reference for the current **Rust edition** in `src/`.

## Status: Reference Only

The active, maintained implementation is the Rust crate (`src/`). This Python
code is preserved for:

- **Historical reference** — understanding the original design decisions
- **Algorithm comparison** — verifying the Rust port against the prototype
- **Backend testing** — the Python evaluator can still run `.O` files independently

## Structure

| File            | Purpose                                    |
|-----------------|--------------------------------------------|
| `__main__.py`   | CLI entry point (`python -m o_lang`)       |
| `cli.py`        | Argument parsing and runner                |
| `parser.py`     | Tokenizer and expression parser            |
| `evaluator.py`  | Recursive evaluator with backend dispatch  |
| `ovalue.py`     | OValue type system (Python dataclasses)    |
| `backends/`     | Language-specific shim scripts             |

## Running (if needed)

```bash
python -m o_lang examples/hello.O
```

> **Note:** For production use, prefer the Rust edition:
> ```bash
> cargo run -- examples/hello.O backends
> ```
