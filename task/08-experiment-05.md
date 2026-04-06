# Task 08: Design And Research Experiment 05

Last updated: 2026-04-05
Status: in progress
Priority: highest

## Goal

Design a final `experiment_05` that is dramatically more contrastive than
`01-04` and is explicitly aimed at becoming the strongest all-around runtime in
the repository.

This task is not a small follow-up optimization. It is a research task.

The primary objective is:

- minimize repeated-run execution time as aggressively as possible

Secondary objectives:

- preserve the shared dependency semantics exactly
- accept substantially higher compile-time cost if runtime improves
- exploit data locality and low-level operations more aggressively than prior
  experiments
- be willing to use locks, custom executors, bitmasks, cache-aware layouts, and
  unusual compiled representations if they improve runtime

## Why A Fifth Experiment Is Needed

The final result summary in `task/07-results-summary.md` leaves a clear gap:

- `experiment_02` is the runtime-overhead champion for `0ms` work, but it loses
  parallelism because of group barriers
- `experiment_03` is the strongest all-around local scheduler, but its runtime
  is still fundamentally a per-job DAG wakeup engine
- `experiment_04` proved that schedule shape matters, but it still rides on the
  same DAG executor core as `03`
- `experiment_01` proved that dynamic choice can recover parallelism, but its
  runtime conflict protocol is too expensive on the hardest shapes

So the final unexplored target is:

- can we get `experiment_02`-like hot-path cheapness
- while keeping `experiment_03`-like fine-grained parallelism
- and still retain some of the dynamic adaptivity that made `01` and `04`
  interesting

That requires a representation that is not just:

- page reservations
- sequential groups
- a plain explicit DAG
- a portfolio of plain explicit DAGs

## Prior Learnings That Must Shape The Design

### From `experiment_01`

- runtime flexibility is real, but broad runtime coordination is expensive
- retry/rollback and general reservation machinery are too costly at scale
- targeted wakeups helped, but the underlying model is still runtime-heavy

### From `experiment_02`

- the absolute cheapest runtime is one that does almost nothing per job
- group barriers hide a lot of parallelism that the scheduler cannot recover at
  run time

### From `experiment_03`

- the best all-around current result came from minimizing runtime work
- criticality ordering and work-first wakeups matter a lot
- direct explicit per-job wakeups are still not the end of the story if the
  execution unit itself could become coarser and more cache-local

### From `experiment_04`

- some workloads do require different legal choices among relaxed conflicts
- a portfolio is valid, but paying for multiple plain DAGs is not obviously the
  final answer
- the interesting part was not “many schedules”; it was “runtime can benefit
  when local decisions stay flexible”

### From External Libraries

- `shipyard` showed that native access metadata plus cheap runtime batching is a
  strong practical combination
- `dagga` showed that schedule quality can justify very heavy compile cost, but
  only if runtime stays worth it
- `dag_exec` showed that a generic explicit DAG runtime is not enough by itself
- `flecs` showed that very low orchestration overhead is possible in a system
  built around cheap structural scheduling, but semantic mismatch can dominate

## Design Space Survey

This section evaluates several radically different directions before choosing
one.

### Hypothesis A: Async/Future Dataflow Scheduler

Idea:

- compile jobs into futures
- use wakers or channels to resume jobs when predecessors/resources unblock
- possibly use an async runtime instead of a thread pool

Potential strengths:

- elegant event model
- easy cancellation and resumption semantics
- can represent dynamic waits naturally

Problems:

- waker traffic and future state machines are likely too expensive at this
  granularity
- executor wakeups are optimized for I/O and cooperative tasks, not dense
  in-memory job graphs
- this is extremely unlikely to beat `experiment_03` on `0ms` or barrier-heavy
  cases

Decision:

- rejected as the primary `experiment_05` direction
- may still be worth a tiny curiosity POC later, but it is not the main line

### Hypothesis B: Global Lock-Based Ready Queue With Resource Wait Lists

Idea:

- embrace locks openly
- keep a global ready queue
- keep explicit per-resource waiter lists and grant work dynamically

Potential strengths:

- simple mental model
- highly dynamic
- could learn from reader/writer lock policies

Problems:

- too close to `experiment_01` in spirit
- likely to reintroduce convoying and cross-resource coordination costs
- hard to see a path to `experiment_02`-class overhead

