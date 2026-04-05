# Experiment 04 Design

## Purpose

`experiment_04` explores a stronger compile-once, run-many assumption than
`experiment_03`.

Instead of betting on one static DAG orientation, it compiles a small portfolio
of legal DAG variants that favor different objectives, then chooses among them
at run time.

## Design Summary

The current implementation:

1. normalizes dependencies exactly like the other experiments
2. builds the fixed strict DAG implied by declaration order
3. computes per-job structural and contention statistics
4. compiles three topological priority orders:
   - `critical_path`
   - `read_heavy`
   - `hot_contention`
5. orients relaxed conflicts according to each priority order
6. sparsifies and transitively reduces each resulting DAG
7. stores one shared job-closure array plus one execution-metadata block per
   variant
8. runs either:
   - a fixed variant, or
   - an adaptive selector that samples variants and then prefers the best
     measured one

In adaptive mode all three variants are compiled. In fixed mode only the pinned
variant is compiled, which keeps benchmark comparisons honest when the goal is
to compare one specific schedule shape rather than the full portfolio.

## Why This Is Contrastive

- `experiment_01` resolves relaxed conflicts dynamically while running
- `experiment_02` commits to one grouped layering
- `experiment_03` commits to one optimized static DAG
- `experiment_04` says that repeated-run workloads may justify compiling
  multiple static DAGs because the best legal orientation depends on workload
  shape

## Runtime Core

Each variant uses the same work-first direct-wakeup executor shape introduced in
`experiment_03`:

- predecessor counters
- flat successor lists
- criticality-ordered roots and successor slices
- one inline ready chain plus spawned side work

The contrast is not the executor itself. The contrast is that multiple executor
graphs coexist inside one compiled schedule.

## Variant Intent

### `critical_path`

Biases toward:

- long strict-path height
- unlock potential
- contention-heavy writes

### `read_heavy`

Biases toward:

- read-biased jobs
- hot shared-read batches
- delaying writes when legal

### `hot_contention`

Biases toward:

- hot write-heavy jobs
- jobs that touch highly contended keys
- burning through chokepoints early

## Policy Modes

### Adaptive

The default mode:

- samples each variant during an initial warmup
- then prefers the lowest measured mean runtime
- occasionally re-samples non-selected variants

### Fixed

The fixed mode pins one variant and exists so benchmarks can compare a specific
schedule shape without adaptive policy noise.

## Shared Storage

The same closures are shared across all variants through `Arc`-backed storage.
Only execution metadata is duplicated per variant.

That keeps the portfolio expensive, but the extra cost is metadata rather than
closure cloning.
