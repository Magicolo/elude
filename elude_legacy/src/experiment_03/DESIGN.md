# Experiment 03 Design

## Purpose

`experiment_03` exists to explore a runtime-first scheduler design for a
repeatedly executed compiled schedule.

The design goal is not "make schedule compilation cheap".

The design goal is:

- minimize scheduler work during `run()`
- preserve the dependency model exactly
- recover more parallelism than sequential layer execution
- avoid the runtime reservation machinery of `experiment_01`

The guiding assumption is that the same compiled schedule will run many times,
for example once per frame in a game loop. Under that assumption, spending more
time in `schedule()` is acceptable if it produces a materially cheaper runtime.

## Key Insight

Relaxed conflicts do not require a specific order. They only require mutual
exclusion.

That means a scheduler is free to choose an order for them.

`experiment_03` takes that freedom seriously:

- strict conflicts remain fixed by the dependency model
- relaxed conflicts are oriented at compile time according to a chosen global
  priority order

Once that orientation is chosen, every conflict becomes an ordinary precedence
constraint.

At that point the runtime no longer needs a lock or reservation protocol. It
only needs to execute a DAG.

## High-Level Pipeline

The compilation pipeline is:

1. normalize each job's dependencies
2. classify every ordered job pair as `None`, `Relaxed`, or `Strict`
3. build the fixed strict DAG implied by declaration order
4. reduce those fixed strict edges transitively
5. choose a global priority order that extends the strict DAG
6. compute final-DAG longest-path height for each job
7. use that priority order to orient relaxed conflicts
8. compress the oriented per-key conflict relationships into sparse
   predecessor edges
9. union the fixed strict edges with the oriented relaxed edges
10. reduce the final DAG transitively
11. order roots and successor slices by final-DAG criticality, then flatten the
    result into:
    - per-job predecessor counters
    - one flat successor array
    - one root list

Only step 11 matters at runtime.

## Dependency Semantics

The dependency rules are the same as in the rest of the experiment family.

### Across jobs

- `Read` vs `Read` on the same key: no conflict
- `Read` vs `Write` on the same key: conflict
- `Write` vs `Write` on the same key: conflict
- `Strict` conflict: no overlap and declaration order preserved
- `Relax` conflict: no overlap, order may be chosen by the scheduler
- `Unknown`: strict barrier against everything

### Within a single job

These are validated before a job is accepted:

- duplicate reads of the same key are allowed
- read/write overlap on the same key is rejected
- write/write overlap on the same key is rejected

This protects Rust's aliasing model at the level the scheduler can enforce.

## Runtime Representation

The runtime representation is intentionally minimal.

Each job stores:

- its executable closure
- its initial predecessor count
- its current predecessor count as an `AtomicU32`
- a range into a flat successor array

The schedule stores:

- one Rayon thread pool
- one flat job slice
- one flat successor slice
- one flat root slice

That is all.

There is no scheduler-owned lock table, no page-state array, no group summary
array, and no runtime conflict scan.
The only extra runtime shaping is that roots and successor slices are stored in
criticality order so the executor can stay work-first without searching.

## Fixed Strict DAG

The compiler first determines all strict constraints that are not optional.

For every pair of jobs `(left, right)` with `left` declared before `right`:

- if they do not conflict: no edge
- if they conflict only under relaxed semantics: record an undirected relaxed
  neighbor relation
- if they have any strict conflict: add a fixed strict candidate edge
  `left -> right`

The fixed strict candidate graph is then transitively reduced.

This fixed strict DAG is non-negotiable. Every later compile-time decision must
respect it.

## Priority Order Heuristic

The next question is:

"In what topological order should relaxed conflicts be oriented?"

`experiment_03` chooses a topological priority order over jobs that already
respects the fixed strict DAG.

The current heuristic is greedy and priority-based:

- prefer jobs with longer remaining strict-path height
- then prefer jobs with higher relaxed conflict degree
- then prefer jobs that perform more writes
- then prefer jobs with more accesses
- then prefer lower declaration index as a stable tie-break

This heuristic is not claiming optimality. It is trying to choose an execution
order that brings forward jobs that are both structurally important and likely
to constrain others.

The design intentionally leaves room for heavier heuristics later:

- local swap improvement
- simulated annealing
- branch-and-bound within antichains
- cost functions informed by measured job duration

