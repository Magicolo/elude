use criterion::{criterion_group, criterion_main, Criterion};
use elude::multex::{Indices, Multex};
use std::{
    thread::{scope, sleep},
    time::Duration,
};

const COUNT: usize = 32;
const BATCHES: &[usize] = &[1, 5, 10, 20];
const OFFSETS: [usize; 5] = [1, 3, 7, 11, 13];

fn standard_mutex(criterion: &mut Criterion) {
    for &batch in BATCHES {
        criterion.bench_function(&format!("standard_mutex-{batch}"), |bencher| {
            let mutexes = [(); COUNT].map(|_| std::sync::Mutex::new(0));
            let batches = (0..batch)
                .map(|i| OFFSETS.map(|offset| &mutexes[(i + offset) % COUNT]))
                .collect::<Box<[_]>>();
            bencher.iter(|| {
                scope(|scope| {
                    for (i, mutexes) in batches.iter().enumerate() {
                        scope.spawn(move || {
                            let mut guards = mutexes.map(|mutex| mutex.lock());
                            for guard in guards.iter_mut() {
                                **guard.as_mut().unwrap() += i;
                            }
                            sleep(Duration::from_micros(i as u64));
                            drop(guards);
                        });
                    }
                })
            });
        });
    }
}

fn parking_lot_mutex(criterion: &mut Criterion) {
    for &batch in BATCHES {
        criterion.bench_function(&format!("parking_lot_mutex-{batch}"), |bencher| {
            let mutexes = [(); COUNT].map(|_| parking_lot::Mutex::new(0));
            let batches = (0..batch)
                .map(|i| OFFSETS.map(|offset| &mutexes[(i + offset) % COUNT]))
                .collect::<Box<[_]>>();
            bencher.iter(|| {
                scope(|scope| {
                    for (i, mutexes) in batches.iter().enumerate() {
                        scope.spawn(move || {
                            let mut guards = mutexes.map(|mutex| mutex.lock());
                            for guard in guards.iter_mut() {
                                **guard += i;
                            }
                            sleep(Duration::from_micros(i as u64));
                            drop(guards);
                        });
                    }
                })
            });
        });
    }
}

fn multex(criterion: &mut Criterion) {
    for &batch in BATCHES {
        criterion.bench_function(&format!("multex-{batch}"), |bencher| {
            let multex = Multex::new([(); COUNT].map(|_| 0));
            let batches = (0..batch)
                .map(|i| Indices::new(OFFSETS.map(|offset| (offset + i) % COUNT)).unwrap())
                .collect::<Box<[_]>>();
            bencher.iter(|| {
                scope(|scope| {
                    let multex = &multex;
                    for (i, key) in batches.iter().enumerate() {
                        scope.spawn(move || {
                            let mut guard = multex.lock(key);
                            for guard in guard.iter_mut() {
                                **guard.as_mut().unwrap() += i;
                            }
                            sleep(Duration::from_micros(i as u64));
                            drop(guard);
                        });
                    }
                })
            });
        });
    }
}

criterion_group!(benches, standard_mutex, parking_lot_mutex, multex);
criterion_main!(benches);
