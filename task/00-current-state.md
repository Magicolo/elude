# Current State Reference

Last updated: 2026-04-04
Status: completed reference document

## Purpose

This file is a cold-start map of the repository as it existed when the task plan was created. Read this before touching any scheduler task.

## Repository State

Confirmed on 2026-04-04:

- `git status --short` is clean
- current experiment docs and task files are already present in-tree
- existing Cargo dependencies are:
  - runtime: `parking_lot`, `rayon`, `anyhow`
  - dev: `checkito`, `criterion`

This matters because future tasks may safely assume a clean baseline before they start making changes.

## Shared Public Surface

The shared experiment-facing API is in `src/experiment/mod.rs`.

The important types and traits are:

- `experiment::Job<S>`: closure plus declared dependencies
- `experiment::Scheduler<S>`: builder plus `schedule()`
- `experiment::CompiledSchedule<S>`: repeated-run execution surface

All experiments currently follow:

`Scheduler::new().add(Job::new(...)).schedule()?.run(&state)?`

Jobs take `&S`, not `&mut S`. Any benchmark or external adapter must preserve that soundness model: the scheduler manages logical concurrency, while mutation happens through interior mutability or explicit synchronization inside `S`.

Additional details that matter for adapters and `experiment_04`:

- `Job::new` stores a boxed `Fn(&S) -> anyhow::Result<()> + Send + Sync + 'static`
- `Scheduler::add` consumes and returns `Self`, so the builder style is persistent/chained rather than `&mut self`
- `CompiledSchedule::run` takes `&mut self`, which means compiled schedules are allowed to own mutable runtime state between runs
- `with_parallelism(None)` is the standard entry point, with each experiment defaulting internally to `thread::available_parallelism()` when possible

## Dependency Model Snapshot

The shared dependency language lives in `src/depend.rs`.

Important facts:

- `Key` currently supports `Identifier`, `Address`, `Type`, and `Path`
- `Order` is only `Relax` or `Strict`
- `Dependency` is `Unknown`, `Read(Key, Order)`, or `Write(Key, Order)`
- `Unknown` behaves as a barrier-like strict outer conflict across the experiment family

There is also an older `Conflict` helper in `src/depend.rs`.

That helper is still part of the library, but the experiment schedulers do not currently use it directly. Future refactors can borrow ideas from it, but should not assume it is already the shared implementation layer for the experiments.

## Broader Crate Layout

`src/lib.rs` exports more than the experiment modules:

- `depend`
- `error`
- `experiment`
- `experiment_01`
- `experiment_02`
- `experiment_03`
- `graph`
- `job`
- `work`

The `graph`, `job`, and `work` modules appear to belong to an older scheduler path and include commented design notes and builder-style APIs. They are useful as historical context, but they are not part of the current experiment benchmark surface.

Future agents should avoid mixing those older modules into the experiment work unless there is a deliberate refactor plan.

## Shared Correctness Coverage

Behavioral tests are centralized in `tests/support/mod.rs`. All current experiments run the same suite through separate test entry points.

The existing suite verifies:

- empty schedules run
- repeated runs execute jobs exactly once per run
- pairwise dependency semantics for `None`, `Relax`, `Strict`, and `Unknown`
- invalid internal dependency combinations are rejected
- duplicate reads within one job are accepted
- strict order is preserved
- simple overlap cases actually overlap on a two-thread scheduler

The suite is good for semantic correctness. It is not a performance or parallelism-diagnostic suite.

Important current coverage gaps:

- no shared test for job error propagation and schedule reset after cancellation
- no shared test for starvation or fairness under heavy relaxed contention
- no shared test for compile-time structure quality such as edge counts, group counts, or page packing quality
- no shared test for repeated-run behavior under failures
- no shared test for explicit `with_parallelism(Some(1))` sequential fallback semantics

Each experiment also has a small internal unit-test block in its `model.rs`, but those tests are mostly local algorithm sanity checks, not broad behavioral coverage.

## Current Benchmark Harness

The current Criterion harness is `benches/experiment.rs`.

What it currently does:

- benchmarks compile time and run time separately
- builds workloads from a small `Workload` struct
- varies:
  - number of jobs
  - conflict family
  - simulated per-job runtime in milliseconds
- compares `experiment_01`, `experiment_02`, and `experiment_03`

Concrete current benchmark shape:

- compile workloads: 6 named cases
- run workloads: 10 named cases
- Criterion configuration:
  - compile groups: sample size 10, flat sampling, 1s measurement, 200ms warmup
  - run groups: sample size 10, flat sampling, 2s measurement, 250ms warmup
