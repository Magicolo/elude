# Experiment 01

`experiment_01` is the high-parallelism scheduler in the experimental family.

Its purpose is to compile as much dependency information as possible ahead of
time, then use a compact dynamic runtime to start jobs as soon as their actual
constraints allow it.

If you have no prior context, the most important point is:

- `experiment_01` preserves the dependency model exactly
- strict conflicts become reduced predecessor counters plus successor lists
- relaxed conflicts stay dynamic and are resolved at run time through page
  reservations
- the runtime is allowed to use narrow mutex-backed waiter queues if that
  improves practical parallelism under contention

This module is meant to be compared directly against
[`crate::experiment_02`](../experiment_02) through the shared public API in
[`crate::experiment`](../experiment):

`Scheduler::new().add(Job::new(..)).schedule()?.run(&state)?`

## Why This Experiment Exists

Traditional dependency schedulers often take this shape:

1. assign jobs to parallel-safe groups
2. run all jobs in group 0
3. wait for the whole group to finish
4. run all jobs in group 1
5. repeat

That design is simple, but it hides parallelism. If one job in an earlier
group finishes quickly, later jobs that only depend on that one job still wait
for the whole group barrier.

`experiment_01` exists to attack that limitation.

The design tries to get the best of both worlds:

- most dependency reasoning is paid at compile time
- the runtime stays flat and data-oriented
- eligible jobs can still start on fine-grained readiness, not only on layer
  boundaries

## Shared Scheduling Model

This scheduler operates on the dependency model exposed by
[`crate::depend`](../depend.rs):

- `Read(key, order)` means shared access to a logical resource
- `Write(key, order)` means exclusive access to a logical resource
- `Relax` means conflicting jobs must not overlap, but may be reordered
- `Strict` means conflicting jobs must not overlap and must preserve
  declaration order
- `Unknown` is treated as a strict barrier against everything else

Jobs receive `&S`, not `&mut S`. The scheduler enforces logical exclusivity
from declared dependencies; actual mutation must happen through interior
mutability or finer-grained synchronization inside `S`.

Each job is also validated before it enters a compiled schedule:

- repeated reads of the same key are allowed and normalized
- `Read(key)` together with `Write(key)` in one job is rejected
- multiple `Write(key)` accesses to the same key in one job are rejected

## What Makes Experiment 01 Different

The key idea is to compile the dependency model into two compact runtime
mechanisms rather than into a heavyweight graph runtime.

### Strict conflicts become ordering state

If two jobs have a strict conflict, the later job must wait for the earlier job
to finish. At runtime this is represented by:

- a predecessor counter per job
- a flat successor list for wakeup

That is enough to preserve order without storing full predecessor vectors in
the hot path.

### Relaxed conflicts become reservation state

If two jobs have a relaxed conflict, they do not need a permanent order, but
they still must not overlap. The runtime therefore represents relaxed
competition as page-local reservation masks over 32-slot pages.

Jobs that cannot currently acquire their pages do not spin in place. They are
queued on the first blocking page and retried from that page when capacity may
have opened up.

### Jobs may touch multiple reservation pages

A job has a home page, but its relaxed conflicts may span multiple pages. The
runtime therefore acquires a sorted list of `(page, mask)` pairs before the job
starts.

That multi-page acquire model is what allows cross-page parallelism while still
keeping each page state small.

## Runtime Shape

The compiled schedule is intentionally flat and dense.

Each job stores only:

- its executable closure
- its home page/slot
- its initial strict wait count
- its current strict wait count
- the page it is currently waiting on, if any
- a range into the flat successor array
- a range into the flat acquire-mask array

Each page stores only:

- one `AtomicU32` reservation state
- one small FIFO waiter queue protected by `parking_lot::Mutex`

The rest of the schedule is stored in flat boxed slices:

- successors
- acquire masks
- roots

This is deliberate. The experiment is still trying to prove that a dynamic
runtime does not need a heavyweight graph of heap-allocated node metadata.

## High-Level Execution Flow

When the schedule runs:

1. root jobs are considered first
2. a job may start only when its strict wait count is zero
3. the runtime tries to reserve every page in that job's acquire list
4. if all reservations succeed, the job is spawned
5. if any reservation fails:
   - earlier reservations are rolled back
   - the job is queued on the first blocking page
   - the runtime returns without blocking
6. when a job finishes:
   - it releases all page reservations
   - it decrements strict successors
   - it wakes waiter queues on the released pages

That gives the runtime a targeted way to rediscover runnable work without
maintaining heavyweight global queues or repeatedly rescanning whole pages of
jobs that are still blocked elsewhere.

## Current Implementation Status

`experiment_01` is implemented and benchmarked.

The current implementation includes:

- dependency normalization
- pairwise relation classification
- local strict transitive reduction
- neighborhood-overlap-aware page packing for relaxed-conflict components
- per-job multi-page acquire lists
- atomic page reservation with CAS
- per-page waiter queues for targeted retries under contention
- repeated execution of a compiled schedule

It also participates in the shared API-level correctness tests and the shared
Criterion benchmark suite.

## Known Tradeoffs

This design is more sophisticated than a sequential-group scheduler, so it also
has more moving parts:

- compile time is intentionally higher than simpler experiments
- page assignment quality still matters for performance
- the runtime uses both atomics and narrow mutex-backed wait queues
- fairness is improved relative to blind rescans, but it is still heuristic,
  not a formal fairness proof

Those tradeoffs are intentional. `experiment_01` exists specifically to test
whether paying that complexity cost is worth it when repeated runs are dominated
by job execution rather than by compile cost.

## How To Read This Module

If you are new to the codebase, the recommended order is:

1. read [`crate::experiment`](../experiment/README.md) for the shared API
2. read [`DESIGN.md`](./DESIGN.md) for the detailed rationale and algorithm
3. read [`model.rs`](./model.rs) for the concrete implementation
4. read [`tests/experiment_01.rs`](../../tests/experiment_01.rs) for the public
   semantics this implementation must satisfy

## One-Sentence Summary

`experiment_01` is the dynamic, page-reservation-based scheduler experiment
whose goal is to maximize safe parallelism with a flat compiled runtime and
targeted contention wakeups.
