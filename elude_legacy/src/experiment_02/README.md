# Experiment 02

`experiment_02` is the traditional layered baseline scheduler in the
experimental family.

Its job is not to be the most dynamic design. Its job is to answer a useful
question:

"How far can a plain sequential-group scheduler be pushed if the compiler is
allowed to work much harder while the runtime stays extremely small?"

If you have no context, this is the quickest way to think about it:

- `experiment_01` maximizes parallelism with dynamic runtime coordination
- `experiment_02` keeps the runtime group-by-group and pushes experimentation
  into compile-time group selection

Both implementations share the same public API from
[`crate::experiment`](../experiment):

`Scheduler::new().add(Job::new(..)).schedule()?.run(&state)?`

## Why This Experiment Exists

Any serious scheduler experiment needs a baseline that is:

- easy to reason about
- easy to verify
- cheap to execute repeatedly
- structurally different from the more ambitious runtimes

`experiment_02` is that baseline.

It represents the familiar approach used by many dependency schedulers:

1. assign jobs into groups that are safe to run together
2. run every job in group 0 in parallel
3. wait for the whole group to finish
4. run every job in group 1 in parallel
5. continue until the schedule is done

That design is intentionally conservative. It will never expose the full
fine-grained overlap of a dynamic scheduler, but it has attractive properties:

- tiny runtime state
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
parallel execution. The scheduler preserves declared logical access semantics;
actual mutation must happen through interior mutability or finer-grained
synchronization inside `S`.

Each individual job is validated before compilation:

- repeated reads of the same key are allowed and normalized
- `Read(key)` together with `Write(key)` in the same job is rejected
- multiple `Write(key)` accesses to the same key in the same job are rejected

## High-Level Strategy

`experiment_02` now treats compile time as the place to explore alternatives,
while keeping one invariant fixed:

- runtime execution is always a sequential loop over precomputed groups

At compile time it builds two candidate layouts:

### Online First-Fit Layout

This is the classic baseline:

- track the earliest legal group from per-key history
- scan forward to the first compatible group
- append the job there

It preserves declaration order aggressively and tends to be stable on realistic
mixed workloads.

### Frontier Batch Layout

This is the more experimental compiler:

- classify pairwise relations up front
- reduce strict edges
- maintain the strict-ready frontier
- build one maximal compatible group from that frontier at a time
- seed on jobs that are hard to place or unlock longer strict chains
- grow the group with compatible jobs that are easy to pack and have similar
  conflict neighborhoods

This can outperform online first-fit on some graphs, especially when the online
ordering is a bad coloring order.

### Thread-Aware Layout Selection

The compiler does not blindly trust the more ambitious layout.

After building both candidates, it chooses the frontier layout only when:

- its estimated execution-wave cost is lower for the configured thread count
- and none of its groups exceeds the available thread budget

Otherwise it falls back to the online first-fit layout.

That keeps the runtime simple while avoiding obvious overfitting from the
heavier compiler.

## Runtime Representation

The compiled schedule stores only:

- one Rayon thread pool
- one thread-count field
- one flat boxed slice of job closures
- one flat boxed slice of groups

Each group stores only a range into the job slice.

That means the runtime loop is still conceptually:

```text
for each group:
    run group jobs in parallel
    wait until group finishes
```

No dependency counters are decremented at runtime. No wakeup lists are walked.
No reservations are acquired. No scheduler-owned atomics are updated.

## Why This Baseline Is Valuable

`experiment_02` matters because it gives the project a clean reference point
for all later experiments.

If a more dynamic scheduler cannot beat this implementation on realistic
benchmarks, then the extra complexity is hard to justify.

It is also useful because it isolates a different design philosophy:

- try harder at compile time
- keep execution mechanically simple
- accept that the barrier model still fundamentally limits overlap

## Current Implementation Status

`experiment_02` is implemented and participates fully in the experiment suite.

It currently includes:

- dependency normalization
- pairwise relation classification
- strict-edge reduction
- classic online first-fit grouping
- an alternative strict-frontier batch builder
- thread-aware layout selection between those two compilers
- repeated execution through the shared compiled-schedule API
- shared correctness tests
- shared Criterion benchmarks

## Known Tradeoffs

The runtime is still intentionally simple, but the compiler is no longer cheap.

The main tradeoffs are:

- compile time is far higher than the original tracker-only baseline
- the whole-group barrier still exists and is still the defining limitation
- the smarter compiler can help only when it finds a better group layout, not
  when the workload is fundamentally barrier-shaped

That is acceptable for this experiment because the project has explicitly
prioritized repeated-run behavior over schedule-time cost.

## How To Read This Module

If you are new to the codebase, the recommended order is:

1. read [`crate::experiment`](../experiment/README.md) for the shared API
2. read [`DESIGN.md`](./DESIGN.md) for the full compile/runtime algorithm
3. read [`model.rs`](./model.rs) for the concrete implementation
4. read [`tests/experiment_02.rs`](../../tests/experiment_02.rs) for the public
   contract this scheduler must satisfy

## One-Sentence Summary

`experiment_02` is the layered baseline whose runtime stays tiny while the
compiler explores multiple group layouts and picks the one that best fits the
available thread budget.
