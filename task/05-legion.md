# Legion Competitor Dossier

Last updated: 2026-04-04
Status: integrated into benchmarks
Primary crate: `legion = 0.4.0`
License: `MIT`
Repository: `https://github.com/amethyst/legion`

## Why This File Exists

This file documents how `legion` was integrated into the benchmark harness and why the adapter looks the way it does.

Read this before changing:

- `LegionAdapter`
- the synthetic order-token mapping
- any benchmark conclusions about Legion versus the local experiments

## Primary Sources Read

Local crate sources inspected:

- `~/.cargo/registry/src/.../legion-0.4.0/src/lib.rs`
- `~/.cargo/registry/src/.../legion-0.4.0/src/systems.rs`
- `~/.cargo/registry/src/.../legion-0.4.0/src/world.rs`
- `~/.cargo/registry/src/.../legion-0.4.0/src/internals/systems/schedule.rs`
- `~/.cargo/registry/src/.../legion-0.4.0/src/internals/systems/system.rs`
- `~/.cargo/registry/src/.../legion-0.4.0/src/internals/systems/resources.rs`
- `~/.cargo/registry/src/.../legion-0.4.0/src/internals/subworld.rs`

## Mental Model

Legion's schedule model is:

- start from a linear order of systems
- derive dependencies from declared resource and component accesses
- run any systems in parallel as long as observed side-effect order is preserved

This is important:

- Legion is not a free-form dynamic DAG scheduler
- it is an "ordered system list plus automatic parallelization" runtime

That makes it a strong competitor for access-based scheduling, but explicit order beyond access conflicts is not its strongest native surface.

## Relevant Public Pieces

Important public types:

- `Runnable`
- `ParallelRunnable`
- `Executor`
- `ResourceTypeId`
- `Resources`
- `UnsafeResources`
- `ArchetypeAccess`

The key hook for this repository is:

- `Executor::new(Vec<Box<dyn ParallelRunnable>>)`

That path lets the benchmark supply custom runtime systems directly without going through Legion's higher-level codegen-based system APIs.

## Scheduler Behavior Relevant To This Repository

### Access Model

Legion reasons about:

- resource reads
- resource writes
- component reads
- component writes

For this benchmark, only resource access is needed.

### Explicit Ordering Limitation

Legion does not expose a native "this job must run after that job even if their normal accesses do not conflict" edge in the low-level `Executor` path.

That is the key adapter challenge.

### Threading

Legion's executor normally uses Rayon.

The benchmark keeps Legion on a benchmark-local Rayon pool by calling the executor from inside `pool.install(...)`.

That required a tiny unsafe pointer wrapper because `legion::Resources` is not directly `Send` enough for the naive closure capture.

## Benchmark Adapter Design

Implementation location:

- `benches/experiment.rs`

Adapter name:

- `LegionAdapter`

Compiled state:

- `LegionCompiled { executor, world, resources, pool }`

### Exact Mapping

Each benchmark job becomes one custom `LegionRunnableJob`.

Per-job mapping:

- real normalized resource accesses:
  - reads map to `read_resources`
  - writes map to `write_resources`
- explicit predecessor overlay:
  - implemented with synthetic order-token resources
  - every job writes its own unique order token
  - every successor reads each predecessor's order token
- component accesses:
  - none
- archetype accesses:
  - `ArchetypeAccess::Some(Default::default())`
  - effectively no archetype-level coupling

### Why Synthetic Order Tokens Were Necessary

Legion's low-level scheduler only derives edges from access conflicts.

The benchmark needs more than that:

- explicit predecessor hints
- `Unknown` barriers
- strict declaration-order overlays

Those are all represented in `normalized.dagga_predecessors`.

The order-token trick preserves them while still letting Legion handle real access-set scheduling natively.

## Runtime Identity Problem

`ResourceTypeId` is type-based only.

Unlike Shipyard or Bevy, Legion does not expose a runtime resource-id constructor.

So the adapter must synthesize many distinct resource types.

The current solution:

- `LegionResourceMarker<const N: usize>`
- `legion_resource_type_id(index)` generated via `seq-macro`

Current capacity:

- `LEGION_MAX_BENCH_RESOURCE_IDS = 6144`

Why this number exists:

- current benchmark suite needs room for:
  - all normalized real resource ids
  - one order token per job

The current suite stays well under that limit.

## Comparison To Local Experiments

Closest local comparison:

- mostly relevant to `experiment_01` and `experiment_02`, because the runtime scheduling decision still comes from access metadata

Major differences:

- Legion preserves a source linear order and derives safe parallelism from it
- it is less naturally expressive for arbitrary extra predecessor edges than `experiment_03` or `dag_exec`
- it is a mature generic ECS scheduler, not a scheduler specialized around dynamic runtime resource identifiers

What Legion is testing against the local experiments:

- how much a well-established ordered access scheduler costs
- whether the local schedulers are materially better than a strong general-purpose ECS baseline

## Verification Results

Commands run:

- `cargo check --bench experiment`
- `cargo bench --bench experiment --no-run`
- `cargo bench --bench experiment -- experiment/run_overhead/legion/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_parallelism/legion/layer_barrier_stress`

Observed results from this pass:

- `run_overhead/wide_independent/jobs512_zero`
  - approximately `377.54 us` to `439.65 us`
- `run_parallelism/layer_barrier_stress`
  - approximately `1.718 ms` to `1.904 ms`

## Caveats

Important caveat:

- explicit order is not modeled with a native Legion feature
- it is modeled through synthetic resource dependencies

Why this is acceptable:

- it stays inside Legion's actual scheduling model
- it does not fake results by bypassing the scheduler
- it is the least-distorting public-path mapping found during integration

Important non-goal:

- this adapter does not try to use Legion's stage/flush builder APIs for extra ordering

That would be a worse fit because it would over-serialize the graph.

## If You Need To Modify This Adapter

Checklist:

1. Re-check whether newer Legion exposes a public explicit dependency edge in the low-level executor path.
2. If yes, remove synthetic order-token resources and map explicit predecessors directly.
3. Keep real access conflicts native.
4. Keep resource-id capacity checks in place.
5. Re-run:
   - `cargo check --bench experiment`
   - one `run_overhead` filtered benchmark
   - one `run_parallelism` filtered benchmark

## Open Questions Worth Revisiting Later

- whether the benchmark should record Legion's static dependency counts directly for compile-time comparison if that metadata becomes accessible
- whether an upstream Legion fork or successor crate offers better public support for runtime-defined resource ids

## Progress Log

- 2026-04-04: Read Legion's executor and runnable internals.
- 2026-04-04: Confirmed that low-level custom `Runnable` integration was feasible.
- 2026-04-04: Integrated `LegionAdapter` using native resource access scheduling plus synthetic order-token resources for explicit predecessor overlays.
