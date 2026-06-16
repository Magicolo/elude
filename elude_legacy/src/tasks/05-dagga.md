# Dagga Competitor Dossier

Last updated: 2026-04-04
Status: integrated into benchmarks
Primary crate: `dagga = 0.2.3`
License: `MIT OR Apache-2.0`
Repository: `https://github.com/schell/dagga`

## Why This File Exists

This file is the cold-start reference for the `DaggaAdapter`.

Read this before changing:

- `DaggaAdapter`
- schedule-time comparison claims
- any future experiment that borrows ideas from batch-oriented global schedule construction

## Primary Sources Read

Primary source used during integration:

- crate source and public API through `cargo info dagga@0.2.3`
- adapter implementation now in `benches/experiment.rs`

## Mental Model

Dagga is a schedule builder, not a complete reusable executor runtime.

It takes:

- nodes
- resource reads
- resource writes
- explicit before/after constraints

and produces:

- a batch schedule

The important design point is that Dagga spends work up front to place nodes into batches that preserve correctness and maximize batch-level parallelism.

That makes it one of the cleanest direct comparisons for this repository's resource-conflict scheduling problem.

## Benchmark Adapter Design

Adapter name:

- `DaggaAdapter`

Compiled state:

- `DaggaCompiled { schedule, pool }`

Exact mapping:

- benchmark resource ids map directly to Dagga's generic resource keys
- relaxed conflicts stay native through `with_reads(...)` and `with_writes(...)`
- strict declaration-order overlays become explicit `run_after(...)`
- `Unknown` becomes a barrier-like predecessor/successor split
- runtime execution:
  - batches are run sequentially
  - jobs inside each batch run in parallel on a benchmark-local Rayon pool

Why the adapter owns execution:

- Dagga itself gives a schedule, not a reusable execution engine

## Comparison To Local Experiments

Closest local comparison:

- strongest contrast to `experiment_01` and `experiment_02`

Why:

- it is access-based
- it clearly favors heavier schedule-time reasoning to improve run-time parallel batches

This is exactly the tradeoff the user said should be explored.

## Verification Results

Verified:

- bench target compiles with `cargo bench --bench experiment --no-run`

Important caveat from the earlier pass:

- filtered Dagga Criterion runs were started multiple times and did not return promptly on the current benchmark sizes
- this is recorded as an observed schedule-time characteristic, not as an integration failure

## Resume Notes

If you touch Dagga later:

1. Keep relaxed conflicts native.
2. Keep strict overlays limited to explicit edges only where required.
3. Re-run at least one compile-heavy workload after changes, because Dagga's main signal is schedule-time behavior.

## Progress Log

- 2026-04-04: Integrated `DaggaAdapter` into the shared benchmark harness.
