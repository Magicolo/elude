# Task 02: Improve Experiment 01

Last updated: 2026-04-04
Status: not started
Priority: high

## Goal

Improve `experiment_01` while preserving its identity:

- compile strict conflicts into counters and successor lists
- handle relaxed conflicts dynamically at runtime
- keep a flat runtime representation
- prioritize maximum safe parallelism over low scheduler overhead

The user explicitly removed the lock-free requirement. That opens up valid new implementation directions as long as the scheduler still behaves like the dynamic reservation experiment.

## Relevant Files

- `src/experiment_01/README.md`
- `src/experiment_01/DESIGN.md`
- `src/experiment_01/mod.rs`
- `src/experiment_01/model.rs`
- `tests/experiment_01.rs`
- `tests/support/mod.rs`
- `benches/experiment.rs`

## Current Design Summary

`experiment_01` currently:

- classifies pairwise job relations
- reduces strict edges
- assigns home pages for relaxed-conflict neighborhoods
- emits per-job multi-page acquire masks
- uses runtime page reservations plus ready-bit rescans

The most relevant current functions are:

- `reduce_strict`
- `assign_homes`
- `attempt_job`
- `run_job`
- `reserve_page`
- `scan_page`

## High-Value Hypotheses To Test

1. Page assignment quality is limiting parallelism.

- Current heuristic is simple degree-based packing into 32-slot pages.
- Better packing may shorten acquire lists, reduce follower traffic, and improve effective concurrency.

2. Wakeup strategy is doing too much broad scanning.

- Current runtime rescans the home page and follower pages after job completion.
- A more targeted ready structure may improve both overhead and actual runnable-job discovery.

3. Hot-page contention may be better handled with locks than with repeated CAS retries.

- The project no longer needs to be lock-free.
- Per-page mutexes, reservation queues, or guarded wait-lists are now fair game if they improve practical parallelism or reduce scheduler thrash.

4. The runtime lacks fairness or anti-starvation behavior.

- If one job repeatedly wins reservation races, another may spin through failed attempts.
- This matters most in `0ms` and hot-contention workloads.

## Improvement Directions To Explore

Explore these in order of likely value.

### A. Better page packing

Potential ideas:

- conflict-graph partitioning that explicitly minimizes cross-page acquire spans
- packing by both degree and neighbor overlap, not degree alone
- special treatment for isolated or nearly isolated jobs
- compile-time metrics such as:
  - average acquire-list length
  - maximum acquire-list length
  - follower fan-out per page

### B. More targeted wakeups

Potential ideas:

- per-page ready queues instead of only ready bits
- a per-page list of currently blocked jobs
- dirty flags that avoid rescanning pages known to have no pending work
- batching follower propagation when multiple released pages point to the same follower

### C. Lock-based runtime exploration

Because lock-free is no longer required, consider prototypes such as:

- mutex-guarded page reservation state
- per-page waiter lists protected by `parking_lot::Mutex`
- explicit handoff or ticketing on hot resources

This is not a license to bloat the runtime arbitrarily. The goal is still maximum practical parallelism on repeated runs.

### D. Reduce unnecessary work in the failure path

Current failed acquisition does:

- rollback
- mark ready in home page
- maybe trigger a compensating scan for lost-wakeup avoidance

Investigate whether the same correctness guarantee can be kept with less redundant work under contention.

## Constraints

- Do not turn `01` into a grouped scheduler.
- Do not turn `01` into a purely static DAG scheduler.
- Preserve semantic correctness under the shared tests.
- Any additional metrics or helper data should stay consistent with repeated schedule reuse.

## Recommended Execution Plan

1. Benchmark current `01` under the redesigned harness.
2. Add temporary instrumentation if needed:
  - acquire-list length stats
  - page count
  - follower count
  - failed reservation count
  - rescan count
3. Choose the most promising improvement path based on data, not aesthetic preference.
4. Implement one coherent improvement set rather than many partially overlapping tweaks.
5. Re-run correctness tests and targeted benchmarks.
6. Update `README.md` and this file with what actually changed and why.

## Potential Dependency Additions

Only if justified by the chosen direction:

- `smallvec`
- `rustc-hash`
- `fixedbitset`
- additional synchronization or queue crates

If a dependency is introduced, document exactly what problem it solved.

## Acceptance Criteria

- `experiment_01` still matches its documented design identity.
- Shared scheduler tests still pass.
- Benchmarks show whether the change improved parallelism-sensitive cases, overhead-sensitive cases, or both.
- Any tradeoff that worsened compile time or overhead is documented explicitly.
- This task file records the chosen approach and results.

## Progress Log

- 2026-04-04: Task created. No implementation yet.
