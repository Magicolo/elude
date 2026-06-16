# Experiment 05 Design

## Summary

`experiment_05` is a hierarchical scheduler:

- compile jobs into small clusters
- keep one queued worker task per active cluster
- let that worker drain multiple local jobs inline
- use local dynamic selection inside the cluster
- keep cross-cluster dependencies explicit and cheap

## Why This Design Exists

The previous experiments exposed a gap:

- per-job wakeup engines retain fine-grained parallelism, but they still pay
  executor overhead for many small scheduling decisions
- whole-group schedulers are cheap, but lose too much flexibility

This experiment tries to split the difference:

- the runtime scheduling unit is a cluster
- the semantic scheduling unit remains a job

## Semantics

Strict conflicts are always preserved in declaration order.

Relaxed conflicts are handled in two ways:

- if the conflicting key stays inside one cluster, the conflict is left dynamic
  and resolved by the cluster's local sequential executor
- otherwise the conflict is oriented statically into the cross-cluster graph

That means `experiment_05` only keeps dynamic freedom where it is actually
bounded and cheap.

## Runtime Model

Each cluster keeps:

- a `ready_mask` mailbox for remotely awakened local jobs
- a `done_mask` for local predecessor-mask checks
- per-job external predecessor counters
- one `queued` bit to guarantee that at most one Rayon task is responsible for a
  cluster at a time

When a cluster task runs, it:

1. pulls any remotely awakened work from the mailbox
2. repeatedly selects one local ready job
3. runs it inline
4. wakes local successors without requeueing
5. emits cross-cluster wakeups only when needed
6. keeps draining until no local work remains

This is the critical contrast to `experiment_03` and `experiment_04`, which
still make the executor aware of every ready job.

## Current Limitations

- clusters are exclusive: only one worker runs inside a cluster at a time
- pure independent jobs still tend to become singleton clusters to preserve
  parallelism
- cross-cluster predecessor counters are reset between runs, so repeated-run
  overhead still includes a schedule-size component

Those are acceptable tradeoffs for the first prototype. The design is built so
later iterations can replace the executor or make the cluster kernel more
aggressive without changing the public API.
