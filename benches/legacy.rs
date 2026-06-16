use bevy_ecs::{
    change_detection::{CheckChangeTicks, Tick},
    component::{
        ComponentCloneBehavior as BevyComponentCloneBehavior,
        ComponentDescriptor as BevyComponentDescriptor, StorageType as BevyStorageType,
    },
    query::FilteredAccessSet as BevyFilteredAccessSet,
    schedule::{IntoScheduleConfigs as _, Schedule as BevySchedule, SystemSet as BevySystemSet},
    system::{
        BoxedSystem as BevyBoxedSystem, RunSystemError as BevyRunSystemError, System as BevySystem,
        SystemParamValidationError as BevySystemParamValidationError,
        SystemStateFlags as BevySystemStateFlags,
    },
    world::{
        DeferredWorld as BevyDeferredWorld, World as BevyWorld,
        unsafe_world_cell::UnsafeWorldCell as BevyUnsafeWorldCell,
    },
};
use bevy_tasks::{ComputeTaskPool, TaskPool, TaskPoolBuilder as BevyTaskPoolBuilder};
use bevy_utils::prelude::DebugName as BevyDebugName;
use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, black_box, criterion_group, criterion_main,
};
use dag_exec::{
    DagBuilder as ExplicitDagBuilder, Executor as DagExecutor, ExecutorConfig as DagExecutorConfig,
};
use dagga::{Dag as DaggaGraph, Node as DaggaNode, Schedule as DaggaSchedule};
use elude::legacy::{
    depend::{
        Dependency,
        Dependency::{Read, Write},
        Key,
        Key::Identifier,
        Order,
        Order::{Relax, Strict},
    },
    experiment::{CompiledSchedule as _, Job, Scheduler as _},
    experiment_01, experiment_02, experiment_03, experiment_04, experiment_05,
};
use flecs_ecs::core::{
    Entity as FlecsEntity, IdOperations as _, QueryBuilderImpl as _, SystemAPI as _,
    TermBuilderImpl as _, World as FlecsWorld,
};
use legion::{
    World as LegionWorld,
    storage::ComponentTypeId as LegionComponentTypeId,
    systems::{
        CommandBuffer as LegionCommandBuffer, Executor as LegionExecutor,
        ParallelRunnable as LegionParallelRunnable, ResourceTypeId as LegionResourceTypeId,
        Resources as LegionResources, Runnable as LegionRunnable,
        UnsafeResources as LegionUnsafeResources,
    },
    world::{ArchetypeAccess as LegionArchetypeAccess, WorldId as LegionWorldId},
};
use rayon::{ThreadPool, ThreadPoolBuilder, prelude::*};
use seq_macro::seq;
use shipyard::{
    Workload as ShipyardWorkload, World as ShipyardWorld,
    advanced::StorageId as ShipyardStorageId,
    borrow::Mutability as ShipyardMutability,
    error::Run as ShipyardRunError,
    scheduler::{WorkloadSystem as ShipyardWorkloadSystem, info::TypeInfo as ShipyardTypeInfo},
};
use std::{
    alloc::Layout,
    any::TypeId,
    collections::{BTreeMap, HashMap},
    convert::Infallible,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed as AtomicRelaxed},
    },
    thread,
    time::Duration,
};

const SHIPYARD_MAX_BENCH_JOBS: usize = 4096;
const LEGION_MAX_BENCH_RESOURCE_IDS: usize = 6144;
const MAINLINE_RANDOM_SEED: u64 = 0x5eed_cafe_d00d_beef;
const MAINLINE_RANDOM_RESOURCE_COUNT: usize = 384;
const MAINLINE_RANDOM_JOB_COUNT: usize = 2_048;
const MAINLINE_RANDOM_LAYER_COUNT: usize = 32;
const MAINLINE_RANDOM_HOT_RESOURCE_COUNT: usize = 24;
const MAINLINE_RANDOM_WARM_RESOURCE_COUNT: usize = 96;
const MAINLINE_RANDOM_SHARD_COUNT: usize = 24;
const MAINLINE_RANDOM_LOCAL_RESOURCE_COUNT: usize = (MAINLINE_RANDOM_RESOURCE_COUNT
    - MAINLINE_RANDOM_HOT_RESOURCE_COUNT
    - MAINLINE_RANDOM_WARM_RESOURCE_COUNT)
    / MAINLINE_RANDOM_SHARD_COUNT;
const MAINLINE_RANDOM_ITERATIONS_PER_MICROSECOND: u32 = 30;

const _: () = {
    assert!(
        MAINLINE_RANDOM_HOT_RESOURCE_COUNT
            + MAINLINE_RANDOM_WARM_RESOURCE_COUNT
            + (MAINLINE_RANDOM_SHARD_COUNT * MAINLINE_RANDOM_LOCAL_RESOURCE_COUNT)
            == MAINLINE_RANDOM_RESOURCE_COUNT
    );
    assert!(MAINLINE_RANDOM_RESOURCE_COUNT <= LEGION_MAX_BENCH_RESOURCE_IDS);
    assert!(MAINLINE_RANDOM_JOB_COUNT <= SHIPYARD_MAX_BENCH_JOBS);
};

