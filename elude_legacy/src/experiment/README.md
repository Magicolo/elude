# Experiment API

This module defines the shared benchmark-facing API for scheduler experiments.

The goal is to compare concrete implementations such as
[`experiment_01`](/home/goulade/Projects/rust/elude/src/experiment_01) and a
future `experiment_02` through the same public workflow instead of benchmarking
their internal data structures directly.

The intended shape is:

`Scheduler::new().add(Job::new(..)).add(Job::new(..)).schedule()?.run(..)?`

Jobs operate on shared state by taking `&S`, not `&mut S`.

That is intentional. Experimental schedulers are expected to execute jobs in
parallel, so the public API cannot hand out aliased mutable references to the
entire state object and still remain sound. Mutation should happen through
interior mutability or external synchronization inside `S`, while the scheduler
uses declared dependencies to control logical exclusivity.

## Rules For Benchmarks

- Benchmarks should construct jobs through `experiment::Job`.
- Benchmarks should depend only on the `experiment::Scheduler` and
  `experiment::CompiledSchedule` traits plus a concrete implementation type.
- Benchmarks should measure compile and run phases through that API surface.
- Benchmarks should not import implementation-internal model types.

## Design Intent

This API is intentionally small:

- `Job` describes executable work plus declared dependencies.
- `Scheduler` is the builder/compile surface.
- `CompiledSchedule` is the repeated execution surface.

That keeps adding `experiment_02`, `experiment_03`, and so on mechanically
simple for both implementation code and Criterion harnesses.
