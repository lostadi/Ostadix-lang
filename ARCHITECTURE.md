# O-lang Architecture

O-lang is a universal polyglot expression framework where every expression
syntactically declares which language it is written in.

```
html^( <p>Result: python^( 2 + 2 )_python</p> )_html
```

## Repository Layout

```
O-lang/
├── src/              # Rust implementation (primary, active)
│   ├── main.rs       #   CLI entry point
│   ├── lib.rs        #   Library crate root
│   ├── parser.rs     #   Tokenizer & expression parser
│   ├── eval.rs       #   Recursive evaluator
│   ├── value.rs      #   OValue universal type system
│   ├── process.rs    #   Subprocess management for backends
│   ├── nix_ops.rs    #   Nix build/realise operations
│   ├── nixos_ops.rs  #   NixOS-specific operations
│   ├── scheduler.rs  #   Parallel evaluation scheduler
│   └── bin/          #   Additional binary targets
├── backends/         # Language shims (Python, Bash, Nix, Racket, Rust, Shell)
├── examples/         # .O example programs
├── c_cpp/            # Complete C17 port (standalone)
├── o_lang/           # Legacy Python prototype (reference only)
├── tests/            # Python-era test suite
├── setup/            # Cross-platform bootstrap scripts
├── tools/            # Development utilities (markdown extraction)
├── scripts/          # Repository management scripts
├── docs/             # Design documents and brainstorms
├── SPEC.md           # Language specification
└── README.md         # Project overview
```

## Evaluation Pipeline

O-lang processes code through a 5-stage pipeline:

1. **Parse** — Tokenize source into typed expression trees. Each expression
   carries a language tag (e.g., `python`, `html`, `nix`).

2. **Evaluate** — Recursively evaluate inner expressions first (applicative
   order). Child results become available to parent expressions.

3. **Render** — Convert child `OValue` results into the parent language's
   native syntax for interpolation.

4. **Dispatch** — Send the rendered source to the appropriate backend shim
   as a subprocess, communicating via JSON over stdin/stdout.

5. **Cache** — Memoize expensive operations (especially Nix
   instantiate/realise) to avoid redundant work.

## Universal Value System (OValue)

Every value crossing language boundaries is represented as one of these types:

| Type           | Purpose                              |
|----------------|--------------------------------------|
| `ONull`        | Absence of value                     |
| `OBool`        | Boolean true/false                   |
| `OInt`         | Integer number                       |
| `OFloat`       | Floating-point number                |
| `OStr`         | Text string                          |
| `OList`        | Ordered collection                   |
| `OMap`         | Key-value mapping                    |
| `OHtml`        | HTML fragment                        |
| `OStorePath`   | Nix store path                       |
| `ONixExpr`     | Unevaluated Nix expression           |
| `ODerivation`  | Nix derivation                       |
| `OBlob`        | Binary data                          |
| `OExpr`        | Unevaluated O expression             |
| `ORequest`     | HTTP/system request                  |
| `OThunk`       | Deferred computation                 |

## Backend Shims

Each supported language has a shim script in `backends/` that:
- Reads JSON input from stdin
- Evaluates the expression in the target language
- Writes JSON output to stdout

Currently supported: Python, Bash, Nix, Racket, Rust, Shell.

## Building & Testing

```bash
# Build
cargo build

# Run an example
cargo run -- examples/hello.O backends

# Run tests
cargo test

# Run example smoke tests
bash test_o_lang_examples.sh
```

## Implementations

| Edition | Directory | Status     |
|---------|-----------|------------|
| Rust    | `src/`    | **Active** |
| C17     | `c_cpp/`  | Complete   |
| Python  | `o_lang/` | Reference  |
