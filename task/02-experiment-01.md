# Task 02: Improve Experiment 01

Last updated: 2026-04-05
Status: completed
Priority: high

## Goal

Improve `experiment_01` while preserving its identity:

- compile strict conflicts into counters and successor lists
- handle relaxed conflicts dynamically at runtime
- keep a flat runtime representation
- prioritize maximum safe parallelism over low scheduler overhead

The user explicitly removed the lock-free requirement. That allowed targeted
queue-based wakeups under contention without turning `01` into a grouped or
purely static scheduler.

## Relevant Files

- `src/experiment_01/README.md`
- `src/experiment_01/DESIGN.md`
- `src/experiment_01/mod.rs`
- `src/experiment_01/model.rs`
- `tests/experiment_01.rs`
- `tests/support/mod.rs`
- `benches/experiment.rs`

## Final Design Chosen

This task did not try many half-overlapping tweaks. It implemented one coherent
change set:

1. Replace broad ready-bit page rescans with targeted per-page waiter queues.
2. Keep the multi-page reservation-mask runtime model.
3. Improve relaxed page assignment so jobs with overlapping relaxed
   neighborhoods are more likely to share a page.

The resulting scheduler is still recognizably `experiment_01`:

- strict dependencies are still reduced into per-job counters and flat
  successors
- relaxed conflicts are still decided dynamically at run time
- jobs still acquire sorted page masks before they can run
- runtime state is still flat, compact, and reusable across repeated runs

## What Changed In Code

### Runtime wakeup path

The old implementation used:

- a page `AtomicU64`
- ready bits in the upper half
- home-page rescans
- follower-page rescans
- compensating rescans in the failed-acquire path

The new implementation uses:

- a page `AtomicU32` reservation word
- a `parking_lot::Mutex<VecDeque<JobId>>` waiter queue per page
- `waiting_on: AtomicU32` per job to prevent duplicate queueing
- first-blocking-page queueing on failed acquire
- bounded waiter wakeup on page release

This change directly targeted the biggest observed weakness in the original
design: hot relaxed contention caused large amounts of wasted retry work,
especially on zero-work jobs.

### Page packing

The old page assignment heuristic:

- found relaxed connected components
- sorted by degree
- chunked the component into 32-slot pages

The new heuristic still works component-by-component, but page fill is now
greedy by neighborhood affinity:

- seed each page with the highest-degree remaining job
- fill remaining slots with jobs that maximize direct relaxed overlap and
  closed-neighborhood overlap with the page so far

This is not a full graph partitioner. It is a deterministic, compile-time
heavier heuristic intended to reduce acquire span and cross-page hotspot
fragmentation.

## Why This Approach Was Chosen

Baseline measurements before the rewrite made the priority clear:

- `layer_barrier_stress` was already fine because it is mostly strict-edge
  latency, not relaxed-contention wakeup traffic
- `mixed_hotspots` was not obviously dominated by the old wakeup path
- `hot_key_write_contention/jobs256_zero` was catastrophically expensive for a
  zero-work benchmark, with samples around `9.24 ms` to `42.63 ms`

That pointed directly at the failure path and retry strategy, not at strict
edge reduction.

The queue-driven design was chosen because it fixes the real pathology:

- jobs stop getting retried just because their home or follower page was
  rescanned
- a blocked job only waits on a page that definitely prevented progress
- a released page wakes a bounded set of jobs that were actually blocked there

## Verification

### Tests run

Commands:

- `cargo test --test experiment_01`
- `cargo test assign_homes_groups_similar_relaxed_neighborhoods`

Results:

- shared public `experiment_01` integration tests passed
- the new internal page-packing unit test passed

### Benchmarks run

Commands:

- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_01/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_01/mixed_hotspots`
- `cargo bench --bench experiment -- experiment/run_overhead/experiment_01/hot_key_write_contention`

Machine parallelism:

- used `thread::available_parallelism()` through the benchmark harness

Results:

- `layer_barrier_stress`
  - before: `[603.80 µs 657.44 µs 712.61 µs]`
  - after: `[679.03 µs 685.71 µs 692.88 µs]`
  - interpretation: no statistically meaningful change
- `mixed_hotspots`
  - before: `[2.1640 ms 2.2672 ms 2.3645 ms]`
  - after: `[2.2125 ms 2.2776 ms 2.3638 ms]`
  - interpretation: no statistically meaningful change
- `hot_key_write_contention/jobs256_zero`
  - before: `[9.2419 ms 20.742 ms 42.629 ms]`
  - after: `[425.57 µs 431.99 µs 438.52 µs]`
  - interpretation: enormous overhead improvement on the contention path the
    rewrite was targeting

## Tradeoffs

- Compile-time cost is expected to be higher because page packing is more
  expensive than simple degree chunking.
- This pass did not capture a pre/post compile benchmark, so that compile-time
  cost is expected but not numerically quantified here.
- The runtime now uses narrow mutex-backed page waiters. This was an explicit
  trade accepted by the user in exchange for better practical parallelism under
  contention.
- Fairness is improved relative to blind rescans, but there is still no formal
  fairness guarantee.

## Dependencies

No new dependency was added for this task.

## Recommended Follow-On Work

- Use the new external baselines from task `05` when tuning `02` further.
- If compile-time cost becomes a concern, benchmark the page-packing heuristic
  directly and consider a cheaper fallback path for very large dense
  components.
- Compare the new waiter model against ideas from `shipyard`, `legion`, and
  `bevy_ecs` once tasks `03` and `04` are underway.

## Progress Log

- 2026-04-04: Task created. No implementation yet.
- 2026-04-05: Measured the pre-change `experiment_01` baseline on targeted
  run-time workloads.
- 2026-04-05: Replaced ready-bit/follower rescans with targeted page waiter
  queues and added per-job `waiting_on` state.
- 2026-04-05: Reworked relaxed page assignment to pack by neighborhood overlap
  instead of plain degree chunking.
- 2026-04-05: Updated `experiment_01` docs and verified tests plus targeted
  benchmarks. Task complete.
