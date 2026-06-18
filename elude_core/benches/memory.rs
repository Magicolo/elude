use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use elude_core::memory::Memory;

const CACHE_CAPACITY: usize = 64;
const BATCH: usize = 32;

fn fast_path_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("fast_path");
    group.bench_function("roundtrip", |b| {
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
    group.finish();
}

fn slow_path_flush(c: &mut Criterion) {
    let mut group = c.benchmark_group("slow_path");
    group.bench_function("flush", |b| {
        b.iter_batched(
            || {
                let pool = Memory::new();
                let mut pages: Vec<_> =
                    (0..=CACHE_CAPACITY).map(|_| unsafe { pool.pop() }).collect();
                for &p in &pages[..CACHE_CAPACITY] {
                    unsafe { pool.push(p) };
                }
                let extra = pages.pop().unwrap();
                (pool, extra)
            },
            |(pool, extra)| {
                unsafe { pool.push(black_box(extra)) };
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

fn slow_path_refill(c: &mut Criterion) {
    let mut group = c.benchmark_group("slow_path");
    group.bench_function("refill", |b| {
        b.iter_batched(
            || {
                let pool = Memory::new();
                let pages: Vec<_> =
                    (0..BATCH).map(|_| unsafe { pool.pop() }).collect();
                unsafe { pool.push_batch(&pages) };
                pool
            },
            |pool| {
                let p = unsafe { pool.pop() };
                unsafe { pool.push(p) };
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

fn slow_path_pop_alloc(c: &mut Criterion) {
    let mut group = c.benchmark_group("slow_path");
    group.bench_function("pop_alloc", |b| {
        b.iter_batched(
            || (Memory::new(),),
            |(pool,)| {
                let p = unsafe { pool.pop() };
                unsafe { pool.push(p) };
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

fn slow_path_try_pop_empty(c: &mut Criterion) {
    let pool = Memory::new();
    c.bench_function("slow_path/try_pop_empty", |b| {
        b.iter(|| {
            black_box(unsafe { pool.try_pop() });
        })
    });
}

fn batch_ops(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch");
    group.bench_function("push_batch", |b| {
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
    group.bench_function("pop_batch", |b| {
        b.iter_batched(
            || {
                let pool = Memory::new();
                let pages: Vec<_> =
                    (0..BATCH).map(|_| unsafe { pool.pop() }).collect();
                unsafe { pool.push_batch(&pages) };
                (pool, [core::ptr::NonNull::<usize>::dangling(); BATCH])
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

fn concurrent_thrashing(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent");
    for &threads in &[2, 4, 8] {
        group.bench_function(format!("thrashing/{}t", threads), |b| {
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
                                unsafe { pool.flush_all() };
                            });
                        }
                    });
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn concurrent_pool_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent");
    for &threads in &[2, 4, 8] {
        group.bench_function(format!("pool_only/{}t", threads), |b| {
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
                                    unsafe { pool.push(page) };
                                }
                                unsafe { pool.flush_all() };
                            });
                        }
                    });
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    fast_path_roundtrip,
    slow_path_flush,
    slow_path_refill,
    slow_path_pop_alloc,
    slow_path_try_pop_empty,
    batch_ops,
    concurrent_thrashing,
    concurrent_pool_only,
);
criterion_main!(benches);
