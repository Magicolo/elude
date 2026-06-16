# Flecs Competitor Dossier

Last updated: 2026-04-04
Status: integrated into benchmarks
Primary crate: `flecs_ecs = 0.2.2`
License: `MIT`
Repository: `https://github.com/Indra-db/Flecs-Rust`
Underlying engine: `flecs` C/C++ ECS by Sander Mertens

## Why This File Exists

Flecs is the most semantically different competitor in the current benchmark set.

Future agents should read this before changing:

- `FlecsAdapter`
- any benchmark interpretation involving Flecs
- any attempt to "make Flecs fairer" by changing the workload representation

## Primary Sources Read

Local crate sources inspected:

- `~/.cargo/registry/src/.../flecs_ecs-0.2.2/Cargo.toml`
- `~/.cargo/registry/src/.../flecs_ecs-0.2.2/src/prelude.rs`
- `~/.cargo/registry/src/.../flecs_ecs-0.2.2/src/core/world/system.rs`
- `~/.cargo/registry/src/.../flecs_ecs-0.2.2/src/addons/system/system_builder.rs`
- `~/.cargo/registry/src/.../flecs_ecs-0.2.2/src/addons/system/mod.rs`
- `~/.cargo/registry/src/.../flecs_ecs-0.2.2/src/core/world/pipeline.rs`
- `~/.cargo/registry/src/.../flecs_ecs-0.2.2/src/core/entity_view/entity_view_mut.rs`
- `~/.cargo/registry/src/.../flecs_ecs-0.2.2/src/core/term.rs`
- `~/.cargo/registry/src/.../flecs_ecs-0.2.2/src/core/query_builder.rs`

Official docs also matter here because the threading model is easy to misremember. The most important statement is from the Flecs pipeline docs and Rust wrapper comments:

- `set_threads` causes systems to distribute matched entities across worker threads
- pipelines and `DependsOn` control execution order and synchronization

## Mental Model

Flecs is not primarily "a small-job graph scheduler".

Its strong path is:

- systems over matched entities
- optional multithreaded iteration within a system
- pipelines managing phase order and synchronization

This matters because the benchmark problem in this repository is:

- many independent or partially-conflicting jobs
- each job is a single logical unit of work
- parallelism is mostly about running many jobs at once, not about splitting one job over many entities

So Flecs is a competitor, but not a perfect semantic match.

## Relevant Public Pieces

Important public surfaces:

- `World::system_named::<...>()`
- `SystemBuilder`
- `System::run(...)`
- `EntityViewMut::depends_on(...)`
- `World::set_threads(...)`
- dynamic query terms:
  - `term()`
  - `set_id(...)`
  - `read_curr()`
  - `write_curr()`
- `World::progress()`

These were enough to integrate Flecs without going through a different language binding or without using a custom C-side scheduler.

## Important Feature-Selection Note

Attempting to compile the Rust wrapper with a reduced feature set exposed an upstream wrapper issue during this task:

- `flecs_ecs = { default-features = false, features = ["flecs_base"] }`
  - did not compile cleanly in this environment because generated bindings referenced alerts-related types behind mismatched feature gating

The benchmark therefore uses:

- `flecs_ecs = "0.2.2"`

That means default crate features are enabled.

Do not silently "optimize" this manifest line back to a smaller feature set unless you re-verify that the wrapper compiles correctly.

## Scheduler Behavior Relevant To This Repository

### Explicit Ordering

Flecs exposes native ordering through system entities and `DependsOn`.

This is the most important ordering primitive for the adapter.

### Access Metadata

Flecs supports dynamic terms with runtime ids and explicit read/write intent.

The adapter uses:

- `term().set_id(resource_entity)`
- `read_curr()` for reads
- `write_curr()` for writes

This declares scheduler-visible access terms without requiring static Rust component types.

### Threading

`World::set_threads(n)` configures worker threads.

Important caveat:

- this does not mean Flecs is primarily running many different systems concurrently the way these benchmarks think about job parallelism
- it mainly enables splitting system work over matched entities

That is the central interpretation hazard.

## Benchmark Adapter Design

Implementation location:

- `benches/experiment.rs`

Adapter name:

- `FlecsAdapter`

Compiled state:

- `FlecsCompiled { world }`

### Exact Mapping

Each benchmark job becomes one Flecs system.

Per-job mapping:

- system identity
  - created with `world.system_named::<()>(&job.name)`
