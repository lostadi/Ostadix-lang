# REVIEW.md — a one-page entry point for outside reviewers

[README.md](README.md) is long (several thousand lines) and the technical
whitepaper is a full paper. If you have a few minutes to decide whether this
project is worth a closer look — or you were pointed here specifically to
check one claim — start here instead of either of those.

## The claim, in one paragraph

Ostadix-lang lets a language boundary be part of an expression's syntax
(`LANG^( body )_LANG`) instead of a file-, module-, or cell-level
declaration. The full argument, with its hedges, lives in
[README.md § Related work and how Ostadix-lang differs](README.md#related-work-and-how-ostadix-lang-differs).
Its claim is that no single prior system combines all six of the properties
below, even though every individual property has prior art on its own:

1. **Expression-granular evaluator selection** — the evaluator is chosen per
   expression, not per file, module, or notebook cell.
2. **Recursive nestability** — evaluator expressions may contain further
   evaluator expressions to arbitrary depth.
3. **A registry-extensible evaluator family** — evaluators are entries in a
   compile-time-extensible registry, not a fixed pair.
4. **Independent runtimes** — evaluators are separately implemented real
   runtimes (CPython, Node.js, Nix, SQLite, Racket, rustc, ...), not
   reimplementations on one shared substrate.
5. **A common value domain** — results cross boundaries through `OValue`, one
   language-neutral value type, instead of pairwise FFI glue.
6. **Global lowering** — the whole heterogeneous program lowers into one
   execution representation (OIR / HGraph) for scheduling and analysis.

This is a **conjunction claim**: no single one of the six ideas is presented
as new by itself. The README names Racket `#lang`, Eco, PyHyp, GraalVM/Truffle,
and polyglot notebooks (Jupyter, .NET Interactive, Org-Babel) directly as the
closest prior art for individual properties above, and does not claim to beat
any of them on their own axis.

## Check it yourself in under a minute

```sh
O examples/six_properties_demo.O backends
```

That one file selects three backends drawn from the same open registry
(`bash`, `html`, `python`), nests them recursively, runs `bash` and `python`
as independent OS subprocesses (see `crates/ostadix-api/src/process.rs`), and
passes every intermediate result — bash's captured stdout, an integer, a
rendered HTML string — through the same `OValue` type. To check that the
whole file lowers to one execution graph rather than a per-block pipeline:

```sh
olangc examples/six_properties_demo.O --target ir --shim-dir backends
```

The output is a single `OIrProgram` and a single `ExecutionPlan`, not several
independent programs stitched together afterward.

## What is verified here, versus asserted

| Claim | Status |
| --- | --- |
| Properties 1–6 exist in this implementation and behave as described | Verified — run the two commands above yourself. |
| No system the author is aware of combines all six properties at once | **Not independently verified.** This rests on one person's non-exhaustive literature search. It is the single claim most worth outside scrutiny. |
| "To our knowledge, Ostadix-lang is the first general-purpose programming system to combine [...]" (README, same section) | Same caveat: a hedge, not a peer-reviewed result. |
| Every other claim about the rest of the system | See [docs/CLAIMS.md](docs/CLAIMS.md), which separates "implemented and tested now" from aspirational claims in the same spirit as this file. |

If you know of a system that already combines these six properties, or a
closer comparator than the ones the README names, please
[open an issue](https://github.com/lostadi/Ostadix-lang/issues/new/choose).
Corrections to the novelty claim are explicitly invited, not merely
tolerated — the README section linked above ends by saying so directly, and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) commits to technical disagreement
that stays "precise, respectful, and directed at the work."

## Where to go next

- Still skeptical of the framing after the one-pager? Read
  [README.md § Related work and how Ostadix-lang differs](README.md#related-work-and-how-ostadix-lang-differs)
  in full for the argument and its explicit caveats.
- Want the formal write-up?
  [Technical whitepaper (PDF)](Ostadix-lang_Technical_Whitepaper.pdf) /
  [TeX source](docs/Ostadix-lang_Technical_Whitepaper.tex).
- Want the claim-by-claim accuracy ledger for the rest of the system?
  [docs/CLAIMS.md](docs/CLAIMS.md).
- Want to cite this work? [CITATION.cff](CITATION.cff).
