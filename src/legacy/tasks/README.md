# Scheduler Experiment Task Plan

Last updated: 2026-04-05
Plan status: original tasks `00-07` are completed; post-plan task `08` is in progress

## Mission

Explore the scheduler experiments as a research program, not as isolated code tweaks.

The user-defined priorities are:

- maximize safe job parallelism above all else
- accept higher schedule-time cost if it improves repeated-run parallelism
- do not preserve lock-free implementation style as a goal
- preserve the high-level identity of experiments `01`, `02`, and `03`
- compare against external libraries that solve a similar scheduling problem
- design a new `04` experiment with a clearly contrastive approach
- make benchmarks expose parallelism differences, not just generic throughput
- add a standard `0ms` job benchmark to estimate runner overhead

The original implementation work is complete. This folder is now also the
ongoing research dossier for post-plan work such as `experiment_05`.

## Important Context

- The repository was rechecked during task `00`; `git status --short` was clean at that time.
- The shared experiment API lives in `src/experiment/mod.rs`.
- The current benchmark harness is `benches/experiment.rs`.
- The current experiments are:
  - `src/experiment_01`
  - `src/experiment_02`
  - `src/experiment_03`
  - `src/experiment_04`
- Shared correctness tests live in `tests/support/mod.rs` and the per-experiment test entry points under `tests/experiment_0{1,2,3,4}.rs`.

## Recommended Execution Order

The order below is recommended, not mandatory, but it is designed to reduce rework.

1. Read `task/00-current-state.md`.
2. Execute `task/05-external-libraries.md` enough to choose which external schedulers will be benchmarked.
3. Execute `task/01-benchmark-foundation.md` so the benchmark harness can compare all schedulers fairly.
4. Execute `task/02-experiment-01.md`.
5. Execute `task/03-experiment-02.md`.
6. Execute `task/04-experiment-03.md`.
7. Execute `task/06-experiment-04.md`.
8. Execute `task/07-validation-and-results.md`.

Tasks `02`, `03`, and `04` can overlap conceptually once task `01` is in place, but benchmarking and documentation should stay synchronized.

## Task Board

| Task | Status | Depends on | Purpose |
| --- | --- | --- | --- |
| `00-current-state.md` | Completed | none | Cold-start summary of the existing repo and likely bottlenecks |
| `01-benchmark-foundation.md` | Completed | partial input from `05` | Refactor benchmarks into a reusable comparison framework and add new workloads |
| `02-experiment-01.md` | Completed | `01` preferred | Improve the dynamic reservation scheduler while preserving its identity |
| `03-experiment-02.md` | Completed | `01` preferred | Improve the layered baseline while keeping it a layered scheduler |
| `04-experiment-03.md` | Completed | `01` preferred | Improve the static DAG scheduler while keeping it a static DAG scheduler |
| `05-external-libraries.md` | Completed | none | Research, select, integrate, and document external schedulers for comparison |
| `06-experiment-04.md` | Completed | `01`, `05`, plus findings from `02-04` | Design and implement the new contrastive experiment |
| `07-validation-and-results.md` | Completed | all prior tasks | Final tests, benchmark runs, results summary, and doc cleanup |

## Rules For Future Agents

- Update this file whenever task status changes materially.
- Update the specific task file you worked on in the same turn.
- Keep a short progress log in each task file with dates and concrete outcomes.
- If the final design of `experiment_04` changes, update both `task/06-experiment-04.md` and this file before coding.
- If external library selection changes, update both `task/05-external-libraries.md` and `task/01-benchmark-foundation.md`.
- If ECS competitor integration changes, also update:
  - `task/05-dagga.md`
  - `task/05-dag-exec.md`
  - `task/05-shipyard.md`
  - `task/05-legion.md`
  - `task/05-bevy-ecs.md`
  - `task/05-flecs.md`
- Benchmark notes should record:
  - command used
  - machine parallelism used
  - whether the run was compile-only or run-time
  - which workloads showed meaningful differences

## Definition Of Success

The overall project is done when:

- experiments `01`, `02`, `03`, and the new `04` all compile behind the shared experiment API
- the benchmark harness can compare local schedulers and selected external schedulers through one coherent abstraction
- there is a standard `0ms` benchmark for all schedulers
- the workloads clearly expose where each scheduler wins or loses on parallelism
- at least two relevant external schedulers are benchmarked and their implementation ideas are documented
- the direct ECS competitors requested by the user are benchmarked through mappings that are explicit about any semantic caveats
- the task files in this folder reflect the actual final state and decisions
- post-plan research tasks document prototype implementations and measured
  outcomes as they evolve

## First File To Read

If you are starting fresh, read `task/00-current-state.md` next.

## Progress Log

- 2026-04-04: Created the initial task dossier.
- 2026-04-04: Completed task `00` and verified the repo was clean at that time.
- 2026-04-04: Completed task `01` with a refactored benchmark IR, local adapter layer, standard `0ms` suite, and new parallelism-oriented workloads.
- 2026-04-04: Completed task `05` first by integrating `dagga` and `dag_exec`.
- 2026-04-04: Revised task `05` after the user required direct ECS benchmarking, then integrated `shipyard`, `legion`, `bevy_ecs`, and `flecs` and added one deep-dive task file per ECS competitor.
- 2026-04-05: Completed task `02` by replacing `experiment_01`'s broad ready-bit rescans with targeted page waiter queues and by improving relaxed page packing with neighborhood-overlap heuristics.
- 2026-04-05: Completed task `03` by upgrading `experiment_02` to a dual-layout compiler that compares classic first-fit grouping against a stricter frontier-batch heuristic and keeps the better thread-aware layout while preserving the same group-by-group runtime.
- 2026-04-05: Completed task `04` by keeping `experiment_03` as a static DAG scheduler but switching its executor to criticality-ordered work-first wakeups, which sharply reduced per-ready-node runtime overhead on the benchmark harness.
- 2026-04-05: Completed task `06` by implementing `experiment_04` as a portfolio of static-DAG variants with adaptive selection, fixed benchmarkable modes, and a new `portfolio_bridge_tradeoff` workload family that exposes when multiple legal orientations materially differ.
- 2026-04-05: Completed task `07` by passing the final hygiene and test checks, attempting the full benchmark sweep, then finishing with a bounded representative final matrix and a dedicated summary in `task/07-results-summary.md`.
- 2026-04-05: Added post-plan task `08-experiment-05.md`, built the first working `experiment_05` prototype, integrated it into the benchmark harness, and recorded the first grounded performance results.
- 2026-04-05: Massively optimized `experiment_05` with cluster runtime modes, work-first inter-cluster chaining, cheaper owner-thread state publication, and a schedule-level trivial-parallel fast path, then reran the key benchmarks sequentially to record the improved results.
- 2026-04-05: Continued `experiment_05` optimization with schedule-level singleton-DAG and serial-chain specializations, pushing the writer-hotspot `0ms` case down to about `1.01 µs` and further improving the bridge benchmarks.
- 2026-04-05: Revised the benchmark harness again so the primary comparison is a deterministic fixed-seed `realistic_random_main` scenario with `384` resources, `2048` jobs, mixed read/write and strict/relaxed constraints, explicit predecessor hints, target work from `0.0ms` to `2.5ms`, and a dedicated `run_mainline` suite that stays separate from compile timing.
