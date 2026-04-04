# Task 01: Benchmark Foundation And Workload Redesign

Last updated: 2026-04-04
Status: not started
Priority: high

## Goal

Refactor the benchmark harness so it can compare:

- local experiments `01`, `02`, `03`, and future `04`
- selected external scheduler libraries
- compile-time behavior
- repeated-run behavior
- standard `0ms` overhead
- workload shapes that clearly expose differences in parallelism

This task exists to make every later optimization measurable and comparable.

## Why This Task Should Happen Early

Without a stronger harness:

- external library integration will be ad hoc
- scheduler changes will be hard to judge
- `0ms` overhead will remain conflated with job-body overhead
- it will be too easy to optimize for the current benchmark matrix instead of the actual parallelism question

## Current Baseline

The current harness in `benches/experiment.rs`:

- uses a single synthetic workload type
- mixes scheduler overhead with `Instant::now()` polling and atomic state updates
- has no abstraction for non-local schedulers
- does not explicitly benchmark the cases where the three designs should diverge

## Scope

This task should produce the benchmark framework and the benchmark workload catalog.

It may include:

- refactoring `benches/experiment.rs` into support modules under `benches/`
- small shared benchmark-only abstractions
- dev-dependency additions in `Cargo.toml`
- adapter types for local schedulers

It should not yet optimize a specific experiment unless that is required to make the adapter abstraction work.

## Target Architecture

The benchmark layer should have a scheduler-agnostic workload description.

Recommended shape:

- define a benchmark IR, for example:
  - job id
  - declared dependencies
  - execution profile
  - optional explicit weight estimate used by some adapters
- define an adapter trait for "compile this IR into a runnable schedule"
- define separate benchmark suites for:
  - compile
  - run
  - `0ms` or near-zero work overhead
  - parallelism-sensitive workloads

The adapter trait must be flexible enough for external libraries whose public API is not closure-list based.

## Required Workloads

At minimum, the new suite should include the following families.

1. `zero_work`

- Purpose: estimate scheduler overhead with as little job-body cost as possible.
- Requirement: do not use `Instant::now()` loops for this case.
- Requirement: keep side effects minimal but sufficient to prevent job elimination.

2. `wide_independent`

- Purpose: show best-case parallel throughput and runtime overhead when there are effectively no conflicts.

3. `strict_chain`

- Purpose: show pure dependency-order overhead and critical-path behavior.

4. `layer_barrier_stress`

- Purpose: expose the whole-group barrier cost in `02`.
- Example shape: neighboring jobs create only partial cross-layer conflicts so `01` and `03` can overlap more than `02`.

5. `straggler_partial_overlap`

- Purpose: expose whether one long job unnecessarily blocks otherwise ready work.
- Expected to separate `02` from `01` and `03`.

6. `hot_key_write_contention`

- Purpose: expose heavy conflict handling and wakeup behavior.
- Useful for comparing page reservation vs static orientation vs groups.

7. `read_heavy_shared_key`

- Purpose: verify that shared-read concurrency is not accidentally lost.

8. `mixed_hotspots`

- Purpose: combine multiple resources, mixed strict/relax order, and uneven durations.
- This should resemble the most realistic synthetic case.

9. `fan_out_fan_in`

- Purpose: stress DAG quality and wakeup efficiency.
- Useful for `03`, and also for observing whether `01` follower/page scans stay productive.

10. `compile_heavy_sparse_keys`

- Purpose: stress schedule-time logic on large graphs where runtime work is tiny.
- This is important because the user explicitly accepts large schedule-time cost.

## Special Notes For `0ms` Workloads

The `0ms` benchmark should not accidentally benchmark:

- `Instant::now()` polling
- heavy checksum atomics
- state contention unrelated to the scheduler

Possible approach:

- create a minimal job body that performs one tiny deterministic side effect, or a per-job `black_box` transform
- consider a dedicated `ZeroWorkState` separate from the heavier current `BenchState`

If necessary, keep both:

- `zero_work_minimal`
- `zero_work_with_shared_state`

## External Library Compatibility Requirements

The workload IR must support adapters for libraries that:

- build a schedule ahead of time
- need a world/resource object instead of plain closures
- need explicit ordering edges in addition to read/write access declarations

Do not hard-code the harness around the current local scheduler API only.

## Likely Files To Change

- `Cargo.toml`
- `benches/experiment.rs`
- new files under `benches/` for shared support, if helpful
- possibly small shared benchmark-facing types in `src/experiment` only if that clearly reduces duplication

## Concrete Execution Plan

1. Freeze the old harness behavior by reading and understanding it before changing structure.
2. Decide the benchmark IR and adapter trait shape.
3. Refactor the harness so local schedulers `01`, `02`, and `03` all use that abstraction.
4. Add the `0ms` benchmark.
5. Add the workload families listed above.
6. Calibrate Criterion settings so compile-heavy and run-heavy suites both remain practical to execute.
7. Leave obvious hooks for external library adapters.
8. Record which benchmark names are intended to expose which scheduler tradeoff.

## Acceptance Criteria

- `cargo bench --bench experiment` compiles and runs with the refactored harness.
- All current local schedulers benchmark through one shared abstraction.
- There is a standard `0ms` benchmark family.
- Benchmark names and workload categories make the design intent obvious.
- The new workload set includes cases that should favor each scheduler differently.
- The task file is updated with any deviations from this plan.

## Progress Log

- 2026-04-04: Task created. No benchmark refactor yet.
