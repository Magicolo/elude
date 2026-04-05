# Task 01: Benchmark Foundation And Workload Redesign

Last updated: 2026-04-05
Status: completed
Priority: high

## Goal

Refactor the benchmark harness so it can compare:

- local experiments `01`, `02`, `03`, and future `04`
- selected external scheduler libraries
- compile-time behavior
- repeated-run behavior
- standard `0ms` overhead
- workload shapes that clearly expose differences in parallelism

This task exists to make every later optimization measurable and comparable.

## Why This Task Should Happen Early

Without a stronger harness:

- external library integration will be ad hoc
- scheduler changes will be hard to judge
- `0ms` overhead will remain conflated with job-body overhead
- it will be too easy to optimize for the current benchmark matrix instead of the actual parallelism question

## Current Baseline

The current harness in `benches/experiment.rs`:

- uses a single synthetic workload type
- mixes scheduler overhead with `Instant::now()` polling and atomic state updates
- has no abstraction for non-local schedulers
- does not explicitly benchmark the cases where the three designs should diverge

## Implemented Architecture

Task `01` was completed directly in `benches/experiment.rs` without splitting into support modules. That was a deliberate tradeoff to keep the benchmark target easy to compile and easy to grep while the external adapter set is still small.

The current harness now provides:

- a benchmark IR:
  - `WorkProfile`
  - `JobSpec`
  - `Workload`
- a scheduler adapter trait:
  - `BenchAdapter`
- a generic local adapter:
  - `ExperimentAdapter<T>` for any `experiment::Scheduler<BenchState>`
- custom local adapters added later during task `06`:
  - `Experiment04AdaptiveAdapter`
  - `Experiment04CriticalPathAdapter`
  - `Experiment04ReadHeavyAdapter`
  - `Experiment04HotContentionAdapter`
- external adapters added later during task `05`:
  - `DaggaAdapter`
  - `DagExecAdapter`
  - `ShipyardAdapter`
  - `LegionAdapter`
  - `BevyAdapter`
  - `FlecsAdapter`
- a lightweight benchmark state:
  - `BenchState { slots: Box<[AtomicU64]> }`

Important design choices:

- the workload IR now carries:
  - declared dependencies
  - optional explicit predecessor hints
  - a compile-time weight hint derived from the work profile
- the local experiments currently ignore explicit predecessor hints, but the IR keeps them for future external-library adapters
- `BenchAdapter::compile` now takes a state reference because `dag_exec` captures runtime state inside compiled task closures
- the runtime job body now uses deterministic loop counts rather than `Instant::now()` polling
- the standard `0ms` suite uses `iterations = 0` and a per-job slot update, which is materially lighter than the previous spin-until-deadline approach
- each benchmark suite is separated by intent:
  - `experiment/compile/<scheduler>`
  - `experiment/run_overhead/<scheduler>`
  - `experiment/run_parallelism/<scheduler>`

No Cargo dependency changes were needed to complete the original local-only task `01`.

Later, task `05` reused this same harness shape and added dev-dependencies for:

- `dagga`
- `dag_exec`
- `shipyard`
- `legion`
- `bevy_ecs`
- `bevy_tasks`
- `bevy_utils`
- `flecs_ecs`
- `seq-macro`

## Scope

This task should produce the benchmark framework and the benchmark workload catalog.

It may include:

- refactoring `benches/experiment.rs` into support modules under `benches/`
- small shared benchmark-only abstractions
- dev-dependency additions in `Cargo.toml`
- adapter types for local schedulers

It should not yet optimize a specific experiment unless that is required to make the adapter abstraction work.

## Target Architecture

The benchmark layer should have a scheduler-agnostic workload description.

Recommended shape:

- define a benchmark IR, for example:
  - job id
  - declared dependencies
  - execution profile
  - optional explicit weight estimate used by some adapters
- define an adapter trait for "compile this IR into a runnable schedule"
- define separate benchmark suites for:
  - compile
  - run
  - `0ms` or near-zero work overhead
  - parallelism-sensitive workloads

The adapter trait must be flexible enough for external libraries whose public API is not closure-list based.

What this turned into in practice:

- `dagga` uses workload-interned resource ids plus explicit predecessor overlays
- `dag_exec` uses a conservative explicit DAG and captures `Arc<BenchState>` inside compiled closures
- `shipyard` uses direct `WorkloadSystem` construction with native borrow metadata and native `after` edges
- `legion` uses custom `Runnable` systems, native access scheduling, and synthetic order-token resources for explicit predecessors
- `bevy_ecs` uses custom boxed `System` implementations, dynamic resource registration, and dynamic `SystemSet` labels
- `flecs` uses dynamic systems, dynamic access terms, and native `DependsOn`

