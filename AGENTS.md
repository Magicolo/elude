# AGENTS.md — Contributor/Agent Guidelines for `elude`

## Purpose

This file is the operating manual for contributors and coding agents working
in the `elude` repository. It explains the project, its structure, quality
standards, and how to work here.

When this document conflicts with current source code, trust the source code
first and update this file as part of the task.

**Agents must automatically and systematically maintain this file as the
project evolves.** Every task that changes architecture, workflow, conventions,
toolchain, testing strategy, or other durable guidance must update this file
accordingly. If a task introduces a new standard, convention, or pattern that
future agents should follow, document it here. Keeping this file current is a
mandatory part of every non-trivial task, not an afterthought.

## Project Summary

`elude` is a Rust 2024 columnar data library (MIT, MSRV 1.85). It provides:

- A **lock-free page pool** with per-thread Treiber stacks, tagged-pointer ABA
  counters, local caches, and batch push/pop (`memory.rs`).
- A **mutex-guarded page pool** fallback (`simple_memory.rs`).
- A **type-level schema system** with pre-computed page capacity (`schema`).
- A **columnar table layout** with concurrent locking protocol (`table`).
- A **query builder** with planned join support (`query`).
- **Derive macros** for schema, table, and column types (`elude_macro`).

## Project Goals

- Correctness of memory management and concurrent access semantics.
- Soundness of unsafe code — every `unsafe` block must be provably safe.
- Hot-path performance (local cache hits, batch operations, lock-free push/pop).
- Composability of schema and table abstractions.

## Architecture

```
elude (workspace root — re-exports)
├── elude_core    — Core library: memory pools, schema, table, query, index
└── elude_macro   — Proc-macro crate (derive macros)
```

## Repository Map

```
├── .github/
│   ├── workflows/test.yml    — CI: single matrix job (build, test, clippy, fmt, doc, miri, deny, cov, msrv, dupe, sort, semver-checks)
│   └── dependabot.yml        — Weekly dependency updates
├── elude_core/src/           — Core library source
├── elude_core/benches/       — Criterion benchmarks
├── elude_macro/src/          — Proc-macro source
├── elude_legacy/             — publish=false, legacy experiments/benchmarks
├── src/lib.rs                — Workspace root re-exports
├── Cargo.toml                — Workspace root
├── rustfmt.toml              — Nightly config
├── README.md                 — Project overview
└── AGENTS.md                 — This file
```

### Editable Areas

`elude_core/src/`, `elude_macro/src/`, `elude_legacy/src/`,
`elude_legacy/tests/`, `elude_legacy/examples/`, `elude_legacy/benches/`,
`elude_core/benches/`, `src/`, `Cargo.toml` files, `.github/`, `README.md`,
`AGENTS.md`.

### Ignored Areas (do not touch unless explicitly told)

- `.local/`: local scratch space, never commit
- `token-optimizer/`: tool-specific output directory

### Generated Areas (do not edit directly)

- `target/`: build artifacts
- `Cargo.lock`: locked dependency graph

## Mandatory Rules

### Engineering Posture

- Be thorough, explicit, skeptical, and precise. Fix root causes, not symptoms.
- Use official Rust documentation when language semantics affect confidence.
- Prefer compile-time guarantees over runtime checks.
- Every `unsafe` block must be provably safe with a `// SAFETY:` comment.

### Concurrent Work Awareness

- Assume concurrent edits. Re-check files before editing, especially after long
  exploration. Keep edits tightly scoped. Never overwrite others' changes.
  Prefer small patches in shared files.

### Forbidden Git Operations

Do not use: `git stash`, `git reset --hard/mixed`, `git checkout -- <file>`,
`git restore <file>`, `git clean -fd`, `git rebase`, `git commit --amend`,
`git push --force/--force-with-lease`, `git merge --abort`,
`git cherry-pick --abort`, `git revert --abort`, `git update-ref -d`.

These destructively hide or rewrite state. If one seems necessary, ask the user.

### Quality

- Test-driven mindset. Add failing case first when fixing bugs.
- Prefer high-value tests over test-count growth.
- Code must compile, pass clippy at `-D warnings`, and be formatted with
  `cargo +nightly fmt` before finishing.
- Leave the codebase stricter than you found it. Fix violations even outside
  the original task scope.
- Error paths matter as much as success paths. Handle zero-length inputs,
  OOM paths, and `Drop` without panic.

### Naming

