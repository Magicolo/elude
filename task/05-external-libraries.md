# Task 05: External Library Research And Integration

Last updated: 2026-04-04
Status: completed
Priority: high

## Goal

Find, study, and benchmark external schedulers that are direct or near-direct competitors for the design space explored by this repository.

This task has two outputs:

1. benchmarkable external baselines integrated into `benches/experiment.rs`
2. enough design-level notes that future agents do not need to re-read each upstream library from scratch

## Final Outcome

The shared benchmark harness now integrates six external schedulers:

- `dagga = 0.2.3`
- `dag_exec = 0.1.1`
- `shipyard = 0.11.2`
- `legion = 0.4.0`
- `bevy_ecs = 0.18.1`
- `flecs_ecs = 0.2.2`

One researched library remains rejected:

- `moongraph = 0.4.3`

The important change from the earlier pass is that the ECS schedulers are no longer treated as "informative only". The user explicitly requested that they be benchmarked as direct competitors, so significant effort was spent to find low-distortion integrations instead of dismissing them.

## Integrated Adapter Set

Current external adapters in `benches/experiment.rs`:

- `DaggaAdapter`
- `DagExecAdapter`
- `ShipyardAdapter`
- `LegionAdapter`
- `BevyAdapter`
- `FlecsAdapter`

All of them run through the same benchmark IR introduced by task `01`:

- `WorkProfile`
- `JobSpec`
- `Workload`
- `BenchAdapter`
- `NormalizedWorkload`

Shared mapping policy across adapters:

- workload dependency keys are interned into dense per-workload integer ids
- `Unknown` is promoted to a full barrier pattern
- explicit predecessor hints are preserved
- strict declaration-order overlays are computed only when conflicting accesses actually require order

Where adapters differ:

- `dagga`, `shipyard`, and `bevy_ecs` can represent access conflicts natively enough that relaxed exclusion remains exclusion without arbitrary extra order
- `dag_exec`, `legion`, and `flecs` need more explicit edge injection in at least part of the mapping

## Added Manifest Dependencies

Benchmark-facing competitors:

- `dagga`
- `dag_exec`
- `shipyard`
- `legion`
- `bevy_ecs`
- `flecs_ecs`

Support dependencies added only to make the adapters practical:

- `bevy_tasks`
- `bevy_utils`
- `seq-macro`

Why the support dependencies were accepted:

- `bevy_tasks` is required to control Bevy scheduler thread count realistically
- `bevy_utils` is required for the public `DebugName` type used by custom Bevy systems
- `seq-macro` avoids checking in huge hand-written type-id tables for `shipyard` and `legion`

## Per-Library Deep Dives

Read these files before touching a specific adapter:

- `task/05-dagga.md`
- `task/05-dag-exec.md`
- `task/05-shipyard.md`
- `task/05-legion.md`
- `task/05-bevy-ecs.md`
- `task/05-flecs.md`

Those files are the real cold-start references for future work. This file is the top-level summary.

## Why Each Integrated Library Matters

### `dagga`

Why it matters:

- closest external comparison to this repository's read/write resource model
- schedule-time heavy, batch-oriented, generic resource ids

What it tells us:

- how far a schedule-quality-first batch builder can push parallelism if setup cost is allowed to grow

### `dag_exec`

Why it matters:

- explicit DAG executor baseline
- strong comparison point for `experiment_03`

What it tells us:

- how much can be gained or lost when everything is pre-oriented into predecessor edges and runtime logic is kept deliberately small

### `shipyard`

Why it matters:

- explicit workload scheduler built around storage borrow metadata
- very close conceptual competitor for access-based system scheduling

What it tells us:

- how a mature ECS workload builder handles cross-system parallelism when access sets are first-class and per-world local pools are supported

### `legion`

Why it matters:

- aggressive resource/component access scheduling with automatic parallelization
- strong "ordered-but-opportunistically parallel" model

What it tells us:

- what a mature access scheduler looks like when it reasons from a linear system list and preserves side-effect order

### `bevy_ecs`

Why it matters:

- current mainstream Rust ECS scheduler with explicit schedule graph infrastructure
- native access analysis plus explicit system ordering overlays

What it tells us:

- how much scheduler overhead and runtime behavior come from a graph-oriented ECS scheduler that was not designed specifically for benchmark-sized dynamic job graphs

### `flecs`

Why it matters:

- major non-Rust ECS competitor with pipeline scheduling and explicit `DependsOn` relationships
- useful contrast because its primary threading model is more pipeline and entity-splitting oriented than the Rust ECS schedulers

What it tells us:

- how far an ECS pipeline scheduler can be pushed when used as a system/job orchestrator instead of its more typical entity-processing role

## Important Design Conclusions

### 1. Type-driven access is the main adapter friction point

This repository's dependency keys are runtime values.

ECS schedulers usually want one of:

- Rust types
- ECS component ids
- storage ids
- system labels

The most successful low-distortion integrations were the ones where the library exposed some public hook for dynamic identities:

- `shipyard`: `StorageId::Custom(u64)`
- `bevy_ecs`: runtime `ComponentId` via `register_resource_with_descriptor`
- `flecs`: runtime entity ids in terms and `DependsOn`

The least ergonomic one was `legion`, because `ResourceTypeId` is type-based only, so synthetic marker types were required.

### 2. Explicit predecessor overlays are still necessary even in ECS schedulers

The benchmark semantics are not just "read/write query access".

They also include:

- strict declaration-order overlays
- barrier-like `Unknown`
- explicit predecessor hints

For realistic integration:

- `shipyard` uses native `after` edges
- `bevy_ecs` uses per-job dynamic `SystemSet` labels plus `.after(...)`
- `legion` uses synthetic order-token resource ids
- `flecs` uses native `DependsOn`

### 3. `flecs` is the most semantically different competitor

Official docs and source strongly indicate:

- worker threads primarily split matched entities for multithreaded systems
- pipelines manage sync points and ordering

That is not the same primary model as this repository's "many small jobs with resource conflicts" problem.

The integrated adapter therefore uses `flecs` as a pipeline/system orchestrator:

- one benchmark job becomes one Flecs system
- access metadata is declared through dynamic terms
- explicit order is enforced through `DependsOn`

This is a fair representation of Flecs used for this specific problem, but it is not a measure of Flecs in its most favorable large-entity-loop use case.

### 4. Local thread-pool ownership is operationally important

The user explicitly relaxed the lock-free requirement. After reading and integrating these libraries, one clear practical lesson is:

- scheduler-owned or world-owned local thread pools are useful
- they make benchmark isolation cleaner
- they are worth considering in future experiment work

This came up directly in:

- `dagga`
- `shipyard`
- the custom `legion` adapter

## Rejected Library

### `moongraph`

Status:

- researched
- not integrated

Why it stayed out:

- it is conceptually close and still useful to read
- its public model remains more type-oriented than `dagga`
- `dagga` already covers the schedule-quality-first batch-solver angle with lower adapter distortion

This library can stay off the benchmark path unless later work specifically wants to compare typed resource maps plus scheduled local nodes.

## Verification

Commands run during this task:

- `cargo info dagga@0.2.3`
- `cargo info dag_exec@0.1.1`
- `cargo info shipyard@0.11.2`
- `cargo info legion@0.4.0`
- `cargo info bevy_ecs@0.18.1`
- `cargo info bevy_tasks@0.18.1`
- `cargo info bevy_utils@0.18.1`
- `cargo info flecs_ecs@0.2.2`
- `cargo check --bench experiment`
- `cargo bench --bench experiment --no-run`
- `cargo bench --bench experiment -- experiment/run_overhead/shipyard/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_overhead/legion/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_overhead/bevy_ecs/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_overhead/flecs/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_parallelism/shipyard/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/legion/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/bevy_ecs/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/flecs/layer_barrier_stress`

Observed filtered results from this pass:

- `shipyard` `run_overhead/wide_independent/jobs512_zero`
  - approximately `113.85 us` to `139.45 us`
- `legion` `run_overhead/wide_independent/jobs512_zero`
  - approximately `377.54 us` to `439.65 us`
- `bevy_ecs` `run_overhead/wide_independent/jobs512_zero`
  - approximately `239.79 us` to `288.97 us`
- `flecs` `run_overhead/wide_independent/jobs512_zero`
  - approximately `43.279 us` to `55.475 us`
- `shipyard` `run_parallelism/layer_barrier_stress`
  - approximately `1.755 ms` to `1.830 ms`
- `legion` `run_parallelism/layer_barrier_stress`
  - approximately `1.718 ms` to `1.904 ms`
- `bevy_ecs` `run_parallelism/layer_barrier_stress`
  - approximately `2.904 ms` to `3.148 ms`
- `flecs` `run_parallelism/layer_barrier_stress`
  - approximately `14.661 ms` to `19.033 ms`

Existing results from the earlier external pass remain relevant:

- `dag_exec` `run_parallelism/layer_barrier_stress`
  - approximately `1.31 ms` to `1.33 ms`
- `dag_exec` `run_overhead/wide_independent/jobs512_zero`
  - approximately `786 us` to `800 us`

## Resume Notes For Future Agents

If you return to task `05`, start here:

1. Read this file.
2. Read the specific per-library dossier for the adapter you want to touch.
3. Re-run `cargo check --bench experiment`.
4. Re-run at least one `run_overhead` and one `run_parallelism` filtered benchmark for that adapter.
5. If you change any adapter mapping, update:
   - this file
   - the per-library dossier
   - `task/01-benchmark-foundation.md`
   - `task/README.md`

## Progress Log

- 2026-04-04: Task created.
- 2026-04-04: Initial external baselines `dagga` and `dag_exec` were integrated.
- 2026-04-04: The user explicitly requested fair direct benchmarking of ECS schedulers, replacing the earlier rejection decision.
- 2026-04-04: Integrated `shipyard`, `legion`, `bevy_ecs`, and `flecs_ecs` into the shared benchmark harness.
- 2026-04-04: Added per-library deep-dive task files so future agents can resume without rereading upstream documentation.
