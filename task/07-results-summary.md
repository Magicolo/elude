# Final Benchmark Summary

Last updated: 2026-04-05

## Validation Context

- machine parallelism used by the benchmark harness: `32` logical CPUs
- correctness and hygiene commands passed:
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`

## Benchmark Method

Task `07` attempted the literal full sweep:

- `cargo bench --bench experiment`

That run progressed through the local schedulers but became impractical once it
reached `dagga`, stalling at:

- `experiment/compile/dagga/wide_independent/jobs1024_zero`

That is itself a real result, not just an operational nuisance: `dagga`'s
compile-time cost can explode enough to make a naive full-matrix Criterion run
unusable on this benchmark set.

To finish the task with a controlled and comparable dataset, a targeted final
matrix was run with per-benchmark timeouts:

- one compile-time case
- one `0ms` overhead case
- one barrier-heavy parallelism case
- one orientation-sensitive tradeoff case

The representative benchmark command shape was:

- `timeout 60s cargo bench --bench experiment -- <filter>`

The selected filters were:

- `experiment/compile/<scheduler>/compile_heavy_sparse_keys`
- `experiment/run_overhead/<scheduler>/wide_independent/jobs512_zero`
- `experiment/run_parallelism/<scheduler>/layer_barrier_stress`
- `experiment/run_parallelism/<scheduler>/portfolio_bridge_tradeoff/pre32_iter1500_bridge400_post96_iter6000`

## Compile-Time Trend

`compile_heavy_sparse_keys`

| Scheduler | Time |
| --- | --- |
| `experiment_01` | `210.83 ms .. 211.15 ms` |
| `experiment_02` | `119.34 ms .. 119.79 ms` |
| `experiment_03` | `20.216 ms .. 20.596 ms` |
| `experiment_04` | `24.452 ms .. 24.843 ms` |
| `experiment_04_critical_path` | `20.985 ms .. 21.301 ms` |
| `experiment_04_read_heavy` | `21.051 ms .. 21.384 ms` |
| `experiment_04_hot_contention` | `20.869 ms .. 21.051 ms` |
| `dagga` | `timeout (>60s)` |
| `dag_exec` | `16.012 ms .. 16.778 ms` |
| `shipyard` | `49.111 ms .. 49.331 ms` |
| `legion` | `18.238 ms .. 18.388 ms` |
| `bevy_ecs` | `133.77 ms .. 133.89 ms` |
| `flecs` | `40.207 ms .. 40.608 ms` |

Interpretation:

- `experiment_03` remains the best local balance between aggressive compile
  work and practical schedule time.
- Adaptive `experiment_04` pays a real but bounded compile premium for the
  portfolio, while fixed variants land close to `experiment_03`.
- `dag_exec` and `legion` are the fastest compilers in the final matrix.
- `dagga` is the clearest example of schedule-quality-first compile cost
  becoming operationally dominant.

## Zero-Work Overhead Trend

`wide_independent/jobs512_zero`

| Scheduler | Time |
| --- | --- |
| `experiment_01` | `277.25 µs .. 282.39 µs` |
| `experiment_02` | `30.232 µs .. 31.471 µs` |
| `experiment_03` | `265.56 µs .. 270.52 µs` |
| `experiment_04` | `264.07 µs .. 267.99 µs` |
| `experiment_04_critical_path` | `260.45 µs .. 266.33 µs` |
| `experiment_04_read_heavy` | `265.60 µs .. 269.25 µs` |
| `experiment_04_hot_contention` | `263.96 µs .. 266.98 µs` |
| `dagga` | `timeout (>60s)` |
| `dag_exec` | `851.15 µs .. 877.97 µs` |
| `shipyard` | `35.956 µs .. 37.197 µs` |
| `legion` | `151.42 µs .. 154.83 µs` |
| `bevy_ecs` | `284.88 µs .. 292.88 µs` |
| `flecs` | `27.797 µs .. 29.083 µs` |

Interpretation:

- `experiment_02` is still the cheapest local runner by a wide margin.
- The static-DAG family (`03` and `04`) is much more expensive than `02` at
  pure runner overhead, but still well below `dag_exec`.
- `flecs` and `shipyard` are extremely cheap in this zero-work configuration.

## Barrier-Heavy Parallelism Trend

`layer_barrier_stress`

| Scheduler | Time |
| --- | --- |
| `experiment_01` | `589.91 µs .. 594.06 µs` |
| `experiment_02` | `635.35 µs .. 640.96 µs` |
| `experiment_03` | `587.66 µs .. 591.29 µs` |
| `experiment_04` | `590.01 µs .. 595.74 µs` |
| `experiment_04_critical_path` | `590.55 µs .. 595.02 µs` |
| `experiment_04_read_heavy` | `591.59 µs .. 599.30 µs` |
| `experiment_04_hot_contention` | `591.95 µs .. 594.52 µs` |
| `dagga` | `timeout (>60s)` |
| `dag_exec` | `1.3210 ms .. 1.3397 ms` |
| `shipyard` | `640.80 µs .. 644.21 µs` |
| `legion` | `752.28 µs .. 758.34 µs` |
| `bevy_ecs` | `604.26 µs .. 606.64 µs` |
| `flecs` | `18.250 ms .. 18.256 ms` |

Interpretation:

- The improved `experiment_03` runtime is still the strongest local result on
  this barrier-heavy case.
- `experiment_01` recovered enough wakeup efficiency in task `02` to stay very
  close here.
- `experiment_02` still pays the expected group-barrier penalty.
- `shipyard` and `bevy_ecs` are credible external competitors on this shape.
- `dag_exec` and especially `flecs` are poor fits for this benchmark's
  resource-conflict scheduling problem.

## Schedule-Shape Tradeoff Trend

`portfolio_bridge_tradeoff/pre32_iter1500_bridge400_post96_iter6000`

| Scheduler | Time |
| --- | --- |
| `experiment_01` | `18.652 ms .. 18.993 ms` |
| `experiment_02` | `561.65 µs .. 568.46 µs` |
| `experiment_03` | `495.12 µs .. 503.07 µs` |
| `experiment_04` | `499.23 µs .. 514.34 µs` |
| `experiment_04_critical_path` | `497.04 µs .. 505.18 µs` |
| `experiment_04_read_heavy` | `536.42 µs .. 539.09 µs` |
| `experiment_04_hot_contention` | `539.34 µs .. 542.29 µs` |
| `dagga` | `570.41 µs .. 577.09 µs` |
| `dag_exec` | `1.5319 ms .. 1.5493 ms` |
| `shipyard` | `562.70 µs .. 569.97 µs` |
| `legion` | `689.79 µs .. 696.10 µs` |
| `bevy_ecs` | `583.63 µs .. 589.39 µs` |
| `flecs` | `18.929 ms .. 18.946 ms` |

Interpretation:

- This workload is one of the clearest separators in the whole suite.
- `experiment_03` and fixed `experiment_04_critical_path` are the best results
  on the post-heavy shape.
- Adaptive `experiment_04` stays close to that best fixed variant, which shows
  the portfolio selector is behaving sensibly.
- `experiment_04_read_heavy` and `experiment_04_hot_contention` are
  measurably worse here, which is exactly what should happen if the workload is
  exposing a real schedule-shape tradeoff.
- `experiment_01` collapses badly on this case, which reinforces the broader
  lesson that runtime flexibility is not free.

This task also inherits the complementary pre-heavy result from task `06`,
where `experiment_04_read_heavy` was the better pinned variant and adaptive
`experiment_04` tracked it closely. Together, those two shapes justify the
portfolio design more clearly than any generic mixed workload would have.

## Final Lessons

1. `experiment_03` is the strongest all-around local scheduler.

- It keeps compile cost in a practical range.
- It keeps runtime simple.
- It repeatedly lands at or near the best result on the parallelism-sensitive
  workloads used in the final matrix.

2. `experiment_04` is a justified contrast, but not a universal replacement.

- The adaptive portfolio is real and measurable.
- It is only clearly worthwhile on workloads with genuine orientation ambiguity.
- Outside those cases, it behaves a lot like `experiment_03` while paying extra
  compile cost.

3. `experiment_02` is still the runner-overhead champion.

- Its `0ms` numbers are far better than every other local experiment.
- That benefit comes with known schedule-quality limitations.

4. `experiment_01` improved materially, but runtime conflict handling remains expensive.

- Task `02` fixed its worst contention-path pathology.
- It can still lose catastrophically on schedule-shape-sensitive workloads.

5. `shipyard` is the strongest external ECS-style competitor in this benchmark family.

- It stays cheap at `0ms`.
- It stays competitive on barrier-heavy and tradeoff workloads.
- Its dynamic `StorageId::Custom` model maps to this repository's runtime key
  problem better than the type-oriented ECS schedulers.

6. `dag_exec` is a good compile-time baseline, not a strong runtime one here.

- It compiles quickly.
- The conservative explicit-edge mapping costs too much parallelism.

7. `flecs` shows the clearest semantic mismatch.

- It has extremely low zero-work overhead.
- It performs very poorly on these job-graph workloads.
- That matches the earlier design reading: its primary strength is not this
  exact scheduling problem.

8. `dagga` taught the strongest compile-time lesson.

- Its schedule-quality-first design is interesting.
- In practice, its compile-time cost is high enough that benchmarking it must
  be filtered and bounded, not treated as an ordinary always-on baseline.
