# Experiment 04

`experiment_04` is the portfolio scheduler.

It compiles several legal static-DAG variants for the same job set, shares the
actual job closures across those variants, and then chooses which variant to
run. The default policy is adaptive for repeated runs, while fixed-variant mode
exists for fair benchmarking and can compile only the pinned variant.

The three initial variants are:

- `critical_path`
- `read_heavy`
- `hot_contention`

Adaptive mode samples those variants, then sticks close to the best measured
one for repeated runs. Fixed mode exists so benchmarks can compare a single
schedule shape without adaptive-policy noise.

This is the most compile-heavy experiment in the repository. It trades memory
and schedule-time work for the claim that no single static orientation is best
for every workload family.

Read [`DESIGN.md`](./DESIGN.md) before changing the implementation.
