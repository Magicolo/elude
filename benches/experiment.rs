use criterion::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput,
};
use elude::{
    depend::{
        Dependency::{Read, Write},
        Key::Identifier,
        Order::{Relax, Strict},
    },
    experiment::{CompiledSchedule as _, Job},
    experiment_01,
    experiment_02,
    experiment_03,
};
use std::{
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, Ordering::Relaxed as AtomicRelaxed},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConflictFlag {
    ReadRead,
    ReadWrite,
    WriteWrite,
    Relax,
    Strict,
}

#[derive(Clone, Copy)]
struct Workload {
    name: &'static str,
    jobs: usize,
    conflicts: &'static [ConflictFlag],
    job_execution_time_ms: u64,
}

const NONE: &[ConflictFlag] = &[];
const READ_READ_RELAX: &[ConflictFlag] = &[ConflictFlag::ReadRead, ConflictFlag::Relax];
const READ_WRITE_RELAX: &[ConflictFlag] = &[ConflictFlag::ReadWrite, ConflictFlag::Relax];
const READ_WRITE_STRICT: &[ConflictFlag] = &[ConflictFlag::ReadWrite, ConflictFlag::Strict];
const WRITE_WRITE_RELAX: &[ConflictFlag] = &[ConflictFlag::WriteWrite, ConflictFlag::Relax];
const WRITE_WRITE_STRICT: &[ConflictFlag] = &[ConflictFlag::WriteWrite, ConflictFlag::Strict];
const MIXED_RELAX: &[ConflictFlag] = &[
    ConflictFlag::ReadRead,
    ConflictFlag::ReadWrite,
    ConflictFlag::WriteWrite,
    ConflictFlag::Relax,
];
const MIXED_STRICT: &[ConflictFlag] = &[
    ConflictFlag::ReadRead,
    ConflictFlag::ReadWrite,
    ConflictFlag::WriteWrite,
    ConflictFlag::Strict,
];
const MIXED_HYBRID: &[ConflictFlag] = &[
    ConflictFlag::ReadRead,
    ConflictFlag::ReadWrite,
    ConflictFlag::WriteWrite,
    ConflictFlag::Relax,
    ConflictFlag::Strict,
];

// Representative sample rather than a full cross-product. The matrix keeps the
// benchmark suite broad enough to compare implementations while keeping a full
// `cargo bench --bench experiment` run comfortably iterative.
const RUN_WORKLOADS: &[Workload] = &[
    Workload {
        name: "jobs10_none_ms8",
        jobs: 10,
        conflicts: NONE,
        job_execution_time_ms: 8,
    },
    Workload {
        name: "jobs10_write_write_strict_ms9",
        jobs: 10,
        conflicts: WRITE_WRITE_STRICT,
        job_execution_time_ms: 9,
    },
    Workload {
        name: "jobs10_mixed_hybrid_ms10",
        jobs: 10,
        conflicts: MIXED_HYBRID,
        job_execution_time_ms: 10,
    },
    Workload {
        name: "jobs100_read_read_relax_ms4",
        jobs: 100,
        conflicts: READ_READ_RELAX,
        job_execution_time_ms: 4,
    },
    Workload {
        name: "jobs100_read_write_strict_ms5",
        jobs: 100,
        conflicts: READ_WRITE_STRICT,
        job_execution_time_ms: 5,
    },
    Workload {
        name: "jobs100_write_write_relax_ms6",
        jobs: 100,
        conflicts: WRITE_WRITE_RELAX,
        job_execution_time_ms: 6,
    },
    Workload {
        name: "jobs100_mixed_relax_ms7",
        jobs: 100,
        conflicts: MIXED_RELAX,
        job_execution_time_ms: 7,
    },
    Workload {
        name: "jobs1000_none_ms1",
        jobs: 1000,
        conflicts: NONE,
        job_execution_time_ms: 1,
    },
    Workload {
        name: "jobs1000_read_write_relax_ms2",
        jobs: 1000,
        conflicts: READ_WRITE_RELAX,
        job_execution_time_ms: 2,
    },
    Workload {
        name: "jobs1000_mixed_strict_ms3",
        jobs: 1000,
        conflicts: MIXED_STRICT,
        job_execution_time_ms: 3,
    },
];

const COMPILE_WORKLOADS: &[Workload] = &[
    Workload {
        name: "compile_jobs10_none",
        jobs: 10,
        conflicts: NONE,
        job_execution_time_ms: 1,
    },
    Workload {
        name: "compile_jobs10_read_write_relax",
        jobs: 10,
        conflicts: READ_WRITE_RELAX,
        job_execution_time_ms: 1,
    },
    Workload {
        name: "compile_jobs10_write_write_strict",
        jobs: 10,
        conflicts: WRITE_WRITE_STRICT,
        job_execution_time_ms: 1,
    },
    Workload {
        name: "compile_jobs100_mixed_hybrid",
        jobs: 100,
        conflicts: MIXED_HYBRID,
        job_execution_time_ms: 1,
    },
    Workload {
        name: "compile_jobs1000_read_read_relax",
        jobs: 1000,
        conflicts: READ_READ_RELAX,
        job_execution_time_ms: 1,
    },
    Workload {
        name: "compile_jobs1000_mixed_strict",
        jobs: 1000,
        conflicts: MIXED_STRICT,
        job_execution_time_ms: 1,
    },
];

struct BenchState {
    counters: Box<[AtomicU64]>,
    checksum: AtomicU64,
}

impl BenchState {
    fn new(slots: usize) -> Self {
        Self {
            counters: (0..slots.max(1))
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            checksum: AtomicU64::new(0),
        }
    }
}

