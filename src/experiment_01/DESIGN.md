# Experiment 01 Design

## Status

This document describes the current `experiment_01` design as implemented in
[`model.rs`](./model.rs), not a hypothetical future rewrite. It should be read
as both:

- a design explanation for cold readers
- a compact map of the important performance and correctness decisions in the
  implementation

If you have no context, start with [`README.md`](./README.md) first. That file
explains why this experiment exists. This document explains exactly how it
works.

## Problem This Experiment Is Trying To Solve

The project wants a scheduler that:

- compiles once and runs many times
- preserves `Read` / `Write` dependency semantics
- preserves `Strict` / `Relax` ordering semantics
- avoids mutexes in the run-time hot path
- keeps per-job execution state small
- extracts more parallelism than a scheduler that only runs precomputed groups
  sequentially

The baseline traditional approach is implemented in
[`crate::experiment_02`](../experiment_02). That design is intentionally simple:
jobs are assigned to parallel-safe layers, and the runtime executes one whole
layer at a time.

`experiment_01` exists because that simplicity has a cost:

- if only one job in group `N` conflicts with a job in group `N + 1`, the rest
  of group `N + 1` still waits for the entire earlier group
- relaxed conflicts are serialized more than necessary
- available parallelism is often hidden behind group barriers

This experiment tries to remove those coarse barriers while still compiling most
of the scheduling work up front.

## Shared Public Model

The experiment participates in the shared API from [`crate::experiment`](../experiment):

`Scheduler::new().add(Job::new(..)).schedule()?.run(&state)?`

Jobs receive `&S`, not `&mut S`. That is a critical soundness decision.
Parallel experimental schedulers must not be able to hand out aliased mutable
references to the whole state object.

The intended model is:

- the scheduler enforces logical access constraints through declared
  dependencies
- the state object exposes interior mutability where real mutation is needed

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
- but declaration order does not need to be preserved once both jobs are
  otherwise eligible

This is the case where the runtime should be free to choose whichever job gets
the resource first.

### Strict

`Strict` means:

- no overlap is allowed for conflicting accesses
- declaration order must be preserved

This introduces a true "must happen after" relation.

### Unknown

`Unknown` is treated as a strict barrier:

- it is always a strict outer conflict
- it effectively says "do not let anything overlap with this unless you have no
  better information"

## Central Design Idea

The design splits the dependency model into two orthogonal runtime mechanisms.

### 1. Strict conflicts become ordering state

Strict conflicts are compiled into a reduced DAG representation:

- a per-job predecessor count
- a flat successor list
- a root list for jobs with zero strict predecessors

At runtime, that is enough to answer:

- is this job logically eligible to start yet
- which jobs become newly eligible when this one finishes

### 2. Relaxed conflicts become reservation state

Relaxed conflicts are compiled into page reservation masks:

- each job is assigned a home page and slot
- each job stores a sorted acquire list of `(page, mask)` pairs
- a job may run only if it can reserve every page in that list

This is the part that gives `experiment_01` more flexibility than a layered
group scheduler. Relaxed conflicts do not become permanent compile-time order.
They become runtime competition over reservation masks.

## Why Pages Exist

Pages are the fixed-size reservation domain for relaxed conflicts.

Each page can represent up to 32 resident jobs because its state fits in a
single `AtomicU64`:

- low 32 bits are reservation bits
- high 32 bits are ready bits

Pages exist for three reasons:

1. they bound the reservation domain to a small atomic word
2. they allow related relaxed-conflict neighborhoods to be packed together
3. they provide a natural unit for wakeups and rescans

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
- `successor_range`
- `acquire_range`

The job does not store:

- explicit predecessor lists
- rivalry lists
- weak edge lists
- cluster metadata beyond its home page/slot

### Per page

Each page stores:

- one `AtomicU64 state`
- `resident_range`
- `follower_range`

### Global flat arrays

The schedule also stores flat boxed slices for:

- `successors`
- `acquires`
- `residents`
- `followers`
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

This is required because a single job must not be allowed to describe
internally overlapping exclusive accesses and still be accepted by the
scheduler.

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

This is currently an `O(n^2)` pass, which is acceptable for the experiment
because the schedule is expected to run many times after being compiled.

### Step 3: reduce strict edges

All strict conflicts could be carried directly into the runtime, but that would
inflate predecessor counts and successor lists unnecessarily. The compiler
therefore performs a local transitive reduction:

