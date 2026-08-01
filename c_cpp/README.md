# O-lang — C edition (easy native build)

Pure C17 implementation of the O-lang meta-language runtime and `olangc` AOT compiler.

> **Every expression carries its own interpreter as part of its syntax.**

This edition requires only a C compiler (`cc` / `clang` / `gcc`) and `make`. No Rust, no CMake for the common path.

## Quickstart

```bash
cd c_cpp

# Build interpreter + compiler (one command)
make

# Run a program (python^ blocks use the shared ../backends/*.py shims)
./O ../examples/hello.O ../backends
# → 2

./O ../examples/html_basic.O ../backends
# output contains: <p>The answer is 42.</p>

# AOT compile to an application bundle (hello + hello.shims/)
mkdir -p /tmp/hello-c17
./olangc ../examples/hello.O -o /tmp/hello-c17/hello
/tmp/hello-c17/hello
# → 2

# The produced binary is native C code; it still needs python3 (and nix if your
# .O program uses nix^) on the machine where you *run* it — exactly like a
# program that calls out to Python.
```

## Easy compile

- `make` — builds `O` and `olangc`
- `make clean`
- `make test` — runs the manifest-selected C17 interpreter examples
- `make olangc-test` — runs the manifest-selected C17 AOT examples
- `make path-test` — proves AOT paths and `CC` are passed literally, without a shell
- `make warnings-as-errors` — clean-builds and tests native plus generated C with `-Werror`
- `make run EX=html_basic` — quick run of an example

The active implementation is built from the C17 `.c` files in `src/` plus `.h` headers in `include/`. The Makefile is deliberately simple (no subdirs, no generated build system); CMake builds the same C17 implementation for users who prefer it.

## What works

- Full typed-paren grammar (`LANG^( ... )_LANG`, `[n]` envs, `{lazy}/{defer}`, `let`, `$var`, calls)
- `html^`, `markdown^`/`latex^`/`text^` structural rendering
- `python^` via the real `python_shim.py` (persistent envs, `__oval_result__`, trailing expressions, stdout capture)
- Basic builtins: `now()`, `lazy()`, `instantiate()`, `realise()`, `activate()`, `current_system()`, `autonomous()`
- Nix rung (when `nix` is in PATH)
- Shebang stripping
- `olangc` AOT that produces a native executable plus a required per-executable `<name>.shims/` directory

See `SPEC.md` (in repo root) for the language specification.

## Requirements

- C17 compiler + make (macOS: Xcode Command Line Tools; Linux: gcc/clang + make)
- `python3` (for any `python^` / `py^` blocks)
- Optional: `nix` (for `nix*` examples and the four-rung lattice)

## olangc (AOT)

`olangc` turns a `.O` file into an application bundle containing:

- a native executable with the O-lang C evaluator and your program source compiled in
- a required sibling `<executable>.shims/` directory containing the backend scripts

```bash
mkdir -p build/myprog
./olangc myprog.O -o build/myprog/myprog
build/myprog/myprog
```

`olangc` launches the compiler directly with a POSIX argv vector. `CC` may be
an executable name or path (including spaces and shell metacharacters), but is
not parsed as shell text and must not contain command-line flags. Use
`OLANGC_WARNINGS_AS_ERRORS=1` to compile the generated runtime with `-Werror`.

The bundle has **no dependency on the `olangc` tool or source tree** at runtime,
but copying the executable without its matching `<executable>.shims/` directory
is unsupported and backend execution exits nonzero. The shim directory name is
derived from the executable's resolved path, so separately named binaries in one
directory retain independent backend assets.

## Layout

```
c_cpp/
├── Makefile          # the easy build
├── include/          # public + internal C headers
│   ├── value.h
│   ├── parser.h
│   ├── process.h
│   ├── eval.h
│   ├── scheduler.h
│   └── nix_ops.h
├── src/
│   ├── value.c       # OValue + 4-byte length-prefixed canonical CBOR wire (core)
│   ├── parser.c      # typed-paren parser + AST
│   ├── process.c     # shim subprocess mgmt + 4-byte length-prefixed canonical CBOR IPC
│   ├── eval.c        # leaves-up evaluator, splice, structural backends, render_child
│   ├── scheduler.c   # (serial for MVP) autonomous + disk cache
│   ├── nix_ops.c     # instantiate / realise / activate
│   ├── nixos_ops.c   # NixOS-related operations
│   ├── main.c        # the `O` interpreter
│   └── olangc.c      # the AOT compiler
├── legacy_cpp/       # historical obsolete C++ prototype (not built)
└── README.md         # this file
```

## Adding a language (for hackers)

1. Add the tag to the registered list in `main.c` / `olangc` generated mains.
2. Implement a `_shim.py` (or native executable) speaking the 4-byte length-prefixed canonical CBOR protocol.
3. Add a `render_*` case in `eval.c` if the language needs special `render_child` rules.
4. For structural behaviour (like `O` / `quote`), handle in `eval_typed_expr`.

See the Python reference implementation and `backends/` for examples.

## Status & limitations

- Matches the core of the Rust edition for the documented examples.
- Basic quote values and scalar `O.eval` callbacks may work, but explicit
  `OScope` callback transport is not implemented. The full `meta_eval.O`
  round-trip is therefore not a supported C17 example and is excluded from
  the C17 manifest corpus.
- Stub shims (bash, rust, racket, shell) return the code text as a string (same as other editions).
- Some advanced scheduler / concurrent Nix behaviour is serial in this port.
- See root `SPEC.md` and `README.md` for the full feature set and known limitations.

## License

Research scaffolding. Use it, extend it, break it.
