# Task 06: Design And Implement Experiment 04

Last updated: 2026-04-04
Status: not started
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

## Default Design Direction: Portfolio Scheduler

The default planned direction is a portfolio scheduler.

Core idea:

- spend even more compile-time and memory than `03`
- generate multiple legal schedule variants for the same job set
- each variant optimizes for a different objective
- runtime chooses one variant cheaply for each run, or benchmarking forces a fixed variant for fairness

Why this is contrastive:

- `01` keeps relaxed conflicts dynamic at runtime
- `02` commits to one group layering
- `03` commits to one static orientation
- `04` would explicitly refuse to believe that one schedule shape is always best

This takes the "compile once, run many" assumption to an extreme.

## Example Variant Families

The portfolio could contain variants such as:

- critical-path-first static DAG
- width-maximizing static DAG
- locality-biased schedule that tries to keep related resources hot
- contention-avoidance orientation that tries to spread hot writes

The runtime policy could be one of:

- fixed variant selected by benchmark case
- round-robin across variants for exploration
- choose the best historical variant based on prior run timing

For benchmarking fairness, fixed-variant mode should exist even if adaptive mode also exists.

## Important Implementation Question

Multiple variants need access to the same job closures.

That means the design likely needs one of:

- shared job storage referenced by multiple compiled metadata variants
- `Arc`-backed job closures
- another structure that avoids cloning the actual closures per variant

This must be resolved before coding begins.

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