Decision:

- rejected as the main direction
- useful as a fallback mental model, but not sufficiently contrastive

### Hypothesis C: Full-Schedule Frontier Scanning With SIMD/Bitsets

Idea:

- keep the whole schedule as bitsets
- repeatedly scan for ready jobs using wide mask operations
- use SIMD or large word operations to accelerate eligibility checks

Potential strengths:

- very compact state
- elegant use of bit operations

Problems:

- full-frontier rescanning is likely too much work per completion
- even very fast scans still scale with schedule size rather than with local
  change
- does not obviously preserve locality under heavy partial overlap

Decision:

- rejected as the whole design
- retained as an inner-kernel optimization idea for any cluster-local engine

### Hypothesis D: Compiled Concurrent Resource Automata

Idea:

- compile each resource key into a local automaton that grants compatible jobs
- jobs fire when all of their resource automata grant them
- execution becomes a composition of concurrent finite-state machines

Potential strengths:

- genuinely different from a DAG
- keeps relaxed conflicts dynamic without global page reservations
- naturally expresses read batches and writer exclusion

Problems:

- multi-key jobs create difficult cross-automaton coordination
- naive locking across several resource automata risks deadlock or rollback
- high semantic elegance, but large implementation risk

Decision:

- partially adopted, but not as a pure per-resource design
- the automaton idea will be kept, but moved to the cluster level where the
  coordination problem is smaller and more local

### Hypothesis E: Hierarchical Cluster Automata With Custom Cluster Executor

Idea:

- compile jobs into cache-local clusters of up to `N` jobs, likely `64`
- represent each cluster’s active execution state as a compact concurrent
  automaton using atomic bitmasks
- keep dynamic choice only inside a cluster
- keep a thinner explicit ordering spine only across clusters
- execute clusters, not jobs, as the unit of scheduling

Potential strengths:

- much more locality than per-job DAG execution
- dynamic decisions survive inside a bounded domain
- hot-path operations become:
  - `u64` mask ops
  - `trailing_zeros`
  - small atomic state updates
- compile-time clustering can absorb huge complexity if it reduces runtime work
- can combine:
  - `experiment_02`’s low runtime overhead instincts
  - `experiment_03`’s compile-once/run-many philosophy
  - `experiment_04`’s local schedule-shape flexibility

Problems:

- cluster partitioning quality is critical
- cross-cluster edges and mailboxes must stay cheap
- custom executor work is non-trivial

Decision:

- chosen as the target design for `experiment_05`

## Chosen Direction

`experiment_05` will target a:

- hierarchical cluster automata scheduler

Working name:

- `cluster_automata`

This is intentionally different from the prior experiments:

- `01`: dynamic reservations over global pages
- `02`: one static group sequence
- `03`: one static per-job DAG
- `04`: several static per-job DAGs
- `05`: compiled local dynamic automata plus a global cluster spine

## Core Concept

The schedule will be compiled into two levels.

### Level 1: Global Cluster Spine

Clusters become the main scheduling unit.

The spine captures:

- strict inter-cluster ordering
- any relaxed conflicts that were too expensive or too global to keep dynamic
- cluster wakeup dependencies

This spine should be much smaller than the per-job DAG in `03`.

### Level 2: Cluster-Local Concurrent Automaton

Inside each cluster:

- jobs are represented by local bit positions
- strict-local eligibility is tracked by local predecessor masks and/or small
  local counters
- relaxed-local conflicts remain dynamic through conflict masks
- the cluster runtime state is a tuple of atomic masks, for example:
  - `ready_mask`
  - `running_mask`
  - `done_mask`
  - `queued_mask`
  - `candidate_mask`

Each completion produces cluster-local transitions:

- clear running bit
- set done bit
- OR a small number of precomputed successor/candidate masks
- admit newly legal jobs by mask tests instead of per-job scans

That local state machine is the “concurrent automaton” part of the design.

## Why This Could Beat All Prior Experiments

### Why It Could Beat `experiment_02`

- no whole-group barrier across the full schedule
- dynamic choices remain inside clusters
- inter-cluster wakeups can be fine-grained

### Why It Could Beat `experiment_03`

