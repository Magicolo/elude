# Experiment 05

`experiment_05` is a cluster scheduler.

It is intentionally different from the earlier experiments:

- `01` keeps runtime flexibility across the whole schedule
- `02` compiles one cheap static group sequence
- `03` compiles one per-job DAG
- `04` compiles several per-job DAG variants
- `05` compiles small cache-local job clusters and schedules clusters, not jobs,
  at runtime

The core idea is:

- keep the runtime scheduling unit coarse enough to reduce executor overhead
- keep dynamic job choice only inside a bounded local domain
- keep cross-cluster coordination cheap and mostly static

This first implementation uses:

- compile-time affinity clustering
- a global cross-cluster predecessor graph for strict edges and non-local
  relaxed conflicts
- dynamic local ordering only for relaxed conflicts that are fully contained
  within one cluster
- a cluster worker that drains as many local ready jobs as it can before
  yielding back to the pool

The runtime still uses a Rayon thread pool for now. The experiment is focused on
reducing scheduling granularity first; a custom executor remains a follow-up
optimization only if it beats the Rayon-backed cluster runtime in grounded
benchmarks.
