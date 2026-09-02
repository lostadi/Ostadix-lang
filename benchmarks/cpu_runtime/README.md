# Backend-free CPU benchmark

`run.py` is a bounded CPU-oriented complement to the wait-heavy hosted HGraph
suite. It executes no foreign runtime, contains no sleeps, and performs no
network or filesystem effects from O code.

The runner measures four cases:

| Case | Purpose |
| --- | --- |
| `startup_check` | Tiny parse-only control showing O process/read/setup overhead |
| `parser_check` | Large generated source through `O --check --json` |
| `evaluator_serial` | Deterministic inline-text DAG through the reference executor |
| `evaluator_graph` | The identical DAG through the graph executor |

The evaluator workload has one seed, a configurable width and depth of pure
inline-text nodes, and one final aggregation node. Every measured and warmup
pair must produce both the same serial/graph OValue and the generator's
independent expected value. The graph case is not expected to be faster for
every size: scheduling overhead is part of what this CPU workload reveals.

The wall clock includes O process startup, input reading, parsing, admission,
evaluation, JSON serialization, and process collection. O's `elapsed_ms` is
also retained for evaluator samples. The tiny control makes startup cost
visible; the runner never subtracts it from another measurement.

## Run

Build a release runtime, keep the device idle and thermally stable, and run:

```bash
cargo build --release --locked --package o-lang --bin O
python3 benchmarks/cpu_runtime/run.py \
  --warmups 2 \
  --repetitions 7 \
  --workers 4 \
  --output cpu-result.json
```

Defaults generate 1,000 parser bindings and a `32 x 4` evaluator DAG with a
64-byte deterministic payload. `--parse-bindings`, `--dag-width`,
`--dag-depth`, and `--payload-bytes` tune the workload. Source and expected
result sizes have hard limits, every O child has a timeout, and all count
arguments are bounded.

The v1 JSON result contains every raw nanosecond sample, medians and median
absolute deviations, alternating execution order, workload/source hashes,
semantic-result identity, runtime hash/version, git state, CPU affinity,
governor, host fingerprint, and the exact performance configuration. Progress
goes to stderr, so stdout is valid JSON when `--output` is omitted.

There is no default performance threshold. Normal runs only fail for invalid
inputs, child failures/timeouts, malformed O output, or semantic divergence.

## Optional regression gate

Any result from this runner can be a baseline. Capture it under controlled
conditions with at least five repetitions (nine or more is preferable), then
compare a later build on the same host:

```bash
python3 benchmarks/cpu_runtime/run.py \
  --warmups 3 --repetitions 9 --workers 4 \
  --output baseline.json

python3 benchmarks/cpu_runtime/run.py \
  --warmups 3 --repetitions 9 --workers 4 \
  --baseline baseline.json \
  --max-regression-percent 15 \
  --min-regression-ms 2 \
  --output candidate.json
```

The gate compares median wall time for parser/check, serial evaluation, and
graph evaluation. A case regresses only when it exceeds both the relative and
absolute guards. This avoids turning timer quantization or a small fixed jitter
into a failure. A baseline is rejected as incompatible if the runner,
host/kernel/affinity/governor fingerprint, generated workloads, worker count,
or workload configuration differs. Binary hashes are deliberately recorded
but excluded from compatibility so a candidate runtime can be compared.

Exit status is `0` for a descriptive run or passing gate, `2` for invalid or
incompatible input, and `3` for a measured regression. A gate is evidence for
that controlled host and configuration, not a portable absolute performance
claim.
