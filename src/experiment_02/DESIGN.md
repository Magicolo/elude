# Experiment 02 Design

## Status

This document describes the current `experiment_02` implementation in
[`model.rs`](./model.rs). It is intended to be detailed enough that an agent
with no prior repository context can understand:

- why this experiment exists
- what problem it is trying to solve
- how its scheduler works
- why it differs from the other experiments

If you want the short version first, read [`README.md`](./README.md). This file
is the deeper algorithm and tradeoff reference.

## Role In The Experiment Set

The project is exploring scheduler designs behind a shared public API. Those
designs are meant to be compared by real benchmarks and public API tests, not
by isolated micro-optimizations on internal data structures.

Within that experiment set:

- [`experiment_01`](../experiment_01) is the more dynamic, higher-parallelism
  design
- `experiment_02` is the traditional layered baseline

That means `experiment_02` exists for two reasons:

1. it is a valid scheduler design in its own right
2. it provides the baseline that justifies or rejects more dynamic approaches

If a more complicated implementation cannot materially outperform
`experiment_02`, then the extra complexity is likely not worth carrying.

## Problem Statement

We want a scheduler that:

- compiles a set of dependency-declared jobs once
- executes that compiled schedule many times
- preserves `Read` / `Write` exclusivity
- preserves `Strict` / `Relax` ordering semantics
- keeps runtime overhead very low
- minimizes scheduler-owned synchronization during execution

`experiment_02` answers that problem in the most traditional way available:

- compute a sequential order of parallel-safe groups at compile time
- execute one whole group at a time at run time

## Shared Public API And Soundness Model

The scheduler implements the shared API from [`crate::experiment`](../experiment):

`Scheduler::new().add(Job::new(..)).schedule()?.run(&state)?`

Jobs take `&S`, not `&mut S`.

That is required because a compiled schedule may execute multiple jobs in
parallel. The scheduler therefore cannot soundly hand out unique mutable
references to the entire state object. Logical exclusivity is expressed by the
dependency declarations, while actual mutation must be done through interior
mutability or some other safe shared-state mechanism inside `S`.

## Dependency Semantics

The dependency language is:

- `Read(key, order)`
- `Write(key, order)`
- `Unknown`

with ordering:

- `Relax`
- `Strict`

The intended behavior is:

### Read/Read

Two reads of the same key may overlap. They do not require serialization.

### Read/Write Or Write/Write With Relax

These conflicts must not overlap, but they do not have to preserve declaration
order. A scheduler is free to execute either one first as long as they do not
run concurrently.

### Read/Write Or Write/Write With Strict

These conflicts must not overlap and must preserve declaration order.

### Unknown

`Unknown` is treated as a strict barrier. It is incompatible with everything
other than an empty concurrent context.

## Core Design Choice

The core decision in `experiment_02` is that runtime execution will never make
fine-grained dependency decisions.

Instead, the compiler decides everything needed to build a sequence of groups:

- all jobs in one group are pairwise safe to overlap
- all jobs in later groups respect any strict predecessors from earlier groups

The runtime then only executes those groups in order.

This is simpler than `experiment_01`, but less parallel, because the unit of
runtime synchronization is an entire group rather than an individual job or
reservation domain.

## Compile Pipeline

Compilation is where all of the interesting logic lives in this design.

### Step 1: normalize dependencies

Each job's raw dependency list is converted into a canonical form:

- `Unknown`, or
- a sorted boxed slice of per-key accesses

Before normalization, the compiler validates that the dependency set is not
internally invalid:

- repeated reads of the same key are allowed
- read/write on the same key inside one job is rejected
- write/write on the same key inside one job is rejected

This is required because a single job must not be allowed to describe
internally overlapping exclusive accesses and still be considered schedulable.

If the same key appears more than once in a single job:

- the access mode becomes write if any entry is a write
- the order becomes the maximum order seen for that key

This reduces later reasoning to a compact per-key representation.

### Step 2: maintain per-key history trackers

