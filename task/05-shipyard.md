# Shipyard Competitor Dossier

Last updated: 2026-04-04
Status: integrated into benchmarks
Primary crate: `shipyard = 0.11.2`
License: `MIT OR Apache-2.0`
Repository: `https://github.com/leudz/shipyard`

## Why This File Exists

This file is the cold-start reference for the Shipyard benchmark adapter.

Future agents should read this before changing:

- the `ShipyardAdapter` in `benches/experiment.rs`
- the benchmark mapping for ECS competitors
- any conclusions that compare Shipyard to local experiments

## Primary Sources Read

Local crate sources inspected during integration:

- `~/.cargo/registry/src/.../shipyard-0.11.2/src/lib.rs`
- `~/.cargo/registry/src/.../shipyard-0.11.2/src/scheduler/system.rs`
- `~/.cargo/registry/src/.../shipyard-0.11.2/src/scheduler/info.rs`
- `~/.cargo/registry/src/.../shipyard-0.11.2/src/scheduler/workload.rs`
- `~/.cargo/registry/src/.../shipyard-0.11.2/src/scheduler/workload/create_workload.rs`
- `~/.cargo/registry/src/.../shipyard-0.11.2/src/world/builder.rs`
- `~/.cargo/registry/src/.../shipyard-0.11.2/src/storage/storage_id.rs`

## Mental Model

Shipyard's workload scheduler is built around explicit metadata per system:

- system identity
- borrow constraints
- optional explicit before/after constraints
- tags and workload-level labels

The scheduler then computes batches of systems that can run in parallel while preserving the required order.

This is one of the cleanest ECS schedulers for comparison with `elude` because the borrow metadata is explicit rather than hidden behind opaque function conversion.

## Relevant Public Pieces

Important public types:

- `Workload`
- `WorkloadSystem`
- `ScheduledWorkload`
- `World::builder()`
- `WorldBuilder::with_local_thread_pool(...)`
- `TypeInfo`
- `Mutability`
- `StorageId`

Important implementation detail that matters for the adapter:

- `Workload::build()` ultimately deduplicates systems by `type_id` through a lookup table in `create_workload.rs`

That means "one benchmark job = one Shipyard system" only works if every benchmark job gets a unique `TypeId`.

## Scheduler Behavior Relevant To This Repository

### Access Model

Shipyard schedules using `TypeInfo` items:

- `storage_id`
- `mutability`
- `thread_safe`

This makes it unusually friendly to runtime-key-based integrations because `StorageId::Custom(u64)` is public.

That is a major win compared with type-only ECS schedulers.

### Explicit Ordering

Shipyard supports native `after` and `before` constraints on `WorkloadSystem`.

This means explicit predecessor overlays do not need to be simulated through fake resources or through stage barriers.

### Threading

Shipyard can run workloads on a per-world local Rayon pool:

- `World::builder().with_local_thread_pool(pool).build()`

That maps very naturally to this repository's benchmark isolation goals.

## Benchmark Adapter Design

Implementation location:

- `benches/experiment.rs`

Adapter name:

- `ShipyardAdapter`

Compiled state:

- `ShipyardCompiled { workload, world }`

### Exact Mapping

Each benchmark job becomes one `WorkloadSystem`.

Per-job mapping:

- `type_id`
  - synthesized from `ShipyardJobMarker<const N: usize>`
  - selected through a generated `shipyard_job_type_id(index)` helper
- `display_name`
  - benchmark job name such as `job42`
- `system_fn`
  - closure that calls `execute_work(...)`
- `borrow_constraints`
  - each normalized resource access becomes one `TypeInfo`
  - `StorageId::Custom(resource_id)` preserves runtime resource identity directly
  - `Mutability::Shared` for reads
  - `Mutability::Exclusive` for writes
- `after`
  - uses `normalized.dagga_predecessors[index]`
  - this preserves:
    - explicit predecessors
    - `Unknown` barriers
    - strict declaration-order overlays
- local thread pool
  - benchmark-local Rayon pool when benchmark parallelism is greater than 1

### Why `dagga_predecessors` Is The Right Overlay Here

Shipyard already understands read/write conflicts natively through `borrow_constraints`.

The overlay is only needed for semantics that are not purely borrow-based:

- strict order when overlap would otherwise be legal
- full barriers for `Unknown`
- explicit predecessor hints

Using `dagga_predecessors` gives that without destroying the native access scheduler.

## Non-Obvious Constraint

The adapter needs synthetic unique `TypeId`s because of Shipyard's internal deduplication.

Why this matters:

- if multiple jobs reuse the same `type_id`, Shipyard will store one runtime system closure and map multiple logical jobs onto it
- that would destroy the benchmark's "one job = one scheduled unit" invariant

The integration solves this by generating a const-generic marker family and a runtime index-to-`TypeId` helper.

Current capacity:

- `SHIPYARD_MAX_BENCH_JOBS = 4096`

This is comfortably above current benchmark sizes.

## Comparison To Local Experiments

Closest local comparison:

- conceptually closest to `experiment_01` and `experiment_02`, because the runtime is access-based rather than explicit-DAG-first

Key differences from local experiments:

- Shipyard is system-centric and expects scheduling to happen around world/storage metadata
- access metadata is storage-oriented rather than page-oriented or custom-reservation-oriented
- it ships with a mature batching/scheduling implementation instead of the repository's specialized narrower problem model

What Shipyard is likely to be good at:

- stable access-set scheduling
- low adapter distortion for runtime-keyed resources
- pragmatic parallel workload execution

What Shipyard is not directly testing:

- any of the repository's custom heuristics around reservations, pages, or static graph reduction

## Verification Results

Commands run:

- `cargo check --bench experiment`
- `cargo bench --bench experiment --no-run`
- `cargo bench --bench experiment -- experiment/run_overhead/shipyard/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_parallelism/shipyard/layer_barrier_stress`

Observed results from this pass:

- `run_overhead/wide_independent/jobs512_zero`
  - approximately `113.85 us` to `139.45 us`
- `run_parallelism/layer_barrier_stress`
  - approximately `1.755 ms` to `1.830 ms`

## Caveats

Important caveat:

- this adapter bypasses Shipyard's normal "convert typed system fn into workload system" route and constructs `WorkloadSystem` directly

That is intentional.

Why it is still fair:

- the benchmark problem is a runtime-generated system graph, not a static set of hand-written Rust system functions
- Shipyard exposes `WorkloadSystem` publicly, so this is a supported low-level path, not a private hack

## If You Need To Modify This Adapter

Follow this checklist:

1. Re-read `create_workload.rs` and confirm Shipyard still deduplicates by `type_id`.
2. Keep unique `TypeId` generation intact unless upstream behavior changes.
3. Do not replace native `borrow_constraints` with explicit edges for relaxed conflicts.
4. Keep explicit overlay edges limited to `dagga_predecessors`, not `explicit_dag_predecessors`.
5. Re-run:
   - `cargo check --bench experiment`
   - one `run_overhead` filtered benchmark
   - one `run_parallelism` filtered benchmark

## Open Questions Worth Revisiting Later

- whether Shipyard's workload builder exposes enough batch metadata to record schedule quality directly in a future benchmark extension
- whether per-job `weight_hint` could be mapped into anything meaningful if a future Shipyard version exposes scheduling priorities

## Progress Log

- 2026-04-04: Read Shipyard scheduler internals and confirmed public low-level hooks were sufficient.
- 2026-04-04: Integrated `ShipyardAdapter` with direct `WorkloadSystem` construction, native borrow constraints, and native `after` edges.
