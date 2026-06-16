# Bevy ECS Competitor Dossier

Last updated: 2026-04-04
Status: integrated into benchmarks
Primary crate: `bevy_ecs = 0.18.1`
Support crates used directly by the benchmark:

- `bevy_tasks = 0.18.1`
- `bevy_utils = 0.18.1`

License: `MIT OR Apache-2.0`
Repository: `https://github.com/bevyengine/bevy`

## Why This File Exists

This file explains the Bevy ECS benchmark adapter and the upstream scheduler model it relies on.

Read this before changing:

- `BevyAdapter`
- the custom boxed system implementation
- the dynamic resource registration strategy
- any claims comparing Bevy's scheduler to the local experiments

## Primary Sources Read

Local crate sources inspected:

- `~/.cargo/registry/src/.../bevy_ecs-0.18.1/src/system/system.rs`
- `~/.cargo/registry/src/.../bevy_ecs-0.18.1/src/schedule/config.rs`
- `~/.cargo/registry/src/.../bevy_ecs-0.18.1/src/schedule/set.rs`
- `~/.cargo/registry/src/.../bevy_ecs-0.18.1/src/schedule/schedule.rs`
- `~/.cargo/registry/src/.../bevy_ecs-0.18.1/src/schedule/executor/multi_threaded.rs`
- `~/.cargo/registry/src/.../bevy_ecs-0.18.1/src/query/access.rs`
- `~/.cargo/registry/src/.../bevy_ecs-0.18.1/src/component/info.rs`
- `~/.cargo/registry/src/.../bevy_ecs-0.18.1/src/world/mod.rs`
- `~/.cargo/registry/src/.../bevy_tasks-0.18.1/src/usages.rs`
- `~/.cargo/registry/src/.../bevy_tasks-0.18.1/src/task_pool.rs`

## Mental Model

Bevy's scheduler is graph-based:

- systems declare access
- systems may also be ordered explicitly through sets and config
- the schedule is built, initialized, and then run repeatedly

This makes it one of the more serious Rust ECS competitors for this repository:

- it has real schedule construction
- it has real access analysis
- it supports explicit order overlays cleanly

The main challenge is that Bevy's public ergonomics are usually type-driven, while this benchmark uses runtime-defined dependency keys.

## Relevant Public Pieces

Important public types:

- `System`
- `BoxedSystem`
- `Schedule`
- `FilteredAccessSet`
- `SystemSet`
- `World::register_resource_with_descriptor(...)`
- `ComponentDescriptor::new_with_layout(...)`
- `ComputeTaskPool::get_or_init(...)`

These public hooks made Bevy integration possible without generating thousands of Rust resource types.

## Scheduler Behavior Relevant To This Repository

### Access Model

Bevy schedules using the access set returned by `System::initialize(...)`.

Critically, `FilteredAccessSet` supports runtime resource ids through:

- `add_unfiltered_resource_read(ComponentId)`
- `add_unfiltered_resource_write(ComponentId)`

That is the key enabler for this benchmark.

### Explicit Ordering

Bevy supports explicit ordering natively through system sets and config:

- `.in_set(...)`
- `.after(...)`
- `.before(...)`

That means explicit predecessor overlays do not need fake resources.

### Threading

Bevy's multithreaded executor uses the global `ComputeTaskPool`.

The benchmark controls this realistically by initializing that pool once with:

- `TaskPoolBuilder::new().num_threads(worker_count).build()`

This is the reason `bevy_tasks` is a direct benchmark dependency.

## Benchmark Adapter Design

Implementation location:

- `benches/experiment.rs`

Adapter name:

- `BevyAdapter`

Compiled state:

- `BevyCompiled { schedule, world }`

### Exact Mapping

Each benchmark job becomes one custom `BevyDynamicSystem`.

Per-job mapping:

- benchmark resource ids:
  - registered dynamically as Bevy resources through `register_resource_with_descriptor`
  - one fresh runtime `ComponentId` per normalized benchmark resource
- system access:
  - encoded in a `FilteredAccessSet`
  - read/write resource access added by runtime `ComponentId`