Those would not change the runtime model, only the quality of the compiled DAG.

## Oriented Relaxed Conflicts

Once the priority order is chosen, every relaxed conflict is oriented from
earlier priority to later priority.

The naive implementation would add every oriented pairwise edge, but that would
produce a large runtime graph and too much per-job wakeup traffic.

`experiment_03` instead compresses the orientation per key.

### Per-key sparse edge construction

For each logical resource key:

1. collect every job that touches that key
2. sort those jobs by priority order
3. walk the sequence while tracking:
   - the last write
   - the current batch of reads since that write

Then emit only the minimal necessary edges:

- a read depends only on the last write
- a write depends on the last write and on every read in the current read batch

This is enough to ensure that:

- writes do not overlap reads
- writes do not overlap writes
- reads still overlap each other

and it does so with far fewer edges than a full pairwise conflict graph.

## Final DAG Reduction

The sparse per-key edges are unioned with the fixed strict DAG.

That union can still contain redundant edges, so the compiler performs a final
transitive reduction pass using the chosen topological order.

This reduction is valuable for runtime because every remaining edge implies:

- one predecessor count contribution
- one successor wakeup decrement later

Fewer edges directly means less runtime atomic traffic.

## Runtime Algorithm

Execution is a pure topological wakeup engine.

### Initialization

- jobs whose predecessor count is zero are roots
- roots are pre-sorted by final-DAG height and priority rank
- one root starts inline on the current worker
- any remaining roots are spawned into the Rayon pool

### Job completion

When a job finishes successfully:

1. reset its own predecessor counter to its initial value for the next run
2. walk its direct successor slice
3. decrement each successor's counter
4. continue inline with the first successor whose counter reaches zero
5. spawn any additional successors whose counters reach zero

### Error handling

If a job returns an error:

- a shared cancellation flag is raised
- the first error is stored
- already running jobs are allowed to drain
- the schedule resets all counters afterward

This matches the broad error-handling shape used by the other experiments.

## Why This Might Beat Experiment 02

`experiment_02` uses sequential parallel groups. That creates barrier costs
that are not visible in the scheduler's internal data structures, but are very
visible in throughput:

- one slow job holds back the whole next group
- a job may wait for unrelated jobs in the same prior group

`experiment_03` removes those barriers entirely. A job waits only on the exact
jobs that remain in its reduced DAG predecessor set.

In workloads with partial overlap, that should expose more concurrency than
layer execution.

## Why This Might Beat Experiment 01

`experiment_01` can potentially find a better per-run interleaving because it
keeps relaxed conflicts dynamic, but it pays runtime costs for that flexibility:

- reservation CAS operations
- rollback on failed acquisition
- ready-bit management
- page scans
- follower-page wakeups

`experiment_03` gives that flexibility up. In exchange, it replaces the runtime
with:

- one atomic counter per job
- one successor walk on completion
- no forced spawn for every ready node
- critical-path-first direct wakeups

If the compile-time orientation is good enough, that simpler runtime may win on
repeated execution even if it occasionally leaves some parallelism unexploited.

## Correctness Invariants

The implementation relies on these invariants:

1. the chosen priority order is a topological extension of the fixed strict DAG
2. every emitted runtime edge points from earlier priority to later priority
3. the per-key sparse construction preserves all required non-overlap rules
4. final transitive reduction never removes the last path enforcing a required
   dependency
5. each job is activated exactly once per run, either inline or by spawn
6. predecessor counters are fully restored after each successful run

The shared public API tests exercise those invariants at the behavioral level.

## Memory Layout Goals

Even though this experiment is willing to spend compile time, the runtime
layout is still intentionally dense:

- boxed slices instead of nested graph allocations in the hot path
- flat successor storage
- flat root storage
- one atomic per job

This aligns with the project-wide goal of keeping runtime execution data small
and cache-friendly.

## Future Directions

The current implementation leaves several obvious research directions open
without changing the execution model:

- stronger compile-time priority optimization
- multiple precompiled orientations and run-to-run rotation
- weighting the heuristic by measured job durations
- splitting large antichains with local search to reduce critical path
- smarter per-key sparse reduction when multi-key jobs dominate

Those are all compile-time quality improvements. The runtime core stays the
same: a sparse DAG executor with direct wakeups.
