# Task 04: Improve Experiment 03

Last updated: 2026-04-05
Status: completed
Priority: high

## Goal

Improve `experiment_03` while preserving its identity:

- spend heavily at compile time
- orient relaxed conflicts statically
- emit a flat DAG executor for repeated runs
- avoid runtime reservation protocols

This task should push the static-DAG approach as far as it can reasonably go.

## Relevant Files

- `src/experiment_03/README.md`
- `src/experiment_03/DESIGN.md`
- `src/experiment_03/mod.rs`
- `src/experiment_03/model.rs`
- `tests/experiment_03.rs`
- `tests/support/mod.rs`
- `benches/experiment.rs`

## Current Design Summary

`experiment_03` now:

- builds fixed strict predecessors
- chooses a topological priority order using a greedy heuristic
- computes final-DAG heights after edge construction
- orients relaxed conflicts according to that order
- compresses per-key edges
- performs final predecessor reduction
- runs a flat predecessor-counter DAG executor with work-first direct wakeups

The core current functions are:

- `build_priority_order`
- `build_job_stats`
- `build_final_predecessors`
- `reduce_predecessors_priority_order`
- `build_final_heights`
- `run_ready_chain`

## High-Value Hypotheses To Test

1. The current priority heuristic is leaving parallelism on the table.

- It uses strict height, relaxed degree, writes, accesses, and declaration order.
- This is plausible, but it is still a fairly light heuristic for a compile-heavy experiment.

2. Static orientation quality could justify much more schedule-time work.

- The user explicitly allows schedule-time cost.
- `03` is the experiment best positioned to spend that cost.

3. The per-key sparse edge construction may be too myopic for multi-key jobs.

- It preserves correctness cheaply, but it may not minimize critical path in mixed-resource workloads.

## Improvement Directions To Explore

### A. Stronger compile-time heuristic search

Potential ideas:

- local swap improvement over the initial topological order
- multiple initial order seeds, then pick the best scored DAG
- longest-estimated-critical-path scoring
- hotspot-aware scoring that prioritizes jobs touching highly contended keys

### B. Duration-aware orientation

If the benchmark IR exposes estimated cost classes, consider using them only at compile time.

Potential ideas:

- prioritize long jobs earlier when that shrinks the critical path
- avoid orienting many short jobs behind a long hot-resource job when another legal order exists

### C. Better final edge reduction

Potential ideas:

- stronger transitive reduction passes
- resource-aware pruning that preserves all legality but reduces wakeup traffic

### D. Optional library-assisted compile algorithms

If a library helps with compile-time graph manipulation, that is acceptable as long as the final runtime representation remains:

- flat
- dense
- scheduler-owned

Do not let a general graph library become the runtime data structure.

## Constraints

- Runtime must remain a pure DAG executor with direct wakeups.
- Do not add runtime conflict reservations.
- Do not add grouped barriers.
- Preserve repeated-run reusability.

## Recommended Execution Plan

1. Benchmark current `03` on the redesigned harness.
2. Identify the workload families where `03` should beat `02` but does not.
3. Add compile-time instrumentation if needed:
   - edge counts before and after reduction
   - root counts
   - critical-path estimates
4. Choose one meaningful improvement strategy.
5. Implement and validate.
6. Update this file with:
   - chosen heuristic
   - compile-time cost change
   - runtime benchmark effect

## Potential Dependency Additions

Possible but not required:

- `smallvec`
- `rustc-hash`
- a graph library used only during compile

Document clearly if a dependency is only used for compile-time graph work.

## Acceptance Criteria

- `experiment_03` remains a static-DAG scheduler.
- Shared tests still pass.
- Benchmarks show whether the new compile-time work improved repeated-run behavior.
- The task file records the actual heuristic chosen and the measured tradeoff.

## Implemented Change

The implemented improvement was not a heavier relaxed-edge search. Benchmarking
showed that `experiment_03` was underperforming on workloads where the static
DAG should already have been good enough, especially barrier-heavy and
partial-overlap cases. That pointed to executor overhead rather than poor
orientation quality.

The chosen change preserves the same static-DAG identity but improves the
executor shape:

- compute final-DAG longest-path heights after the final reduced graph exists
- sort roots and each successor slice by descending final height, then by the
  original priority rank
- replace "spawn every ready job" with a work-first ready-chain executor
- keep one newly ready successor inline on the current worker
- spawn only additional newly ready successors

This keeps runtime purely DAG-based:

- no runtime conflict reservations
- no grouped barriers
- no runtime search for legal jobs
- no change to dependency semantics

## Why This Was Chosen

The benchmark evidence before the change was:

- `straggler_partial_overlap`: about `1.008 ms .. 1.120 ms`
- `mixed_hotspots`: about `2.243 ms .. 2.505 ms`
- `layer_barrier_stress`: about `1.052 ms .. 1.208 ms`
- `compile_heavy_sparse_keys`: about `25.321 ms .. 27.022 ms`

Those numbers suggested that the static graph itself was acceptable, but the
executor was paying too much per-ready-node overhead by spawning every newly
ready job as an independent Rayon task.

## Validation

Correctness checks run:

- `cargo test --test experiment_03`
- `cargo test compile_keeps_read_batches_free_of_read_read_edges`
- `cargo test compile_treats_unknown_as_a_strict_barrier`

Benchmark commands run:

- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_03/straggler_partial_overlap`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_03/mixed_hotspots`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_03/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/compile/experiment_03/compile_heavy_sparse_keys`

## Measured Outcome

After the change:

- `straggler_partial_overlap`: `784.74 µs .. 869.01 µs`
- `mixed_hotspots`: `1.511 ms .. 1.538 ms`
- `layer_barrier_stress`: `562.53 µs .. 565.90 µs`
- `compile_heavy_sparse_keys`: `20.067 ms .. 20.218 ms`

Observed effect:

- strong repeated-run improvement on all three targeted runtime workloads
- especially large gains on strict-layer and mixed-hotspot shapes
- compile cost did not regress; it improved on the measured compile workload
- `experiment_03` remains a compile-heavy static-DAG executor, but now with a
  much cheaper ready-job hot path

## Progress Log

- 2026-04-04: Task created. No implementation yet.
- 2026-04-05: Benchmarked the existing `experiment_03` implementation and
  determined that runtime wakeup overhead, not relaxed-edge orientation
  quality, was the main weakness on the redesigned harness.
- 2026-04-05: Implemented criticality-ordered roots/successors plus a
  work-first ready-chain executor in `src/experiment_03/model.rs`.
- 2026-04-05: Verified the change with targeted tests and benchmarks and marked
  the task complete.