- jobs are no longer the runtime scheduling unit
- many completions may stay inside one cluster with only mask operations
- fewer spawned units should mean much less executor overhead
- cluster-local dynamic conflict handling avoids orienting every relaxed choice
  globally

### Why It Could Beat `experiment_04`

- instead of compiling multiple whole DAGs, keep local flexibility in one
  execution representation
- exploit runtime-local state rather than portfolio duplication

### Why It Could Beat `experiment_01`

- no broad reservation rollback protocol
- no multi-page acquisition loops
- no general per-resource runtime search across the whole schedule

## Runtime Architecture Draft

### Hot Data Layout

The hot path should use aligned contiguous storage.

Likely structure:

- one `ClusterHot` per cluster, `#[repr(align(64))]`
- one compact `JobHot` array per cluster
- separate cold metadata for debug/docs/testing

Hot cluster fields may include:

- `ready_mask: AtomicU64`
- `running_mask: AtomicU64`
- `done_mask: AtomicU64`
- `queued: AtomicBool`
- `remote_pending: AtomicU32` or small mailbox counters
- `best_job_hint: AtomicU8` or cheap selection hint

### Cluster Execution Unit

A worker that pops a cluster id does not run one job.

It should:

1. acquire or claim the cluster
2. drain as much ready local work as possible
3. run several compatible local jobs before yielding the cluster
4. emit cross-cluster completion events in bulk
5. requeue the cluster only if work remains

This is the most important runtime contrast to the earlier designs.

### Global Executor

The current Rayon-based per-job spawn model is unlikely to be ideal here.

`experiment_05` should seriously consider a custom fixed worker pool with:

- one local queue/deque per worker
- one global injection queue for wakeups
- cluster ids as the only queued units
- blocking/parking only when all queues are empty

Likely tools to evaluate:

- `crossbeam-deque`
- `crossbeam-utils`
- `parking_lot::Condvar`

If Rayon is retained at first, it should be treated as a bootstrap step, not as
the final runtime assumption.

## Compile Pipeline Draft

The compile step may become much heavier than `03`.

Expected stages:

1. normalize dependencies
2. build strict and relaxed relation graphs
3. estimate neighborhood similarity and resource locality
4. partition jobs into clusters with a compile-heavy heuristic
5. choose which relaxed conflicts stay dynamic inside clusters
6. orient or collapse the remaining cross-cluster conflicts
7. build per-cluster automata tables and masks
8. build the global cluster spine
9. lay out hot/cold data contiguously and cache-align it

## Cluster Partitioning Hypotheses

This will likely determine whether the whole design works.

Partition goals:

- keep dense relaxed-conflict neighborhoods together
- keep strict chains with short fanout together when possible
- avoid clusters with too many cross-cluster conflict edges
- preserve locality for jobs likely to wake each other

Candidate heuristics:

- greedy neighborhood-overlap packing
- hot-resource seeding
- frontier-growth packing from strict-critical jobs
- cluster refinement by local swap

## Dynamic Policy Inside A Cluster

The local automaton should not just pick the first ready bit.

Candidate local policies:

- unlock potential first
- conflict relief first
- ready-batch preservation
- small hybrid score:
  - `unlocks_blocked_jobs`
  - minus `conflicting_ready_jobs`
  - plus `strict_successor_pressure`

This is where `experiment_04`'s central lesson gets reused without compiling
multiple whole schedules.

## SIMD And Bit Tricks

This experiment should aggressively use wide scalar bit operations first:

- `u64` masks
- `trailing_zeros`
- `count_ones`
- batched OR/ANDNOT updates

Portable SIMD is not the main dependency assumption yet because stable Rust
support is still less convenient than plain integer masks for this problem.

However:

- cluster-local candidate filtering across multiple `u64` words may later justify
  explicit SIMD or target intrinsics
- this should be a follow-up optimization once the cluster automaton model is
  working

## Data-Locality Principles

This experiment must treat locality as a first-class design constraint.

Rules:

- hot execution state stays compact and aligned
- cross-cluster events are batched, not emitted one pointer chase at a time
- per-job cold metadata stays out of the hot path
- worker ownership should prefer draining one cluster deeply before bouncing to
  another

## Initial Hypothesis Ranking

Ranked by expected ability to become the best all-around runtime:

1. Hierarchical cluster automata with custom cluster executor
2. Pure per-resource automata
3. Global lock-heavy dynamic scheduler
4. Full-schedule bitset/SIMD frontier scanner
5. Async/future dataflow scheduler

## Early POC Plan

To keep the task grounded, early work should not jump straight into
`src/experiment_05`.

The first POCs should answer:

1. does coarsening the runtime scheduling unit from job to cluster drastically
   reduce overhead?
2. can a local dynamic scoring rule capture the `experiment_04` tradeoff
   without compiling multiple whole DAGs?
3. does a cheap cluster-local automaton kernel look measurably better than a
   plain per-job wakeup kernel?

Planned initial examples:

- `examples/poc_cluster_granularity.rs`
- `examples/poc_bridge_policy.rs`
- `examples/poc_cluster_kernel.rs`
- `examples/poc_fixed_pool_clusters.rs`

## Initial POC Results

### POC 1: Cluster Granularity On The Existing Rayon Pool

Example:

- `cargo run --release --example poc_cluster_granularity`

Purpose:

- isolate the cost of the runtime scheduling unit itself without changing the
  thread pool
- compare:
  - one Rayon task per job
  - one Rayon task per cluster, draining jobs locally
  - one Rayon task per cluster, draining a local `u64` bitmask

Observed result on this machine:

- `rayon_job_tasks`
  - total about `653.99 ms`
  - average round about `2179.96 us`
- `rayon_cluster_loops`
  - total about `12.31 ms`
  - average round about `41.02 us`
- `rayon_cluster_masks`
  - total about `12.30 ms`
  - average round about `41.01 us`
- cluster execution was about `53x` faster than per-job task spawning

Interpretation:

- this is the strongest grounding result so far
- the runtime scheduling unit absolutely should move from “job” to “cluster”
- the win appears before changing any executor internals
- that makes cluster coarsening a mandatory part of `experiment_05`, not an
  optional optimization

### POC 2: Structural Bridge-Tradeoff Policy Choice

Example:

- `cargo run --release --example poc_bridge_policy`

Purpose:

- test whether a cheap local dynamic score can differentiate between the
  pre-heavy and post-heavy bridge shapes without compiling multiple whole DAGs

Observed result:

- `critical_path`
  - always chooses `bridge_first`
- `read_heavy`
  - always chooses `preserve_reads`
- `unlock_balance`
  - chooses `preserve_reads` when:
    - `ready_conflicting_reads = 96`
    - `blocked_successors = 32`
  - chooses `bridge_first` when:
    - `ready_conflicting_reads = 32`
    - `blocked_successors = 96`

Interpretation:

- a local runtime score can reproduce the core `experiment_04` tradeoff signal
  without compiling multiple whole schedules
- this supports keeping local dynamic choice inside clusters

Important limitation:

- this POC is structural, not a throughput benchmark
- it validates decision direction, not final runtime quality

### POC 3: Naive Mask Kernel Versus Per-Job Successor Counters

Example:

- `cargo run --release --example poc_cluster_kernel`

Purpose:

- test whether a naive cluster-local bitmask automaton kernel is already faster
  than a compact per-job successor-counter kernel

Observed result:

- `counter_kernel`
  - total about `99.38 ms`
  - average round about `198.77 ns`
- `mask_kernel`
  - total about `205.01 ms`
  - average round about `410.02 ns`
- naive mask kernel was only about `0.48x` as fast as the counter kernel

Interpretation:

- “replace successors with bitmasks” is not sufficient
- a useful automaton kernel must avoid repeated candidate rescans
- cluster-local masks remain promising, but only with heavier compile-time
  precomputation or more incremental transition tables

This is an important negative result and should shape the implementation plan.

### POC 4: Naive Fixed Pool Versus Rayon For Cluster Tasks

Example:

- `cargo run --release --example poc_fixed_pool_clusters`

Purpose:

- test whether a simple custom fixed thread pool already beats Rayon once the
  runtime unit is coarsened to clusters

Observed result:

- `rayon_cluster_loops`
  - total about `12.93 ms`
  - average round about `43.08 us`
- `fixed_pool_clusters`
  - total about `27.35 ms`
  - average round about `91.16 us`
- naive fixed pool was only about `0.47x` as fast as the Rayon cluster version

Interpretation:

