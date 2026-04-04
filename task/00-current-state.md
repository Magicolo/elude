# Current State Reference

Last updated: 2026-04-04
Status: reference document

## Purpose

This file is a cold-start map of the repository as it existed when the task plan was created. Read this before touching any scheduler task.

## Shared Public Surface

The shared experiment-facing API is in `src/experiment/mod.rs`.

The important types and traits are:

- `experiment::Job<S>`: closure plus declared dependencies
- `experiment::Scheduler<S>`: builder plus `schedule()`
- `experiment::CompiledSchedule<S>`: repeated-run execution surface

All experiments currently follow:

`Scheduler::new().add(Job::new(...)).schedule()?.run(&state)?`

Jobs take `&S`, not `&mut S`. Any benchmark or external adapter must preserve that soundness model: the scheduler manages logical concurrency, while mutation happens through interior mutability or explicit synchronization inside `S`.

## Shared Correctness Coverage

Behavioral tests are centralized in `tests/support/mod.rs`. All current experiments run the same suite through separate test entry points.

The existing suite verifies:

- empty schedules run
- repeated runs execute jobs exactly once per run
- pairwise dependency semantics for `None`, `Relax`, `Strict`, and `Unknown`
- invalid internal dependency combinations are rejected
- duplicate reads within one job are accepted
- strict order is preserved
- simple overlap cases actually overlap on a two-thread scheduler

The suite is good for semantic correctness. It is not a performance or parallelism-diagnostic suite.

## Current Benchmark Harness

The current Criterion harness is `benches/experiment.rs`.

What it currently does:

- benchmarks compile time and run time separately
- builds workloads from a small `Workload` struct
- varies:
  - number of jobs
  - conflict family
  - simulated per-job runtime in milliseconds
- compares `experiment_01`, `experiment_02`, and `experiment_03`

Current limitations:

- no external-library adapters
- no standard `0ms` workload
- current job body always uses `Instant::now()` loops and atomic checksum updates, which means very short jobs measure more than scheduler overhead
- workload matrix is broad but not explicitly designed to expose the known design differences between:
  - dynamic reservation
  - sequential groups
  - static DAG orientation
- no benchmark naming or structure for:
  - barrier sensitivity
  - straggler sensitivity
  - cross-layer overlap opportunities
  - compile-heavy graph quality heuristics

## Experiment 01 Snapshot

Location:

- `src/experiment_01/README.md`
- `src/experiment_01/DESIGN.md`
- `src/experiment_01/model.rs`

High-level identity:

- compile strict conflicts into predecessor counters plus flat successor lists
- compile relaxed conflicts into page reservation masks
- runtime tries jobs dynamically using page reservations and ready-bit rescans

Important current functions and structures:

- `Schedule::compile`
- `reduce_strict`
- `assign_homes`
- `attempt_job`
- `run_job`
- `reserve_page`
- `scan_page`
- `followers_by_page` construction inside compile

What it is currently optimizing for:

- recover parallelism beyond sequential layers
- keep runtime representation flat and compact
- avoid heavyweight per-job graph state

Likely bottlenecks or investigation targets:

- `O(n^2)` pairwise relation classification
- simple page assignment heuristic: connected relaxed components, degree sort, chunks of 32
- potentially noisy follower-page scans after every completion
- ready-bit rescans may do useless work when conflicts are dense
- repeated CAS retries on hot pages
- no explicit fairness strategy

What must remain true if this experiment is improved:

- it should still be recognizably the dynamic reservation scheduler
- it should not collapse into pure sequential groups
- it should not collapse into a pre-oriented static DAG only

## Experiment 02 Snapshot

Location:

- `src/experiment_02/README.md`
- `src/experiment_02/DESIGN.md`
- `src/experiment_02/model.rs`

High-level identity:

- compile jobs into sequential groups that are safe to run in parallel internally
- runtime executes one whole group at a time

Important current functions and structures:

- `earliest_group`
- `GroupSummary::is_compatible`
- `update_trackers`
- compile-time forward scan for first compatible group
- runtime group loop using Rayon `par_iter`

What it is currently optimizing for:

- tiny runtime overhead
- low scheduler-owned synchronization at run time
- repeated runs of a precomputed grouping

Likely bottlenecks or investigation targets:

- group barriers hide parallelism
- first-fit forward scan may produce avoidable barriers
- `HashMap<Key, GroupAccess>` summaries may be expensive and cache-unfriendly
- compile logic duplicates dependency normalization patterns from other experiments
- no use of workload-specific cost hints

What must remain true if this experiment is improved:

- it should still be a layered scheduler
- runtime should remain group-by-group, not per-job dynamic scheduling

## Experiment 03 Snapshot

Location:

- `src/experiment_03/README.md`
- `src/experiment_03/DESIGN.md`
- `src/experiment_03/model.rs`

High-level identity:

- compile a strict DAG
- choose a priority order that extends the strict DAG
- orient relaxed conflicts statically using that order
- run a flat DAG executor with predecessor counters and direct successor wakeups

Important current functions and structures:

- `build_priority_order`
- `build_job_stats`
- `build_final_predecessors`
- `reduce_predecessors_declaration_order`
- `reduce_predecessors_priority_order`
- `spawn_job`
- `run_job`

What it is currently optimizing for:

- very small runtime hot path
- finer granularity than sequential groups
- no runtime reservation protocol

Likely bottlenecks or investigation targets:

- current heuristic for priority order is simple and may leave parallelism on the table
- per-key sparse orientation ignores some multi-key coupling effects
- compile time is already heavy and can likely be increased further if graph quality improves
- like `01`, it pays `O(n^2)` pairwise relation cost

What must remain true if this experiment is improved:

- it should still be a static DAG executor
- it should not grow a runtime conflict reservation protocol
- it should not become a grouped/barrier scheduler

## Cross-Cutting Observations

1. Shared logic is duplicated.

- Dependency normalization is implemented separately in all three experiments.
- Pairwise relation classification logic exists separately in `01` and `03`.
- This is acceptable for experiments, but adding external schedulers and `04` will increase maintenance pressure. A shared helper layer may be worth introducing if it does not distort the experiment boundaries.

2. Benchmarks are currently the weakest link.

- The existing harness compares implementations, but it does not yet isolate the key research question: where does each scheduler expose more or less parallelism?
- Workload redesign is not optional. It is foundational.

3. The lock-free requirement has been dropped.

- This materially changes the search space for `01` and for the new `04`.
- It also means external-library lessons from lock-based schedulers are now directly relevant.

4. The repo already assumes repeated schedule execution is important.

- `01` and `03` both explicitly optimize for compile-once / run-many.
- `04` should take that assumption seriously rather than treating each run as independent.

## Suggested Commands For Future Work

Read-only inspection:

- `rg --files`
- `sed -n '1,240p' benches/experiment.rs`
- `sed -n '1,260p' src/experiment_01/model.rs`
- `sed -n '1,260p' src/experiment_02/model.rs`
- `sed -n '1,260p' src/experiment_03/model.rs`

Likely validation commands later:

- `cargo test`
- `cargo test --test experiment_01`
- `cargo test --test experiment_02`
- `cargo test --test experiment_03`
- `cargo bench --bench experiment`

## Progress Log

- 2026-04-04: File created from a repo reading pass. No implementation changes yet.
