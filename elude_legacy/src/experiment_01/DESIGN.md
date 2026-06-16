# Experiment 01 Design

## Status

This document describes the current `experiment_01` design as implemented in
[`model.rs`](./model.rs), not an abandoned lock-free draft. It should be read
as both:

- a design explanation for cold readers
- a compact map of the important performance and correctness decisions in the
  implementation

If you have no context, start with [`README.md`](./README.md) first. That file
explains why this experiment exists. This document explains exactly how it
works today.

## Problem This Experiment Is Trying To Solve

The project wants a scheduler that:

- compiles once and runs many times
- preserves `Read` / `Write` dependency semantics
- preserves `Strict` / `Relax` ordering semantics
- extracts more parallelism than a scheduler that only runs precomputed groups
  sequentially
- keeps per-job runtime state small and flat
- tolerates higher compile-time cost if it improves repeated-run behavior

The baseline traditional approach is implemented in
[`crate::experiment_02`](../experiment_02). That design is intentionally
simple: jobs are assigned to parallel-safe layers, and the runtime executes one
whole layer at a time.

`experiment_01` exists because that simplicity has a cost:

- if only one job in group `N` conflicts with a job in group `N + 1`, the rest
  of group `N + 1` still waits for the entire earlier group
- relaxed conflicts are serialized more than necessary
- available parallelism is often hidden behind group barriers

This experiment tries to remove those coarse barriers while still compiling
most of the scheduling work up front.

## Shared Public Model

The experiment participates in the shared API from
[`crate::experiment`](../experiment):

`Scheduler::new().add(Job::new(..)).schedule()?.run(&state)?`

Jobs receive `&S`, not `&mut S`. That is a critical soundness decision.
Parallel experimental schedulers must not hand out aliased mutable references
to the whole state object.

The intended model is:

- the scheduler enforces logical access constraints through declared
  dependencies
- the state object exposes interior mutability or finer-grained synchronization
  where real mutation is needed

## Dependency Semantics

The dependency model comes from [`crate::depend`](../depend.rs).

### Read

`Read(key, order)` means shared access to `key`.

- `Read` may overlap with `Read` on the same key
- `Read` conflicts with `Write` on the same key

### Write

`Write(key, order)` means exclusive access to `key`.

- `Write` conflicts with `Read` on the same key
- `Write` conflicts with `Write` on the same key

### Relax

`Relax` means:

- no overlap is allowed for conflicting accesses
- declaration order does not need to be preserved once both jobs are otherwise
  eligible

This is the case where the runtime should be free to choose whichever job gets
the resource first.

### Strict

`Strict` means:

- no overlap is allowed for conflicting accesses
- declaration order must be preserved

This introduces a true `must happen after` relation.

### Unknown

`Unknown` is treated as a strict barrier:

- it is always a strict outer conflict
- it effectively says "do not let anything overlap with this unless you have
  no better information"

## Central Design Idea

The design splits the dependency model into two orthogonal runtime mechanisms.

### 1. Strict conflicts become ordering state

Strict conflicts are compiled into a reduced DAG representation:

- a per-job predecessor count
- a flat successor list
- a root list for jobs with zero strict predecessors

At runtime, that is enough to answer:

- is this job logically eligible yet
- which jobs become newly eligible when this one finishes

### 2. Relaxed conflicts become reservation state

Relaxed conflicts are compiled into page reservation masks:

- each job is assigned a home page and slot
- each job stores a sorted acquire list of `(page, mask)` pairs
- a job may run only if it can reserve every page in that list

Relaxed conflicts do not become permanent compile-time order. They become
runtime competition over reservation masks.

### 3. Blocked relaxed work uses targeted page waiters

The original ready-bit and follower-page design proved the model was possible,
but it did too much broad retry work under hot contention. The current design
therefore keeps the page-reservation model but changes the wakeup strategy:

- when a job fails to reserve a page, it rolls back earlier reservations
- it queues itself on the first blocking page
- when reservations are released on a page, that page wakes a bounded set of
  queued jobs whose masks might now fit

This preserves the identity of the experiment while avoiding repeated whole-page
rescans of jobs that are still blocked elsewhere.

## Why Pages Exist

Pages are the fixed-size reservation domain for relaxed conflicts.