- replacing Rayon with a naive mutex/condvar pool is not enough
- a custom executor remains possible, but only if it uses a much stronger
  design, likely:
  - work stealing
  - local deques
  - cheaper wakeup rules
- `experiment_05` should not assume that “custom pool” automatically means
  “faster”

## Direction Change Triggered By The POCs

The early plan assumed that `experiment_05` would likely want a custom
executor immediately.

After the POCs, the revised stance is:

- cluster-sized execution is definitely the right direction
- local dynamic policy is definitely worth keeping
- naive mask kernels are not good enough
- naive custom thread pools are not good enough

So the implementation order should become:

1. build the cluster automata model first
2. get the cluster runtime working even if it temporarily rides on Rayon
3. only replace the executor if a stronger pool design is proven faster by a
   later POC

## Revised Experiment 05 Shape

The chosen design remains hierarchical cluster automata, but with a more
precise focus:

- primary novelty:
  - cluster-local concurrent automata
  - cluster-sized execution units
  - local dynamic policy for relaxed conflicts
- secondary, still open research track:
  - whether a custom cluster executor can beat Rayon after the cluster model is
    in place

## Concrete Next Steps

1. Build a compile-time clusterer that packs dense relaxed neighborhoods into
   `<= 64` job regions.
2. Start with a hybrid runtime:
   - cluster-local dynamic masks
   - explicit cross-cluster spine
   - Rayon only as a temporary cluster executor
3. Eliminate naive candidate rescans by compiling stronger local transition
   tables than the `poc_cluster_kernel` prototype used.
4. Add one later executor POC with:
   - local worker deques
   - a global injection queue
   - cluster ids only
   before committing to a non-Rayon final runtime.

## Implemented Prototype

The first real `experiment_05` prototype now exists in:

- `src/experiment_05/mod.rs`
- `src/experiment_05/model.rs`
- `src/experiment_05/README.md`
- `src/experiment_05/DESIGN.md`
- `tests/experiment_05.rs`

It is a working scheduler behind the shared API, not just a design note.

### Prototype Architecture

The current implementation uses:

- compile-time affinity clustering with a hard cap of `16` jobs per cluster
- one queued Rayon task per active cluster
- cluster-local dynamic ordering
- local `u64` predecessor masks
- explicit cross-cluster predecessor counters

The cluster task drains multiple local jobs inline before yielding.

That is the key runtime contrast with `03` and `04`: the executor sees
clusters, not individual ready jobs.

### Local Dynamic Rule That Shipped

The prototype only keeps local dynamic freedom for relaxed conflicting keys
that are fully contained within one cluster.

This matters because it avoids the unsound or overcomplicated case where a
local runtime choice would need to coordinate with the rest of the global
schedule.

### Important Heuristic Correction

The first cluster affinity heuristic over-grouped around hot relaxed keys.

That was a mistake.

It hurt workloads like `portfolio_bridge_tradeoff`, because a wide read/write
hotspot that spans many jobs cannot become a local-only relaxed key anyway.
Packing those jobs together just serializes them under cluster exclusivity.

The heuristic was then revised:

1. penalize relaxed grouping when the shared conflicting key is wider than the
   cluster size and the pair is read/write
2. allow a smaller positive affinity for wide writer/writer hotspots, because
   those become serial once globally oriented anyway and can still benefit from
   cluster draining

This second version produced the benchmark results below.

## Optimization Round 2

After the first prototype landed, the next optimization pass focused on the
remaining generic-runtime waste rather than changing the representation again.

The main changes were:

- compile each cluster into one of three runtime modes:
  - `Dynamic`
  - `Static`
  - `Serial`
- switch inter-cluster wakeups to a work-first chain, so one newly-ready
  cluster can continue inline instead of always bouncing through the pool
- stop using `fetch_or` on `done_mask` in the cluster owner thread and publish a
  locally-maintained `done_mask` with plain atomic stores
- add a schedule-level `trivial_parallel` fast path that bypasses the entire
  cluster runtime when the whole schedule compiles to terminal singleton
  clusters

The last item is especially important. It means `experiment_05` no longer pays
cluster machinery at all on the “just run many independent jobs” shape.

## Optimization Round 3

The next pass added schedule-level specializations on top of the cluster
runtime.

