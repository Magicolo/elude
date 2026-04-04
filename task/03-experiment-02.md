# Task 03: Improve Experiment 02

Last updated: 2026-04-04
Status: not started
Priority: medium-high

## Goal

Improve `experiment_02` while preserving its identity:

- compile jobs into sequential groups
- keep runtime simple and group-by-group
- accept that the unit of runtime synchronization is a group

This task is not about making `02` imitate `01` or `03`. It is about finding how far the layered design can be pushed before its barrier model fundamentally limits it.

## Relevant Files

- `src/experiment_02/README.md`
- `src/experiment_02/DESIGN.md`
- `src/experiment_02/mod.rs`
- `src/experiment_02/model.rs`
- `tests/experiment_02.rs`
- `tests/support/mod.rs`
- `benches/experiment.rs`

## Current Design Summary

`experiment_02` currently:

- normalizes dependencies
- tracks per-key group history
- computes the earliest legal group
- scans forward to the first compatible group
- executes groups sequentially, with parallelism only inside each group

The core current functions are:

- `earliest_group`
- `GroupSummary::is_compatible`
- `update_trackers`
- the compile-time group scan loop

## High-Value Hypotheses To Test

1. Group placement quality is weaker than it needs to be.

- The current algorithm effectively uses a first-fit scan from the earliest legal group.
- A different placement heuristic may reduce total group count or critical-path barriers while keeping the same runtime model.

2. Group summaries are more expensive than necessary.

- Current summaries use `HashMap<Key, GroupAccess>`.
- Dense benchmark keys may benefit from indexed or bitset-style summaries.

3. The scheduler could remain layered while becoming more parallel in practice.

- Better compile placement may increase average group width and reduce unnecessary later groups.
- This is especially relevant in partially overlapping relaxed-conflict workloads.

## Improvement Directions To Explore

### A. Better group selection heuristics

Potential ideas:

- best-fit instead of first-fit when multiple compatible groups exist
- heuristics that minimize future conflict blockage, not just current compatibility
- duration-aware placement if benchmark IR exposes a cost hint
- separate treatment for hot keys versus sparse keys

### B. Faster group compatibility checks

Potential ideas:

- indexed summaries for `Key::Identifier`
- small-vector or sorted-slice summaries for tiny groups
- bitset summaries when workload keys are dense and bounded

### C. Better compile-time frontier tracking

Potential ideas:

- maintain likely candidate groups per key to reduce forward scanning
- cache the last compatible or incompatible region for recurring key patterns

### D. Optional group-local ordering improvements

These are secondary and must not change semantics.

Potential ideas:

- reorder jobs inside a group for cache locality
- stable ordering choices that reduce benchmark noise

## Constraints

- Runtime must remain a sequential loop over groups.
- Do not add a runtime per-job wakeup engine.
- Do not add runtime conflict reservations.
- If the compile algorithm becomes more complex, document why the extra complexity still belongs in the baseline layered design.

## Recommended Execution Plan

1. Benchmark current `02` under the redesigned harness, especially:
  - `layer_barrier_stress`
  - `straggler_partial_overlap`
  - `zero_work`
2. Determine whether compile-time or run-time is the larger weakness on the new suite.
3. Choose one main compile strategy improvement.
4. Implement it cleanly and keep the runtime loop simple.
5. Re-run tests and compare against pre-change benchmark data.
6. Update this file with the chosen heuristic and observed behavior.

## Potential Dependency Additions

Only if warranted:

- `smallvec`
- `rustc-hash`
- `fixedbitset`

The default should still be a simple implementation. Add dependencies only when they clearly improve clarity or measurable performance.

## Acceptance Criteria

- `experiment_02` still runs one group at a time.
- Shared tests still pass.
- Benchmark results clearly show whether group placement improved or not.
- Any remaining barrier-driven losses are documented rather than hand-waved.
- This task file records the final heuristic and tradeoffs.

## Progress Log

- 2026-04-04: Task created. No implementation yet.
