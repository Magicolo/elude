# Experiment 03

`experiment_03` is the "compile an aggressively optimized execution graph, then
run it like a plain DAG" scheduler in the experimental family.

Its starting point is the main lesson from the first two experiments:

- `experiment_01` proves that dynamic runtime reservation can recover
  substantial parallelism
- `experiment_02` proves that an extremely small runtime can be very cheap, but
  loses parallelism because of whole-group barriers

`experiment_03` asks a different question:

"What if relaxed conflicts are not handled dynamically at runtime at all, and
 are instead treated as compile-time optimization variables?"

That is the core idea.

This experiment assumes the schedule will be compiled once and run many times.
Because of that, it is willing to spend significantly more work during
`schedule()` if doing so removes runtime coordination and shrinks the hot path
of `run()`.

## One-Sentence Mental Model

`experiment_03` turns the entire job set into a pre-oriented sparse DAG and
then executes that DAG with nothing more than predecessor counters and direct
successor wakeups.

The result is:

- no runtime page reservation
- no sequential group barriers
- no scheduler-owned conflict locks during execution
- no runtime search for which job should go next

All of that decision-making is pushed into compilation.

## Shared Public API

Like the other experiments, this module implements the public interface from
[`crate::experiment`](../experiment):

`Scheduler::new().add(Job::new(..)).schedule()?.run(&state)?`

That shared API matters because the benchmark suite compares complete
implementations, not isolated helper functions.

## Shared Dependency Model

`experiment_03` respects exactly the same dependency model as the other
experiments.

- `Read(key, order)` means shared access to a logical resource
- `Write(key, order)` means exclusive access to a logical resource
- `Relax` means conflicts may be reordered, but may not overlap
- `Strict` means conflicts may not overlap and must preserve declaration order
- `Unknown` behaves like a strict barrier

The public API gives each job `&S`, not `&mut S`. The scheduler is preserving
declared logical access semantics over shared state, not manufacturing unique
whole-state references at runtime.

Each individual job is validated before compilation:

- repeated reads of the same key are accepted and normalized
- `Read(key)` together with `Write(key)` in the same job is rejected
- multiple `Write(key)` accesses to the same key in the same job are rejected

That rule is non-negotiable. A single job is not allowed to describe an
internally contradictory aliasing model and still enter the schedule.

## What Makes Experiment 03 Different

The unusual part of `experiment_03` is that it does not treat relaxed conflicts
as runtime scheduling state.

Instead it does three things:

1. it computes the strict-order constraints that are fixed by the dependency
   model
2. it chooses a compile-time priority order for jobs that extends those strict
   constraints
3. it computes final-DAG criticality so runtime wakeups can prefer longer
   remaining paths
4. it orients relaxed conflicts according to that priority order and compresses
   them into a sparse predecessor/successor DAG

After that, runtime execution is just graph execution.

This is why `experiment_03` is "outside the box" relative to the first two
experiments.

### Compared to Experiment 01

`experiment_01` keeps relaxed conflicts unresolved until runtime and uses
atomics to decide which eligible job actually reserves shared conflict pages
first.

`experiment_03` does the opposite:

- it refuses to resolve relaxed conflicts dynamically
- it chooses one orientation ahead of time
- it accepts the compile-time cost
- it tries to win by making each repeated run almost mechanically trivial

### Compared to Experiment 02

`experiment_02` resolves relaxed conflicts by placing jobs into sequential
parallel groups.

That is cheap, but it introduces artificial barriers: if any job in a group is
still running, all jobs in later groups wait.

`experiment_03` avoids those full-group barriers. A job only waits for the
specific predecessor jobs that were compiled into its sparse DAG slice.

That is the central promise of this design:

- simpler runtime than `experiment_01`
- finer-grained parallelism than `experiment_02`

## Execution Strategy

At a high level, `experiment_03` executes like this:

1. jobs whose predecessor counter is zero are roots
2. roots and successor slices are pre-sorted by final DAG height
3. one ready chain stays inline on the current worker
4. when a job finishes, it decrements the counters of its direct successors
5. the first newly ready successor continues inline and any additional ready
   successors are spawned immediately
6. the schedule completes when the DAG drains

That is the entire runtime model.

There are no scheduler-managed reservations, no scan-for-ready passes, and no
group boundaries. The work-first inline chain is there only to reduce ready-job
spawn overhead; it does not change the compiled DAG semantics.

## Why This Design May Work Well

This experiment is optimizing for repeated `run()` calls, not for cheap
`schedule()`.

That matters because repeated-run throughput is dominated by:

- how many atomics are touched per completed job
- how much scheduler-owned memory must be walked in the hot path
- whether runnable jobs can be discovered directly or must be searched for
- whether safe overlap is blocked by coarse barriers

`experiment_03` tries to optimize all four:

- one counter per job
- one flat successor array
- direct wakeup only
- critical-path-first wakeup ordering
- fewer spawned tasks on the hot path
- no whole-group synchronization

The compile step is intentionally more aggressive if it makes those runtime
properties better.

## Known Tradeoffs

This design has real tradeoffs.

- It may spend a lot more work during `schedule()`
- It commits to one static orientation of relaxed conflicts for all future runs
- If the compile-time priority heuristic is poor, the runtime DAG may still
  serialize more than necessary
- It cannot adapt dynamically to job-duration skew inside a single run the way
  a reservation-based runtime can

Those tradeoffs are accepted on purpose. This experiment is explicitly testing
whether a very high-quality static DAG can capture most of the practical
parallelism without any runtime conflict protocol.

## How To Read This Module

If you are new to the codebase, the recommended order is:

1. read [`crate::experiment`](../experiment/README.md) for the shared API
2. read [`DESIGN.md`](./DESIGN.md) for the full algorithm and invariants
3. read [`model.rs`](./model.rs) for the implementation
4. read [`tests/experiment_03.rs`](../../tests/experiment_03.rs) for the
   public contract the scheduler must satisfy

## Bottom Line

`experiment_03` treats relaxed conflicts as compile-time optimization choices,
not runtime scheduling state, and spends schedule time to produce the simplest
possible repeated-run executor: a sparse static DAG with direct wakeups.
