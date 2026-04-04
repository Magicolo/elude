# Experiment 01

`experiment_01` is the high-parallelism scheduler in the experimental family.

Its purpose is to explore a scheduler design that compiles as much dependency
information as possible ahead of time, then uses a small amount of lock-free
runtime coordination to unlock more parallelism than a traditional
"parallel groups run sequentially" design.

If you have no prior context, the most important point is:

- `experiment_01` is trying to preserve the dependency model exactly
- while avoiding runtime mutexes
- while avoiding heavyweight per-job graph state
- while allowing jobs from different logical groups to overlap when that is safe

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

That design is simple and often fast enough, but it leaves parallelism on the
table. If one job in an earlier group finishes quickly, later jobs that only
depend on that one job still cannot start until the entire group finishes.

`experiment_01` exists to attack that limitation.

The design tries to get the best of both worlds:

- most dependency reasoning is still compiled ahead of time
- the runtime remains small and data-oriented
- but scheduling decisions are still fine-grained enough to start jobs as soon
  as their actual constraints allow it

In other words, `experiment_01` is the "more dynamic, more ambitious" side of
the experiment set.

## Shared Scheduling Model

This scheduler operates on the dependency model exposed by
[`crate::depend`](../depend.rs):

- `Read(key, order)` means shared access to a logical resource
- `Write(key, order)` means exclusive access to a logical resource
- `Relax` means conflicting jobs must not overlap, but may be reordered
- `Strict` means conflicting jobs must not overlap and must preserve
  declaration order
- `Unknown` is a barrier-like dependency and is treated as a strict conflict
  with everything else

The public experiment API intentionally gives each job `&S`, not `&mut S`.
That is not an accident. These schedulers are designed to run jobs in parallel,
so the overall state object must be shared, and mutation must happen through
interior mutability or finer-grained synchronization inside `S`.

The scheduler's job is not to hand out unique Rust references to whole-state
memory. Its job is to preserve the logical exclusivity promised by the declared
dependencies.

Each job is also validated internally before it is accepted into a compiled
schedule:

- repeated reads of the same key are allowed and normalized
- `Read(key)` together with `Write(key)` in the same job is rejected
- multiple `Write(key)` accesses to the same key in the same job are rejected

That rule matters because a single job must not declare an internally
contradictory aliasing story and then expect the scheduler to make it sound.

## What Makes Experiment 01 Different

The key idea is to compile the dependency model into two compact runtime
mechanisms rather than into a heavy graph of separate node-level structures.

### Strict conflicts become ordering state

If two jobs have a strict conflict, the later job must wait for the earlier job
to finish. At runtime this is represented by:

- a predecessor counter per job
- a flat successor list for wakeup

That is enough to preserve order without storing full predecessor vectors or
transitive edge sets in the hot path.

### Relaxed conflicts become reservation state

If two jobs have a relaxed conflict, they do not need a permanent order, but
they still must not overlap. Instead of compiling them into strict edges, the
runtime represents them as page-local reservation masks over small groups of
jobs.

That allows this scheduler to decide dynamically which eligible job actually
gets to run first when a relaxed conflict exists, instead of forcing a compile-
time ordering.

### Jobs may touch multiple reservation pages

This is the main difference from a simple cluster design. A job is assigned a
home page, but its relaxed conflicts may span multiple pages. The runtime
therefore acquires a sorted list of page masks before the job starts.

That is how `experiment_01` safely allows cross-page and cross-cluster
parallelism.

## Runtime Shape

The compiled schedule is intentionally flat and dense.

Each job stores only:

- its executable closure
- its home page/slot
- its initial strict wait count
- its current strict wait count
- a range into the flat successor array
- a range into the flat acquire-mask array

Each page stores only:

- one `AtomicU64` state
- a range of resident jobs
- a range of follower pages

The 64-bit page state is split into:

- low 32 bits: reservation bits
- high 32 bits: ready bits

The rest of the schedule is stored in flat boxed slices:

- successors
- acquire masks
- residents
- follower pages
- roots

This is deliberate. The experiment is trying to prove that a dynamic runtime
does not require a graph representation full of heap-allocated adjacency lists.

## High-Level Execution Flow

When the schedule runs:

1. root jobs are considered first
2. a job may start only when its strict wait count is zero
3. the runtime tries to reserve every page in that job's acquire list
4. if all reservations succeed, the job is spawned
5. if any reservation fails:
   - earlier reservations are rolled back
   - the job marks itself ready in its home page
   - the runtime returns without blocking
6. when a job finishes:
   - it releases all page reservations
   - it scans its home page for ready jobs
   - it notifies follower pages that may now make progress
   - it decrements strict successors

That gives the runtime a way to keep discovering new runnable work without
maintaining heavyweight global queues or per-job rivalry structures.

## Why This Experiment Matters

`experiment_01` is the design to beat if the goal is maximum run-time
parallelism under this dependency model.

It matters because it measures whether the additional complexity of dynamic
page reservation is justified by better schedule throughput over repeated runs.

If it performs well, it demonstrates that:

- strict and relaxed constraints can be unified into a compact runtime
- lock-free-style coordination with atomics can outperform layered group
  execution
- most scheduling cost can still be paid at compile time

If it performs poorly, then the project learns that the extra runtime machinery
is not buying enough relative to simpler designs such as `experiment_02`.

## Current Implementation Status

`experiment_01` is not just a paper design. It is implemented and benchmarked.

The current implementation already includes:

- dependency normalization
- pairwise relation classification
- local strict transitive reduction
- page assignment for relaxed conflict neighborhoods
- per-job acquire lists
- lock-free-style page reservation with CAS
- follower-page wakeups
- repeated execution of a compiled schedule

It also participates in the shared API-level correctness tests and the shared
Criterion benchmark suite.

## Known Tradeoffs

This design is more sophisticated than a sequential-group scheduler, so it also
has more moving parts:

- compile time is currently more expensive
- the runtime uses atomics heavily
- page assignment quality matters for performance
- cross-page ready/wakeup behavior is more subtle than in traditional designs

Those tradeoffs are intentional. `experiment_01` exists specifically to test
whether paying that complexity cost is worth it.

## How To Read This Module

If you are new to the codebase, the recommended order is:

1. read [`crate::experiment`](../experiment/README.md) for the shared API
2. read [`DESIGN.md`](./DESIGN.md) for the detailed rationale and algorithm
3. read [`model.rs`](./model.rs) for the concrete implementation
4. read [`tests/experiment_01.rs`](../../tests/experiment_01.rs) for the public
   semantics this implementation must satisfy

## One-Sentence Summary

`experiment_01` is the dynamic, page-reservation-based scheduler experiment
whose goal is to maximize safe parallelism while keeping the compiled runtime
representation flat and compact.
