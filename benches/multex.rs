use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use elude::multex::{AtN, Multex64};
use rayon::ThreadPoolBuilder;

const COUNT: usize = usize::BITS as usize;
const ITERATIONS: usize = 1_000;
const BATCHES: [usize; 3] = [1, 10, 25];
const OFFSETS: [usize; 7] = [1, 3, 7, 11, 13, 17, 19];

fn standard_mutex(criterion: &mut Criterion) {
    for batch in BATCHES.iter() {
        criterion.bench_with_input(
            BenchmarkId::new("standard_mutex", batch),
            batch,
            |bencher, &batch| {
                let pool = ThreadPoolBuilder::new().build().unwrap();
                let mutexes = [(); COUNT].map(|_| std::sync::Mutex::new(0));
                let batches = (0..batch)
                    .map(|i| OFFSETS.map(|offset| &mutexes[(i + offset) % COUNT]))
                    .collect::<Box<[_]>>();
                bencher.iter(|| {
                    pool.scope(|scope| {
                        for (i, mutexes) in batches.iter().enumerate() {
                            scope.spawn(move |_| {
                                for _ in 0..ITERATIONS {
                                    let mut guards = mutexes.map(|mutex| mutex.lock());
                                    for guard in guards.iter_mut() {
                                        **guard.as_mut().unwrap() += i;
                                    }
                                }
                            });
                        }
                    })
                });
                black_box(mutexes);
            },
        );
    }
}

fn parking_lot_mutex(criterion: &mut Criterion) {
    for batch in BATCHES.iter() {
        criterion.bench_with_input(
            BenchmarkId::new("parking_lot_mutex", batch),
            batch,
            |bencher, &batch| {
                let pool = ThreadPoolBuilder::new().build().unwrap();
                let mutexes = [(); COUNT].map(|_| parking_lot::Mutex::new(0));
                let batches = (0..batch)
                    .map(|i| OFFSETS.map(|offset| &mutexes[(i + offset) % COUNT]))
                    .collect::<Box<[_]>>();
                bencher.iter(|| {
                    pool.scope(|scope| {
                        for (i, mutexes) in batches.iter().enumerate() {
                            scope.spawn(move |_| {
                                for _ in 0..ITERATIONS {
                                    let mut guards = mutexes.map(|mutex| mutex.lock());
                                    for guard in guards.iter_mut() {
                                        **guard += i;
                                    }
                                }
                            });
                        }
                    })
                });
                black_box(mutexes);
            },
        );
    }
}

fn multex(criterion: &mut Criterion) {
    for batch in BATCHES.iter() {
        criterion.bench_with_input(
            BenchmarkId::new("multex", batch),
            batch,
            |bencher, &batch| {
                let pool = ThreadPoolBuilder::new().build().unwrap();
                let multex = Multex64::new([(); COUNT].map(|_| 0));
                let batches = (0..batch)
                    .map(|i| {
                        AtN::<_, [_; 7]>::new(OFFSETS.map(|offset| (offset + i) % COUNT)).unwrap()
                    })
                    .collect::<Box<[_]>>();
                bencher.iter(|| {
                    pool.scope(|scope| {
                        let multex = &multex;
                        for (i, key) in batches.iter().enumerate() {
                            scope.spawn(move |_| {
                                for _ in 0..ITERATIONS {
                                    let mut guard = multex.lock_with(key, false);
                                    for guard in guard.iter_mut() {
                                        **guard.as_mut().unwrap() += i;
                                    }
                                }
                            });
                        }
                    })
                });
                black_box(multex);
            },
        );
    }
}

criterion_group!(benches, standard_mutex, parking_lot_mutex, multex);
criterion_main!(benches);
