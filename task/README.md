# Scheduler Experiment Task Plan

Last updated: 2026-04-04
Plan status: planning only

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

No implementation was performed in this planning pass. This folder is the execution dossier for later prompts.

## Important Context

- The worktree was already dirty on 2026-04-04. Do not assume a clean repository and do not revert unrelated changes.
- The shared experiment API lives in `src/experiment/mod.rs`.
- The current benchmark harness is `benches/experiment.rs`.
- The current experiments are:
  - `src/experiment_01`
  - `src/experiment_02`
  - `src/experiment_03`
- Shared correctness tests live in `tests/support/mod.rs` and the per-experiment test entry points under `tests/experiment_0{1,2,3}.rs`.

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
| `00-current-state.md` | Reference | none | Cold-start summary of the existing repo and likely bottlenecks |
| `01-benchmark-foundation.md` | Not started | partial input from `05` | Refactor benchmarks into a reusable comparison framework and add new workloads |
| `02-experiment-01.md` | Not started | `01` preferred | Improve the dynamic reservation scheduler while preserving its identity |
| `03-experiment-02.md` | Not started | `01` preferred | Improve the layered baseline while keeping it a layered scheduler |
| `04-experiment-03.md` | Not started | `01` preferred | Improve the static DAG scheduler while keeping it a static DAG scheduler |
| `05-external-libraries.md` | Not started | none | Research, select, and integrate external schedulers for comparison |
| `06-experiment-04.md` | Not started | `01`, `05`, plus findings from `02-04` | Design and implement the new contrastive experiment |
| `07-validation-and-results.md` | Not started | all prior tasks | Final tests, benchmark runs, results summary, and doc cleanup |

## Rules For Future Agents

- Update this file whenever task status changes materially.
- Update the specific task file you worked on in the same turn.
- Keep a short progress log in each task file with dates and concrete outcomes.
- If the final design of `experiment_04` changes, update both `task/06-experiment-04.md` and this file before coding.
- If external library selection changes, update both `task/05-external-libraries.md` and `task/01-benchmark-foundation.md`.
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
- the task files in this folder reflect the actual final state and decisions

## First File To Read

If you are starting fresh, read `task/00-current-state.md` next.
