# O-lang Developer Guide

*Build, test, architecture, and conventions for working on O-lang.*

O-lang is a polyglot meta-language where every expression carries its own evaluator tag: `LANG^( ... )_LANG`. The repo contains two implementations: a **Rust runtime** (`src/`) that is the primary binary, and a **Python reference implementation** (`o_lang/`) used by the Python test suite.

---

## Build, Test, and Run

```bash
# Build and run a .O file (shim_dir defaults to backends/)
cargo run -- examples/hello.O
cargo run -- examples/hello.O backends

# Build only
cargo build

# Rust unit tests (inline #[cfg(test)] in src/value.rs)
cargo test
cargo test round_trip_all_variants   # run a single test by name

# Python reference impl tests (tests against o_lang/)
python -m tests.test_parser
python -m tests.test_evaluator

# Integration smoke tests (runs cargo + greps output)
./test_o_lang_examples.sh

# Python reference impl CLI
python -m o_lang examples/hello.O
python -m o_lang examples/hello.O --dump-ast
python -m o_lang examples/hello.O --as json
```

---

## Architecture

### Rust runtime (`src/`)

```
src/
├── value.rs    — OValue sum type + wire protocol (OWireCommand / OWireResponse). Pure data layer; no deps on parser or eval.
├── parser.rs   — Typed-paren parser → ONode tree (RawText | VarRef | LetBinding | TypedExpr)
├── eval.rs     — Applicative-order leaves-up evaluator + render_child dispatch per language
├── process.rs  — ProcessRegistry: spawns and keeps alive one subprocess per (lang, env_id) key
└── main.rs     — CLI entry; hardcodes registered_backends HashSet; dispatches Parser → Evaluator
```

### Backend shims (`backends/`)

Each language is a separate subprocess (Python script or executable). The Rust runtime communicates with shims via **newline-delimited JSON IPC**:

- Runtime → shim: `OWireCommand` (`exec` / `cleanup` / `ping`)
- Shim → runtime: `OWireResponse` (`{"status":"ok","value":{...}}` or `{"status":"err","message":"..."}`)

Shims are resolved by name: `{lang}_shim.py` → `{lang}_shim` → `{lang}.py` → `{lang}`, all under `shim_dir`.

**Exception:** `html` is handled entirely inline in `eval.rs`—no subprocess is spawned.

### Python reference implementation (`o_lang/`)

Five-file structure: `ovalue.py` (OValue types), `parser.py` (typed-paren parser), `evaluator.py` (leaves-up evaluator), `cli.py` (entry point), `backends/` (Python class implementations of each language backend).

---

## Key Conventions

### OValue wire format

Every value crossing a language boundary is an `OValue`. JSON encoding uses a `t` discriminant:

```json
{"t":"null"}
{"t":"bool","v":true}
{"t":"int","v":42}
{"t":"float","v":3.14}
{"t":"str","v":"hello"}
{"t":"html","v":"<p>...</p>"}
{"t":"store_path","path":"/nix/store/..."}
{"t":"list","v":[...]}
{"t":"map","v":{...}}
{"t":"blob","v":"<base64>","mime":"image/png"}
```

`OValue::Html` and `OValue::StorePath` are Rust-edition extensions—not present in the Python MVP's `OValue`.

### Environment IDs

- `lang[n]^(...)_lang[n]` — persistent env keyed by `(lang, n)`; survives across all expressions in the document referencing the same `[n]`
- `lang^(...)_lang` — shorthand for env index `0`; opener/closer must match textually
- `env_id == u32::MAX` — ephemeral: env is torn down after each expression

### Python shim result resolution

In `python_shim.py`, if `__oval_result__` is set in the env dict after `exec()`, that value is returned. Otherwise, captured stdout is returned as an `OStr`.

### Adding a new language (Rust runtime)

1. Write `backends/{lang}_shim.py` implementing the `exec` / `cleanup` / `ping` command loop.
2. Add the tag to `registered_backends` in `src/main.rs`.
3. Add a `render_child` branch in `eval.rs` if the language needs non-default value splicing.

### Parser safety

The parser only treats an identifier as a typed-paren opener if it is in `registered_backends`. Unregistered identifiers followed by `^(` are emitted as raw text—this is what keeps `2 ^ (x+1)` safe inside a Python body.

### `let` bindings

Top-level `let name = LANG^(...)_LANG` binds the result to `$name` for use via `$var` splice in subsequent expressions. `let` bindings are currently only supported at document top level (not inside a `TypedExpr` body).

---

## Tests

- Rust unit tests: inline in `src/value.rs` under `#[cfg(test)]`—cover OValue JSON round-trips and wire protocol correctness.
- Python unit tests: `tests/test_parser.py` and `tests/test_evaluator.py`—cover the Python reference implementation. Run with `python -m tests.test_parser` (not `pytest`).
- Integration tests: `test_o_lang_examples.sh`—runs `cargo run` against `.O` files in `examples/` and greps expected output.
