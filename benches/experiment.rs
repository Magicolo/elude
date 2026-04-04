use criterion::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput,
};
use elude::{
    depend::{
        Dependency,
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
    time::Duration,
};

#[derive(Clone, Copy, Debug)]
struct WorkProfile {
    iterations: u32,
}

impl WorkProfile {
    const ZERO: Self = Self { iterations: 0 };

    const fn busy(iterations: u32) -> Self {
        Self { iterations }
    }

    const fn iterations(self) -> u32 {
        self.iterations
    }

    fn label(self) -> String {
        match self.iterations {
            0 => "zero".to_string(),
            iterations => format!("iter{iterations}"),
        }
    }
}

#[derive(Clone, Debug)]
struct JobSpec {
    dependencies: Box<[Dependency]>,
    predecessors: Box<[usize]>,
    work: WorkProfile,
    weight_hint: u32,
}

impl JobSpec {
    fn new(
        work: WorkProfile,
        dependencies: impl IntoIterator<Item = Dependency>,
    ) -> Self {
        Self {
            dependencies: dependencies.into_iter().collect::<Vec<_>>().into_boxed_slice(),
            predecessors: Vec::new().into_boxed_slice(),
            weight_hint: work.iterations(),
            work,
        }
    }

    fn with_predecessors(mut self, predecessors: impl IntoIterator<Item = usize>) -> Self {
        self.predecessors = predecessors
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self
    }
}

#[derive(Clone, Debug)]
struct Workload {
    family: &'static str,
    name: String,
    jobs: Box<[JobSpec]>,
    state_slots: usize,
}

impl Workload {
    fn new(family: &'static str, name: String, jobs: Vec<JobSpec>) -> Self {
        let state_slots = jobs.len().max(1);
        Self {
            family,
            name,
            jobs: jobs.into_boxed_slice(),
            state_slots,
        }
    }

    fn job_count(&self) -> usize {
        self.jobs.len()
    }
}

struct BenchState {
    slots: Box<[AtomicU64]>,
}

impl BenchState {
    fn new(workload: &Workload) -> Self {
        Self {
            slots: (0..workload.state_slots.max(1))
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }

    fn record(&self, job_index: usize, value: u64) {
        self.slots[job_index % self.slots.len()].fetch_add(value | 1, AtomicRelaxed);
    }

    fn sample(&self) -> u64 {
        let first = self.slots[0].load(AtomicRelaxed);
        let last = self.slots[self.slots.len() - 1].load(AtomicRelaxed);
        first ^ last.rotate_left(11)
    }
}

fn execute_work(work: WorkProfile, state: &BenchState, job_index: usize) {
    let mut value =
        (job_index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xa5a5_a5a5_a5a5_a5a5;

    for step in 0..work.iterations() {
        value = value
            .rotate_left(7)
            .wrapping_mul(0x94d0_49bb_1331_11eb)
            .wrapping_add(step as u64);
        std::hint::spin_loop();
    }

    state.record(job_index, value);
}

trait BenchAdapter {
    const NAME: &'static str;

    type State;
    type Compiled;

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
    ) -> anyhow::Result<Self::Compiled>;

    fn make_state(workload: &Workload) -> Self::State;

    fn run(compiled: &mut Self::Compiled, state: &Self::State) -> anyhow::Result<()>;

    fn sample(state: &Self::State) -> u64;
}

struct ExperimentAdapter<T>(std::marker::PhantomData<T>);

impl<T> BenchAdapter for ExperimentAdapter<T>
where
    T: elude::experiment::Scheduler<BenchState>,
{
    const NAME: &'static str = T::NAME;

    type State = BenchState;
    type Compiled = T::Schedule;

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
    ) -> anyhow::Result<Self::Compiled> {
        workload
            .jobs
            .iter()
            .enumerate()
            .fold(T::with_parallelism(parallelism), |scheduler, (index, spec)| {
                scheduler.add(make_experiment_job(index, spec))
            })
            .schedule()
    }

    fn make_state(workload: &Workload) -> Self::State {
        BenchState::new(workload)
    }

    fn run(compiled: &mut Self::Compiled, state: &Self::State) -> anyhow::Result<()> {
        compiled.run(state)
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

fn make_experiment_job(index: usize, spec: &JobSpec) -> Job<BenchState> {
    let _ = &spec.predecessors;
    debug_assert_eq!(spec.weight_hint, spec.work.iterations());
    let work = spec.work;

    Job::new(move |state: &BenchState| {
        execute_work(work, state, index);
        Ok(())
    })
    .depend(spec.dependencies.iter().cloned())
}

fn write_key(key: usize, strict: bool) -> Dependency {
    Write(Identifier(key), if strict { Strict } else { Relax })
}

fn read_key(key: usize, strict: bool) -> Dependency {
    Read(Identifier(key), if strict { Strict } else { Relax })
}

fn wide_independent(job_count: usize, work: WorkProfile) -> Workload {
    Workload::new(
        "wide_independent",
        format!("jobs{job_count}_{}", work.label()),
        (0..job_count).map(|_| JobSpec::new(work, [])).collect(),
    )
}

fn strict_chain(job_count: usize, work: WorkProfile) -> Workload {
    Workload::new(
        "strict_chain",
        format!("jobs{job_count}_{}", work.label()),
        (0..job_count)
            .map(|index| {
                let predecessors = index.checked_sub(1).into_iter().collect::<Vec<_>>();
                JobSpec::new(work, [write_key(0, true)]).with_predecessors(predecessors)
            })
            .collect(),
    )
}

fn hot_key_write_contention(job_count: usize, work: WorkProfile) -> Workload {
    Workload::new(
        "hot_key_write_contention",
        format!("jobs{job_count}_{}", work.label()),
        (0..job_count)
            .map(|index| {
                let predecessors = index.checked_sub(1).into_iter().collect::<Vec<_>>();
                JobSpec::new(work, [write_key(0, false)]).with_predecessors(predecessors)
            })
            .collect(),
    )
}

fn read_heavy_shared_key(job_count: usize, work: WorkProfile) -> Workload {
    Workload::new(
        "read_heavy_shared_key",
        format!("jobs{job_count}_{}", work.label()),
        (0..job_count)
            .map(|index| JobSpec::new(work, [read_key(0, index % 2 != 0)]))
            .collect(),
    )
}

fn layer_barrier_stress(
    lanes: usize,
    fast_iterations: u32,
    slow_iterations: u32,
    follower_iterations: u32,
) -> Workload {
    let mut jobs = Vec::with_capacity(lanes * 2);

    for lane in 0..lanes {
        let work = if lane % 2 == 0 {
            WorkProfile::busy(fast_iterations)
        } else {
            WorkProfile::busy(slow_iterations)
        };
        jobs.push(JobSpec::new(work, [write_key(lane, true)]));
    }

    for lane in 0..lanes {
        jobs.push(
            JobSpec::new(WorkProfile::busy(follower_iterations), [write_key(lane, true)])
                .with_predecessors([lane]),
        );
    }

    Workload::new(
        "layer_barrier_stress",
        format!(
            "lanes{lanes}_fast{fast_iterations}_slow{slow_iterations}_follow{follower_iterations}"
        ),
        jobs,
    )
}

fn straggler_partial_overlap(
    lanes: usize,
    fast_iterations: u32,
    straggler_iterations: u32,
    follower_iterations: u32,
) -> Workload {
    let mut jobs = Vec::with_capacity(lanes * 2);

    for lane in 0..lanes {
        let work = if lane == 0 {
            WorkProfile::busy(straggler_iterations)
        } else {
            WorkProfile::busy(fast_iterations)
        };
        jobs.push(JobSpec::new(work, [write_key(lane, true)]));
    }

    for lane in 0..lanes {
        jobs.push(
            JobSpec::new(WorkProfile::busy(follower_iterations), [write_key(lane, true)])
                .with_predecessors([lane]),
        );
    }

    Workload::new(
        "straggler_partial_overlap",
        format!(
            "lanes{lanes}_fast{fast_iterations}_straggler{straggler_iterations}_follow{follower_iterations}"
        ),
        jobs,
    )
}

fn fan_out_fan_in(
    fan_out: usize,
    root_iterations: u32,
    leaf_iterations: u32,
    sink_iterations: u32,
) -> Workload {
    let mut jobs = Vec::with_capacity(fan_out + 2);

    jobs.push(JobSpec::new(
        WorkProfile::busy(root_iterations),
        [write_key(0, true)],
    ));

    for _ in 0..fan_out {
        jobs.push(
            JobSpec::new(WorkProfile::busy(leaf_iterations), [read_key(0, true)])
                .with_predecessors([0]),
        );
    }

    jobs.push(
        JobSpec::new(WorkProfile::busy(sink_iterations), [write_key(0, true)])
            .with_predecessors(1..=fan_out),
    );

    Workload::new(
        "fan_out_fan_in",
        format!("fanout{fan_out}_root{root_iterations}_leaf{leaf_iterations}_sink{sink_iterations}"),
        jobs,
    )
}

fn mixed_hotspots(blocks: usize) -> Workload {
    let mut jobs = Vec::with_capacity(blocks * 6);
    let lane_base = 10_000usize;

    for block in 0..blocks {
        let lane = lane_base + block;
        let hot = 1 + (block % 4);

        jobs.push(JobSpec::new(WorkProfile::busy(400), [read_key(0, false)]));
        jobs.push(JobSpec::new(WorkProfile::busy(1_100), [write_key(0, false)]));
        jobs.push(JobSpec::new(WorkProfile::busy(2_200), [write_key(hot, true)]));
        jobs.push(JobSpec::new(WorkProfile::busy(500), [read_key(hot, false)]));
        jobs.push(JobSpec::new(WorkProfile::busy(900), [write_key(lane, false)]));
        jobs.push(JobSpec::new(
            WorkProfile::busy(700),
            [read_key(lane, false), write_key(100 + (block % 3), true)],
        ));
    }

    Workload::new("mixed_hotspots", format!("blocks{blocks}"), jobs)
}

fn compile_heavy_sparse_keys(job_count: usize) -> Workload {
    let mut jobs = Vec::with_capacity(job_count);
    let unique_base = job_count * 10;

    for index in 0..job_count {
        let bucket_a = 50_000 + (index % 97);
        let bucket_b = 60_000 + ((index * 11 + 7) % 193);
        let bucket_c = 70_000 + ((index * 17 + 13) % 53);

        jobs.push(JobSpec::new(
            WorkProfile::ZERO,
            [
                read_key(unique_base + (index * 2), index % 5 == 0),
                read_key(unique_base + (index * 2) + 1, false),
                write_key(bucket_a, index % 7 == 0),
                if index % 3 == 0 {
                    write_key(bucket_b, false)
                } else {
                    read_key(bucket_c, index % 11 == 0)
                },
            ],
        ));
    }

    Workload::new(
        "compile_heavy_sparse_keys",
        format!("jobs{job_count}_multi_key_sparse_zero"),
        jobs,
    )
}

fn compile_workloads() -> Vec<Workload> {
    vec![
        wide_independent(1_024, WorkProfile::ZERO),
        hot_key_write_contention(768, WorkProfile::ZERO),
        layer_barrier_stress(256, 0, 0, 0),
        fan_out_fan_in(511, 0, 0, 0),
        mixed_hotspots(96),
        compile_heavy_sparse_keys(1_536),
    ]
}

fn overhead_workloads() -> Vec<Workload> {
    vec![
        wide_independent(512, WorkProfile::ZERO),
        read_heavy_shared_key(512, WorkProfile::ZERO),
        strict_chain(256, WorkProfile::ZERO),
        hot_key_write_contention(256, WorkProfile::ZERO),
    ]
}

fn parallelism_workloads() -> Vec<Workload> {
    vec![
        wide_independent(512, WorkProfile::busy(4_000)),
        read_heavy_shared_key(512, WorkProfile::busy(4_000)),
        strict_chain(256, WorkProfile::busy(2_000)),
        hot_key_write_contention(256, WorkProfile::busy(2_000)),
        layer_barrier_stress(128, 400, 5_000, 2_000),
        straggler_partial_overlap(128, 400, 25_000, 1_800),
        fan_out_fan_in(255, 6_000, 1_000, 6_000),
        mixed_hotspots(48),
    ]
}

fn benchmark_parallelism() -> Option<NonZeroUsize> {
    thread::available_parallelism().ok()
}

fn configure_compile_group(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
) {
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.measurement_time(Duration::from_secs(1));
    group.warm_up_time(Duration::from_millis(200));
}

fn configure_overhead_group(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
) {
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.measurement_time(Duration::from_secs(2));
    group.warm_up_time(Duration::from_millis(250));
}

fn configure_parallelism_group(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
) {
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_millis(300));
}

fn bench_compile<A>(c: &mut Criterion, workloads: &[Workload])
where
    A: BenchAdapter,
{
    let mut group = c.benchmark_group(format!("experiment/compile/{}", A::NAME));
    configure_compile_group(&mut group);

    for workload in workloads.iter() {
        group.throughput(Throughput::Elements(workload.job_count() as u64));
        group.bench_with_input(
            BenchmarkId::new(workload.family, workload.name.as_str()),
            workload,
            |b, workload| {
                b.iter(|| {
                    black_box(A::compile(workload, benchmark_parallelism()).unwrap())
                })
            },
        );
    }

    group.finish();
}

fn bench_run_suite<A>(
    c: &mut Criterion,
    suite: &str,
    workloads: &[Workload],
    configure: fn(&mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>),
) where
    A: BenchAdapter,
{
    let mut group = c.benchmark_group(format!("experiment/{suite}/{}", A::NAME));
    configure(&mut group);

    for workload in workloads.iter() {
        group.throughput(Throughput::Elements(workload.job_count() as u64));
        group.bench_with_input(
            BenchmarkId::new(workload.family, workload.name.as_str()),
            workload,
            |b, workload| {
                let mut compiled = A::compile(workload, benchmark_parallelism()).unwrap();
                let state = A::make_state(workload);
                b.iter(|| {
                    A::run(&mut compiled, &state).unwrap();
                    black_box(A::sample(&state));
                })
            },
        );
    }

    group.finish();
}

fn bench_adapter<A>(c: &mut Criterion)
where
    A: BenchAdapter,
{
    let compile = compile_workloads();
    let overhead = overhead_workloads();
    let parallelism = parallelism_workloads();

    bench_compile::<A>(c, &compile);
    bench_run_suite::<A>(c, "run_overhead", &overhead, configure_overhead_group);
    bench_run_suite::<A>(
        c,
        "run_parallelism",
        &parallelism,
        configure_parallelism_group,
    );
}

fn bench_experiment_01(c: &mut Criterion) {
    bench_adapter::<ExperimentAdapter<experiment_01::Scheduler<BenchState>>>(c);
}

fn bench_experiment_02(c: &mut Criterion) {
    bench_adapter::<ExperimentAdapter<experiment_02::Scheduler<BenchState>>>(c);
}

fn bench_experiment_03(c: &mut Criterion) {
    bench_adapter::<ExperimentAdapter<experiment_03::Scheduler<BenchState>>>(c);
}

criterion_group!(
    experiment_benches,
    bench_experiment_01,
    bench_experiment_02,
    bench_experiment_03
);
criterion_main!(experiment_benches);
