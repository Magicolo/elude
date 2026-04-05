use parking_lot::{Condvar, Mutex};
use rayon::ThreadPoolBuilder;
use std::{
    hint::black_box,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

const JOBS: usize = 4_096;
const CLUSTER_SIZE: usize = 64;
const CLUSTERS: usize = JOBS / CLUSTER_SIZE;
const ROUNDS: usize = 300;

fn touch_job(slots: &[AtomicU64], job: usize, round: usize) {
    let value = ((job as u64) << 16) ^ round as u64 ^ 0xfeed_beef_dead_babe;
    slots[job % slots.len()].fetch_add(value | 1, Relaxed);
}

fn sample(slots: &[AtomicU64]) -> u64 {
    let first = slots[0].load(Relaxed);
    let last = slots[slots.len() - 1].load(Relaxed);
    black_box(first ^ last.rotate_left(13))
}

fn run_rayon_cluster_loops(slots: &[AtomicU64], threads: usize) -> Duration {
    let pool = ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let start = Instant::now();
    for round in 0..ROUNDS {
        pool.scope(|scope| {
            for cluster in 0..CLUSTERS {
                scope.spawn(move |_| {
                    let start_job = cluster * CLUSTER_SIZE;
                    for bit in 0..CLUSTER_SIZE {
                        touch_job(slots, start_job + bit, round);
                    }
                });
            }
        });
    }
    black_box(sample(slots));
    start.elapsed()
}

#[derive(Default)]
struct Control {
    generation: usize,
    next_cluster: usize,
    remaining: usize,
    active: bool,
}

struct Shared {
    control: Mutex<Control>,
    worker_cv: Condvar,
    done_cv: Condvar,
    shutdown: AtomicBool,
    slots: Arc<[AtomicU64]>,
}

fn run_fixed_pool_clusters(slots: Arc<[AtomicU64]>, threads: usize) -> Duration {
    let shared = Arc::new(Shared {
        control: Mutex::new(Control::default()),
        worker_cv: Condvar::new(),
        done_cv: Condvar::new(),
        shutdown: AtomicBool::new(false),
        slots,
    });

    let mut handles = Vec::new();
    for _ in 0..threads {
        let shared = Arc::clone(&shared);
        handles.push(thread::spawn(move || {
            let mut seen_generation = 0usize;
            loop {
                let mut control = shared.control.lock();
                while !shared.shutdown.load(Relaxed) && control.generation == seen_generation {
                    shared.worker_cv.wait(&mut control);
                }
                if shared.shutdown.load(Relaxed) {
                    return;
                }
                seen_generation = control.generation;

                loop {
                    if control.next_cluster < CLUSTERS {
                        let cluster = control.next_cluster;
                        let round = control.generation - 1;
                        control.next_cluster += 1;
                        drop(control);

                        let start_job = cluster * CLUSTER_SIZE;
                        for bit in 0..CLUSTER_SIZE {
                            touch_job(shared.slots.as_ref(), start_job + bit, round);
                        }

                        control = shared.control.lock();
                        control.remaining -= 1;
                        if control.remaining == 0 {
                            control.active = false;
                            shared.done_cv.notify_one();
                            shared.worker_cv.notify_all();
                        }
                    } else if control.active {
                        shared.worker_cv.wait(&mut control);
                    } else {
                        break;
                    }
                }
            }
        }));
    }

    let start = Instant::now();
    for round in 0..ROUNDS {
        let mut control = shared.control.lock();
        control.generation = round + 1;
        control.next_cluster = 0;
        control.remaining = CLUSTERS;
        control.active = true;
        shared.worker_cv.notify_all();
        while control.active {
            shared.done_cv.wait(&mut control);
        }
    }
    let elapsed = start.elapsed();

    shared.shutdown.store(true, Relaxed);
    shared.worker_cv.notify_all();
    for handle in handles {
        handle.join().unwrap();
    }

    black_box(sample(shared.slots.as_ref()));
    elapsed
}

fn print_result(label: &str, elapsed: Duration) {
    let per_round = elapsed.as_secs_f64() * 1_000_000.0 / ROUNDS as f64;
    println!("{label:24} total={elapsed:?} avg_round={per_round:.2}us");
}

fn main() {
    let threads = thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);
    let slots = (0..CLUSTER_SIZE)
        .map(|_| AtomicU64::new(0))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let slots: Arc<[AtomicU64]> = slots.into();

    println!(
        "fixed pool cluster poc: jobs={JOBS} clusters={CLUSTERS} cluster_size={CLUSTER_SIZE} rounds={ROUNDS} threads={threads}"
    );

    let rayon_clusters = run_rayon_cluster_loops(slots.as_ref(), threads);
    let fixed_pool_clusters = run_fixed_pool_clusters(Arc::clone(&slots), threads);

    print_result("rayon_cluster_loops", rayon_clusters);
    print_result("fixed_pool_clusters", fixed_pool_clusters);
    println!(
        "speedup fixed_vs_rayon={:.2}x",
        rayon_clusters.as_secs_f64() / fixed_pool_clusters.as_secs_f64()
    );
}
