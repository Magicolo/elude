# Dag Exec Competitor Dossier

Last updated: 2026-04-04
Status: integrated into benchmarks
Primary crate: `dag_exec = 0.1.1`
License: `MIT OR Apache-2.0`
Repository: `https://github.com/reymom/rust-dag-executor`

## Why This File Exists

This file documents the explicit-DAG baseline used by the benchmark harness.

Read this before changing:

- `DagExecAdapter`
- explicit-DAG comparisons with `experiment_03`
- any claim that a fully explicit graph executor outperforms or underperforms the local schedulers

## Primary Sources Read

Primary sources used during integration:

- crate metadata via `cargo info dag_exec@0.1.1`
- adapter implementation in `benches/experiment.rs`

## Mental Model

`dag_exec` is an explicit task-graph executor:

- tasks have predecessor lists
- execution is driven from ready tasks
- worker count and queue bounds are configurable

This is the cleanest external contrast to `experiment_03`, because it represents the "compile everything into explicit edges first, then keep runtime execution simple" side of the design space.

## Benchmark Adapter Design

Adapter name:

- `DagExecAdapter`

Compiled state:

- `DagExecCompiled { dag, executor, outputs }`

Exact mapping:

- each benchmark job becomes one explicit task
- explicit predecessor hints are preserved
- relaxed access conflicts are conservatively converted into declaration-order edges because `dag_exec` lacks a native "exclude but do not orient" primitive
- strict conflicts are explicit edges
- `Unknown` becomes a barrier-like predecessor split
- job closures capture `Arc<BenchState>`
- task outputs are `u64`, which lets downstream tasks mix dependency results into their seed

## Comparison To Local Experiments

Closest local comparison:

- direct baseline for `experiment_03`

Why:

- both approaches favor more compile-time graph construction
- both rely on a smaller runtime once explicit edges exist

This adapter is also useful as a negative control against access-based schedulers:

- if the explicit graph underperforms badly, that means compile-time orientation lost too much parallelism
- if it performs well, it suggests heavier orientation or reduction may be worth importing into future work

## Verification Results

Commands previously run:

- `cargo bench --bench experiment --no-run`
- `cargo bench --bench experiment -- experiment/run_parallelism/dag_exec/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_overhead/dag_exec/wide_independent/jobs512_zero`

Observed results:

- `run_parallelism/layer_barrier_stress`
  - approximately `1.31 ms` to `1.33 ms`
- `run_overhead/wide_independent/jobs512_zero`
  - approximately `786 us` to `800 us`

## Caveat

The conservative edge orientation for relaxed conflicts is deliberate.

It is not a perfect semantic match for the repository's access model, but it is the least misleading mapping available within `dag_exec`'s explicit-DAG design.

## Resume Notes

If you modify this adapter later:

1. Re-evaluate whether a newer `dag_exec` version exposes a better mutual exclusion primitive.
2. If not, keep the conservative orientation explicit in docs.
3. Re-run at least one overhead and one parallelism benchmark.

## Progress Log

- 2026-04-04: Integrated `DagExecAdapter` into the shared benchmark harness.
