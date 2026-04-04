# Task 04: Improve Experiment 03

Last updated: 2026-04-04
Status: not started
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

`experiment_03` currently:

- builds fixed strict predecessors
- chooses a topological priority order using a greedy heuristic
- orients relaxed conflicts according to that order
- compresses per-key edges
- performs final predecessor reduction
- runs a flat predecessor-counter DAG executor

The core current functions are:

- `build_priority_order`
- `build_job_stats`
- `build_final_predecessors`
- `reduce_predecessors_priority_order`

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

## Progress Log

- 2026-04-04: Task created. No implementation yet.
