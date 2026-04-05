# Task 06: Design And Implement Experiment 04

Last updated: 2026-04-05
Status: completed
Priority: high

## Goal

Create a new `experiment_04` with a deliberately contrastive approach.

This experiment should not be a minor variation of:

- `01` dynamic page reservation
- `02` sequential parallel groups
- `03` single optimized static DAG

It should explore a different tradeoff space and be justified by lessons learned from:

- the improved benchmark suite
- external library research
- observed strengths and weaknesses of `01`, `02`, and `03`

## Decision Gate

Do not finalize `04` before:

- task `05` has produced external-library notes
- task `01` has produced the new benchmark harness
- at least a first benchmark pass exists for the current or improved `01`, `02`, and `03`

Before coding `04`, update this file with the final design choice if it changes from the default direction below.

## Final Design Direction: Portfolio Scheduler

The chosen direction remains the portfolio scheduler.

Concrete design:

- spend more compile-time and memory than `03`
- generate multiple legal static-DAG variants for the same normalized job set
- share the actual job closures across all variants
- let each variant use a different topological priority heuristic
- use a tiny adaptive runtime selector for repeated runs
- expose fixed-variant mode so benchmarks can pin a single variant when needed

Why this is still contrastive:

- `01` keeps relaxed conflicts dynamic at runtime
- `02` commits to one group layering
- `03` commits to one static orientation
- `04` explicitly refuses to believe that one schedule shape is always best

This takes the "compile once, run many" assumption to an extreme.

## Chosen Variant Family

The implemented scheduler compiles three variants:

1. `critical_path`

- closest to `experiment_03`
- favors long strict-path height and strong unlock potential
- intended to do well on layered, chain-heavy, and straggler workloads

2. `read_heavy`

- prefers read-biased jobs and delays writes when legal
- intended to preserve wider read batches on hot shared resources
- intended to do well on mixed-hotspot and shared-read-heavy workloads

3. `hot_contention`

- favors jobs with many hot writes or contention-heavy resource touches
- intended to burn through serialized choke points earlier
- intended to do well when hot writers dominate the graph

The implemented heuristics do keep those three goals distinct:

- `critical_path`
  - prioritizes strict height, unlock fanout, relaxed degree, and hot writes
- `read_heavy`
  - prioritizes strict height, reads, hot-read score, and delaying writes
- `hot_contention`
  - prioritizes strict height, hot-write score, relaxed degree, and writes

## Chosen Runtime Policy

Implemented default runtime policy:

- run each variant during a short warmup phase
- then pick the best measured variant for repeated runs
- occasionally re-sample non-selected variants at a low rate so the schedule is
  adaptive rather than permanently sticky after the first samples

Implemented benchmark/fairness policy:

- fixed mode compiles only the selected variant
- benchmark harness now includes:
  - adaptive `experiment_04`
  - `experiment_04_critical_path`
  - `experiment_04_read_heavy`
  - `experiment_04_hot_contention`

## Shared Job Storage Decision

Multiple variants will not clone job closures.

Implemented storage:

- convert runtime closures into shared `Arc`-backed job storage
- store per-variant execution metadata separately
- keep per-variant predecessor counters independent

This keeps the portfolio memory-heavy but not closure-duplication-heavy.

## Why This Beat The Fallback

The fallback lock-based ready-queue scheduler remains plausible, but it was not
chosen because:

- task `02` already explored a dynamic runtime-heavy direction
- task `03` showed that runtime hot-path shape matters a lot
- the benchmark suite and external-library research both reinforce that
  schedule-shape quality can dominate behavior when runs are repeated
- a portfolio of contrasting static DAGs is a cleaner "surprising" extension of
  the repository's existing lessons

## Fallback Design If Portfolio Proves Too Distorted

If the portfolio approach turns out not to be a good contrast after tasks `01-05`, the fallback direction is:

- a lock-based queue scheduler with explicit per-resource waiter structures and a global ready queue

That fallback would still be contrastive because:

- it embraces lock-based coordination directly
- it is neither a grouped scheduler nor a static DAG
- it can use explicit waiting structures rather than `01` page masks

If the fallback is chosen, update this file and `task/README.md` first.

## Likely Files To Add

- `src/experiment_04/mod.rs`
- `src/experiment_04/README.md`
- `src/experiment_04/DESIGN.md`
- `src/experiment_04/model.rs`
- `tests/experiment_04.rs`
- updates to `src/lib.rs`
- benchmark harness integration

## Required Outputs

- a documented design rationale
- a compiled scheduler implementing the shared experiment API
- shared correctness tests wired through a new `tests/experiment_04.rs`
- benchmark integration

## Implemented Files

- `src/experiment_04/mod.rs`
- `src/experiment_04/README.md`
- `src/experiment_04/DESIGN.md`
- `src/experiment_04/model.rs`
- `tests/experiment_04.rs`
- `src/lib.rs`
- `benches/experiment.rs`

## Benchmark Extension Added During This Task

The pre-existing benchmark matrix did not cleanly separate the portfolio
variants, so task `06` also added a new workload family:

- `portfolio_bridge_tradeoff`

Why it was needed:

- the portfolio design is specifically about choosing between competing legal
  orientations
- some of the original workload families did not force a strong enough tradeoff
  to make the fixed variants diverge materially
- without a separating workload, the scheduler would still be correct, but the
  benchmark suite would not prove that the portfolio idea matters