Each page can represent up to 32 resident jobs because its state fits in a
single `AtomicU32`:

- each bit corresponds to one resident slot
- a set bit means that slot is currently reserved by a running job

Pages exist for three reasons:

1. they bound the reservation domain to a small atomic word
2. they allow related relaxed-conflict neighborhoods to be packed together
3. they provide a natural wakeup unit for blocked relaxed work

The important subtlety is that a job is not restricted to only one page. It has
a home page, but its relaxed conflicts may require reservation bits from
multiple pages.

That multi-page acquire model is what makes cross-page concurrency safe.

## Compiled Runtime Layout

The compiled schedule stores the minimum runtime information needed to execute
the algorithm repeatedly.

### Per job

Each job stores:

- the executable closure
- `home: { page, slot }`
- `strict_wait_initial`
- `strict_wait`
- `waiting_on`
- `successor_range`
- `acquire_range`

The job does not store:

- explicit predecessor lists
- permanent rivalry lists
- per-job heap nodes for wakeups

### Per page

Each page stores:

- one `AtomicU32 state`
- one `parking_lot::Mutex<VecDeque<JobId>> waiters`

The mutex is intentionally narrow. It protects only queue operations, not the
whole run loop.

### Global flat arrays

The schedule also stores flat boxed slices for:

- `successors`
- `acquires`
- `roots`

The design goal is simple: runtime traversal should mostly mean pointer chasing
through dense contiguous arrays, not through nested `Vec<Vec<_>>` structures.

## Compile Pipeline

The compiler is intentionally more expensive than the runtime. This experiment
optimizes for repeated schedule execution, not for one-off compile speed.

### Step 1: normalize dependencies

Each job's declared dependencies are converted into a canonical form:

- `Unknown`, or
- a sorted boxed slice of accesses by key

Before normalization, the compiler validates that the job's dependency set is
not internally contradictory:

- repeated reads of the same key are allowed
- read/write on the same key inside one job is rejected
- write/write on the same key inside one job is rejected

If multiple dependencies mention the same key:

- their order is merged using the maximum order
- the access becomes a write if any input access was a write

This reduces later conflict detection to a straightforward merge-like walk over
sorted key lists.

### Step 2: classify pairwise relations

The compiler compares each job against all earlier jobs and classifies the
relation as one of:

- `None`
- `Relaxed`
- `Strict`

This is an `O(n^2)` pass, which is acceptable for the experiment because the
schedule is expected to run many times after being compiled.

### Step 3: reduce strict edges

All strict conflicts could be carried directly into the runtime, but that would
inflate predecessor counts and successor lists unnecessarily. The compiler
therefore performs a local transitive reduction:

- candidate strict predecessors are collected first
- obviously redundant strict edges are removed
- the result is a smaller predecessor set per job

This keeps strict wakeup traffic lower without needing a full graph runtime.

### Step 4: build the relaxed-conflict graph

Relaxed conflicts are treated as an undirected compile-time graph.

This graph is not stored directly at runtime. It is used only to decide page
placement and acquire masks.

### Step 5: pack relaxed neighborhoods into pages

The current page assignment is more intentional than the original
degree-sorted chunking pass.

For each connected relaxed-conflict component:

- isolated jobs are handled separately and packed densely
- non-isolated components are broken into 32-slot pages
- the first seed of a page is the highest-degree remaining job
- the rest of the page is filled greedily with jobs whose relaxed neighborhoods
  most overlap with the page so far

The affinity heuristic combines:

- direct relaxed edges to jobs already on the page
- overlap in closed relaxed neighborhoods

The goal is not to solve graph partitioning optimally. The goal is to reduce:

- acquire-list span across many pages
- cross-page wake traffic
- accidental fragmentation of one logical hotspot across many pages

### Step 6: build acquire masks

Each job always acquires at least one bit: its own home slot.

For each relaxed neighbor, the compiler adds the neighbor's home slot bit.
Those bits are then coalesced by page into a sorted list of `Acquire` entries.

At runtime, the job must reserve every page in this list before it can run.

### Step 7: emit dense runtime arrays

Once predecessors, successors, page placement, and acquires are known, the
compiler emits:

- `Node` records
- `Page` records
- flat successor storage
- flat acquire storage
- root jobs

The result is a reusable compiled schedule that can be run repeatedly.

