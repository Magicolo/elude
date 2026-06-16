use rayon::ThreadPoolBuilder;
use std::{
    hint::black_box,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    thread,
    time::{Duration, Instant},
};

const JOBS: usize = 4_096;
const CLUSTER_SIZE: usize = 64;
const CLUSTERS: usize = JOBS / CLUSTER_SIZE;
const ROUNDS: usize = 300;

fn touch_job(slots: &[AtomicU64], job: usize, round: usize) {
    let value = ((job as u64) << 16) ^ round as u64 ^ 0xa5a5_a5a5_a5a5_a5a5;
    slots[job % slots.len()].fetch_add(value | 1, Relaxed);
}

fn sample(slots: &[AtomicU64]) -> u64 {
    let first = slots[0].load(Relaxed);
    let last = slots[slots.len() - 1].load(Relaxed);
    black_box(first ^ last.rotate_left(9))
}

fn run_rayon_job_tasks(slots: &[AtomicU64], threads: usize) -> Duration {
    let pool = ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let start = Instant::now();
    for round in 0..ROUNDS {
        pool.scope(|scope| {
            for job in 0..JOBS {
                scope.spawn(move |_| touch_job(slots, job, round));
            }
        });
    }
    black_box(sample(slots));
    start.elapsed()
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

fn run_rayon_cluster_masks(slots: &[AtomicU64], threads: usize) -> Duration {
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
                    let mut remaining = u64::MAX;
                    while remaining != 0 {
                        let bit = remaining.trailing_zeros() as usize;
                        remaining &= remaining - 1;
                        touch_job(slots, start_job + bit, round);
                    }
                });
            }
        });
    }
    black_box(sample(slots));
    start.elapsed()
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
        .collect::<Vec<_>>();

    println!(
        "cluster granularity poc: jobs={JOBS} clusters={CLUSTERS} cluster_size={CLUSTER_SIZE} rounds={ROUNDS} threads={threads}"
    );

    let job_tasks = run_rayon_job_tasks(&slots, threads);
    let cluster_loops = run_rayon_cluster_loops(&slots, threads);
    let cluster_masks = run_rayon_cluster_masks(&slots, threads);

    print_result("rayon_job_tasks", job_tasks);
    print_result("rayon_cluster_loops", cluster_loops);
    print_result("rayon_cluster_masks", cluster_masks);

    println!(
        "speedup loop_vs_jobs={:.2}x mask_vs_jobs={:.2}x",
        job_tasks.as_secs_f64() / cluster_loops.as_secs_f64(),
        job_tasks.as_secs_f64() / cluster_masks.as_secs_f64()
    );
}