- runtime state:
  - `BenchState { counters: Box<[AtomicU64]>, checksum: AtomicU64 }`
  - job bodies spin until `Instant::now() + duration`
  - after the spin loop each job performs two relaxed atomic updates
- `stripe_count` currently maps:
  - `0..=16 -> 1`
  - `17..=128 -> 4`
  - `>=129 -> 16`
- benchmark parallelism is always `thread::available_parallelism()`

Current limitations:

- no external-library adapters
- no standard `0ms` workload
- current job body always uses `Instant::now()` loops and atomic checksum updates, which means very short jobs measure more than scheduler overhead
- workload matrix is broad but not explicitly designed to expose the known design differences between:
  - dynamic reservation
  - sequential groups
  - static DAG orientation
- no benchmark naming or structure for:
  - barrier sensitivity
  - straggler sensitivity
  - cross-layer overlap opportunities
  - compile-heavy graph quality heuristics

Why the current job body is especially misleading for `0ms`-like cases:

- it uses wall-clock polling in a tight loop
- it performs shared atomic updates on completion
- it therefore measures more than scheduler dispatch and dependency coordination

That makes task `01` foundational rather than optional.

## Experiment 01 Snapshot

Location:

- `src/experiment_01/README.md`
- `src/experiment_01/DESIGN.md`
- `src/experiment_01/model.rs`

High-level identity:

- compile strict conflicts into predecessor counters plus flat successor lists
- compile relaxed conflicts into page reservation masks
- runtime tries jobs dynamically using page reservations and ready-bit rescans

Runtime representation:

- one Rayon thread pool
- flat boxed slices for:
  - jobs
  - pages
  - successors
  - acquires
  - residents
  - followers
  - roots
- per-page `AtomicU64` state:
  - low 32 bits: reservation mask
  - high 32 bits: ready bits
- per-job:
  - `home`
  - `strict_wait_initial`
  - `strict_wait`
  - successor range
  - acquire range

Important current functions and structures:

- `Schedule::compile`
- `reduce_strict`
- `assign_homes`
- `attempt_job`
- `run_job`
- `reserve_page`
- `scan_page`
- `followers_by_page` construction inside compile

What it is currently optimizing for:

- recover parallelism beyond sequential layers
- keep runtime representation flat and compact
- avoid heavyweight per-job graph state

Likely bottlenecks or investigation targets:

- `O(n^2)` pairwise relation classification
- simple page assignment heuristic: connected relaxed components, degree sort, chunks of 32
- potentially noisy follower-page scans after every completion
- ready-bit rescans may do useless work when conflicts are dense
- repeated CAS retries on hot pages
- no explicit fairness strategy
- duplicated normalization and relation logic with `03`
- runtime scans are page-scoped rather than job-targeted, which is elegant but may be coarse under contention

What must remain true if this experiment is improved:

- it should still be recognizably the dynamic reservation scheduler
- it should not collapse into pure sequential groups
- it should not collapse into a pre-oriented static DAG only

## Experiment 02 Snapshot

Location:

- `src/experiment_02/README.md`
- `src/experiment_02/DESIGN.md`
- `src/experiment_02/model.rs`

High-level identity:

- compile jobs into sequential groups that are safe to run in parallel internally
- runtime executes one whole group at a time

Runtime representation:

- one Rayon thread pool
- one `threads` count
- one flat boxed slice of job closures
- one flat boxed slice of `Group { job_range }`

Compile-time structures of note:

- `HashMap<Key, Tracker>` history
- `Vec<GroupSummary>`
- `Vec<Vec<Run<S>>>` before flattening

Important current functions and structures:

- `earliest_group`
- `GroupSummary::is_compatible`
- `update_trackers`
- compile-time forward scan for first compatible group
- runtime group loop using Rayon `par_iter`

What it is currently optimizing for:

- tiny runtime overhead
- low scheduler-owned synchronization at run time
- repeated runs of a precomputed grouping

Likely bottlenecks or investigation targets:

- group barriers hide parallelism
- first-fit forward scan may produce avoidable barriers
- `HashMap<Key, GroupAccess>` summaries may be expensive and cache-unfriendly
- compile logic duplicates dependency normalization patterns from other experiments
- no use of workload-specific cost hints
- current design has no explicit compile metric tracking such as total groups, average group width, or hot-key density
- because runtime is intentionally tiny, most meaningful improvements must come from better compile placement rather than runtime tricks

