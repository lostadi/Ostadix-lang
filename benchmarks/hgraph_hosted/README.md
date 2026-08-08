# Hosted HGraph benchmark shapes

This directory contains the fixed inputs for
[`scripts/benchmark_hgraph_hosted.sh`](../../scripts/benchmark_hgraph_hosted.sh).
The suite compares the serial reference evaluator with the evidence-admitted
graph executor. It is descriptive: the runner records measurements and checks
results, but it does not enforce a speedup threshold.

| Shape | Topology | Analyzer-predicted width | Analyzer-predicted span | Required runtimes |
| --- | --- | ---: | ---: | --- |
| `heterogeneous.O` | one autonomous Python + Bash + Node.js batch | 3 | 1 | `python3`, `bash`, `node` |
| `chained.O` | four genuinely dependent Python stages | 1 | 4 | `python3` |
| `mixed_width.O` | one Python seed, four Python branches, one Python aggregate | 4 | 3 | `python3` |
| `realistic.O` | Python fetch/parse, Bash and Node transforms, Python aggregate | 2 | 3 | `python3`, `bash`, `node` |

The table records the reviewed fixture expectations, but the runner does not
copy those numbers. It renders the exact timed source, invokes the release
`olangc --target ir --explain-schedule`, and consumes the versioned
`oexec.schedule-prediction/v1` record produced by the evidence-bound admission.
That record is derived after admission and lies outside the admission digest;
its `admission-sha256` field must match the enclosing v3 admission binding.
Missing, duplicated, malformed, or internally inconsistent prediction records
or a mismatched admission reference fail before either executor runs.

The prediction assigns unit cost to every admitted shim-backed hosted operation
and zero cost to scope loads, stores, groups, schedule controls, and other
coordinator bookkeeping. Longest-path depth through the admitted dependency DAG
defines hosted-task layers; predicted span is the number of layers and predicted
width is the largest layer. The unit is therefore **hosted-task layers**, not
milliseconds. The prediction is static topology, not observed overlap or proof
that a layer fits current CPU, memory, process, runtime, or placement capacity.
The explicit `--workers` limit and dynamic graph readiness still bound actual
dispatch.

The middle stages use each backend's injected lexical bindings (`seed`,
`${fetched}`, and `fetched`) deliberately. An O-level `$name` splice inside an
autonomous hosted body would add a load child and make that body ineligible for
the current prepared hosted-task adapter. Source and group control still gate
the `1 -> N -> 1` stages, while the hosted programs consume the injected values.
The realistic fixture's “fetch” is deterministic local parsing; it performs no
network access.

## Run

Build the release runtime through the canonical setup path, then run all shapes:

```bash
./setup.sh -y --minimal
scripts/benchmark_hgraph_hosted.sh \
  --warmups 1 \
  --repetitions 5 \
  --sleep 0.25 \
  --workers 4 \
  --missing-runtime fail
```

Use `--shape heterogeneous`, `chained`, `mixed_width`, or `realistic` to run
one shape. `--missing-runtime skip` emits an explicit skipped block with
`not-measured` metrics; `fail` emits the same block and returns nonzero.
`python3` is also the JSON/statistics harness and is therefore a hard runner
requirement. Runtime paths and version strings are included in the provenance
header. The paths and SHA-256 digests of both the executing `O` binary and the
analyzing `olangc` binary are recorded, and each shape records the exact
admission digest that produced its prediction.

The runner analyzes each rendered fixture once before timing, alternates
serial-first and graph-first pairs, emits every raw pair timing, reports medians
and ranges, and checks every sample in two ways:

1. serial and graph must have identical canonical `ok`, `type`, and returned
   OValue JSON; and
2. that canonical triple must match the fixture's checked-in
   `*.expected.json` result.

This semantic-equivalence check is deliberately narrower than complete
observational equivalence. It does **not** compare filesystem or network
effects, scope snapshots, traces, event timing, or external-effect ordering.
These four fixtures return deterministic values and do not claim equivalence
for effects outside that returned-value boundary.

The CI invocation supplies its co-built debug `O` and `olangc` binaries and uses
zero hosted delay. It validates that every real fixture is analyzed and
executes, and that the serial, graph, and checked-in results agree; it is a
semantic smoke test, not a performance gate. CI does not enforce a speedup.

Timing includes hosted process startup and protocol overhead. Speedup on a
single CPU core can still occur because sleeping hosted subprocesses overlap;
that is wait concurrency, not evidence of parallel CPU computation. Treat
reported ratios as measurements of this fixture/runtime/machine combination,
not as a general scheduler-performance claim.

The current [analyzer-bound 2026-08-08 M1 Max result
record](RESULTS-2026-08-08-f216771.md) links its complete
[raw transcript](TRANSCRIPT-2026-08-08-f216771.log) and binds the analyzer,
executor, admission, source, machine, runtime, and measurement identities.

The historical [earlier 2026-08-08 M1 Max result
record](RESULTS-2026-08-08-be68dfef.md) binds its retained medians to the
measured source commit and `O` digest. It predates analyzer-derived predictions
and correctly labels its width/span values as manual. Keep result records
append-only: do not replace or relabel an older result when the source, either
binary, machine, runtime, parameters, or prediction method changes.