- resource identities
  - each normalized benchmark resource becomes one Flecs entity
- access terms
  - declared dynamically with `term().set_id(resource_entity)`
  - reads use `read_curr()`
  - writes use `write_curr()`
- explicit ordering
  - each system gets `depends_on(...)` edges for `normalized.dagga_predecessors[index]`
- runtime work
  - the system callback runs `execute_work(...)`
- schedule execution
  - adapter runs `world.progress()`

### Why The Adapter Uses `dagga_predecessors`

This is the most important design choice in the Flecs integration.

Why it was done:

- the benchmark semantics require real exclusion and explicit order overlays
- Flecs's normal scheduling model is centered on pipelines, phases, sync points, and entity processing
- relying on access terms alone would not be a trustworthy way to preserve this benchmark's semantics for write/write and barrier-like cases

So the adapter uses native Flecs order edges to preserve correctness and comparability.

This makes the Flecs result a realistic measure of Flecs used as a job orchestrator for this problem, not as a best-case Flecs entity-processing showcase.

## Comparison To Local Experiments

Closest local comparison:

- there is no perfect one

Why:

- local experiments `01`, `02`, and `03` are purpose-built for runtime job scheduling
- Flecs is a general ECS engine whose scheduler is optimized around system pipelines and entity iteration

What the benchmark is really asking when it includes Flecs:

- if a team used Flecs to orchestrate comparable jobs as systems, how much overhead and how much parallelism would they actually get

That is a fair comparison.

It is not trying to answer:

- how fast Flecs is for large multithreaded entity loops

## Verification Results

Commands run:

- `cargo check --bench experiment`
- `cargo bench --bench experiment --no-run`
- `cargo bench --bench experiment -- experiment/run_overhead/flecs/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_parallelism/flecs/layer_barrier_stress`

Observed results from this pass:

- `run_overhead/wide_independent/jobs512_zero`
  - approximately `43.279 us` to `55.475 us`
- `run_parallelism/layer_barrier_stress`
  - approximately `14.661 ms` to `19.033 ms`

Interpretation:

- Flecs is very fast on the zero-work orchestration case in this adapter
- it performs much worse on the conflict-heavy layer-barrier case than the Rust ECS schedulers integrated here

That is consistent with the earlier design analysis that Flecs is not primarily optimized for this job-graph problem shape.

## Caveats

This is the most important caveat list in the current external benchmark set.

### Caveat 1: This is not Flecs's favorite problem

Flecs is strongest when systems process many entities and worker threads can split that work.

This benchmark mostly uses:

- one logical callback per job/system
- explicit system dependencies
- resource-style conflicts

That is a legitimate use of Flecs, but not its peak use case.

### Caveat 2: The adapter does not synthesize fake entity batches just to help Flecs

That idea was considered and rejected.

Why it was rejected:

- it would stop being the same benchmark problem
- it would give Flecs an entity-splitting advantage unrelated to the user's main question, which is job-level scheduler parallelism

### Caveat 3: Default crate features are enabled

This is currently a wrapper stability decision, not a modeling preference.

If the wrapper fixes reduced-feature compilation later, this should be revisited.

## If You Need To Modify This Adapter

Checklist:

1. Re-read the threading comments in `world/pipeline.rs`.
2. Decide explicitly whether you are benchmarking:
   - Flecs as a job orchestrator
   - or Flecs as an entity-splitting ECS runtime
3. Do not mix those two interpretations silently.
4. Keep `DependsOn`-based ordering unless you have a stronger correctness-preserving alternative.
5. Re-run:
   - `cargo check --bench experiment`
   - one `run_overhead` filtered benchmark
   - one `run_parallelism` filtered benchmark

## Open Questions Worth Revisiting Later

- whether a second Flecs-specific benchmark family should be added in the future to show Flecs in its natural multithreaded entity-processing mode, clearly labeled as ECS-native rather than job-orchestration
- whether upstream Flecs documentation or examples expose a better direct representation of system-level mutual exclusion without over-serializing through explicit edges

## Progress Log

- 2026-04-04: Read Flecs world/system/pipeline/term APIs and confirmed dynamic system construction was feasible.
- 2026-04-04: Found that the reduced-feature Rust wrapper configuration did not compile cleanly in this environment.
- 2026-04-04: Integrated `FlecsAdapter` using dynamic systems, dynamic access terms, native `DependsOn`, and `World::progress()` execution.