- explicit predecessor overlay:
  - encoded by assigning each system to a unique `DynamicBevySet(index)`
  - each predecessor becomes `.after(DynamicBevySet(predecessor))`
- runtime work:
  - `run_unsafe` calls `execute_work(...)`
  - no world data is actually read or written

### Why The Custom `System` Implementation Exists

The normal Bevy surface expects typed function systems.

That is not a good fit here because:

- benchmark dependency keys are runtime values
- generating one Rust resource type per benchmark key would be large and brittle
- the benchmark wants one job = one dynamic scheduled unit

The custom `System` path keeps the mapping faithful and direct.

## Dynamic Resource Registration Strategy

The adapter does not insert actual resource values.

It only registers scheduler-visible resource identities with:

- `ComponentDescriptor::new_with_layout(...)`
- `World::register_resource_with_descriptor(...)`

Why that works:

- the custom system never tries to read those resources through Bevy params
- `validate_param_unsafe` is a no-op
- the scheduler only needs the access metadata, not the resource payload

This is an important detail. Do not "fix" it by inserting dummy resource values unless a future Bevy version requires it.

## Explicit Order Mapping

The adapter uses:

- native access analysis for relaxed conflicts
- `normalized.dagga_predecessors` for overlays that must stay ordered

That preserves the benchmark semantics while still letting Bevy do real access scheduling.

This is the same philosophy as the Shipyard integration.

## Comparison To Local Experiments

Closest local comparison:

- strongest comparison point for `experiment_01` and `experiment_02`
- also relevant as a graph-oriented compile/run scheduler baseline against `experiment_03`

Major differences:

- Bevy pays for a more general schedule graph system
- it is designed for ECS workloads, not for highly specialized runtime job-graph problems
- its access model is component/resource oriented rather than the repository's native dependency representation

What Bevy is good at in this benchmark:

- native combination of access-based scheduling and explicit ordering
- realistic repeated-run schedule execution

What Bevy is not testing:

- any repository-specific optimizations around reservations, pages, or hand-tuned wake-up structures

## Verification Results

Commands run:

- `cargo check --bench experiment`
- `cargo bench --bench experiment --no-run`
- `cargo bench --bench experiment -- experiment/run_overhead/bevy_ecs/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_parallelism/bevy_ecs/layer_barrier_stress`

Observed results from this pass:

- `run_overhead/wide_independent/jobs512_zero`
  - approximately `239.79 us` to `288.97 us`
- `run_parallelism/layer_barrier_stress`
  - approximately `2.904 ms` to `3.148 ms`

## Caveats

Important caveat:

- Bevy uses a global compute task pool, not a truly per-scheduler local pool

Why the benchmark still accepts this:

- Bevy's own executor is built around that global pool
- using `ComputeTaskPool::get_or_init(...)` is the public, intended control point
- benchmark parallelism is fixed to the machine's available parallelism anyway

Important non-goal:

- the adapter does not try to benchmark Bevy's normal typed system conversion ergonomics

The goal here is scheduler comparison, not user-facing API ergonomics.

## If You Need To Modify This Adapter

Checklist:

1. Re-check that `FilteredAccessSet` still supports runtime resource ids.
2. Re-check that dynamic resource registration by descriptor still works without inserted values.
3. Keep `DynamicBevySet`-based explicit ordering unless Bevy exposes a better dynamic per-system ordering label path.
4. Keep task-pool initialization deterministic.
5. Re-run:
   - `cargo check --bench experiment`
   - one `run_overhead` filtered benchmark
   - one `run_parallelism` filtered benchmark

## Open Questions Worth Revisiting Later

- whether Bevy's schedule graph exposes schedule-build metrics worth capturing in a future compile benchmark extension
- whether schedule initialization cost should be broken out separately from graph construction if Bevy becomes a central comparison target

## Progress Log

- 2026-04-04: Read Bevy ECS scheduler, set, and dynamic resource registration internals.
- 2026-04-04: Confirmed that a custom boxed `System` with runtime resource ids was feasible.
- 2026-04-04: Integrated `BevyAdapter` using dynamic resource registration, custom `System` implementations, and dynamic `SystemSet` labels for explicit predecessor overlays.
