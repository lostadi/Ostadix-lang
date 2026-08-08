# Hosted HGraph benchmark shapes

This directory contains the fixed inputs for
[`scripts/benchmark_hgraph_hosted.sh`](../../scripts/benchmark_hgraph_hosted.sh).
The suite compares the serial reference evaluator with the evidence-admitted
graph executor. It is descriptive: the runner records measurements and checks
results, but it does not enforce a speedup threshold.

| Shape | Topology | Predicted width | Predicted span | Required runtimes |
| --- | --- | ---: | ---: | --- |
| `heterogeneous.O` | one autonomous Python + Bash + Node.js batch | 3 | 1 | `python3`, `bash`, `node` |
| `chained.O` | four genuinely dependent Python stages | 1 | 4 | `python3` |
| `mixed_width.O` | one Python seed, four Python branches, one Python aggregate | 4 | 3 | `python3` |
| `realistic.O` | Python fetch/parse, Bash and Node transforms, Python aggregate | 2 | 3 | `python3`, `bash`, `node` |

The span unit is **unit-cost hosted-task layers**, not milliseconds. It omits
scope loads, stores, group/schedule bookkeeping, process startup, and teardown.
Width is the shape's structural maximum. The explicit `--workers` limit and
graph readiness bound selected concurrency; hardware capacity affects observed
execution but is not admitted or reserved.

The middle stages use each backend's injected lexical bindings (`seed`,
`${fetched}`, and `fetched`) deliberately. An O-level `$name` splice inside an
autonomous hosted body would add a load child and make that body ineligible for
the current prepared hosted-task adapter. Source and group control still gate
the `1 -> N -> 1` stages, while the hosted programs consume the injected values.
The realistic fixture's “fetch” is deterministic local parsing; it performs no
network access.

## Run

Build the release runtime first, then run all shapes:

```bash
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
header.

The runner alternates serial-first and graph-first pairs, reports medians and
ranges, and checks every sample in two ways:

1. serial and graph must have identical canonical `ok`, `type`, and returned
   OValue JSON; and
2. that canonical triple must match the fixture's checked-in
   `*.expected.json` result.

Timing includes hosted process startup and protocol overhead. Speedup on a
single CPU core can still occur because sleeping hosted subprocesses overlap;
that is wait concurrency, not evidence of parallel CPU computation. Treat
reported ratios as measurements of this fixture/runtime/machine combination,
not as a general scheduler-performance claim.