- candidate strict predecessors are collected first
- obviously redundant strict edges are removed
- the result is a smaller predecessor set per job

This keeps strict wakeup traffic lower without needing a fully general graph
data structure at runtime.

### Step 4: build the relaxed-conflict neighborhood graph

Relaxed conflicts are treated as an undirected compile-time graph.

This graph is not kept at runtime. It is only used to answer:

- which jobs should be grouped into the same reservation neighborhoods
- which jobs need reservation masks against each other

### Step 5: assign home pages

Jobs are assigned a home page and slot.

The current heuristic is intentionally simple:

- connected relaxed-conflict components are discovered
- within a component, jobs are ordered by relaxed degree
- jobs are chunked into pages of at most 32 residents
- isolated jobs are also packed into pages

This is a practical heuristic, not a theoretical optimum. The purpose is to get
reasonably dense conflict neighborhoods into the same page so that acquire lists
stay short and page-local scans are more productive.

### Step 6: emit acquire lists

For each job:

- the compiler includes its home page in the acquire list
- the compiler adds acquire bits for every page containing relaxed-conflict
  neighbors
- acquire entries on the same page are merged into a single bitmask
- the final acquire list is sorted by page id

The sorted global page order is crucial. It means all jobs reserve pages in the
same order, which prevents deadlock during multi-page acquisition.

### Step 7: emit residents and followers

Each page needs two kinds of metadata:

- which jobs are resident in the page
- which other pages may contain jobs blocked by this page

Those second pages are called followers. They allow the runtime to propagate
wakeups at page granularity instead of retaining explicit weak-edge lists.

## Runtime Algorithm

The runtime is centered around three operations:

- attempt to start a job
- run a job
- rescan a page

### Attempting a job

A job can only start if:

1. its strict predecessor count is zero
2. every page in its acquire list can be reserved

The runtime checks the strict counter first. If it is nonzero, the job is not
yet eligible.

If the job is strictly eligible, the runtime walks its acquire list in page-id
order. For each page:

- load the current page state
- check whether any needed reservation bit is already held
- if not, CAS in the new reservation word

If every page reservation succeeds, the job is spawned.

If any reservation fails:

- already-acquired pages are released in reverse order
- the job marks its ready bit in its home page
- the runtime returns immediately

The job does not block. It simply records that it wants to run once the relevant
page activity changes.

### Lost-wakeup prevention

There is an important race around failed reservation:

- a job may fail to reserve a page
- then mark itself ready in its home page
- while the conflicting reservation is released at nearly the same time

To avoid missing that transition, the runtime performs an additional check after
marking ready. If the page that caused the failure is now free for the needed
mask, the runtime proactively rescans the job's home page.

This is one of the most subtle parts of the design and is required to make the
ready-bit wakeup scheme reliable.

### Running a job

Once spawned, the job simply executes its closure.

After completion the runtime:

1. releases all acquired page masks
2. restores the job's strict wait counter to its initial value
3. scans the job's home page
4. scans follower pages of every released page
5. decrements strict successors and attempts newly unblocked ones

That order matters:

- reservations must be released before rescans
- home/follower rescans expose relaxed-conflict progress
- strict successor decrements expose ordering progress

### Page scans

A page scan loads the page's ready bits and iterates resident jobs whose ready
bit is set. Each such job is re-attempted.

The page scan does not need to know why the job became blocked. It only needs to
know that some condition related to that page may have changed.

This is a key simplification:

- there are no per-job wait queues
- there are no explicit rival vectors
- the page is the wakeup domain

## Repeated Execution Model

The schedule is designed to run many times.

On successful completion:

- every page state should have returned to zero
- every job's strict wait counter should have returned to its initial value

That means the schedule is naturally reusable without a full reset pass after
every successful iteration.

On error:

- the runtime currently performs an explicit reset of page state and strict wait
  counters before returning

This keeps the post-error state predictable even though the happy path is tuned
to avoid extra reset work.

## Correctness Invariants

The implementation depends on a small number of important invariants.

### Invariant 1: strict order is preserved

If job `B` has a strict predecessor `A`, then `B` must not start until `A` has
finished. This is enforced by the predecessor counter and successor wakeup path.

### Invariant 2: relaxed conflicts never overlap