## Required Workloads

At minimum, the new suite should include the following families.

1. `zero_work`

- Purpose: estimate scheduler overhead with as little job-body cost as possible.
- Requirement: do not use `Instant::now()` loops for this case.
- Requirement: keep side effects minimal but sufficient to prevent job elimination.

2. `wide_independent`

- Purpose: show best-case parallel throughput and runtime overhead when there are effectively no conflicts.

3. `strict_chain`

- Purpose: show pure dependency-order overhead and critical-path behavior.

4. `layer_barrier_stress`

- Purpose: expose the whole-group barrier cost in `02`.
- Example shape: neighboring jobs create only partial cross-layer conflicts so `01` and `03` can overlap more than `02`.

5. `straggler_partial_overlap`

- Purpose: expose whether one long job unnecessarily blocks otherwise ready work.
- Expected to separate `02` from `01` and `03`.

6. `hot_key_write_contention`

- Purpose: expose heavy conflict handling and wakeup behavior.
- Useful for comparing page reservation vs static orientation vs groups.

7. `read_heavy_shared_key`

- Purpose: verify that shared-read concurrency is not accidentally lost.

8. `mixed_hotspots`

- Purpose: combine multiple resources, mixed strict/relax order, and uneven durations.
- This should resemble the most realistic synthetic case.

9. `portfolio_bridge_tradeoff`

- Purpose: force a real orientation tradeoff between keeping a large read batch
  ahead of a bridge writer and firing that writer early to unlock a large
  follower batch.
- This workload was added during task `06` specifically so `experiment_04`'s
  fixed variants could be separated by the benchmark suite instead of only by
  internal tests.

10. `fan_out_fan_in`

- Purpose: stress DAG quality and wakeup efficiency.
- Useful for `03`, and also for observing whether `01` follower/page scans stay productive.

11. `compile_heavy_sparse_keys`

- Purpose: stress schedule-time logic on large graphs where runtime work is tiny.
- This is important because the user explicitly accepts large schedule-time cost.

## Special Notes For `0ms` Workloads

The `0ms` benchmark should not accidentally benchmark:

- `Instant::now()` polling
- heavy checksum atomics
- state contention unrelated to the scheduler

Possible approach:

- create a minimal job body that performs one tiny deterministic side effect, or a per-job `black_box` transform
- consider a dedicated `ZeroWorkState` separate from the heavier current `BenchState`

If necessary, keep both:

- `zero_work_minimal`
- `zero_work_with_shared_state`

What was actually implemented:

- one standard `0ms` family using minimal per-job slot updates
- no dedicated shared-state `0ms` variant yet

That was an intentional simplification because the immediate problem was removing artificial timing overhead from the old harness, not adding a second state-contention benchmark family.

## External Library Compatibility Requirements

The workload IR must support adapters for libraries that:

- build a schedule ahead of time
- need a world/resource object instead of plain closures
- need explicit ordering edges in addition to read/write access declarations

Do not hard-code the harness around the current local scheduler API only.

This turned out to be essential, not theoretical. The final external set now spans:

- batch schedulers
- explicit DAG executors
- ECS workload schedulers
- graph-oriented ECS schedulers
- pipeline/system schedulers

## Likely Files To Change

- `Cargo.toml`
- `benches/experiment.rs`
- new files under `benches/` for shared support, if helpful
- possibly small shared benchmark-facing types in `src/experiment` only if that clearly reduces duplication

## Implemented Workload Catalog

The current harness now includes these workload families.

Compile suite:

- `wide_independent`
- `hot_key_write_contention`
- `layer_barrier_stress`
- `fan_out_fan_in`
- `mixed_hotspots`
- `compile_heavy_sparse_keys`

Run-overhead suite:

- `wide_independent`
- `read_heavy_shared_key`
- `strict_chain`
- `hot_key_write_contention`

Run-parallelism suite:

- `wide_independent`
- `read_heavy_shared_key`
- `strict_chain`
- `hot_key_write_contention`
- `portfolio_bridge_tradeoff`
- `layer_barrier_stress`
- `straggler_partial_overlap`
- `fan_out_fan_in`
- `mixed_hotspots`

## Post-Task Extensions

The original task `01` delivered the shared IR and adapter abstraction. Later
tasks extended that foundation in two important ways:

- task `05` added external adapters for `dagga`, `dag_exec`, `shipyard`,
  `legion`, `bevy_ecs`, and `flecs`