As jobs are processed from declaration order, the compiler maintains history for
each key:

- the most recent group containing any read
- the most recent group containing any write
- the most recent group containing a strict read
- the most recent group containing a strict write

There is also a global `last_unknown` tracker.

These trackers answer the question:

"What is the earliest group this next job is even allowed to enter?"

The distinction between "any" and "strict" history is what lets relaxed
conflicts reorder without violating strict semantics.

### Step 3: compute the earliest legal group

For each new job, the compiler first computes the earliest group that could
possibly hold it.

The logic is:

- if the job is `Unknown`, it must go after all currently existing groups
- if the job reads with `Relax`, it only needs to stay after earlier strict
  writes on the same key
- if the job reads with `Strict`, it must stay after any earlier write on the
  same key
- if the job writes with `Relax`, it only needs to stay after earlier strict
  reads or strict writes on the same key
- if the job writes with `Strict`, it must stay after any earlier read or write
  on the same key
- if any earlier job was `Unknown`, later jobs must be placed after that group

This step preserves declaration order where the semantics require it, while
allowing relaxed conflicts to be placed as early as possible.

### Step 4: scan forward for a compatible group

The earliest legal group is not automatically safe, because that group may
already contain jobs that would overlap illegally with the new job.

So the compiler scans forward until it finds a group summary that is compatible.

Each group summary stores:

- whether the group contains `Unknown`
- for each key touched by the group, whether the group contains any read and/or
  any write

Compatibility rules are simple:

- a read is incompatible with an existing write on the same key
- a write is incompatible with an existing read or write on the same key
- `Unknown` is incompatible with any non-empty group
- a non-unknown job is incompatible with a group containing `Unknown`

The first compatible group at or after the earliest legal position becomes the
job's assigned group.

### Step 5: update trackers and summaries

Once the compiler chooses a group:

- that group's summary is updated with the job's accesses
- per-key history trackers are updated to point to that group
- the job-to-group assignment is recorded

After all jobs are assigned, the compiler flattens them into:

- one boxed slice of job closures ordered by group
- one boxed slice of group ranges into that job slice

## Why This Placement Rule Is Correct

The design depends on two independent facts being true.

### Fact 1: earliest-group placement preserves strict order

The history trackers ensure that any job requiring strict ordering is placed
strictly after the latest earlier conflicting group that matters to it.

So even before checking overlap safety, the compiler already knows it is not
placing the job too early in a way that would violate order.

### Fact 2: group compatibility preserves non-overlap

The group summary check ensures that all jobs within one group are pairwise safe
to overlap.

Therefore:

- jobs in the same group may run in parallel
- jobs in different groups may not overlap because groups run sequentially

Taken together, those two facts are enough to preserve the required dependency
semantics.

## Runtime Layout

The runtime representation is intentionally tiny.

The compiled schedule stores:

- one Rayon `ThreadPool`
- one `usize` thread count
- one `Box<[Run<S>]>`
- one `Box<[Group]>`

Each `Group` stores only:

- `job_range: Range<u32>`

There are no runtime structures for:

- predecessor counts
- wakeup queues
- reservations
- atomics owned by the scheduler
- dynamic edge traversal

This is the defining property of `experiment_02`.

## Runtime Algorithm

Execution is straightforward.

For each group in order:

1. take the group's slice of job closures
2. if the group has one job or the pool is effectively single-threaded, run it
   sequentially
3. otherwise use Rayon parallel iteration to execute the whole group
4. wait for the group to finish
5. continue to the next group

That means the scheduler-managed hot path is basically:

- read group range
- dispatch closures
- wait for completion

This is exactly why `experiment_02` is a useful performance baseline. It shows
what the runtime cost looks like when almost all scheduler work has been pushed
into compilation.

## Why This Runtime Is Attractive

This design has several desirable runtime properties:

- no scheduler-managed atomics during `run`
- no lock acquisition or release in the scheduler
- no retries
- no wakeup propagation
- no reset pass after successful execution
- no runtime allocation