The main additions were:

- a `SingletonDag` specialization for schedules that compile entirely to
  singleton clusters
- a `SerialChain` specialization for schedules whose final dependency graph is a
  single linear chain
- removal of the eager wait-counter reset sweep from the singleton-DAG hot path

The design intent is simple:

- if clustering collapses completely to one-job clusters, `05` should stop
  pretending it is a cluster runtime and run a direct work-first DAG path
- if the final graph is globally serial, `05` should just run a plain loop

This keeps the cluster-automata representation as the default, but lets the
compiled schedule escape to a cheaper runtime when the structure proves that the
full machinery is unnecessary.

## Grounded Benchmark Results

Commands used:

- `cargo bench --bench experiment -- experiment/run_overhead/experiment_05/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_overhead/experiment_03/wide_independent/jobs512_zero`
- `cargo bench --bench experiment -- experiment/run_overhead/experiment_05/hot_key_write_contention/jobs256_zero`
- `cargo bench --bench experiment -- experiment/run_overhead/experiment_03/hot_key_write_contention/jobs256_zero`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_05/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_03/layer_barrier_stress`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_05/portfolio_bridge_tradeoff`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_03/portfolio_bridge_tradeoff`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_04/portfolio_bridge_tradeoff`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_05/mixed_hotspots`
- `cargo bench --bench experiment -- experiment/run_parallelism/experiment_03/mixed_hotspots`
- `cargo bench --bench experiment -- experiment/compile/experiment_05/compile_heavy_sparse_keys`

All benchmark comparisons below were rerun sequentially during this optimization
pass. Earlier concurrent Criterion runs were discarded as invalid for
comparison.

### Best Signals

- `run_overhead / wide_independent / jobs512_zero`
  - `experiment_05`: about `31.12 µs .. 31.78 µs`
  - `experiment_03`: about `264.92 µs .. 269.18 µs`
  - interpretation:
    - the schedule-level trivial-parallel fast path turned this into a major
      win for `05`
- `run_parallelism / layer_barrier_stress`
  - `experiment_05`: about `590.29 µs .. 594.72 µs`
  - `experiment_03`: about `588.73 µs .. 594.12 µs`
  - interpretation:
    - `05` is now effectively tied with `03` on this barrier-heavy case
- `run_parallelism / portfolio_bridge_tradeoff / pre96...post32...`
  - `experiment_05`: about `490.26 µs .. 495.77 µs`
  - `experiment_03`: about `528.47 µs .. 531.91 µs`
  - `experiment_04`: about `1.351 ms .. 1.394 ms`
  - interpretation:
    - the singleton-DAG specialization improved this shape further and `05`
      still beats both `03` and `04` here
- `run_parallelism / mixed_hotspots / blocks48`
  - `experiment_05`: about `1.511 ms .. 1.535 ms`
  - `experiment_03`: about `1.508 ms .. 1.560 ms`
  - interpretation:
    - `05` is now in the same class as `03` on this mixed locality/conflict
      workload instead of trailing badly

### Current Weak Spots

- `run_overhead / hot_key_write_contention / jobs256_zero`
  - `experiment_05`: about `1.009 µs .. 1.022 µs`
  - `experiment_03`: about `6.052 µs .. 6.275 µs`
  - interpretation:
    - the serial-chain specialization completely flipped this case and `05` is
      now far ahead of `03`
- `run_parallelism / portfolio_bridge_tradeoff / pre32...post96...`
  - `experiment_05`: about `527.59 µs .. 530.32 µs`
  - `experiment_03`: about `493.49 µs .. 501.20 µs`
  - `experiment_04`: about `1.320 ms .. 1.390 ms`
  - interpretation:
    - the singleton-DAG path helped slightly, but `05` still loses this bridge
      orientation to `03`, even though it remains far ahead of `04`

### Compile Cost

- `compile / compile_heavy_sparse_keys / jobs1536_multi_key_sparse_zero`
  - `experiment_05`: about `25.87 ms .. 27.21 ms`

This compile cost is acceptable so far given the explicit research priority on
repeated-run runtime speed.

## Current Conclusion

The hierarchical cluster direction is now grounded enough to keep.

The prototype is now substantially stronger than the first `05` version.

- it now beats `03` very clearly on `wide_independent/jobs512_zero`
- it ties `03` on `layer_barrier_stress`
- it beats `03` and `04` on the pre-heavy bridge shape
- it now beats `03` decisively on the zero-work serialized hotspot case
- it is in the same class as `03` on `mixed_hotspots`
- it is still behind `03` on:
  - the post-heavy bridge orientation

That means the next likely improvements are:

- keep the cluster-automata representation
- add a better local policy or cluster shape for the post-heavy bridge
  orientation
- continue specializing schedules whose cluster structure degenerates into a
  simpler runtime model

## Files Added So Far

- `src/experiment_05/mod.rs`
- `src/experiment_05/README.md`
- `src/experiment_05/DESIGN.md`
- `src/experiment_05/model.rs`
- `tests/experiment_05.rs`
- benchmark harness integration

## Acceptance Criteria For This Research Task

Before `experiment_05` implementation is considered promising, we want evidence
for all of the following:

- a bounded-domain dynamic runtime can preserve more flexibility than `03`
  without paying `01`-class overhead
- cluster execution materially reduces scheduler overhead compared with
  per-job execution
- the design remains implementable behind the existing public API
- the chosen runtime representation has a plausible path to beating `03` on
  both:
  - zero-work overhead
  - parallelism-sensitive workloads

## Progress Log

- 2026-04-05: Task created.
- 2026-04-05: Reviewed final results and prior designs, rejected async/futures,
  full-schedule bitset scanning, and a global lock-heavy queue scheduler as the
  primary `experiment_05` direction.
- 2026-04-05: Chose hierarchical cluster automata with a custom cluster
  executor as the target design for `experiment_05`.
- 2026-04-05: Added `examples/poc_cluster_granularity.rs` and confirmed that
  cluster-sized execution on the existing Rayon pool can be roughly `53x`
  cheaper than one task per job.
- 2026-04-05: Added `examples/poc_bridge_policy.rs` and confirmed that a local
  unlock-balance score can flip between pre-heavy and post-heavy bridge shapes
  without a portfolio of whole schedules.
- 2026-04-05: Added `examples/poc_cluster_kernel.rs` and found that a naive
  mask kernel is slower than a compact per-job counter kernel, so the automaton
  design needs stronger compile-time transition tables.
- 2026-04-05: Added `examples/poc_fixed_pool_clusters.rs` and found that a
  naive fixed pool is slower than Rayon for cluster tasks, so a custom executor
  remains an open sub-hypothesis rather than an immediate assumption.
- 2026-04-05: Implemented the first real `experiment_05` prototype behind the
  shared API and integrated it into the shared benchmark harness.
- 2026-04-05: Revised the clustering heuristic after benchmark evidence showed
  that over-grouping around wide relaxed hotspots was hurting `portfolio`
  shapes.
- 2026-04-05: Confirmed that the revised prototype is now competitive with `03`
  on `wide_independent/jobs512_zero`, clearly faster on
  `layer_barrier_stress`, dramatically faster on both
  `portfolio_bridge_tradeoff` shapes, and much faster on `mixed_hotspots`,
  while still lagging on `hot_key_write_contention/jobs256_zero`.
- 2026-04-05: Added a second optimization round with explicit cluster runtime
  modes, work-first inter-cluster chaining, cheaper `done_mask` publication,
  and a schedule-level trivial-parallel fast path.
- 2026-04-05: Reran the key benchmarks sequentially and found that `05` now
  wins decisively on `wide_independent/jobs512_zero`, ties `03` on
  `layer_barrier_stress`, beats `03` on the pre-heavy bridge shape, stays close
  to `03` on `mixed_hotspots`, and narrows the serialized hotspot gap from
  roughly `47-49 µs` down to roughly `7.7-8.3 µs`.
- 2026-04-05: Added a third optimization round with a singleton-DAG
  specialization for all-singleton schedules and a serial-chain specialization
  for globally serial schedules.
- 2026-04-05: Verified that the serial-chain fast path reduced
  `hot_key_write_contention/jobs256_zero` to roughly `1.01 µs`, far ahead of
  `experiment_03`, while the singleton-DAG fast path improved the pre-heavy
  bridge shape to roughly `490-496 µs` and nudged the post-heavy bridge shape
  down to roughly `528-530 µs`.