The family creates one bridge writer that conflicts with:

- a pre-bridge read batch on key `A`
- a post-bridge read batch on key `B`

Moving that bridge writer earlier or later changes whether the schedule favors:

- preserving the first read batch
- unlocking the second read batch sooner

This is exactly the kind of tradeoff the portfolio scheduler is supposed to
capture.

## Validation

Correctness commands run:

- `cargo test --test experiment_04`
- `cargo test`

Build/hygiene commands run during implementation:

- `cargo bench --bench experiment --no-run`

Targeted benchmark commands run:

- `cargo bench --bench experiment -- experiment/compile/experiment_04/compile_heavy_sparse_keys`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04_critical_path/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04/mixed_hotspots`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04/read_heavy_shared_key`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04_read_heavy/read_heavy_shared_key`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04/hot_key_write_contention`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04_hot_contention/hot_key_write_contention`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04_read_heavy/portfolio_bridge_tradeoff/pre96_iter6000_bridge400_post32_iter1500`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04_critical_path/portfolio_bridge_tradeoff/pre96_iter6000_bridge400_post32_iter1500`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04/portfolio_bridge_tradeoff/pre96_iter6000_bridge400_post32_iter1500`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04_critical_path/portfolio_bridge_tradeoff/pre32_iter1500_bridge400_post96_iter6000`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04_read_heavy/portfolio_bridge_tradeoff/pre32_iter1500_bridge400_post96_iter6000`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04/portfolio_bridge_tradeoff/pre32_iter1500_bridge400_post96_iter6000`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_03/portfolio_bridge_tradeoff/pre96_iter6000_bridge400_post32_iter1500`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_03/portfolio_bridge_tradeoff/pre32_iter1500_bridge400_post96_iter6000`

## Measured Outcome

Compile cost for adaptive `experiment_04`:

- `compile_heavy_sparse_keys`: `24.226 ms .. 24.437 ms`

Representative runtime results:

- `layer_barrier_stress`
  - adaptive `experiment_04`: `563.15 µs .. 566.03 µs`
  - fixed `critical_path`: `563.97 µs .. 568.87 µs`
- `mixed_hotspots`
  - adaptive `experiment_04`: `1.504 ms .. 1.540 ms`
- `read_heavy_shared_key`
  - adaptive `experiment_04`: `1.369 ms .. 1.375 ms`
  - fixed `read_heavy`: `1.369 ms .. 1.379 ms`
- `hot_key_write_contention`
  - adaptive `experiment_04`: `14.426 ms .. 14.800 ms`
  - fixed `hot_contention`: `14.494 ms .. 15.017 ms`

Most of the pre-existing workload families do not separate the fixed variants
much. That is itself informative: after transitive reduction, many realistic
cases collapse toward the same effective DAG.

The new `portfolio_bridge_tradeoff` family does separate them:

- pre-heavy case `pre96_iter6000_bridge400_post32_iter1500`
  - fixed `read_heavy`: `509.06 µs .. 512.11 µs`
  - fixed `critical_path`: `511.10 µs .. 512.65 µs`
  - adaptive `experiment_04`: `508.98 µs .. 512.48 µs`
  - `experiment_03`: `511.48 µs .. 513.82 µs`
- post-heavy case `pre32_iter1500_bridge400_post96_iter6000`
  - fixed `critical_path`: `458.82 µs .. 463.17 µs`
  - fixed `read_heavy`: `508.94 µs .. 511.53 µs`
  - adaptive `experiment_04`: `461.53 µs .. 472.79 µs`
  - `experiment_03`: `459.21 µs .. 463.57 µs`

Interpretation:

- the portfolio design is real, not just theoretical, because the fixed
  variants diverge materially on the tradeoff workload
- the adaptive policy converges close to the better fixed variant on both
  shapes
- on ordinary workloads, `experiment_04` behaves much like the improved
  `experiment_03`, which is expected because the executor core is shared
- on workloads where no single static orientation is dominant, `experiment_04`
  can avoid the "one heuristic fits all" limitation of `experiment_03`

## Recommended Execution Plan

1. Re-read the benchmark results and library notes.
2. Decide whether the default portfolio plan still looks like the best contrast.
3. Update this file with the final design choice before coding.
4. Write `README.md` and `DESIGN.md` for `experiment_04`.
5. Implement the scheduler.
6. Add tests and benchmark entries.
7. Compare `04` directly against `01`, `02`, `03`, and external libraries on the new workload suite.

## Acceptance Criteria

- `experiment_04` is materially distinct from the prior experiments.
- The design rationale is explicit and documented.
- Shared correctness tests pass.
- The benchmark suite includes `04`.
- The task file records why this design was chosen over other options.

## Progress Log

- 2026-04-04: Task created. Default direction is the portfolio scheduler, but this is intentionally a decision gate, not a locked choice.
- 2026-04-05: Chose the portfolio scheduler as the final direction.
- 2026-04-05: Locked the first implementation to three static-DAG variants with
  adaptive selection plus fixed-variant support for benchmarks.
- 2026-04-05: Implemented `src/experiment_04`, added `tests/experiment_04.rs`,
  wired adaptive and fixed `04` adapters into the benchmark harness, and added
  the `portfolio_bridge_tradeoff` workload family.
- 2026-04-05: Verified that the portfolio variants diverge on the new tradeoff
  workload and that the adaptive default tracks the better fixed variant on
  both tested shapes.
