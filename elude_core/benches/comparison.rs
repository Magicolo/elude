use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use elude_core::{memory::Memory, simple_memory::SimpleMemory};
use core::ptr::NonNull;

const CACHE_CAPACITY: usize = 8;
const BATCH: usize = 4;

// ════════════════════════════════════════════════════════════════════
// Fast path: thread-local (lock-free) vs mutex
// ════════════════════════════════════════════════════════════════════

fn compare_fast_path_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("fast_path/roundtrip");

    group.bench_function("lockfree", |b| {
        b.iter_batched(
            || {
                let pool = Memory::new();
                let pages: Vec<_> =
                    (0..CACHE_CAPACITY).map(|_| unsafe { pool.pop() }).collect();
                for &p in &pages {
                    unsafe { pool.push(p) };
                }
                pool
            },
            |pool| {
                for _ in 0..CACHE_CAPACITY {
                    let p = unsafe { pool.pop() };
                    unsafe { pool.push(p) };
                }
            },
            BatchSize::SmallInput,
        )
    });

    group.bench_function("mutex", |b| {
        b.iter_batched(
            || {
                let pool = SimpleMemory::new();
                let pages: Vec<_> =
                    (0..CACHE_CAPACITY).map(|_| unsafe { pool.pop() }).collect();
                for &p in &pages {
                    unsafe { pool.push(p) };
                }
                pool
            },
            |pool| {
                for _ in 0..CACHE_CAPACITY {
                    let p = unsafe { pool.pop() };
                    unsafe { pool.push(p) };
                }
            },
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

// ════════════════════════════════════════════════════════════════════
// Slow path: pop when allocation is needed
// ════════════════════════════════════════════════════════════════════

fn compare_pop_alloc(c: &mut Criterion) {
    let mut group = c.benchmark_group("slow_path/pop_alloc");

    group.bench_function("lockfree", |b| {
        b.iter_batched(
            || (Memory::new(),),
            |(pool,)| {
                let p = unsafe { pool.pop() };
                unsafe { pool.push(p) };
            },
            BatchSize::PerIteration,
        )
    });

    group.bench_function("mutex", |b| {
        b.iter_batched(
            || (SimpleMemory::new(),),
            |(pool,)| {
                let p = unsafe { pool.pop() };
                unsafe { pool.push(p) };
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

fn compare_try_pop_empty(c: &mut Criterion) {
    let mut group = c.benchmark_group("slow_path/try_pop_empty");

    {
        let pool = Memory::new();
        group.bench_function("lockfree", |b| {
            b.iter(|| {
                black_box(unsafe { pool.try_pop() });
            })
        });
    }

    {
        let pool = SimpleMemory::new();
        group.bench_function("mutex", |b| {
            b.iter(|| {
                black_box(unsafe { pool.try_pop() });
            })
        });
    }

    group.finish();
}

// ════════════════════════════════════════════════════════════════════
// Batch operations
// ════════════════════════════════════════════════════════════════════

fn compare_batch_ops(c: &mut Criterion) {
    // ── push_batch ──────────────────────────────────────────────────
    let mut group = c.benchmark_group("batch/push_batch");

    group.bench_function("lockfree", |b| {
        b.iter_batched(
            || {
                let pool = Memory::new();
                let pages: Vec<_> =
                    (0..BATCH).map(|_| unsafe { pool.pop() }).collect();
                (pool, pages)
            },
            |(pool, pages)| {
                unsafe { pool.push_batch(black_box(&pages)) };
            },
            BatchSize::PerIteration,
        )
    });

    group.bench_function("mutex", |b| {
        b.iter_batched(
            || {
                let pool = SimpleMemory::new();
                let pages: Vec<_> =
                    (0..BATCH).map(|_| unsafe { pool.pop() }).collect();
                (pool, pages)
            },
            |(pool, pages)| {
                unsafe { pool.push_batch(black_box(&pages)) };
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();

    // ── pop_batch ──────────────────────────────────────────────────
    let mut group = c.benchmark_group("batch/pop_batch");

    group.bench_function("lockfree", |b| {
        b.iter_batched(
            || {
                let pool = Memory::new();
                let pages: Vec<_> =
                    (0..BATCH).map(|_| unsafe { pool.pop() }).collect();
                unsafe { pool.push_batch(&pages) };
                (pool, [NonNull::<usize>::dangling(); BATCH])
            },
            |(pool, mut out)| {
                let n = unsafe { pool.pop_batch(black_box(&mut out), BATCH) };
                for i in 0..n {
                    unsafe { pool.push_batch(&[out[i]]) };
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.bench_function("mutex", |b| {
        b.iter_batched(
            || {
                let pool = SimpleMemory::new();
                let pages: Vec<_> =
                    (0..BATCH).map(|_| unsafe { pool.pop() }).collect();
                unsafe { pool.push_batch(&pages) };
                (pool, [NonNull::<usize>::dangling(); BATCH])
            },
            |(pool, mut out)| {
                let n = unsafe { pool.pop_batch(black_box(&mut out), BATCH) };
                for i in 0..n {
                    unsafe { pool.push_batch(&[out[i]]) };
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

// ════════════════════════════════════════════════════════════════════
// Multi-threaded thrashing
// ════════════════════════════════════════════════════════════════════

fn compare_concurrent_thrashing(c: &mut Criterion) {
    for &threads in &[2, 4, 8] {
        let mut group = c.benchmark_group(format!("concurrent/thrashing/{}t", threads));

        group.bench_function("lockfree", |b| {
            b.iter_batched(
                || {
                    let pool = Memory::new();
                    let total = threads * CACHE_CAPACITY * 4;
                    let pages: Vec<_> =
                        (0..total).map(|_| unsafe { pool.pop() }).collect();
                    unsafe { pool.push_batch(&pages) };
                    pool
                },
                |pool| {
                    std::thread::scope(|s| {
                        for _ in 0..threads {
                            s.spawn(|| {
                                for _ in 0..100 {
                                    let page = unsafe { pool.pop() };
                                    unsafe { *(page.cast::<u8>().as_ptr()) = 0xAB };
                                    unsafe { pool.push(page) };
                                }
                            });
                        }
                    });
                },
                BatchSize::SmallInput,
            )
        });

        group.bench_function("mutex", |b| {
            b.iter_batched(
                || {
                    let pool = SimpleMemory::new();
                    let total = threads * CACHE_CAPACITY * 4;
                    let pages: Vec<_> =
                        (0..total).map(|_| unsafe { pool.pop() }).collect();
                    unsafe { pool.push_batch(&pages) };
                    pool
                },
                |pool| {
                    std::thread::scope(|s| {
                        for _ in 0..threads {
                            s.spawn(|| {
                                for _ in 0..100 {
                                    let page = unsafe { pool.pop() };
                                    unsafe { *(page.cast::<u8>().as_ptr()) = 0xAB };
                                    unsafe { pool.push(page) };
                                }
                            });
                        }
                    });
                },
                BatchSize::SmallInput,
            )
        });

        group.finish();
    }
}

// ════════════════════════════════════════════════════════════════════
// Criterion harness
// ════════════════════════════════════════════════════════════════════

criterion_group!(
    comparison,
    compare_fast_path_roundtrip,
    compare_pop_alloc,
    compare_try_pop_empty,
    compare_batch_ops,
    compare_concurrent_thrashing,
);
criterion_main!(comparison);