## Runtime Algorithm

## Entry condition

When `run()` starts:

- every job with zero strict predecessors is considered a root
- each root is passed to `attempt_job`
- no global ready queue is built

The runtime discovers more work from:

- strict successor counters reaching zero
- waiter queues on released pages

### `attempt_job`

`attempt_job(job)` performs the following checks in order:

1. return immediately if the run was cancelled
2. return if the job is already queued on a blocking page
3. return if the job still has unfinished strict predecessors
4. walk the job's sorted acquire list and try to reserve each page atomically

If every reservation succeeds:

- the job is spawned into the Rayon scope

If some reservation fails:

- earlier successful reservations are rolled back
- the job is queued on the first blocking page

Choosing the first blocking page is deliberate. It minimizes wake noise because
the job only waits on a page that is definitely preventing progress.

### Reservation mechanics

Page reservation uses compare-and-swap on the page's `AtomicU32`.

For a requested mask:

- if any needed bit is already set, reservation fails
- otherwise the bits are atomically ORed in

Because every job acquires its own home slot bit, duplicate concurrent starts of
the same job are prevented without a separate runtime node state machine.

### Queueing blocked jobs

If a job fails on page `P`:

- it records `waiting_on = P`
- it pushes its `JobId` into `P.waiters`
- it immediately rechecks whether `P` is still blocking the needed mask

That final recheck closes the lost-wakeup race where the page becomes free
between the failed acquire and the queue insertion. If the page is already
usable, the job removes itself from the queue and retries immediately.

### `run_job`

When a spawned job finishes successfully, the runtime:

1. releases all acquired page bits
2. resets the job's strict counter to its initial value for future runs
3. decrements strict successors and directly attempts any that become ready
4. wakes waiter queues on each released page

If the job returns an error:

- cancellation is set
- the first error is retained
- the schedule is reset before the run returns

### Waking waiters

Page wakeups are intentionally bounded.

For a released page, the runtime:

- inspects the current page reservation bits
- scans the page's waiter queue in FIFO order
- selects a bounded set of queued jobs whose masks on that page do not conflict
  with already-selected masks
- clears their `waiting_on` state
- re-attempts them outside the page queue lock

This is the core contrast with the older rescan design. The page does not
retry every resident job blindly. It only wakes jobs that were actually blocked
by this page and that have a plausible chance to fit now.

## Correctness Notes

### Why a job acquires its own home bit

The home bit is doing real work:

- it gives the job a stable identity inside the page model
- it prevents duplicate concurrent starts of the same job
- it keeps the acquire model uniform even for jobs with no relaxed neighbors

### Why waiting on the first failed page is safe

When a job fails while walking sorted acquires:

- every earlier page in the list was available at that moment
- the failed page is therefore a real blocker
- later pages are irrelevant until that blocker is cleared

Waking on the first failed page is therefore a sound targeted retry strategy.

### Why strict and relaxed mechanisms stay separate

Strict edges mean semantic order. Relaxed conflicts mean only non-overlap.

Merging them into one runtime queue would make the algorithm harder to reason
about and easier to accidentally over-serialize. Keeping them separate lets the
runtime:

- react immediately to true order completion through successor counters
- keep relaxed competition dynamic through reservation masks

## Tradeoffs

The current design improves hot-contention behavior substantially, but the
tradeoffs are explicit:

- compile-time page packing is costlier than simple degree chunking
- the runtime now uses narrow mutexes for waiter queues
- fairness is improved by queueing, but not formally guaranteed
- page wakeups are still heuristic because a released page cannot know whether a
  woken job will later fail on another page

Those tradeoffs are acceptable for this experiment because the project has
explicitly prioritized practical repeated-run parallelism over minimal
schedule-time cost.

## What To Compare Against

When comparing `experiment_01` to other schedulers, the important questions are:

- does the extra dynamic machinery uncover parallelism that layered schedulers
  miss
- does the targeted waiter design keep contention costs under control
- does better page packing reduce acquire span in mixed-hotspot workloads
- is the compile-time cost justified by repeated-run behavior

## Summary

`experiment_01` compiles strict dependencies into predecessor counters and flat
successor lists, compiles relaxed conflicts into multi-page reservation masks,
and uses small page-local waiter queues to retry blocked work only where the
actual contention occurred.