If two jobs have a relaxed conflict, then at least one of them must fail page
reservation while the other is running. This is enforced by acquire masks.

### Invariant 3: page reservation cannot deadlock

Every job acquires pages in the same ascending page order. Failed acquisitions
roll back immediately. Therefore the runtime cannot deadlock on reservation
ordering.

### Invariant 4: wakeups are page-complete enough

If a page release could make some blocked job runnable, then either:

- the releasing job scans the relevant page or follower page, or
- the blocked job's failed-acquire path triggers a compensating rescan

This is what makes the ready-bit and follower-page model viable.

### Invariant 5: runtime state stays compact

No job needs to retain heavy graph state during execution. All runtime behavior
must be derivable from:

- its strict counter
- its acquire list
- its home page
- the flat successor/follower arrays

## Why This Design Can Outperform Layered Scheduling

The design is trying to exploit a specific opportunity:

- strict dependencies are often much sparser than "whole earlier group must
  finish"
- relaxed conflicts often do not justify a permanent order

By treating relaxed conflicts as dynamic reservation rather than layered order,
`experiment_01` can run jobs earlier when:

- their true strict predecessors are already done
- their needed reservation masks are currently available

That lets the runtime overlap work that a group scheduler would serialize.

## Why This Design Is More Complex

The extra parallelism is not free.

Compared with `experiment_02`, this design has:

- more compile-time machinery
- more runtime atomics
- more subtle wakeup behavior
- sensitivity to page assignment quality

This is acceptable only if the extra parallelism actually shows up in the
benchmarks. That is why `experiment_01` and `experiment_02` share the same API
tests and the same benchmark harness.

## Heap Allocation Strategy

The runtime representation is intentionally flattened into boxed slices to avoid
capacity waste and to keep memory traversal predictable:

- jobs are stored in one boxed slice
- pages are stored in one boxed slice
- successors are stored in one boxed slice
- acquires are stored in one boxed slice
- residents are stored in one boxed slice
- followers are stored in one boxed slice
- roots are stored in one boxed slice

Compile-time helper structures still use temporary `Vec`s because clarity and
iteration speed matter more than squeezing the compile pipeline at this stage.

## Complexity Profile

Current rough costs:

- dependency normalization: per-job sort and merge
- pairwise relation detection: `O(n^2)`
- strict reduction: higher than linear, but done once at compile time
- page assignment: linear in the relaxed graph size plus sorting within
  components
- runtime job attempt: proportional to acquire-list length
- runtime wakeup cost: proportional to resident page scans and successor/follower
  fan-out

This is the intended shape:

- heavier compile
- lighter repeated runs
- dynamic runtime decisions only where they buy parallelism

## Failure And Error Handling

Job closures return `anyhow::Result<()>`.

If a job returns an error:

- the schedule is marked cancelled
- the error is collected
- no new useful work should be started after cancellation is observed

The schedule is then reset to a reusable state before the error is surfaced.

This part of the design is deliberately conservative. The primary focus of the
experiment is scheduling performance, not sophisticated partial-failure policy.

## Limitations And Open Questions

The current implementation is strong enough to benchmark, but several questions
remain open.

### Page assignment quality

The current heuristic is simple and deterministic, but it may not be the best
packing for:

- long acquire lists
- cross-page follower traffic
- cache locality

### Compile-time cost

The pairwise relation pass is deliberately brute-force. If compile time becomes
a problem, key-indexed acceleration or sparse conflict discovery may be needed.

### Wakeup efficiency

Page-level rescans avoid per-job rivalry lists, but the best follower strategy
is still an empirical question.

### Fairness

The design aims for throughput, not fairness guarantees. If one ready job keeps
winning reservation races, another may be retried many times before running.

## How To Evaluate This Experiment

When comparing `experiment_01` to other schedulers, the important questions are:

- does it produce measurably better run-time throughput on mixed workloads
- does that gain persist across repeated runs of the same compiled schedule
- is the extra compile-time cost acceptable
- is the flatter runtime representation enough to justify the more dynamic logic

That is the standard this experiment should be held to.

## One-Paragraph Mental Model

`experiment_01` compiles strict dependencies into predecessor counters and flat
successor lists, compiles relaxed conflicts into page reservation masks, and
then uses small atomic page states plus ready-bit rescans to start jobs as soon
as both their ordering constraints and their reservation constraints allow it.
