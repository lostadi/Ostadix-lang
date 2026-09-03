# Real-world serial-versus-graph benchmark

This suite runs the same O program through the serial reference evaluator and
the evidence-admitted graph executor while doing useful work on this
repository's own assets and tests. It contains no artificial sleeps. The
[runner](../../scripts/benchmark_real_world.sh) records measurements and checks
plans, returned values, and generated artifacts, but it never enforces a
speedup threshold.

This is a bounded, reproducible benchmark suite, not a claim that every
program becomes faster. A dependency-bound program cannot gain parallelism,
small tasks can lose to scheduling overhead, and CPU, memory, storage, and
thermal limits remain properties of the machine being measured.

## Workloads

| Workload | Real work | Static hosted-task topology | Required tools |
| --- | --- | --- | --- |
| `asset_pipeline` | Generate 18 AVIF/WebP release derivatives at three sizes from three project images | 3 tasks, width 3, span 1 | Bash, ImageMagick, standard Unix tools |
| `ci_shards` | Run three independent repository unit-test shards and retain their complete logs | 3 tasks, width 3, span 1 | Bash, Python 3, standard Unix tools |
| `video_previews` | Decode nine project GIF animations and encode nine 768×832, 30-fps VP9 WebM previews | 9 tasks, width 9, span 1 | Bash, FFmpeg with VP9, `ffprobe` |

The exact programs are [asset_pipeline.O](asset_pipeline.O),
[ci_shards.O](ci_shards.O), and [video_previews.O](video_previews.O). The
responsive-image lanes use `MAGICK_THREAD_LIMIT=1`, and the video helper uses
one FFmpeg encoder thread with row multithreading disabled. These controls make
cross-lane concurrency primarily the graph executor's responsibility instead
of silently delegating it to each external tool.

`autonomous` has a precise meaning here. The fixture author explicitly declares
that the lanes may run unordered; each lane owns a disjoint output subtree or filename.
Ostadix then admits the graph, derives its ready set, applies the
worker limit, dispatches the hosted processes, and joins their results. The
suite does not claim that Ostadix inferred arbitrary shell-command effects or
found hidden parallelism inside an opaque executable.

## Run it

Build the release evaluator and schedule analyzer first:

```bash
cargo build --release --locked --package o-lang --bin O --bin olangc
```

A quick descriptive run uses the runner defaults of one paired warmup, four
measured pairs (a balanced AB/BA order), and four workers:

```bash
scripts/benchmark_real_world.sh --workload all --missing-tool fail
```

For a result worth retaining, use at least seven measured pairs, keep the
device idle and thermally stable, and retain both the evidence tree and the
complete transcript:

```bash
set -o pipefail
evidence_dir=$(mktemp -d target/real-world-evidence.XXXXXX)
transcript="${evidence_dir}.log"
scripts/benchmark_real_world.sh \
  --workload all \
  --warmups 1 \
  --repetitions 7 \
  --workers 3 \
  --missing-tool fail \
  --evidence-dir "$evidence_dir" 2>&1 | tee "$transcript"
```

The evidence path must be empty when the runner starts. If `--evidence-dir` is
omitted, the runner uses a temporary directory and removes it at exit. Use
`--missing-tool skip` only for an explicitly partial run; skipped workloads are
reported as not measured.

Run one workload by selecting its name:

```bash
scripts/benchmark_real_world.sh \
  --workload video_previews \
  --warmups 1 \
  --repetitions 7 \
  --workers 3 \
  --missing-tool fail
```

The standard video workload makes three-second previews. The helper accepts an
`OSTADIX_PREVIEW_SECONDS` override for local experimentation, but changing it
changes the workload. The runner records `preview_seconds` and the expected
decoded-frame count; treat an overridden run as a different benchmark rather
than comparing it directly with the standard result.

## See the result

With retained evidence, every measured pair has separate serial and graph
artifact directories. On Termux, open a generated preview from the first graph
sample with:

```bash
termux-open "$evidence_dir/video_previews/sample-1-graph-artifacts/idle.webm"
```

On a desktop with FFmpeg tools, use:

```bash
ffplay "$evidence_dir/video_previews/sample-1-graph-artifacts/idle.webm"
```

The neighboring `sample-1-graph-manifest.json` records the checked codec,
dimensions, duration, decoded-frame count, and a SHA-256 digest of FFmpeg's
decoded-frame MD5 stream. WebM container bytes are not used as the semantic
oracle because container metadata need not be byte-identical even when every
decoded frame is identical.

The image workload leaves 18 directly inspectable AVIF/WebP files under its
sample artifact directory. The CI workload leaves the complete `o-cli`,
`setup`, and `boot-iso` test logs.

## What is checked

Before timing a workload, `olangc --explain-schedule` must emit a valid
`oexec.schedule-prediction/v1` record bound to the enclosing admission digest.
The runner rejects an unexpected task count, width, span, or malformed
prediction.

Every serial/graph pair then has two independent equivalence checks:

1. O must return identical canonical `ok`, type, and value fields.
2. Independently inspected artifacts must produce identical canonical
   manifests.

For responsive images, the manifest checks the complete 18-file inventory,
format, dimensions, non-empty payload, byte count, and SHA-256. For CI shards,
every log must contain a passing `OK` summary and a parsed test count; the total
is derived rather than trusted as a permanent constant. For video previews,
`ffprobe` must report VP9, 768×832, `yuv420p`, and the requested duration, while
FFmpeg must decode exactly 30 frames per second and yield the same decoded-frame
digest in both modes.

The work happens in executor-specific directories, so serial and graph runs
cannot validate or overwrite each other's output. A failure, missing artifact,
extra artifact, test failure, decode failure, or manifest difference fails the
workload rather than being summarized as a performance result.

## Read the measurements

The headline comparison is whole-process wall time around each O invocation.
It includes O process startup, source loading, parsing, admission, scheduling,
hosted-runtime startup, external application work, result collection, and O
JSON serialization. The runner obtains it from the no-shell
[`timed_exec.py`](timed_exec.py) helper's monotonic `perf_counter_ns` interval.
O's own `elapsed_ms` is retained as a diagnostic, not used as the primary
performance headline. Artifact verification runs after the O invocation and
is deliberately outside that execution timer.

Pairs alternate order: odd pairs run serial then graph, and even pairs run
graph then serial. Warmup pairs are checked but excluded from the statistics.
Use the paired geometric-mean wall-time speedup as the most order-resistant
summary, alongside both raw paired wall times, medians, ranges, and latency
reduction. Do not report only the fastest sample.

The principal transcript fields are:

| Field | Interpretation |
| --- | --- |
| `paired_wall_ms` | Every measured serial/graph wall-time pair, in execution-pair order |
| `serial_wall_ms`, `graph_wall_ms` | Median, median absolute deviation, minimum, and maximum whole-process wall time |
| `paired_geometric_mean_speedup` | Geometric mean of each pair's `serial / graph` ratio; preferred headline ratio |
| `ratio_of_median_wall_times` | Serial wall-time median divided by graph wall-time median; useful secondary summary |
| `median_latency_reduction_percent` | Reduction calculated from the two wall-time medians |
| `serial_internal_elapsed_ms`, `graph_internal_elapsed_ms` | O-reported internal elapsed time; diagnostic only |
| `effective_unit_cost_reference` | Minimum of the unit-cost work/span ratio and configured worker count; diagnostic only |

The runner also reports a unit-cost work/span reference derived from the
admitted topology and the configured worker limit. It assigns one unit to every
hosted task, not the task's measured milliseconds. It is not a speedup ceiling:
adaptive frequency, core placement, cache state, internal tool parallelism,
unequal task costs, process startup, and memory bandwidth can all change task
service rates between arms. Consequently, the runner does not report a
percentage of this reference "captured."

For example, with three workers all three workloads have an effective
unit-cost reference of 3×: the image and CI graphs each expose three ready
tasks, while the nine preview tasks are dispatched in successive ready waves.
A 2.8× paired wall-time result should be reported directly alongside that
diagnostic reference; it would not mean that arbitrary programs or a single
VP9 encode become 2.8× faster.

## Controls and caveats

Run the graph executor with one worker to expose scheduler and subprocess
overhead when no worker concurrency is available:

```bash
scripts/benchmark_real_world.sh \
  --workload all \
  --warmups 1 \
  --repetitions 7 \
  --workers 1 \
  --missing-tool fail
```

Repeat with `--workers 2` and `--workers 3` to check whether throughput follows
available capacity and then stops improving when another resource becomes the
bottleneck. The width-one `chained` shape in the separate
[hosted HGraph suite](../hgraph_hosted/README.md) is the dependency negative
control; adding workers must not materially accelerate that chain.

Keep these limitations attached to any published result:

- Results apply to the recorded commit, O and `olangc` binary hashes, tool
  versions, worker count, workload configuration, and machine. They are not
  portable constants.
- Mobile dynamic voltage/frequency scaling, thermal throttling, background
  applications, battery policy, and filesystem cache state can materially move
  the numbers. Alternate order, retain raw pairs, and repeat across thermal
  sessions before making a strong claim.
- Image and video tasks are CPU- and memory-intensive. Concurrent lanes may
  contend for memory bandwidth even though they write different files.
- The Python shards are real repository tests, but their count and cost can
  change with the source tree. Passing these shards is not the same as running
  every project test.
- The suite shows that one admitted execution mechanism can govern several
  kinds of external program. It cannot establish a universal speedup: opaque
  sequential work, dependency chains, tiny tasks, and already-saturated tools
  may show no gain or a regression.

The focused runner contract tests live in
[test_benchmark_real_world.py](../../tests/test_benchmark_real_world.py). The
deterministic source-release builder includes this README, the runner,
fixtures, helper, and exact source assets so the suite can be reproduced from
a published source archive.