#[derive(Clone, Debug)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn index(&mut self, upper: usize) -> usize {
        debug_assert!(upper > 0);
        (self.next_u64() % (upper as u64)) as usize
    }

    fn chance(&mut self, numerator: u32, denominator: u32) -> bool {
        debug_assert!(denominator > 0);
        debug_assert!(numerator <= denominator);
        (self.next_u64() % (denominator as u64)) < (numerator as u64)
    }

    fn weighted_index(&mut self, weights: &[u32]) -> usize {
        let total = weights.iter().copied().sum::<u32>();
        debug_assert!(total > 0);

        let mut ticket = (self.next_u64() % (total as u64)) as u32;
        for (index, weight) in weights.iter().copied().enumerate() {
            if ticket < weight {
                return index;
            }
            ticket -= weight;
        }

        weights.len() - 1
    }
}

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
    fn new(work: WorkProfile, dependencies: impl IntoIterator<Item = Dependency>) -> Self {
        Self {
            dependencies: dependencies
                .into_iter()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
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

type SharedBenchState = Arc<BenchState>;

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

fn compute_work_value(work: WorkProfile, job_index: usize, seed: u64) -> u64 {
    let mut value =
        seed ^ (job_index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xa5a5_a5a5_a5a5_a5a5;

    for step in 0..work.iterations() {
        value = value
            .rotate_left(7)
            .wrapping_mul(0x94d0_49bb_1331_11eb)
            .wrapping_add(step as u64);
        std::hint::spin_loop();
    }

    value
}

fn execute_work(work: WorkProfile, state: &BenchState, job_index: usize, seed: u64) -> u64 {
    let value = compute_work_value(work, job_index, seed);
    state.record(job_index, value);
    value
}

trait BenchAdapter {
    const NAME: &'static str;

    type State;
    type Compiled;

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        state: &Self::State,
    ) -> anyhow::Result<Self::Compiled>;

    fn make_state(workload: &Workload) -> Self::State;

    fn run(compiled: &mut Self::Compiled, state: &Self::State) -> anyhow::Result<()>;

    fn sample(state: &Self::State) -> u64;
}

struct ExperimentAdapter<T>(std::marker::PhantomData<T>);

impl<T> BenchAdapter for ExperimentAdapter<T>
where
    T: elude::legacy::experiment::Scheduler<BenchState>,
{
    type Compiled = T::Schedule;
    type State = SharedBenchState;

    const NAME: &'static str = T::NAME;

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        _state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        workload
            .jobs
            .iter()
            .enumerate()
            .fold(
                T::with_parallelism(parallelism),
                |scheduler, (index, spec)| scheduler.add(make_experiment_job(index, spec)),
            )
            .schedule()
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, state: &Self::State) -> anyhow::Result<()> {
        compiled.run(state.as_ref())
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

fn compile_experiment_04(
    workload: &Workload,
    parallelism: Option<NonZeroUsize>,
    policy: experiment_04::Policy,
) -> anyhow::Result<experiment_04::Compiled<BenchState>> {
    workload
        .jobs
        .iter()
        .enumerate()
        .fold(
            experiment_04::Scheduler::with_parallelism_and_policy(parallelism, policy),
            |scheduler, (index, spec)| scheduler.add(make_experiment_job(index, spec)),
        )
        .schedule()
}

struct Experiment04AdaptiveAdapter;

impl BenchAdapter for Experiment04AdaptiveAdapter {
    type Compiled = experiment_04::Compiled<BenchState>;
    type State = SharedBenchState;

    const NAME: &'static str = "experiment_04";

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        _state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        compile_experiment_04(workload, parallelism, experiment_04::Policy::Adaptive)
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, state: &Self::State) -> anyhow::Result<()> {
        compiled.run(state.as_ref())
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

struct Experiment04CriticalPathAdapter;

impl BenchAdapter for Experiment04CriticalPathAdapter {
    type Compiled = experiment_04::Compiled<BenchState>;
    type State = SharedBenchState;

    const NAME: &'static str = "experiment_04_critical_path";

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        _state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        compile_experiment_04(
            workload,
            parallelism,
            experiment_04::Policy::Fixed(experiment_04::VariantKind::CriticalPath),
        )
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, state: &Self::State) -> anyhow::Result<()> {
        compiled.run(state.as_ref())
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

struct Experiment04ReadHeavyAdapter;

impl BenchAdapter for Experiment04ReadHeavyAdapter {
    type Compiled = experiment_04::Compiled<BenchState>;
    type State = SharedBenchState;

    const NAME: &'static str = "experiment_04_read_heavy";

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        _state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        compile_experiment_04(
            workload,
            parallelism,
            experiment_04::Policy::Fixed(experiment_04::VariantKind::ReadHeavy),
        )
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, state: &Self::State) -> anyhow::Result<()> {
        compiled.run(state.as_ref())
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

struct Experiment04HotContentionAdapter;

impl BenchAdapter for Experiment04HotContentionAdapter {
    type Compiled = experiment_04::Compiled<BenchState>;
    type State = SharedBenchState;

    const NAME: &'static str = "experiment_04_hot_contention";

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        _state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        compile_experiment_04(
            workload,
            parallelism,
            experiment_04::Policy::Fixed(experiment_04::VariantKind::HotContention),
        )
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, state: &Self::State) -> anyhow::Result<()> {
        compiled.run(state.as_ref())
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
        execute_work(work, state, index, 0);
        Ok(())
    })
    .depend(spec.dependencies.iter().cloned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AccessSpec {
    resource: u64,
    write: bool,
    order: Order,
}

#[derive(Clone, Debug)]
struct NormalizedJob {
    name: String,
    accesses: Box<[AccessSpec]>,
    predecessors: Box<[usize]>,
    work: WorkProfile,
    weight_hint: u32,
    unknown: bool,
}

#[derive(Clone, Debug)]
struct NormalizedWorkload {
    jobs: Box<[NormalizedJob]>,
    dagga_predecessors: Box<[Box<[usize]>]>,
    explicit_dag_predecessors: Box<[Box<[usize]>]>,
}

fn normalize_workload(workload: &Workload) -> NormalizedWorkload {
    let mut resource_ids = HashMap::<Key, u64>::new();
    let mut next_resource = 1u64;

    let jobs = workload
        .jobs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let mut accesses = BTreeMap::<u64, AccessSpec>::new();
            let mut unknown = false;

            for dependency in spec.dependencies.iter() {
                match dependency {
                    Dependency::Unknown => unknown = true,
                    Read(key, order) | Write(key, order) => {
                        let is_write = matches!(dependency, Write(_, _));
                        let resource = *resource_ids.entry(key.clone()).or_insert_with(|| {
                            let assigned = next_resource;
                            next_resource += 1;
                            assigned
                        });

                        accesses
                            .entry(resource)
                            .and_modify(|access| {
                                access.write |= is_write;
                                access.order = access.order.max(*order);
                            })
                            .or_insert(AccessSpec {
                                resource,
                                write: is_write,
                                order: *order,
                            });
                    }
                }
            }

            NormalizedJob {
                name: format!("job{index}"),
                accesses: accesses
                    .into_values()
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                predecessors: spec.predecessors.clone(),
                work: spec.work,
                weight_hint: spec.weight_hint,
                unknown,
            }
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();

    let dagga_predecessors = build_predecessors(&jobs, true);
    let explicit_dag_predecessors = build_predecessors(&jobs, false);

    NormalizedWorkload {
        jobs,
        dagga_predecessors,
        explicit_dag_predecessors,
    }
}

fn build_predecessors(
    jobs: &[NormalizedJob],
    preserve_relaxed_parallelism: bool,
) -> Box<[Box<[usize]>]> {
    let mut predecessors = jobs
        .iter()
        .map(|job| job.predecessors.to_vec())
        .collect::<Vec<_>>();

    for barrier in 0..jobs.len() {
        if !jobs[barrier].unknown {
            continue;
        }

        for predecessor in 0..barrier {
            predecessors[barrier].push(predecessor);
        }
        for follower_predecessors in predecessors.iter_mut().skip(barrier + 1) {
            follower_predecessors.push(barrier);
        }
    }

    for right in 0..jobs.len() {
        for left in 0..right {
            let needs_edge = if preserve_relaxed_parallelism {
                needs_strict_order(&jobs[left], &jobs[right])
            } else {
                jobs_conflict(&jobs[left], &jobs[right])
            };

            if needs_edge {
                predecessors[right].push(left);
            }
        }
    }

    predecessors
        .into_iter()
        .map(|mut job_predecessors| {
            job_predecessors.sort_unstable();
            job_predecessors.dedup();
            job_predecessors.into_boxed_slice()
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn needs_strict_order(left: &NormalizedJob, right: &NormalizedJob) -> bool {
    if left.unknown || right.unknown {
        return false;
    }

    let mut left_index = 0usize;
    let mut right_index = 0usize;

    while let (Some(left_access), Some(right_access)) = (
        left.accesses.get(left_index),
        right.accesses.get(right_index),
    ) {
        match left_access.resource.cmp(&right_access.resource) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => {
                if (left_access.write || right_access.write)
                    && left_access.order.max(right_access.order) == Strict
                {
                    return true;
                }
                left_index += 1;
                right_index += 1;
            }
        }
    }

    false
}

fn jobs_conflict(left: &NormalizedJob, right: &NormalizedJob) -> bool {
    if left.unknown || right.unknown {
        return true;
    }

    let mut left_index = 0usize;
    let mut right_index = 0usize;

    while let (Some(left_access), Some(right_access)) = (
        left.accesses.get(left_index),
        right.accesses.get(right_index),
    ) {
        match left_access.resource.cmp(&right_access.resource) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => {
                if left_access.write || right_access.write {
                    return true;
                }
                left_index += 1;
                right_index += 1;
            }
        }
    }

    false
}

struct ShipyardJobMarker<const N: usize>;

fn shipyard_job_type_id(index: usize) -> TypeId {
    if index >= SHIPYARD_MAX_BENCH_JOBS {
        panic!("shipyard adapter supports at most {SHIPYARD_MAX_BENCH_JOBS} jobs, got {index}");
    }

    seq!(N in 0..4096 {
        match index {
            #(N => TypeId::of::<ShipyardJobMarker<N>>(),)*
            _ => unreachable!("shipyard job index is bounds-checked above"),
        }
    })
}

struct LegionResourceMarker<const N: usize>;

fn legion_resource_type_id(index: usize) -> LegionResourceTypeId {
    if index >= LEGION_MAX_BENCH_RESOURCE_IDS {
        panic!(
            "legion adapter supports at most {LEGION_MAX_BENCH_RESOURCE_IDS} resource ids, got {index}"
        );
    }

    seq!(N in 0..6144 {
        match index {
            #(N => LegionResourceTypeId::of::<LegionResourceMarker<N>>(),)*
            _ => unreachable!("legion resource index is bounds-checked above"),
        }
    })
}

fn normalized_resource_count(normalized: &NormalizedWorkload) -> usize {
    normalized
        .jobs
        .iter()
        .flat_map(|job| job.accesses.iter().map(|access| access.resource as usize))
        .max()
        .unwrap_or(0)
}

fn init_bevy_task_pool(parallelism: Option<NonZeroUsize>) {
    let workers = worker_count(parallelism);
    let _ = ComputeTaskPool::get_or_init(|| {
        if workers <= 1 {
            TaskPool::new()
        } else {
            BevyTaskPoolBuilder::new().num_threads(workers).build()
        }
    });
}

fn bevy_resource_descriptor(resource: u64) -> BevyComponentDescriptor {
    // The benchmark only needs scheduler-visible resource identities.
    unsafe {
        BevyComponentDescriptor::new_with_layout(
            format!("bench_resource_{resource}"),
            BevyStorageType::Table,
            Layout::new::<()>(),
            None,
            true,
            BevyComponentCloneBehavior::Default,
            None,
        )
    }
}

fn worker_count(parallelism: Option<NonZeroUsize>) -> usize {
    parallelism
        .map(NonZeroUsize::get)
        .or_else(|| thread::available_parallelism().ok().map(NonZeroUsize::get))
        .unwrap_or(1)
        .max(1)
}

fn build_thread_pool(parallelism: Option<NonZeroUsize>) -> anyhow::Result<Option<ThreadPool>> {
    let workers = worker_count(parallelism);
    if workers <= 1 {
        Ok(None)
    } else {
        Ok(Some(ThreadPoolBuilder::new().num_threads(workers).build()?))
    }
}

#[derive(Clone, Copy, Debug)]
struct DaggaJob {
    index: usize,
    work: WorkProfile,
}

struct DaggaCompiled {
    schedule: DaggaSchedule<DaggaNode<DaggaJob, u64>>,
    pool: Option<ThreadPool>,
}

struct DaggaAdapter;

impl BenchAdapter for DaggaAdapter {
    type Compiled = DaggaCompiled;
    type State = SharedBenchState;

    const NAME: &'static str = "dagga";

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        _state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        let normalized = normalize_workload(workload);
        let mut dag = DaggaGraph::default();

        for (index, job) in normalized.jobs.iter().enumerate() {
            debug_assert_eq!(job.weight_hint, job.work.iterations());
            let mut node = DaggaNode::new(DaggaJob {
                index,
                work: job.work,
            })
            .with_name(job.name.clone());

            if !job.unknown {
                node = node.with_reads(
                    job.accesses
                        .iter()
                        .filter(|access| !access.write)
                        .map(|access| access.resource),
                );
                node = node.with_writes(
                    job.accesses
                        .iter()
                        .filter(|access| access.write)
                        .map(|access| access.resource),
                );
            }

            for &predecessor in normalized.dagga_predecessors[index].iter() {
                node = node.run_after(normalized.jobs[predecessor].name.clone());
            }

            dag.add_node(node);
        }

        let schedule = dag
            .build_schedule()
            .map_err(|error| anyhow::anyhow!("{error}"))?;

        Ok(DaggaCompiled {
            schedule,
            pool: build_thread_pool(parallelism)?,
        })
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, state: &Self::State) -> anyhow::Result<()> {
        for batch in compiled.schedule.batches.iter() {
            if let Some(pool) = &compiled.pool {
                if batch.len() > 1 {
                    pool.install(|| {
                        batch.par_iter().for_each(|node| {
                            let job = node.inner();
                            execute_work(job.work, state.as_ref(), job.index, 0);
                        });
                    });
                    continue;
                }
            }

            for node in batch.iter() {
                let job = node.inner();
                execute_work(job.work, state.as_ref(), job.index, 0);
            }
        }

        Ok(())
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

struct DagExecCompiled {
    dag: dag_exec::Dag<usize, u64, Infallible>,
    executor: DagExecutor,
    outputs: Box<[usize]>,
}

struct DagExecAdapter;

impl BenchAdapter for DagExecAdapter {
    type Compiled = DagExecCompiled;
    type State = SharedBenchState;

    const NAME: &'static str = "dag_exec";

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        let normalized = normalize_workload(workload);
        let mut builder = ExplicitDagBuilder::<usize, u64, Infallible>::new();

        for (index, job) in normalized.jobs.iter().enumerate() {
            debug_assert_eq!(job.weight_hint, job.work.iterations());
            let work = job.work;
            let state = Arc::clone(state);
            let predecessors = normalized.explicit_dag_predecessors[index].to_vec();

            builder
                .add_task(index, predecessors, move |inputs| {
                    let seed = inputs.iter().fold(index as u64, |acc, value| {
                        acc ^ (**value).rotate_left(((acc as u32) & 31) + 1)
                    });
                    let value = execute_work(work, state.as_ref(), index, seed);
                    Ok(value)
                })
                .map_err(|error| anyhow::anyhow!("{error}"))?;
        }

        let workers = worker_count(parallelism);
        let dag = builder
            .build()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let outputs = (0..normalized.jobs.len())
            .collect::<Vec<_>>()
            .into_boxed_slice();

        Ok(DagExecCompiled {
            dag,
            executor: DagExecutor::new(DagExecutorConfig {
                max_workers: workers,
                max_in_flight: workers.saturating_mul(2).max(1),
                worker_queue_cap: 2,
            }),
            outputs,
        })
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, state: &Self::State) -> anyhow::Result<()> {
        compiled
            .executor
            .run_parallel(&compiled.dag, compiled.outputs.iter().copied())
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        black_box(state.sample());
        Ok(())
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

struct ShipyardCompiled {
    workload: shipyard::scheduler::ScheduledWorkload,
    world: ShipyardWorld,
}

struct ShipyardAdapter;

impl BenchAdapter for ShipyardAdapter {
    type Compiled = ShipyardCompiled;
    type State = SharedBenchState;

    const NAME: &'static str = "shipyard";

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        let normalized = normalize_workload(workload);
        anyhow::ensure!(
            normalized.jobs.len() <= SHIPYARD_MAX_BENCH_JOBS,
            "shipyard adapter supports at most {SHIPYARD_MAX_BENCH_JOBS} jobs, got {}",
            normalized.jobs.len()
        );

        let mut workload_builder = ShipyardWorkload::new("benchmark");

        for (index, job) in normalized.jobs.iter().enumerate() {
            debug_assert_eq!(job.weight_hint, job.work.iterations());
            let state = Arc::clone(state);
            let work = job.work;
            let type_id = shipyard_job_type_id(index);
            let system = ShipyardWorkloadSystem {
                type_id,
                display_name: Box::new(job.name.clone()),
                system_fn: Box::new(move |_world| -> Result<(), ShipyardRunError> {
                    execute_work(work, state.as_ref(), index, 0);
                    Ok(())
                }),
                borrow_constraints: job
                    .accesses
                    .iter()
                    .map(|access| ShipyardTypeInfo {
                        name: format!("resource{}", access.resource).into(),
                        mutability: if access.write {
                            ShipyardMutability::Exclusive
                        } else {
                            ShipyardMutability::Shared
                        },
                        storage_id: ShipyardStorageId::Custom(access.resource),
                        thread_safe: true,
                    })
                    .collect(),
                tracking_to_enable: Vec::new(),
                generator: Box::new(move |_| type_id),
                run_if: None,
                tags: Vec::new(),
                before_all: Default::default(),
                after_all: Default::default(),
                after: normalized.dagga_predecessors[index].to_vec(),
                before: Vec::new(),
                unique_id: 0,
                require_in_workload: Default::default(),
                require_before: Default::default(),
                require_after: Default::default(),
            };

            workload_builder = workload_builder.with_system(system);
        }

        let world = match build_thread_pool(parallelism)? {
            Some(pool) => ShipyardWorld::builder()
                .with_local_thread_pool(pool)
                .build(),
            None => ShipyardWorld::builder().build(),
        };
        let (workload, _) = workload_builder
            .build()
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;

        Ok(ShipyardCompiled { workload, world })
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, _state: &Self::State) -> anyhow::Result<()> {
        compiled
            .workload
            .run_with_world(&compiled.world)
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        Ok(())
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

struct LegionRunnableJob {
    name: String,
    read_resources: Box<[LegionResourceTypeId]>,
    write_resources: Box<[LegionResourceTypeId]>,
    archetypes: LegionArchetypeAccess,
    work: WorkProfile,
    state: SharedBenchState,
    index: usize,
}

impl LegionRunnable for LegionRunnableJob {
    fn name(&self) -> Option<&legion::systems::SystemId> {
        let _ = &self.name;
        None
    }

    fn reads(&self) -> (&[LegionResourceTypeId], &[LegionComponentTypeId]) {
        static NO_COMPONENTS: [LegionComponentTypeId; 0] = [];
        (&self.read_resources, &NO_COMPONENTS)
    }

    fn writes(&self) -> (&[LegionResourceTypeId], &[LegionComponentTypeId]) {
        static NO_COMPONENTS: [LegionComponentTypeId; 0] = [];
        (&self.write_resources, &NO_COMPONENTS)
    }

    fn prepare(&mut self, _world: &LegionWorld) {}

    fn accesses_archetypes(&self) -> &LegionArchetypeAccess {
        &self.archetypes
    }

    unsafe fn run_unsafe(&mut self, _world: &LegionWorld, _resources: &LegionUnsafeResources) {
        execute_work(self.work, self.state.as_ref(), self.index, 0);
    }

    fn command_buffer_mut(&mut self, _world: LegionWorldId) -> Option<&mut LegionCommandBuffer> {
        None
    }
}

struct LegionCompiled {
    executor: LegionExecutor,
    world: LegionWorld,
    resources: LegionResources,
    pool: Option<ThreadPool>,
}

struct LegionPoolRunPtrs {
    executor: *mut LegionExecutor,
    world: *mut LegionWorld,
    resources: *mut LegionResources,
}

unsafe impl Send for LegionPoolRunPtrs {}

impl LegionPoolRunPtrs {
    unsafe fn execute(self) {
        (*self.executor).execute(&mut *self.world, &mut *self.resources);
    }
}

struct LegionAdapter;

impl BenchAdapter for LegionAdapter {
    type Compiled = LegionCompiled;
    type State = SharedBenchState;

    const NAME: &'static str = "legion";

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        let normalized = normalize_workload(workload);
        let resource_count = normalized_resource_count(&normalized);
        let order_token_base = resource_count + 1;
        let required_ids = order_token_base + normalized.jobs.len();
        anyhow::ensure!(
            required_ids <= LEGION_MAX_BENCH_RESOURCE_IDS,
            "legion adapter supports at most {LEGION_MAX_BENCH_RESOURCE_IDS} resource ids, needs {required_ids}"
        );

        let mut systems =
            Vec::<Box<dyn LegionParallelRunnable>>::with_capacity(normalized.jobs.len());

        for (index, job) in normalized.jobs.iter().enumerate() {
            debug_assert_eq!(job.weight_hint, job.work.iterations());
            let mut read_resources = job
                .accesses
                .iter()
                .filter(|access| !access.write)
                .map(|access| legion_resource_type_id(access.resource as usize))
                .collect::<Vec<_>>();
            let mut write_resources = job
                .accesses
                .iter()
                .filter(|access| access.write)
                .map(|access| legion_resource_type_id(access.resource as usize))
                .collect::<Vec<_>>();

            for &predecessor in normalized.dagga_predecessors[index].iter() {
                read_resources.push(legion_resource_type_id(order_token_base + predecessor));
            }

            write_resources.push(legion_resource_type_id(order_token_base + index));

            systems.push(Box::new(LegionRunnableJob {
                name: job.name.clone(),
                read_resources: read_resources.into_boxed_slice(),
                write_resources: write_resources.into_boxed_slice(),
                archetypes: LegionArchetypeAccess::Some(Default::default()),
                work: job.work,
                state: Arc::clone(state),
                index,
            }));
        }

        Ok(LegionCompiled {
            executor: LegionExecutor::new(systems),
            world: LegionWorld::default(),
            resources: LegionResources::default(),
            pool: build_thread_pool(parallelism)?,
        })
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, _state: &Self::State) -> anyhow::Result<()> {
        if let Some(pool) = &compiled.pool {
            let ptrs = LegionPoolRunPtrs {
                executor: &mut compiled.executor,
                world: &mut compiled.world,
                resources: &mut compiled.resources,
            };
            pool.install(move || unsafe {
                ptrs.execute();
            });
        } else {
            compiled
                .executor
                .execute(&mut compiled.world, &mut compiled.resources);
        }
        Ok(())
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct DynamicBevySet(usize);

impl BevySystemSet for DynamicBevySet {
    fn dyn_clone(&self) -> Box<dyn BevySystemSet> {
        Box::new(*self)
    }
}

struct BevyDynamicSystem {
    name: String,
    access: BevyFilteredAccessSet,
    work: WorkProfile,
    state: SharedBenchState,
    index: usize,
    last_run: Tick,
}

impl BevySystem for BevyDynamicSystem {
    type In = ();
    type Out = ();

    fn name(&self) -> BevyDebugName {
        self.name.clone().into()
    }

    fn flags(&self) -> BevySystemStateFlags {
        BevySystemStateFlags::empty()
    }

    unsafe fn run_unsafe(
        &mut self,
        _input: (),
        _world: BevyUnsafeWorldCell,
    ) -> Result<(), BevyRunSystemError> {
        execute_work(self.work, self.state.as_ref(), self.index, 0);
        Ok(())
    }

    fn apply_deferred(&mut self, _world: &mut BevyWorld) {}

    fn queue_deferred(&mut self, _world: BevyDeferredWorld) {}

    unsafe fn validate_param_unsafe(
        &mut self,
        _world: BevyUnsafeWorldCell,
    ) -> Result<(), BevySystemParamValidationError> {
        Ok(())
    }

    fn initialize(&mut self, _world: &mut BevyWorld) -> BevyFilteredAccessSet {
        self.access.clone()
    }

    fn check_change_tick(&mut self, check: CheckChangeTicks) {
        let _ = self.last_run.check_tick(check);
    }

    fn get_last_run(&self) -> Tick {
        self.last_run
    }

    fn set_last_run(&mut self, last_run: Tick) {
        self.last_run = last_run;
    }
}

struct BevyCompiled {
    schedule: BevySchedule,
    world: BevyWorld,
}

struct BevyAdapter;

impl BenchAdapter for BevyAdapter {
    type Compiled = BevyCompiled;
    type State = SharedBenchState;

    const NAME: &'static str = "bevy_ecs";

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        init_bevy_task_pool(parallelism);
        let normalized = normalize_workload(workload);
        let mut world = BevyWorld::new();
        let resource_count = normalized_resource_count(&normalized);
        let mut resource_ids = vec![None; resource_count + 1];

        for (resource, resource_id) in resource_ids
            .iter_mut()
            .enumerate()
            .take(resource_count + 1)
            .skip(1)
        {
            *resource_id = Some(
                world.register_resource_with_descriptor(bevy_resource_descriptor(resource as u64)),
            );
        }

        let mut schedule = BevySchedule::default();

        for (index, job) in normalized.jobs.iter().enumerate() {
            debug_assert_eq!(job.weight_hint, job.work.iterations());
            let mut access = BevyFilteredAccessSet::new();
            for entry in job.accesses.iter() {
                let component_id = resource_ids[entry.resource as usize]
                    .expect("every normalized bevy resource is registered");
                if entry.write {
                    access.add_unfiltered_resource_write(component_id);
                } else {
                    access.add_unfiltered_resource_read(component_id);
                }
            }

            let system: BevyBoxedSystem<(), ()> = Box::new(BevyDynamicSystem {
                name: job.name.clone(),
                access,
                work: job.work,
                state: Arc::clone(state),
                index,
                last_run: Tick::new(0),
            });

            let mut config = system.in_set(DynamicBevySet(index));
            for &predecessor in normalized.dagga_predecessors[index].iter() {
                config = config.after(DynamicBevySet(predecessor));
            }
            schedule.add_systems(config);
        }

        schedule
            .initialize(&mut world)
            .map_err(|error| anyhow::anyhow!("{error}"))?;

        Ok(BevyCompiled { schedule, world })
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, _state: &Self::State) -> anyhow::Result<()> {
        compiled.schedule.run(&mut compiled.world);
        Ok(())
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

struct FlecsCompiled {
    world: FlecsWorld,
}

struct FlecsAdapter;

impl BenchAdapter for FlecsAdapter {
    type Compiled = FlecsCompiled;
    type State = SharedBenchState;

    const NAME: &'static str = "flecs";

    fn compile(
        workload: &Workload,
        parallelism: Option<NonZeroUsize>,
        state: &Self::State,
    ) -> anyhow::Result<Self::Compiled> {
        let normalized = normalize_workload(workload);
        let world = FlecsWorld::new();
        world.set_threads(worker_count(parallelism) as i32);

        let resource_count = normalized_resource_count(&normalized);
        let mut resource_entities = vec![None; resource_count + 1];
        let mut system_entities = Vec::<FlecsEntity>::with_capacity(normalized.jobs.len());

        for (resource, resource_entity) in resource_entities
            .iter_mut()
            .enumerate()
            .take(resource_count + 1)
            .skip(1)
        {
            *resource_entity = Some(FlecsEntity(
                *world
                    .entity_named(&format!("bench_resource_{resource}"))
                    .id(),
            ));
        }

        for (index, job) in normalized.jobs.iter().enumerate() {
            debug_assert_eq!(job.weight_hint, job.work.iterations());
            let mut builder = world.system_named::<()>(&job.name);

            for access in job.accesses.iter() {
                let resource = resource_entities[access.resource as usize]
                    .expect("every normalized flecs resource is registered");
                builder.term().set_id(resource);
                if access.write {
                    builder.write_curr();
                } else {
                    builder.read_curr();
                }
            }

            let state = Arc::clone(state);
            let work = job.work;
            let system = builder.run(move |_iter| {
                execute_work(work, state.as_ref(), index, 0);
            });

            for &predecessor in normalized.dagga_predecessors[index].iter() {
                system.depends_on(system_entities[predecessor]);
            }

            system_entities.push(FlecsEntity(*system.id()));
        }

        Ok(FlecsCompiled { world })
    }

    fn make_state(workload: &Workload) -> Self::State {
        Arc::new(BenchState::new(workload))
    }

    fn run(compiled: &mut Self::Compiled, _state: &Self::State) -> anyhow::Result<()> {
        compiled.world.progress();
        Ok(())
    }

    fn sample(state: &Self::State) -> u64 {
        state.sample()
    }
}

fn write_key(key: usize, strict: bool) -> Dependency {
    Write(Identifier(key), if strict { Strict } else { Relax })
}

fn read_key(key: usize, strict: bool) -> Dependency {
    Read(Identifier(key), if strict { Strict } else { Relax })
}

fn work_target_us(target_us: u32) -> WorkProfile {
    if target_us == 0 {
        WorkProfile::ZERO
    } else {
        WorkProfile::busy(target_us.saturating_mul(MAINLINE_RANDOM_ITERATIONS_PER_MICROSECOND))
    }
}

fn weighted_duration_us(rng: &mut DeterministicRng, options: &[(u32, u32)]) -> u32 {
    let total = options.iter().map(|(_, weight)| *weight).sum::<u32>();
    debug_assert!(total > 0);

    let mut ticket = (rng.next_u64() % (total as u64)) as u32;
    for &(duration_us, weight) in options {
        if ticket < weight {
            return duration_us;
        }
        ticket -= weight;
    }

    options[options.len() - 1].0
}

fn mainline_hot_resource(rng: &mut DeterministicRng) -> usize {
    rng.index(MAINLINE_RANDOM_HOT_RESOURCE_COUNT)
}

fn mainline_warm_resource(rng: &mut DeterministicRng) -> usize {
    MAINLINE_RANDOM_HOT_RESOURCE_COUNT + rng.index(MAINLINE_RANDOM_WARM_RESOURCE_COUNT)
}

fn mainline_local_resource(shard: usize, slot: usize) -> usize {
    debug_assert!(shard < MAINLINE_RANDOM_SHARD_COUNT);
    debug_assert!(slot < MAINLINE_RANDOM_LOCAL_RESOURCE_COUNT);

    MAINLINE_RANDOM_HOT_RESOURCE_COUNT
        + MAINLINE_RANDOM_WARM_RESOURCE_COUNT
        + (shard * MAINLINE_RANDOM_LOCAL_RESOURCE_COUNT)
        + slot
}

fn mainline_random_local_resource(rng: &mut DeterministicRng, shard: usize) -> usize {
    mainline_local_resource(shard, rng.index(MAINLINE_RANDOM_LOCAL_RESOURCE_COUNT))
}

fn push_recent_predecessor(
    predecessors: &mut Vec<usize>,
    candidates: &[usize],
    rng: &mut DeterministicRng,
) {
    if candidates.is_empty() {
        return;
    }

    let window = candidates.len().min(24);
    let offset = rng.index(window);
    predecessors.push(candidates[candidates.len() - 1 - offset]);
}

fn push_previous_layer_predecessor(
    predecessors: &mut Vec<usize>,
    layer_jobs: &[Vec<usize>],
    layer: usize,
    rng: &mut DeterministicRng,
) {
    if layer == 0 {
        return;
    }

    let lookback = 1 + rng.index(layer.min(3));
    push_recent_predecessor(predecessors, &layer_jobs[layer - lookback], rng);
}

fn compact_dependencies(mut dependencies: Vec<Dependency>) -> Vec<Dependency> {
    let mut compact = Vec::<Dependency>::with_capacity(dependencies.len());

    for dependency in dependencies.drain(..) {
        match dependency {
            Dependency::Unknown => return vec![Dependency::Unknown],
            Read(key, order) => {
                let mut merged = false;
                for existing in compact.iter_mut() {
                    match existing {
                        Read(existing_key, existing_order)
                        | Write(existing_key, existing_order)
                            if *existing_key == key =>
                        {
                            *existing_order = (*existing_order).max(order);
                            merged = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !merged {
                    compact.push(Read(key, order));
                }
            }
            Write(key, order) => {
                let mut merged = false;
                for existing in compact.iter_mut() {
                    match existing {
                        Read(existing_key, existing_order) if *existing_key == key => {
                            let merged_order = (*existing_order).max(order);
                            *existing = Write(key.clone(), merged_order);
                            merged = true;
                            break;
                        }
                        Write(existing_key, existing_order) if *existing_key == key => {
                            *existing_order = (*existing_order).max(order);
                            merged = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !merged {
                    compact.push(Write(key, order));
                }
            }
        }
    }

    compact
}

fn realistic_random_main() -> Workload {
    let mut rng = DeterministicRng::new(MAINLINE_RANDOM_SEED);
    let mut jobs = Vec::with_capacity(MAINLINE_RANDOM_JOB_COUNT);
    let mut layer_jobs = vec![Vec::<usize>::new(); MAINLINE_RANDOM_LAYER_COUNT];
    let mut shard_recent = vec![Vec::<usize>::new(); MAINLINE_RANDOM_SHARD_COUNT];
    let mut shard_pipeline_tail = vec![None; MAINLINE_RANDOM_SHARD_COUNT];
    let mut global_recent = Vec::<usize>::with_capacity(MAINLINE_RANDOM_JOB_COUNT);

    for index in 0..MAINLINE_RANDOM_JOB_COUNT {
        let layer = index * MAINLINE_RANDOM_LAYER_COUNT / MAINLINE_RANDOM_JOB_COUNT;
        let shard = rng.index(MAINLINE_RANDOM_SHARD_COUNT);
        let role = rng.weighted_index(&[28, 24, 16, 14, 12, 6]);
        let other_shard =
            (shard + 1 + rng.index(MAINLINE_RANDOM_SHARD_COUNT - 1)) % MAINLINE_RANDOM_SHARD_COUNT;

        let mut dependencies = Vec::with_capacity(5);
        let mut predecessors = Vec::with_capacity(4);

        let work = match role {
            0 => {
                dependencies.push(read_key(
                    mainline_random_local_resource(&mut rng, shard),
                    false,
                ));
                dependencies.push(read_key(mainline_warm_resource(&mut rng), rng.chance(1, 5)));
                if rng.chance(1, 3) {
                    dependencies.push(read_key(
                        mainline_random_local_resource(&mut rng, shard),
                        rng.chance(1, 6),
                    ));
                }
                if rng.chance(1, 4) {
                    dependencies.push(read_key(mainline_hot_resource(&mut rng), false));
                }
                if rng.chance(1, 8) {
                    dependencies.push(write_key(
                        mainline_random_local_resource(&mut rng, shard),
                        false,
                    ));
                }
                if rng.chance(3, 10) {
                    push_previous_layer_predecessor(
                        &mut predecessors,
                        &layer_jobs,
                        layer,
                        &mut rng,
                    );
                }
                if rng.chance(1, 6) {
                    push_recent_predecessor(&mut predecessors, &shard_recent[shard], &mut rng);
                }
                work_target_us(weighted_duration_us(
                    &mut rng,
                    &[(0, 10), (25, 20), (75, 35), (175, 25), (400, 10)],
                ))
            }
            1 => {
                dependencies.push(read_key(
                    mainline_random_local_resource(&mut rng, shard),
                    false,
                ));
                dependencies.push(write_key(
                    mainline_random_local_resource(&mut rng, shard),
                    rng.chance(2, 5),
                ));
                if rng.chance(1, 2) {
                    dependencies.push(read_key(mainline_warm_resource(&mut rng), rng.chance(1, 4)));
                }
                if rng.chance(1, 3) {
                    dependencies.push(write_key(
                        mainline_warm_resource(&mut rng),
                        rng.chance(1, 6),
                    ));
                }
                if rng.chance(3, 5) {
                    push_previous_layer_predecessor(
                        &mut predecessors,
                        &layer_jobs,
                        layer,
                        &mut rng,
                    );
                }
                if rng.chance(1, 2) {
                    push_recent_predecessor(&mut predecessors, &shard_recent[shard], &mut rng);
                }
                work_target_us(weighted_duration_us(
                    &mut rng,
                    &[(25, 10), (75, 20), (175, 30), (400, 25), (900, 15)],
                ))
            }
            2 => {
                dependencies.push(read_key(
                    mainline_random_local_resource(&mut rng, shard),
                    rng.chance(1, 4),
                ));
                dependencies.push(read_key(
                    mainline_random_local_resource(&mut rng, other_shard),
                    rng.chance(2, 5),
                ));
                dependencies.push(write_key(
                    mainline_warm_resource(&mut rng),
                    rng.chance(1, 2),
                ));
                if rng.chance(1, 3) {
                    dependencies.push(read_key(mainline_hot_resource(&mut rng), rng.chance(1, 2)));
                }
                if rng.chance(7, 10) {
                    push_previous_layer_predecessor(
                        &mut predecessors,
                        &layer_jobs,
                        layer,
                        &mut rng,
                    );
                }
                if rng.chance(3, 5) {
                    push_recent_predecessor(
                        &mut predecessors,
                        &shard_recent[other_shard],
                        &mut rng,
                    );
                }
                if rng.chance(1, 2) {
                    push_recent_predecessor(&mut predecessors, &shard_recent[shard], &mut rng);
                }
                work_target_us(weighted_duration_us(
                    &mut rng,
                    &[
                        (75, 5),
                        (175, 15),
                        (400, 30),
                        (900, 25),
                        (1_500, 15),
                        (2_500, 10),
                    ],
                ))
            }
            3 => {
                dependencies.push(write_key(mainline_local_resource(shard, 0), true));
                dependencies.push(read_key(
                    mainline_local_resource(
                        shard,
                        1 + rng.index(MAINLINE_RANDOM_LOCAL_RESOURCE_COUNT - 1),
                    ),
                    rng.chance(1, 2),
                ));
                if rng.chance(1, 3) {
                    dependencies.push(read_key(mainline_warm_resource(&mut rng), true));
                }
                if let Some(predecessor) = shard_pipeline_tail[shard] {
                    predecessors.push(predecessor);
                } else {
                    push_previous_layer_predecessor(
                        &mut predecessors,
                        &layer_jobs,
                        layer,
                        &mut rng,
                    );
                }
                work_target_us(weighted_duration_us(
                    &mut rng,
                    &[
                        (25, 5),
                        (75, 20),
                        (175, 30),
                        (400, 25),
                        (900, 15),
                        (1_500, 5),
                    ],
                ))
            }
            4 => {
                dependencies.push(read_key(mainline_hot_resource(&mut rng), rng.chance(1, 3)));
                dependencies.push(write_key(mainline_hot_resource(&mut rng), rng.chance(1, 2)));
                dependencies.push(read_key(
                    mainline_random_local_resource(&mut rng, shard),
                    false,
                ));
                if rng.chance(1, 2) {
                    dependencies.push(write_key(mainline_warm_resource(&mut rng), true));
                }
                if rng.chance(3, 5) {
                    push_previous_layer_predecessor(
                        &mut predecessors,
                        &layer_jobs,
                        layer,
                        &mut rng,
                    );
                }
                if rng.chance(2, 5) {
                    push_recent_predecessor(&mut predecessors, &global_recent, &mut rng);
                }
                work_target_us(weighted_duration_us(
                    &mut rng,
                    &[(0, 20), (25, 20), (75, 20), (175, 20), (400, 15), (900, 5)],
                ))
            }
            _ => {
                if rng.chance(1, 2) {
                    dependencies.push(read_key(
                        mainline_random_local_resource(&mut rng, shard),
                        false,
                    ));
                } else {
                    dependencies.push(read_key(mainline_warm_resource(&mut rng), false));
                }
                if rng.chance(1, 10) {
                    dependencies.push(write_key(
                        mainline_random_local_resource(&mut rng, shard),
                        false,
                    ));
                }
                if rng.chance(1, 5) {
                    push_previous_layer_predecessor(
                        &mut predecessors,
                        &layer_jobs,
                        layer,
                        &mut rng,
                    );
                }
                work_target_us(weighted_duration_us(
                    &mut rng,
                    &[(0, 40), (25, 30), (75, 20), (175, 10)],
                ))
            }
        };

        if rng.chance(1, 12) {
            push_recent_predecessor(&mut predecessors, &global_recent, &mut rng);
        }
        if rng.chance(1, 10) {
            dependencies.push(read_key(mainline_warm_resource(&mut rng), true));
        }

        predecessors.sort_unstable();
        predecessors.dedup();

        jobs.push(
            JobSpec::new(work, compact_dependencies(dependencies)).with_predecessors(predecessors),
        );
        layer_jobs[layer].push(index);
        shard_recent[shard].push(index);
        global_recent.push(index);
        if role == 3 {
            shard_pipeline_tail[shard] = Some(index);
        }
    }

    Workload::new(
        "realistic_random_main",
        format!(
            "seed{:016x}_res{}_jobs{}_layers{}_us0to2500",
            MAINLINE_RANDOM_SEED,
            MAINLINE_RANDOM_RESOURCE_COUNT,
            MAINLINE_RANDOM_JOB_COUNT,
            MAINLINE_RANDOM_LAYER_COUNT,
        ),
        jobs,
    )
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
            JobSpec::new(
                WorkProfile::busy(follower_iterations),
                [write_key(lane, true)],
            )
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
            JobSpec::new(
                WorkProfile::busy(follower_iterations),
                [write_key(lane, true)],
            )
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

fn portfolio_bridge_tradeoff(
    pre_jobs: usize,
    pre_iterations: u32,
    bridge_iterations: u32,
    post_jobs: usize,
    post_iterations: u32,
) -> Workload {
    let mut jobs = Vec::with_capacity(pre_jobs + post_jobs + 1);

    for _ in 0..pre_jobs {
        jobs.push(JobSpec::new(
            WorkProfile::busy(pre_iterations),
            [read_key(20_000, false)],
        ));
    }

    jobs.push(JobSpec::new(
        WorkProfile::busy(bridge_iterations),
        [write_key(20_000, false), write_key(20_001, false)],
    ));

    for _ in 0..post_jobs {
        jobs.push(JobSpec::new(
            WorkProfile::busy(post_iterations),
            [read_key(20_001, false)],
        ));
    }

    Workload::new(
        "portfolio_bridge_tradeoff",
        format!(
            "pre{pre_jobs}_iter{pre_iterations}_bridge{bridge_iterations}_post{post_jobs}_iter{post_iterations}"
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
        format!(
            "fanout{fan_out}_root{root_iterations}_leaf{leaf_iterations}_sink{sink_iterations}"
        ),
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
        jobs.push(JobSpec::new(
            WorkProfile::busy(1_100),
            [write_key(0, false)],
        ));
        jobs.push(JobSpec::new(
            WorkProfile::busy(2_200),
            [write_key(hot, true)],
        ));
        jobs.push(JobSpec::new(WorkProfile::busy(500), [read_key(hot, false)]));
        jobs.push(JobSpec::new(
            WorkProfile::busy(900),
            [write_key(lane, false)],
        ));
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

fn mainline_workloads() -> Vec<Workload> {
    vec![realistic_random_main()]
}

fn compile_workloads() -> Vec<Workload> {
    vec![realistic_random_main(), compile_heavy_sparse_keys(1_536)]
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
        portfolio_bridge_tradeoff(96, 6_000, 400, 32, 1_500),
        portfolio_bridge_tradeoff(32, 1_500, 400, 96, 6_000),
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
                let state = A::make_state(workload);
                b.iter(|| black_box(A::compile(workload, benchmark_parallelism(), &state).unwrap()))
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
                let state = A::make_state(workload);
                let mut compiled = A::compile(workload, benchmark_parallelism(), &state).unwrap();
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
    let mainline = mainline_workloads();
    let compile = compile_workloads();
    let overhead = overhead_workloads();
    let parallelism = parallelism_workloads();

    bench_compile::<A>(c, &compile);
    bench_run_suite::<A>(c, "run_mainline", &mainline, configure_parallelism_group);
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

fn bench_experiment_04(c: &mut Criterion) {
    bench_adapter::<Experiment04AdaptiveAdapter>(c);
}

fn bench_experiment_05(c: &mut Criterion) {
    bench_adapter::<ExperimentAdapter<experiment_05::Scheduler<BenchState>>>(c);
}

fn bench_experiment_04_critical_path(c: &mut Criterion) {
    bench_adapter::<Experiment04CriticalPathAdapter>(c);
}

fn bench_experiment_04_read_heavy(c: &mut Criterion) {
    bench_adapter::<Experiment04ReadHeavyAdapter>(c);
}

fn bench_experiment_04_hot_contention(c: &mut Criterion) {
    bench_adapter::<Experiment04HotContentionAdapter>(c);
}

fn bench_dagga(c: &mut Criterion) {
    bench_adapter::<DaggaAdapter>(c);
}

fn bench_dag_exec(c: &mut Criterion) {
    bench_adapter::<DagExecAdapter>(c);
}

fn bench_shipyard(c: &mut Criterion) {
    bench_adapter::<ShipyardAdapter>(c);
}

fn bench_legion(c: &mut Criterion) {
    bench_adapter::<LegionAdapter>(c);
}

fn bench_bevy_ecs(c: &mut Criterion) {
    bench_adapter::<BevyAdapter>(c);
}

fn bench_flecs(c: &mut Criterion) {
    bench_adapter::<FlecsAdapter>(c);
}

criterion_group!(
    experiment_benches,
    bench_experiment_01,
    bench_experiment_02,
    bench_experiment_03,
    bench_experiment_04,
    bench_experiment_05,
    bench_experiment_04_critical_path,
    bench_experiment_04_read_heavy,
    bench_experiment_04_hot_contention,
    bench_dagga,
    bench_dag_exec,
    bench_shipyard,
    bench_legion,
    bench_bevy_ecs,
    bench_flecs
);
criterion_main!(experiment_benches);
