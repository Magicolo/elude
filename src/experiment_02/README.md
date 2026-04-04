# Experiment 02

`experiment_02` is the traditional baseline scheduler in the experimental
family.

Its job is not to be the most ambitious design. Its job is to answer a very
useful question:

"How fast and how small can the scheduler runtime be if we compile everything
into sequential parallel-safe groups and do almost nothing clever while
executing?"

If you have no context, this is the quickest way to think about it:

- `experiment_01` tries to maximize parallelism with dynamic runtime
  reservations
- `experiment_02` accepts some extra serialization in exchange for a much
  simpler run-time model

Both implementations share the same public API from
[`crate::experiment`](../experiment):

`Scheduler::new().add(Job::new(..)).schedule()?.run(&state)?`

That shared API is the whole point. The project wants to compare complete
scheduler implementations, not benchmark their internals in isolation.

## Why This Experiment Exists

Any serious scheduler experiment needs a baseline that is:

- easy to reason about
- easy to verify
- cheap to run repeatedly
- structurally different from the more ambitious design

`experiment_02` is that baseline.

It represents the familiar approach used by many dependency schedulers:

1. assign jobs into groups that are safe to run together
2. run every job in group 0 in parallel
3. wait for the whole group to finish
4. run every job in group 1 in parallel
5. continue until the schedule is done

That design is intentionally conservative. It may not expose the absolute
maximum parallelism available in the job graph, but it has attractive
properties:

- very small runtime state
- simple execution flow
- no scheduler-managed synchronization during execution
- easy repeated runs of a compiled schedule

## Shared Dependency Model

The scheduler respects the same dependency model as the rest of the
experiments.

- `Read(key, order)` means shared access to a logical resource
- `Write(key, order)` means exclusive access to a logical resource
- `Relax` means conflicting jobs must not overlap, but may be reordered
- `Strict` means conflicting jobs must not overlap and must preserve
  declaration order
- `Unknown` behaves like a strict barrier

The public API gives each job `&S`, not `&mut S`. That is required for sound
parallel execution. The scheduler is preserving declared logical access
semantics, not handing out unique whole-state references.

Each individual job is validated before compilation:

- repeated reads of the same key are allowed and normalized
- `Read(key)` together with `Write(key)` in the same job is rejected
- multiple `Write(key)` accesses to the same key in the same job are rejected

That prevents a single job from declaring an internally unsound access pattern
and then slipping through scheduling.

## High-Level Strategy

`experiment_02` pushes all scheduling decisions into compile time.

At compile time it decides:

- which jobs must come after which earlier jobs because of strict conflicts
- which jobs may share a group because they are overlap-safe
- how to flatten those groups into dense runtime arrays

At run time it only does this:

1. take the next precomputed group
2. run that group's jobs in parallel on Rayon
3. wait for the group to finish
4. move to the next group

No dependency counters are decremented at run time. No wakeup lists are walked.
No page reservations are acquired. No scheduler-owned atomics are updated.

That is the core appeal of this design.

## How It Differs From Experiment 01

The difference between the two experiments is not the dependency model. It is
where the scheduler pays for its decisions.

### Experiment 01

- pays more at runtime
- uses atomics and dynamic reservation
- aims to start jobs as soon as their true constraints allow
- tries to recover parallelism hidden by group barriers

### Experiment 02

- pays more in lost opportunity than in runtime work
- accepts whole-group barriers
- uses a simpler compile-time placement algorithm
- aims for a tiny runtime hot path

This makes `experiment_02` the right comparison point for the question:

"Is the extra dynamic machinery of `experiment_01` actually worth it?"

## Runtime Representation

The compiled schedule stores only:

- one Rayon thread pool
- one thread-count field
- one flat boxed slice of job closures
- one flat boxed slice of groups

Each group stores only a range into the job slice.

That means the runtime loop is conceptually:

```text
for each group:
    run group jobs in parallel
    wait until group finishes
```

This is about as simple as a parallel scheduler runtime can get while still
respecting a dependency model richer than a plain "spawn everything" executor.

## Why This Baseline Is Valuable

`experiment_02` matters because it gives the project a clean reference point
for all later experiments.

If a more dynamic scheduler cannot beat this implementation on realistic
benchmarks, then the extra complexity is hard to justify.

It is also useful because it isolates a different design philosophy:

- make compilation smart enough
- make execution mechanically simple
- accept that some safe overlap will be missed

That philosophy is often the right tradeoff in practice.

## Current Implementation Status

`experiment_02` is implemented and participates fully in the experiment suite.

It currently includes:

- dependency normalization
- compile-time earliest-group placement
- group summaries for overlap checks
- strict-order tracking through per-key history
- repeated execution through the shared compiled-schedule API
- shared correctness tests
- shared Criterion benchmarks

## Known Tradeoffs

This design is intentionally not optimal for maximum parallelism.

The main downside is the group barrier:

- if one job in a group is still running, every job in the next group waits,
  even if some of them could have started safely

That means `experiment_02` may underutilize the machine on workloads where the
dependency graph has a lot of partial overlap between neighboring groups.

The tradeoff is that the runtime is extremely simple and cheap.

## How To Read This Module

If you are new to the codebase, the recommended order is:

1. read [`crate::experiment`](../experiment/README.md) for the shared API
2. read [`DESIGN.md`](./DESIGN.md) for the full compile/runtime algorithm
3. read [`model.rs`](./model.rs) for the concrete implementation
4. read [`tests/experiment_02.rs`](../../tests/experiment_02.rs) for the public
   contract this scheduler must satisfy

## One-Sentence Summary

`experiment_02` is the compact sequential-layer baseline that maximizes
compile-time simplification and minimizes runtime scheduler work, even though it
necessarily gives up some parallelism.