- task `06` added benchmark-visible fixed-policy adapters for `experiment_04`
  and introduced the `portfolio_bridge_tradeoff` family to expose schedule
  shape differences directly

Important shapes:

- `layer_barrier_stress` is implemented as `[A lanes][B lanes]` with strict same-key pairs, so `02` must wait on the whole first layer while `01` and `03` can unlock followers lane-by-lane
- `straggler_partial_overlap` is the same pattern but with one extreme straggler lane in the first wave
- `fan_out_fan_in` is root write, many shared reads, then sink write on the same key
- `compile_heavy_sparse_keys` uses many multi-key jobs with sparse shared buckets to stress schedule-time logic without relying on runtime work

## Concrete Execution Plan

1. Freeze the old harness behavior by reading and understanding it before changing structure.
2. Decide the benchmark IR and adapter trait shape.
3. Refactor the harness so local schedulers `01`, `02`, and `03` all use that abstraction.
4. Add the `0ms` benchmark.
5. Add the workload families listed above.
6. Calibrate Criterion settings so compile-heavy and run-heavy suites both remain practical to execute.
7. Leave obvious hooks for external library adapters.
8. Record which benchmark names are intended to expose which scheduler tradeoff.

## Acceptance Criteria

- `cargo bench --bench experiment` compiles and runs with the refactored harness.
- All current local schedulers benchmark through one shared abstraction.
- There is a standard `0ms` benchmark family.
- Benchmark names and workload categories make the design intent obvious.
- The new workload set includes cases that should favor each scheduler differently.
- The task file is updated with any deviations from this plan.

## Verification Performed

Commands run during this task:

- `cargo bench --bench experiment --no-run`

Additional verification after task `05` extended this harness:

- `cargo check --bench experiment`
- `cargo bench --bench experiment -- experiment/run_parallelism/dag_exec/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_overhead/dag_exec/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_overhead/shipyard/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_overhead/legion/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_overhead/bevy_ecs/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_overhead/flecs/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_parallelism/shipyard/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/legion/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/bevy_ecs/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/flecs/layer_barrier_stress`

The `dagga` adapter also compiles through the shared benchmark target, but its schedule-time cost is high enough on the current benchmark sizes that filtered Criterion runs did not finish promptly during task `05`.
- `cargo bench --bench experiment -- experiment/run_overhead/experiment_01/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_02/layer_barrier_stress`

Observed outcomes:

- the refactored benchmark target compiles successfully
- a filtered overhead benchmark runs successfully under the new naming scheme
- a filtered parallelism-sensitive benchmark runs successfully under the new naming scheme

Selected observed timings from the filtered verification runs:

- `experiment/run_overhead/experiment_01/wide_independent/jobs512_zero`
  - about `225-230 µs`
- `experiment/run_parallelism/experiment_02/layer_barrier_stress/lanes128_fast400_slow5000_follow2000`
  - about `1.08-1.89 ms`
- `experiment/run_overhead/shipyard/wide_independent/jobs512_zero`
  - about `114-139 µs`
- `experiment/run_overhead/legion/wide_independent/jobs512_zero`
  - about `378-440 µs`
- `experiment/run_overhead/bevy_ecs/wide_independent/jobs512_zero`
  - about `240-289 µs`
- `experiment/run_overhead/flecs/wide_independent/jobs512_zero`
  - about `43-55 µs`
- `experiment/run_parallelism/shipyard/layer_barrier_stress/lanes128_fast400_slow5000_follow2000`
  - about `1.76-1.83 ms`
- `experiment/run_parallelism/legion/layer_barrier_stress/lanes128_fast400_slow5000_follow2000`
  - about `1.72-1.90 ms`
- `experiment/run_parallelism/bevy_ecs/layer_barrier_stress/lanes128_fast400_slow5000_follow2000`
  - about `2.90-3.15 ms`
- `experiment/run_parallelism/flecs/layer_barrier_stress/lanes128_fast400_slow5000_follow2000`
  - about `14.66-19.03 ms`

These numbers are only smoke-test results, not the final benchmark analysis.

## Progress Log

- 2026-04-04: Task created. No benchmark refactor yet.
- 2026-04-04: Replaced the old single-workload harness with a workload IR, local adapter layer, separated compile/overhead/parallelism suites, and a standard `0ms` benchmark family.
- 2026-04-04: Verified the new bench target with `cargo bench --bench experiment --no-run` and filtered Criterion runs.
- 2026-04-04: Task `05` extended the same harness to include `shipyard`, `legion`, `bevy_ecs`, and `flecs` as direct competitors, not just `dagga` and `dag_exec`.
