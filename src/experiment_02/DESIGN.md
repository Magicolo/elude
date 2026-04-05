# Experiment 02 Design

## Status

This document describes the current `experiment_02` implementation in
[`model.rs`](./model.rs). It is intended to be detailed enough that an agent
with no prior repository context can understand:

- why this experiment exists
- what problem it is trying to solve
- how its scheduler works today
- why it differs from the other experiments

If you want the short version first, read [`README.md`](./README.md). This file
is the deeper algorithm and tradeoff reference.

## Role In The Experiment Set

The project is exploring scheduler designs behind a shared public API. Those
designs are meant to be compared by real benchmarks and public API tests, not
by isolated internal microbenchmarks.

Within that experiment set:

- [`experiment_01`](../experiment_01) is the more dynamic, higher-parallelism
  design
- `experiment_02` is the layered baseline

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
- keeps the unit of runtime synchronization at the group level

`experiment_02` answers that problem with one hard rule:

- runtime execution is always a sequential order of parallel-safe groups

The experimentation happens entirely in how those groups are chosen at compile
time.

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
order.

### Read/Write Or Write/Write With Strict

These conflicts must not overlap and must preserve declaration order.

### Unknown

`Unknown` is treated as a strict barrier.

## Core Design Choice

The core decision in `experiment_02` is that runtime execution will never make
fine-grained dependency decisions.

Instead, the compiler decides everything needed to build a sequence of groups:

- all jobs in one group are pairwise safe to overlap
- all jobs in later groups respect any strict predecessors from earlier groups

The runtime then only executes those groups in order.

This is simpler than `experiment_01`, but less parallel, because the unit of
runtime synchronization is an entire group rather than an individual job or a
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

If the same key appears more than once in a single job:

- the access mode becomes write if any entry is a write
- the order becomes the maximum order seen for that key

### Step 2: classify pairwise relations

The compiler compares each job against all earlier jobs and classifies the
relation as one of:

- `None`
- `Relaxed`
- `Strict`

This is an `O(n^2)` pass. That is much more expensive than the original
tracker-only baseline, but acceptable for this experiment because the runtime is
expected to dominate repeated use.

This relation pass feeds the experimental frontier compiler. The classic
first-fit compiler still works directly from normalized dependencies.

### Step 3: build two candidate layouts

The implementation now builds two different layerings.

#### Candidate A: online first-fit layering

This is the classic `experiment_02` algorithm.

While processing jobs in declaration order, it maintains per-key history:

- the most recent group containing any read
- the most recent group containing any write
- the most recent group containing a strict read
- the most recent group containing a strict write
- one `last_unknown` barrier tracker

For each job:

1. compute the earliest legal group from those trackers
2. scan forward to the first compatible group
3. place the job there
4. update the trackers

This algorithm is fast, stable, and often good enough.

#### Candidate B: strict-frontier batch layering

This is the more ambitious compiler.

It starts from the pairwise relation graph:

- strict conflicts become a reduced DAG
- relaxed conflicts become an undirected compatibility barrier graph

The compiler then repeatedly:

1. collects the current strict-ready frontier
2. seeds a new group with a high-priority ready job
3. grows that group greedily with more compatible ready jobs
4. finishes the group and releases newly ready strict successors

The seed heuristic favors jobs that:

- sit on longer strict chains
- still conflict with many unscheduled relaxed neighbors
- touch more keys

The group-growth heuristic then prefers compatible jobs that:

- have fewer remaining relaxed conflicts
- look similar to the current group's conflict neighborhood
- still help push critical work forward

This is not meant to be optimal graph coloring. It is a compile-time heuristic
that can beat online first-fit on some graphs without changing the runtime
model.

### Step 4: score and choose the layout

The compiler does not automatically trust the frontier layout.

It compares the two candidates using a thread-aware score:

- estimated execution waves: `sum(ceil(group_size / threads))`
- total group count
- maximum group width

It then applies one additional safety check:

- the frontier layout is only eligible if none of its groups is wider than the
  configured thread budget

That rule matters in practice. A layered scheduler can easily make itself worse
by reducing group count while creating oversized barrier groups that do not fit
the available machine parallelism.

If the frontier layout does not beat the online first-fit layout under that
score, the implementation falls back to first-fit.

### Step 5: flatten the chosen layout

Once a layout is chosen, the compiler emits:

- one boxed slice of job closures ordered by group
- one boxed slice of `Group { job_range }`

The runtime consumes only that flat representation.

## Why This Placement Rule Is Correct

The design depends on two independent facts being true.

### Fact 1: jobs inside one group are overlap-safe

Both candidate compilers enforce pairwise compatibility for every job placed
into the same group.

That means:

- reads may overlap reads
- reads never overlap writes on the same key
- writes never overlap reads or writes on the same key
- `Unknown` jobs never share a group with anything else

### Fact 2: strict order is preserved by group placement

In the first-fit compiler, strict ordering is enforced by earliest-group
trackers.

In the frontier compiler, strict ordering is enforced by the reduced DAG and
the ready-frontier rule:

- a job is not eligible until all strict predecessors were placed in earlier
  groups

Taken together, those facts are enough to preserve the dependency semantics
while still keeping runtime execution trivial.

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
- scheduler-owned atomics
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
what runtime cost looks like when almost all scheduler work has been pushed
into compilation.

## Why This Runtime Leaves Parallelism On The Table

The price of the simple runtime is still the group barrier.

If one job in a group runs long, every job in the next group waits, even if
some of them could have started safely in a dynamic scheduler.

No compile-time heuristic can fully remove that limitation without breaking the
identity of `experiment_02`.

## Complexity Profile

Let:

- `J` be the number of jobs
- `D` be the number of normalized accesses per job

Then rough costs are:

- normalization: `O(J * D log D)`
- first-fit compilation: near the original baseline shape
- frontier compilation: roughly `O(J^2)` relation work plus greedy frontier
  batching
- runtime: `O(total jobs)` plus Rayon scheduling overhead per group

In practice, the frontier compiler is much more expensive than the old design.
That tradeoff is explicit and should be measured, not hidden.

## Benchmark Expectations

`experiment_02` should generally still be strong when:

- conflict patterns naturally produce clean layers
- runtime scheduler overhead matters more than absolute maximum parallelism
- the chosen layout fits the machine width reasonably well

It should still generally be weaker when:

- there are many mixed relaxed conflicts with profitable cross-group overlap
- one slow job can hold a whole later group behind a barrier
- the job graph really wants runtime wakeups instead of compile-time layers

The new compiler can improve some groupings, but it does not change the
fundamental barrier model.

## Comparison To Experiment 01

The fastest way to compare the two designs is:

### Experiment 01

- more dynamic at runtime
- more scheduler machinery while running
- better chance of exposing hidden overlap across barriers

### Experiment 02

- simpler runtime
- heavier compile-time experimentation
- barrier-based execution remains the final authority

Neither is "the right answer" in the abstract. The point of the experiment set
is to measure which tradeoff is actually better for the target workloads.

## One-Paragraph Mental Model

`experiment_02` still executes one parallel-safe group at a time, but its
compiler no longer relies on only one placement heuristic. It builds the classic
online first-fit layering, builds a more ambitious strict-frontier batching,
then picks the one that appears to fit the configured thread budget better
without changing the tiny runtime loop.
