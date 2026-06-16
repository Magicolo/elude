# Task 03: Improve Experiment 02

Last updated: 2026-04-05
Status: completed
Priority: medium-high

## Goal

Improve `experiment_02` while preserving its identity:

- compile jobs into sequential groups
- keep runtime simple and group-by-group
- accept that the unit of runtime synchronization is a group

This task was explicitly not about turning `02` into `01` or `03`. The runtime
had to remain a sequential group loop.

## Relevant Files

- `src/experiment_02/README.md`
- `src/experiment_02/DESIGN.md`
- `src/experiment_02/mod.rs`
- `src/experiment_02/model.rs`
- `tests/experiment_02.rs`
- `tests/support/mod.rs`
- `benches/experiment.rs`

## Final Design Chosen

The final implementation keeps the layered runtime intact, but makes the
compiler substantially more ambitious.

It now builds two candidate layerings:

1. The original online first-fit layering based on per-key history trackers.
2. A new strict-frontier batch layering based on pairwise relation
   classification, strict-edge reduction, and greedy ready-frontier batching.

The compiler then chooses between them with a thread-aware rule:

- estimate execution waves as `sum(ceil(group_size / threads))`
- compare group count and max group width
- only allow the frontier layout when none of its groups exceeds the available
  thread budget

If the frontier layout does not clearly beat first-fit under that rule, the
compiler falls back to first-fit.

That last fallback is important. Early iterations of task `03` proved that a
more "clever" grouping can easily make realistic mixed workloads worse if it
creates oversized barrier groups.

## What Changed In Code

### New compile-time relation graph

`experiment_02` now computes pairwise relations across all jobs:

- `None`
- `Relaxed`
- `Strict`

Strict relations are then transitively reduced into a smaller DAG. Relaxed
relations are kept as neighborhood data for the frontier batch heuristic.

### Original first-fit compiler retained

The old compiler logic was not thrown away. It remains as one of the candidate
layouts and still matters in practice because:

- it is stable on realistic mixed workloads
- it preserves a declaration-order-like grouping
- it avoids overfitting compile-time heuristics

### New frontier batch compiler

The new compiler:

- tracks the current strict-ready frontier
- seeds each group from a job with high strict height / high remaining relaxed
  pressure
- grows the group greedily with compatible ready jobs
- prefers jobs that are easier to place and have similar conflict neighborhoods

This can beat online first-fit on bad coloring orders. The internal crown-graph
unit test exists specifically to prove that point.

### Thread-aware layout selector

The key pragmatic piece is the chooser:

- build both layouts
- score both against the configured thread count
- reject frontier layouts that create groups wider than the thread pool
- otherwise keep the layout with lower estimated execution waves

This keeps `experiment_02` grounded in realistic execution rather than blindly
preferring denser-but-wider groups.

## Why This Approach Was Chosen

The original hypothesis for task `03` was that the online first-fit placement
was leaving parallelism on the table.

That was true on some graphs, but early implementation attempts also showed an
important constraint:

- for a layered scheduler, fewer groups is not automatically better
- oversized groups can create worse barriers when they do not fit the actual
  machine width

The final design keeps the new compiler ideas, but only applies them when they
beat the classic layout under a thread-aware cost model.

That is a better fit for `experiment_02`'s identity than permanently replacing
the original algorithm.

## Verification

### Tests run

Commands:

- `cargo test --test experiment_02`
- `cargo test compile_can_outperform_online_first_fit_on_crown_conflicts`

Results:

- shared public `experiment_02` integration tests passed
- the new internal crown-graph test passed

### Benchmarks run

Commands:

- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_02/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_02/straggler_partial_overlap`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_02/mixed_hotspots`
- `cargo bench --bench experiment -- experiment/run_overhead/experiment_02/wide_independent`
- `cargo bench --bench experiment -- experiment/compile/experiment_02/compile_heavy_sparse_keys`

Machine parallelism:

- used `thread::available_parallelism()` through the benchmark harness

Observed baseline before task `03` work:

- `layer_barrier_stress`
  - `[1.2398 ms 1.4547 ms 1.6425 ms]`
- `straggler_partial_overlap`
  - `[1.6180 ms 1.7074 ms 1.7894 ms]`
- `mixed_hotspots`
  - `[2.1870 ms 2.2227 ms 2.2601 ms]`
- `compile_heavy_sparse_keys`
  - `[1.1908 ms 1.2028 ms 1.2165 ms]`
- `wide_independent/jobs512_zero`
  - `[230.13 µs 236.10 µs 241.71 µs]`

Observed final results after the thread-aware chooser:

- `layer_barrier_stress`
  - `[777.88 µs 868.91 µs 979.44 µs]`
  - observed improvement on this machine/run
- `straggler_partial_overlap`
  - `[839.48 µs 849.46 µs 860.35 µs]`
  - observed improvement on this machine/run
- `mixed_hotspots`
  - `[2.1998 ms 2.2349 ms 2.2699 ms]`
  - effectively flat versus the original baseline
- `wide_independent/jobs512_zero`
  - `[97.707 µs 138.28 µs 179.62 µs]`
  - noisy but not a concerning regression
- `compile_heavy_sparse_keys`
  - `[148.80 ms 155.11 ms 163.72 ms]`
  - compile time became dramatically more expensive

## Interpretation

The final result is mixed but coherent:

- the runtime model of `02` stayed intact
- the new compiler can find better layerings on some graphs
- the thread-aware chooser prevents the worst mixed-workload regressions from
  earlier drafts
- compile time is now vastly higher than the original baseline

This is acceptable only because the user explicitly prioritized practical
parallelism over schedule-time cost. If compile time becomes more important
later, this task should be revisited.

## Tradeoffs

- Compile time increased by roughly two orders of magnitude on the
  `compile_heavy_sparse_keys` benchmark.
- The frontier compiler is useful, but only as an optional alternative. Using it
  unconditionally regressed realistic mixed workloads during this task.
- The whole-group barrier remains the defining limit of `experiment_02`.
- Some run-time improvements were observed on barrier-shaped benchmarks, but the
  strongest guaranteed structural win is the new ability to avoid bad online
  coloring orders when the machine width makes that worthwhile.

## Dependencies

No new dependency was added for this task.

## Recommended Follow-On Work

- Revisit the layout chooser if `experiment_02` becomes compile-time dominated in
  real usage.
- Consider recording compile-time metrics such as chosen layout type, group
  count, wave estimate, and max width for research/debugging.
- Compare the chosen layouts against ECS competitor batching strategies when
  interpreting final benchmark results in task `07`.

## Progress Log

- 2026-04-04: Task created. No implementation yet.
- 2026-04-05: Replaced the old single-strategy compiler with a dual-layout
  compiler: online first-fit plus strict-frontier batching.
- 2026-04-05: Added a thread-aware chooser to keep the experimental frontier
  layout from hurting realistic mixed workloads.
- 2026-04-05: Added a crown-graph unit test to lock in a case where the new
  compiler can beat online first-fit.
- 2026-04-05: Updated docs and verified the final implementation with tests plus
  targeted benchmarks. Task complete.