fn make_job(workload: Workload, index: usize) -> Job<BenchState> {
    let duration = Duration::from_millis(workload.job_execution_time_ms);
    let job = Job::new(move |state: &BenchState| {
        let mut value = (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xa5a5_a5a5_a5a5_a5a5;
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            value = value.rotate_left(7).wrapping_mul(0x94d0_49bb_1331_11eb);
            std::hint::spin_loop();
        }

        let slot = index % state.counters.len();
        state.counters[slot].fetch_add(value, AtomicRelaxed);
        state.checksum.fetch_add(value ^ slot as u64, AtomicRelaxed);
        Ok(())
    });

    apply_conflicts(job, workload, index)
}

fn apply_conflicts(mut job: Job<BenchState>, workload: Workload, index: usize) -> Job<BenchState> {
    let stripes = stripe_count(workload.jobs);
    let relax = has_flag(workload.conflicts, ConflictFlag::Relax);
    let strict = has_flag(workload.conflicts, ConflictFlag::Strict);

    if has_flag(workload.conflicts, ConflictFlag::ReadRead) {
        let key = Identifier(index % stripes);
        job = job.depend([Read(key, select_order(relax, strict, AccessFamily::ReadRead, index))]);
    }

    if has_flag(workload.conflicts, ConflictFlag::ReadWrite) {
        let key = Identifier(stripes + index % stripes);
        let order = select_order(relax, strict, AccessFamily::ReadWrite, index);
        job = if index % 2 == 0 {
            job.depend([Read(key, order)])
        } else {
            job.depend([Write(key, order)])
        };
    }

    if has_flag(workload.conflicts, ConflictFlag::WriteWrite) {
        let key = Identifier((2 * stripes) + index % stripes);
        let order = select_order(relax, strict, AccessFamily::WriteWrite, index);
        job = job.depend([Write(key, order)]);
    }

    job
}

#[derive(Clone, Copy)]
enum AccessFamily {
    ReadRead,
    ReadWrite,
    WriteWrite,
}

fn select_order(relax: bool, strict: bool, family: AccessFamily, index: usize) -> elude::depend::Order {
    match (relax, strict) {
        (true, false) => Relax,
        (false, true) => Strict,
        (true, true) => match family {
            AccessFamily::ReadWrite => Relax,
            AccessFamily::WriteWrite => Strict,
            AccessFamily::ReadRead => {
                if index % 2 == 0 {
                    Relax
                } else {
                    Strict
                }
            }
        },
        (false, false) => Relax,
    }
}

fn has_flag(flags: &[ConflictFlag], needle: ConflictFlag) -> bool {
    flags.iter().any(|flag| *flag == needle)
}

fn stripe_count(jobs: usize) -> usize {
    match jobs {
        0..=16 => 1,
        17..=128 => 4,
        _ => 16,
    }
}

fn benchmark_parallelism() -> Option<NonZeroUsize> {
    thread::available_parallelism().ok()
}

fn build_scheduler<T>(workload: Workload) -> T
where
    T: elude::experiment::Scheduler<BenchState>,
{
    (0..workload.jobs).fold(
        T::with_parallelism(benchmark_parallelism()),
        |scheduler, index| scheduler.add(make_job(workload, index)),
    )
}

fn configure_compile_group(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.measurement_time(Duration::from_secs(1));
    group.warm_up_time(Duration::from_millis(200));
}

fn configure_run_group(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.measurement_time(Duration::from_secs(2));
    group.warm_up_time(Duration::from_millis(250));
}

fn bench_compile<T>(c: &mut Criterion, workloads: &[Workload])
where
    T: elude::experiment::Scheduler<BenchState>,
{
    let mut group = c.benchmark_group(format!("experiment/compile/{}", T::NAME));
    configure_compile_group(&mut group);
    for workload in workloads.iter().copied() {
        group.throughput(Throughput::Elements(workload.jobs as u64));
        group.bench_with_input(BenchmarkId::from_parameter(workload.name), &workload, |b, workload| {
            b.iter(|| black_box(build_scheduler::<T>(*workload).schedule().unwrap()))
        });
    }
    group.finish();
}

fn bench_run<T>(c: &mut Criterion, workloads: &[Workload])
where
    T: elude::experiment::Scheduler<BenchState>,
{
    let mut group = c.benchmark_group(format!("experiment/run/{}", T::NAME));
    configure_run_group(&mut group);
    for workload in workloads.iter().copied() {
        group.throughput(Throughput::Elements(workload.jobs as u64));
        group.bench_with_input(BenchmarkId::from_parameter(workload.name), &workload, |b, workload| {
            let mut compiled = build_scheduler::<T>(*workload).schedule().unwrap();
            let state = BenchState::new(stripe_count(workload.jobs) * 3);
            b.iter(|| {
                compiled.run(&state).unwrap();
                black_box(state.checksum.load(AtomicRelaxed))
            })
        });
    }
    group.finish();
}

fn bench_experiment_01(c: &mut Criterion) {
    bench_compile::<experiment_01::Scheduler<BenchState>>(c, COMPILE_WORKLOADS);
    bench_run::<experiment_01::Scheduler<BenchState>>(c, RUN_WORKLOADS);
}

fn bench_experiment_02(c: &mut Criterion) {
    bench_compile::<experiment_02::Scheduler<BenchState>>(c, COMPILE_WORKLOADS);
    bench_run::<experiment_02::Scheduler<BenchState>>(c, RUN_WORKLOADS);
}

fn bench_experiment_03(c: &mut Criterion) {
    bench_compile::<experiment_03::Scheduler<BenchState>>(c, COMPILE_WORKLOADS);
    bench_run::<experiment_03::Scheduler<BenchState>>(c, RUN_WORKLOADS);
}

criterion_group!(
    experiment_benches,
    bench_experiment_01,
    bench_experiment_02,
    bench_experiment_03
);
criterion_main!(experiment_benches);