- Full descriptive names. No abbreviations except established domain terms
  (`ABA`, `CAS`, `OOM`).
- Prefer names like `flush_all`, `refill_from_head` over `flush`, `refill`.

### Comments And Documentation

- Self-documenting code over explanatory comments. Comments explain *why*.
- Every `unsafe` block requires `// SAFETY:` explaining invariants,
  preconditions, and memory model guarantees.
- Tagged-pointer encoding and concurrency protocols must document bit layout,
  lock ordering, ABA strategy, and memory ordering.
- Keep documentation synchronized with actual code.

### Maintenance

- **This file must be updated by every non-trivial task.** If the task changes
  architecture, workflow, conventions, toolchain, testing strategy, or adds a
  new standard/convention, update `AGENTS.md` as part of the task — not as an
  afterthought. This is mandatory.
- Version bumps are manual in workspace `Cargo.toml`. All sub-crates release
  together at the same version. No `RELEASES.md` maintained yet.
- `.github/instructions/` and `.github/prompts/` are not used. Everything lives
  in `AGENTS.md`.

### Validation

Before finishing, run the relevant checks:

- Formatting: `cargo +nightly fmt --all --check`
- Linting + build: `cargo hack clippy --workspace --feature-powerset -- -D warnings`
- Tests: `cargo hack nextest run --workspace --feature-powerset --no-tests pass`
- Miri: `cargo +nightly miri nextest run` (for unsafe/concurrency changes)
- Coverage: `cargo llvm-cov nextest --workspace --all-features --fail-under-regions 50`
- Security: `cargo deny check advisories` (for dependency changes)
- Bans: `cargo deny check bans`
- Duplicates: `cargo dupe` (for dependency changes)
- Ordering: `cargo sort --check --workspace`
- MSRV: `cargo msrv verify --workspace`
- Semver: `cargo semver-checks --workspace`
- Docs: `cargo doc --no-deps --release --all-features --workspace` (must pass without error/warning, `RUSTDOCFLAGS=-D warnings` set in environment)

Run the narrowest meaningful command first.

## Required Agent Workflow

1. **Read relevant code paths first.** Trace call chain from symptom to cause.
2. **Confirm assumptions** against implementation and official docs.
3. **Make the smallest change that fixes the root cause.**
4. **Add or update tests** for changed behavior.
5. **Add failing case first** when fixing a bug.
6. **Run a review pass** — refactor, simplify, fix naming, tighten validation.
7. **Run formatting, linting, tests, and type-checking.**
8. **Update `AGENTS.md`** if architecture, standards, or workflows changed.

The review pass is mandatory. If it works but has avoidable awkwardness, it is
not done.

## Development Workflow

1. **Understand fast and slow paths.** The lock-free page pool has a demarcated
   fast path (local cache hit) and slow paths (flush/refill, OS fallback).
2. **Test first.** Write or extend tests before changing behavior.
3. **Keep unsafe minimal.** Every `unsafe` block needs `// SAFETY:`. Prefer
   safe abstractions (e.g., `parking_lot::Mutex`) where possible.
4. **Format with nightly rustfmt.** `cargo +nightly fmt`
5. **Run the full suite:** `cargo +nightly hack nextest run --workspace --feature-powerset --no-tests pass`,
   `cargo +nightly hack clippy --workspace --feature-powerset -- -D warnings`,
   `cargo +nightly fmt --all --check`, `cargo +nightly miri nextest run --workspace --all-features` (if unsafe),
   `cargo deny check advisories`, `cargo deny check bans`,
   `cargo dupe`, `cargo sort --check --workspace`,
   `cargo llvm-cov nextest --workspace --all-features --fail-under-regions 50`,
   `cargo msrv verify --workspace`, `cargo semver-checks --workspace`,
   `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --release --all-features --workspace`.
6. **Version bumps are manual** in workspace `Cargo.toml`.

## Coding Standards

### Rust Idioms

- Safe-by-default. Isolate unsafe in leaf modules with narrow safe APIs.
- Prefer enums, structs, newtypes, and traits over raw integers.
- Avoid `unwrap`/`expect` in runtime paths unless the invariant is obvious and
  local. `pop()` panics on OOM via `handle_alloc_error` — intentional.
- Use checked operations and `from`/`try_from` over unchecked casts.
- Named constants over magic numbers. No speculative abstractions (interface
  with one implementation, factory for one product, config for a value that
  never changes).

### Item Ordering