The compiled schedule is also naturally reusable. Running it multiple times is
just rerunning the same group loop.

## Why This Runtime Leaves Parallelism On The Table

The price of the simple runtime is the group barrier.

Consider:

- jobs `A` and `B` are in group 0
- jobs `C` and `D` are in group 1
- `C` only depends on `A`
- `D` only depends on `B`

In a more dynamic scheduler:

- if `A` finishes early, `C` could potentially start before `B` is done

In `experiment_02`:

- `C` and `D` both wait until the entire group 0 finishes

That is the core tradeoff.

This scheduler can therefore be very fast in low-contention or naturally layered
workloads, but it will underutilize available parallelism in graphs where safe
overlap cuts across group boundaries.

## Heap Allocation Strategy

The runtime storage is flattened to minimize overhead:

- jobs live in one boxed slice
- groups live in one boxed slice

Temporary compile-time structures still use `Vec` and `HashMap` because:

- compile time is paid once per schedule
- the implementation remains clear and easy to iterate on

If compile-time memory or speed later becomes a problem, likely optimization
directions include:

- specialized handling for dense `Identifier` keys
- interning or indexing of `Key`
- replacing hash maps with small sorted vectors for small groups

## Complexity Profile

Let:

- `J` be the number of jobs
- `D` be the number of normalized accesses per job
- `G` be the number of groups

Then rough costs are:

- normalization: `O(J * D log D)`
- earliest-group computation: `O(D)` per job
- placement: `O(groups scanned)` per job
- runtime: `O(total jobs)` plus Rayon scheduling overhead per group

In the worst case, group scanning can trend toward `O(J * G)`, but that is a
compile-time cost, and `experiment_02` explicitly prioritizes cheap repeated
execution over minimal compile work.

## Correctness Invariants

The implementation relies on these invariants:

### Invariant 1: jobs within a group are overlap-safe

If two jobs ended up in the same group, the group summary compatibility rules
must have guaranteed that they can run in parallel without violating the
dependency model.

### Invariant 2: strict order is preserved by group placement

If job `B` must be strictly ordered after an earlier conflicting job `A`, then
the earliest-group calculation must place `B` in a strictly later group.

### Invariant 3: `Unknown` creates a hard boundary

A group containing `Unknown` cannot contain any other job, and later jobs must
be placed after it.

### Invariant 4: runtime never reorders groups

The runtime executes groups in the exact order chosen by the compiler.

These invariants are what make the extremely simple runtime possible.

## Benchmark Expectations

`experiment_02` should generally be strong when:

- conflict patterns naturally produce clean layers
- jobs are not so tiny that group barriers dominate
- runtime scheduler overhead matters more than maximum parallelism

It should generally be weaker when:

- there are many mixed relaxed conflicts
- dependency graphs allow safe cross-layer overlap
- job durations are imbalanced inside a group

That expected behavior is useful. It provides a clear baseline shape against
which more dynamic schedulers can be judged.

## Comparison To Experiment 01

The fastest way to compare the two designs is:

### Experiment 01

- more dynamic at runtime
- more atomics
- more scheduling machinery
- better chance of exposing hidden parallelism

### Experiment 02

- simpler runtime
- fewer moving parts
- lower scheduler overhead per run
- less parallelism because of whole-group barriers

Neither is "the right answer" in the abstract. The point of the experiment set
is to measure which tradeoff is actually better for the target workloads.

## Future Directions

Likely future improvements for this implementation are:

- better group-selection heuristics to reduce forward scanning
- denser summaries for common key types
- ordering jobs within a group for better cache locality
- optional compile-time metrics such as total group count and average group
  width
- specialized code paths for very small groups

## One-Paragraph Mental Model

`experiment_02` compiles each job into the earliest later group that preserves
strict dependencies and is overlap-safe with the jobs already inside that group,
then executes those groups one after another with parallelism only inside each
group. It gives up some potential concurrency in exchange for an extremely small
and predictable scheduler runtime.