What must remain true if this experiment is improved:

- it should still be a layered scheduler
- runtime should remain group-by-group, not per-job dynamic scheduling

## Experiment 03 Snapshot

Location:

- `src/experiment_03/README.md`
- `src/experiment_03/DESIGN.md`
- `src/experiment_03/model.rs`

High-level identity:

- compile a strict DAG
- choose a priority order that extends the strict DAG
- orient relaxed conflicts statically using that order
- run a flat DAG executor with predecessor counters and direct successor wakeups

Runtime representation:

- one Rayon thread pool
- flat boxed slices for:
  - jobs
  - successors
  - roots
- per-job:
  - `wait_initial`
  - atomic `wait`
  - successor range

Compile-time structures of note:

- pairwise strict/relaxed classification
- reduced strict DAG
- greedy priority order
- per-key access event lists
- final predecessor reduction in priority order

Important current functions and structures:

- `build_priority_order`
- `build_job_stats`
- `build_final_predecessors`
- `reduce_predecessors_declaration_order`
- `reduce_predecessors_priority_order`
- `spawn_job`
- `run_job`

What it is currently optimizing for:

- very small runtime hot path
- finer granularity than sequential groups
- no runtime reservation protocol

Likely bottlenecks or investigation targets:

- current heuristic for priority order is simple and may leave parallelism on the table
- per-key sparse orientation ignores some multi-key coupling effects
- compile time is already heavy and can likely be increased further if graph quality improves
- like `01`, it pays `O(n^2)` pairwise relation cost
- the current heuristic is purely structural and has no duration or empirical runtime feedback
- cancellation/reset handling exists, but the shared test suite does not currently stress it

What must remain true if this experiment is improved:

- it should still be a static DAG executor
- it should not grow a runtime conflict reservation protocol
- it should not become a grouped/barrier scheduler

## Cross-Cutting Observations

1. Shared logic is duplicated.

- Dependency normalization is implemented separately in all three experiments.
- Pairwise relation classification logic exists separately in `01` and `03`.
- This is acceptable for experiments, but adding external schedulers and `04` will increase maintenance pressure. A shared helper layer may be worth introducing if it does not distort the experiment boundaries.

2. Runtime representations are already intentionally flat.

- `01` uses flat arrays plus page state.
- `02` uses flat job and group slices.
- `03` uses flat successor slices and atomic wait counters.
- This is a project-wide design instinct and should be preserved unless a different layout wins clearly in benchmarks.

3. Benchmarks are currently the weakest link.

- The existing harness compares implementations, but it does not yet isolate the key research question: where does each scheduler expose more or less parallelism?
- Workload redesign is not optional. It is foundational.

4. The lock-free requirement has been dropped.

- This materially changes the search space for `01` and for the new `04`.
- It also means external-library lessons from lock-based schedulers are now directly relevant.

5. The repo already assumes repeated schedule execution is important.

- `01` and `03` both explicitly optimize for compile-once / run-many.
- `04` should take that assumption seriously rather than treating each run as independent.

6. Current tests are semantically useful but weak on scheduler quality.

- They can catch wrong overlap or wrong ordering.
- They will not tell you whether a change destroyed useful parallelism while keeping semantics technically correct.
- That is why task `01` should happen before serious scheduler tuning.

## Cold-Start Checklist For Future Agents

If you are resuming the project with no context:

1. Confirm repo state with `git status --short`.
2. Read `task/README.md`.
3. Read this file fully.
4. Re-read:
   - `src/experiment/mod.rs`
   - `src/depend.rs`
   - `benches/experiment.rs`
   - `tests/support/mod.rs`
5. Only then start the next task file.

## Suggested Commands For Future Work

Read-only inspection:

- `rg --files`
- `sed -n '1,240p' benches/experiment.rs`
- `sed -n '1,260p' src/experiment_01/model.rs`
- `sed -n '1,260p' src/experiment_02/model.rs`
- `sed -n '1,260p' src/experiment_03/model.rs`

Likely validation commands later:

- `cargo test`
- `cargo test --test experiment_01`
- `cargo test --test experiment_02`
- `cargo test --test experiment_03`
- `cargo bench --bench experiment`

## Progress Log

- 2026-04-04: File created from a repo reading pass.
- 2026-04-04: Expanded with repository-state confirmation, dependency model details, benchmark specifics, runtime layout notes, and current coverage gaps. Task `00` is complete.
