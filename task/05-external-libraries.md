# Task 05: External Library Research And Integration

Last updated: 2026-04-04
Status: not started
Priority: high

## Goal

Find, study, and benchmark external libraries that implement a meaningfully similar scheduler or executor.

This task has two purposes:

1. establish stronger performance baselines than the local experiments alone
2. learn implementation ideas worth importing into local experiments and the new `04`

## What Counts As "Similar"

A candidate library should satisfy most of the following:

- it schedules work for parallel execution
- it reasons about conflicts, dependencies, access sets, stages, or DAG structure
- it can be compiled or configured once and run repeatedly
- it is benchmarkable under a synthetic workload harness without extreme adapter distortion

Libraries that only provide a thread pool without scheduling logic are not enough by themselves.

## Initial Candidate Leads

These are leads, not committed choices. Verify them before integrating.

- `bevy_ecs` scheduler / systems execution
- `shipyard` workload scheduling
- `legion` scheduling
- at least one explicit DAG/task-graph executor crate discovered during research

Rationale:

- ECS schedulers are relevant because they optimize read/write-based system parallelism, which is very close to this project's dependency model.
- A DAG/task-graph executor is relevant because `experiment_03` is explicitly a static DAG design.

## Research Checklist Per Library

For each candidate, collect and record:

- crate and repository
- license
- how it models dependencies or access conflicts
- whether it has a compile-once / run-many flow
- how it executes work at runtime
- whether it preserves ordering or only exclusivity
- how hard it is to map this project's `Relax` vs `Strict`
- whether it is fair to benchmark directly
- the most interesting implementation ideas learned from its source

If a library is rejected, record why.

## Benchmark Mapping Requirements

The adapter for an external library must preserve benchmark semantics as fairly as possible.

Special care is needed for:

### `Relax`

- The library must prevent overlap but allow reordering when possible.
- If the library lacks native relaxed-vs-strict distinction, document the approximation used.

### `Strict`

- The library must preserve declaration order or an equivalent explicit precedence.
- If the library lacks native ordering edges, emulate strict order with stages, explicit dependencies, or wrapper nodes.

### `Unknown`

- The library must treat this as a barrier-like dependency.

If a library cannot be made semantically comparable without excessive distortion, reject it.

## Likely Files To Change

- `Cargo.toml`
- `benches/experiment.rs`
- new benchmark support modules or adapters
- possibly additional notes under `task/` if the research becomes large

## Deliverables

Minimum expected output from this task:

- selection of at least two external libraries to benchmark
- a short research note per selected library
- a clear statement of how each library maps to the benchmark IR
- benchmark adapters integrated into the shared harness
- notes on implementation ideas worth borrowing

## Recommended Execution Plan

1. Browse candidate libraries and their source.
2. Shortlist only those with a fair semantic mapping.
3. Record the mapping and the reason for selection.
4. Update `task/01-benchmark-foundation.md` if adapter requirements change.
5. Implement benchmark adapters only after the semantic mapping is written down.
6. Add benchmark entries and compare against local experiments.
7. Record lessons that affect tasks `02`, `03`, `04`, and `06`.

## Lessons To Look For

Specifically look for ideas such as:

- better access-set indexing
- stronger compile-time batching or graph optimization
- wakeup strategies
- fairness mechanisms
- schedule caching
- lock-based designs that win in practice despite higher theoretical overhead

## Acceptance Criteria

- At least two external schedulers are integrated into the benchmark suite, or rejected with explicit technical reasons if only one fair comparison exists.
- The mapping from benchmark IR to each library is documented.
- The task file contains enough detail for a future agent to explain why each library was chosen.
- Any ideas imported into local experiments are referenced here.

## Progress Log

- 2026-04-04: Task created. No external library research yet.
