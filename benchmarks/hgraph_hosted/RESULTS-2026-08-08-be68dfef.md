# Hosted HGraph benchmark result — 2026-08-08

This is the retained local measurement record for the four hosted HGraph
fixtures. It is bound to the measured source and executable as follows:

| Field | Value |
| --- | --- |
| Measurement date | 2026-08-08 |
| Source commit | `be68dfef528ea0d4e287916ee18028b1aa7a5c5f` |
| `target/release/O` SHA-256 | `adacf7c77d98d886c35420598e6f330f705b4952ff50e35b854b2a027f8fd35c` |
| Machine | Apple M1 Max |
| Harness-reported logical CPUs | 10 |
| Warmup pairs | 1 |
| Measured pairs per main result | 5 |
| Hosted delay per task | 0.25 seconds |
| Graph worker override | 4 |
| Missing-runtime policy | `fail` |

The measured command shape was:

```bash
scripts/benchmark_hgraph_hosted.sh \
  --warmups 1 \
  --repetitions 5 \
  --sleep 0.25 \
  --workers 4 \
  --missing-runtime fail
```

The source commit identifies the benchmark fixtures and runtime source. The
binary digest identifies the executable that was actually measured. This file
was added after that run, so its containing documentation commit is not the
benchmark subject. This is a human-readable measurement record, not a signed
attestation or a retained raw transcript. Raw per-sample timings were not
retained here and are not reconstructed.

## Main result

Times are medians in milliseconds. `Speedup` is serial median divided by graph
median. `Equivalent` means every measured serial/graph pair had identical
canonical `ok`, `type`, and returned OValue JSON. `Expected` means those fields
also matched the fixture's checked-in `*.expected.json`.

| Program | Predicted width | Predicted span | Serial median (ms) | Graph median (ms) | Speedup | Equivalent | Expected |
| --- | ---: | ---: | ---: | ---: | ---: | --- | --- |
| `heterogeneous.O` | 3 | 1 | 981 | 347 | 2.827089x | true | true |
| `chained.O` | 1 | 4 | 1389 | 1402 | 0.990728x | true | true |
| `mixed_width.O` | 4 | 3 | 2033 | 1024 | 1.985352x | true | true |
| `realistic.O` | 2 | 3 | 1316 | 1009 | 1.304262x | true | true |

Width and span are manually declared, reviewed predictions for the fixture
topology. They are not inferred by the runner or `--explain-schedule`, and they
do not prove observed overlap. The mixed-width fixture's equal-cost model has
six task units over three predicted layers, so its ideal bound is 2x; the
measured 1.985352x is consistent with that bounded model on this run. The
width-one chain is the negative control and shows no meaningful speedup.

## Chained worker-count control

The width-one chain was also measured at several worker settings. The four-
worker row is the five-pair main result above; the other rows used three
measured pairs with the same 0.25-second hosted delay.

| Workers | Repetitions | Serial median (ms) | Graph median (ms) | Speedup |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 3 | 1371 | 1388 | 0.987752x |
| 4 | 5 | 1389 | 1402 | 0.990728x |
| 8 | 3 | 1394 | 1391 | 1.002157x |

Increasing pool capacity does not accelerate a true dependency chain. That is
the expected negative-control behavior.

## Interpretation and non-claims

These are wait-heavy subprocess fixtures. They measure the benefit of
overlapping blocked waits across hosted runtimes, not parallel CPU computation.
The main suite above ran on the stated 10-logical-CPU M1 Max. A previously
mentioned 3.28x result from a one-logical-CPU environment was not reproduced or
bound to this commit and executable, so this record does not claim it.

Semantic equivalence here covers only `ok`, type, and canonical returned OValue
JSON. It excludes filesystem and network effects, scope snapshots, traces,
timing, and external-effect ordering. In particular, the result does not widen
the documented `explicit-autonomous-unordered` effect semantics into strict
serial equivalence. The four fixtures are a bounded mechanism/model benchmark,
not a general CPU-throughput, application-throughput, or scheduler-optimality
claim.

## Capturing a future run

Build through the canonical release path, record the clean source identity and
executable digest, and retain the complete harness stream before summarizing it:

```bash
./setup.sh -y --minimal
git status --short
git rev-parse HEAD
shasum -a 256 target/release/O
set -o pipefail
scripts/benchmark_hgraph_hosted.sh \
  --warmups 1 \
  --repetitions 5 \
  --sleep 0.25 \
  --workers 4 \
  --missing-runtime fail 2>&1 | tee hosted-hgraph-YYYY-MM-DD.log
```

Preserve the unedited log separately and record its SHA-256 alongside a new,
dated result document. The runner's provenance header supplies machine, logical
CPU, runtime path, and runtime-version fields. Do not overwrite this record or
reuse its measurements when any bound input differs.