Inside every Rust file/module/block:

1. **Preamble:** `use` imports, `mod` declarations.
2. **Type declarations:** `struct`, `enum`, `union`, `trait`, `type`, `const`,
   `static`.
3. **`impl` blocks:** grouped by target type in declaration order.
4. **Free-standing functions.**
5. **Inline submodules** (`mod foo { ... }`), including `#[cfg(test)] mod tests`.

Cross-cutting: visibility first (`pub` > `pub(crate)` > private). Within impl
blocks, associated functions before methods. In test modules, `#[test]` before
non-test functions. `macro_rules!` as late as possible.

## Testing Expectations

- Tests live in `#[cfg(test)] mod tests` at module bottom.
- Cover happy path, edge cases (empty, full, single, boundary values,
  zero-length batch, wraparound), and concurrency scenarios.
- Concurrency tests use `std::thread::scope`. Cover: concurrent push-only,
  pop-only, mixed, high-contention, thread migration, batch-between-workers.
- Verify no duplicate/lost pages, all pages accounted for, drop safety,
  pointer integrity, alignment.
- All tests pass under `cargo +nightly miri nextest run`.
- Use TDD: add failing case, implement, add regression coverage.
- Benchmarks use `criterion` with `BatchSize::PerIteration`, grouped by
  category (`fast_path`, `slow_path`, `batch`, `concurrent`).

## CI/CD

A single `test` job runs all quality gates via a strategy matrix in
`ghcr.io/magicolo/rust` container:

- Build, test, clippy across all feature combinations (`cargo hack`).
- Formatting (`cargo +nightly fmt`).
- Documentation (`RUSTDOCFLAGS=-D warnings cargo doc`).
- Miri (`cargo +nightly miri nextest run`).
- Security advisories + bans (`cargo deny`).
- Duplicate dependency detection (`cargo dupe`).
- Cargo.toml ordering (`cargo sort`).
- MSRV verification (`cargo msrv verify`).
- Semver API compatibility (`cargo semver-checks`).
- Coverage (`cargo llvm-cov`).

MSRV: 1.85.

## Security Posture

- `cargo deny check advisories` in CI. Block on high-severity advisories.
- Every `unsafe` block annotated with `// SAFETY:`, must be reviewed.
- Miri in CI. Soundness bugs are P0, fix within 48 hours.
- Minimize unsafe surface. No `#[repr(packed)]` on types with `AtomicUsize` or
  `NonNull` without explicit alignment verification.

## Change Playbook

### Before Editing

- Identify canonical source file. Trace call chain from public API to leaf.
- Read types, traits, boundary definitions, and existing tests first.
- For bug fixes: grep every caller of the function you are about to touch.
  Root cause may be one level up.

### While Editing

- Keep naming explicit. Preserve module boundaries and fast/slow path
  demarcation. Every `unsafe` block needs `// SAFETY:`. Prefer smallest
  root-cause fix.

### After Editing

1. Review pass (refactor, simplify, fix naming, tighten validation).
2. Add failing case if bug fix.
3. `cargo +nightly fmt`
4. Run validation (test, clippy, fmt --check, miri if unsafe, deny if deps).
5. Update `AGENTS.md` if guidance changed.

## Scope Boundaries

This file covers: library architecture, memory pool design (lock-free +
mutex-based), schema/table abstractions, proc-macro derives, concurrency
protocols, workflow, and coding standards.

Not covered: unsupported performance claims without benchmarks, correctness
without Miri, external system behavior, `#[no_std]` (not yet supported),
Display/serde (not yet implemented), `elude_legacy` semver (publish=false).

Note: `pop()` panics on OOM via `handle_alloc_error` — intentional. Use
`try_pop()` if OOM tolerance needed.

## Start Here

Read these files first:
`README.md` → `Cargo.toml` (workspace) → `elude_core/Cargo.toml` →
`elude_macro/Cargo.toml` → `elude_core/src/lib.rs` →
`elude_core/src/memory.rs` → `elude_core/src/simple_memory.rs` → `src/lib.rs`

## When In Doubt

- Prefer source code over assumptions.
- Prefer official Rust documentation over memory.
- Prefer explicit validation over implicit behavior.
- Prefer smaller, well-tested refactors over clever patches.
- If you cannot write a `// SAFETY:` comment, you do not understand the invariant.
- If behavior is not tested, it is broken by default.
- Keep this `AGENTS.md` current as the repository evolves.
