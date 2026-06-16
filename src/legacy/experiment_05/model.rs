use crate::legacy::{
    depend::{Dependency, Key, Order, Scope as DependScope},
    error::Error as ScheduleError,
    experiment::Job as ApiJob,
};
use anyhow::Result;
use parking_lot::Mutex;
use rayon::{prelude::*, Scope as RayonScope, ThreadPool, ThreadPoolBuilder};
use std::{
    cmp::{max, Ordering, Reverse},
    collections::HashMap,
    num::NonZeroUsize,
    ops::Range,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*},
        Arc,
    },
    thread,
};

type JobId = u32;
type ClusterId = u32;
type LocalIndex = u8;
type SharedRun<S> = Arc<dyn Fn(&S) -> anyhow::Result<()> + Send + Sync + 'static>;

const MAX_CLUSTER_JOBS: usize = 16;
#[derive(Clone, Debug, PartialEq, Eq)]
struct Access {
    key: Key,
    order: Order,
    write: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NormalizedDeps {
    Unknown,
    Known(Box<[Access]>),
}

struct PendingJob<S> {
    run: SharedRun<S>,
    dependencies: NormalizedDeps,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Relation {
    None,
    Relaxed,
    Strict,
}

#[derive(Clone, Copy, Debug, Default)]
struct KeyHeat {
    touches: u32,
    writes: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct JobStats {
    strict_height: u32,
    strict_fanout: u32,
    relaxed_degree: u32,
    writes: u32,
    reads: u32,
    accesses: u32,
    hot_access_score: u32,
    hot_read_score: u32,
    hot_write_score: u32,
}

#[derive(Clone, Copy, Debug)]
struct AccessEvent {
    job: JobId,
    write: bool,
}

#[derive(Clone, Debug, Default)]
struct KeyPlan {
    events: Vec<AccessEvent>,
    any_write: bool,
    all_relax: bool,
    dynamic_cluster: Option<ClusterId>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ClusterPlacement {
    cluster: ClusterId,
    local: LocalIndex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RemoteSuccessor {
    cluster: ClusterId,
    job: LocalIndex,
}

#[derive(Clone, Copy, Debug, Default)]
struct LocalPriority {
    strict_height: u32,
    remote_successors: u32,
    local_successors: u32,
    dynamic_degree: u32,
    hot_write_score: u32,
    writes: u32,
    reads: u32,
    accesses: u32,
}

struct ClusterJob<S> {
    run: SharedRun<S>,
    local_predecessor_mask: u64,
    external_wait_initial: u32,
    external_wait: AtomicU32,
    dynamic_conflict_mask: u64,
    local_successor_range: Range<u32>,
    remote_successor_range: Range<u32>,
    priority: LocalPriority,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClusterMode {
    Dynamic,
    Static,
    Serial,
}

#[repr(align(64))]
struct Cluster<S> {
    mode: ClusterMode,
    ready_mask_initial: u64,
    ready_mask: AtomicU64,
    done_mask: AtomicU64,
    queued: AtomicBool,
    jobs: Box<[ClusterJob<S>]>,
    local_successors: Box<[LocalIndex]>,
    remote_successors: Box<[RemoteSuccessor]>,
    priority_height: u32,
}

struct SingletonNode<S> {
    run: SharedRun<S>,
    wait_initial: u32,
    wait: AtomicU32,
    successor_range: Range<u32>,
}

struct SingletonDag<S> {
    nodes: Box<[SingletonNode<S>]>,
    successors: Box<[JobId]>,
    roots: Box<[JobId]>,
}

pub struct Schedule<S: Sync> {
    pool: ThreadPool,
    clusters: Box<[Cluster<S>]>,
    roots: Box<[ClusterId]>,
    serial_chain_runs: Option<Box<[SharedRun<S>]>>,
    trivial_parallel: bool,
    singleton_dag: Option<SingletonDag<S>>,
}

struct RunContext<'schedule, 'state, 'sync, S: Sync> {
    schedule: &'schedule Schedule<S>,
    state: &'state S,
    errors: &'sync Mutex<Vec<anyhow::Error>>,
    cancelled: &'sync AtomicBool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReadyScore {
    unlock_balance: i32,
    remote_unlocks: u32,
    strict_height: u32,
    remote_successors: u32,
    local_successors: u32,
    hot_write_score: u32,
    dynamic_degree: u32,
    reads: u32,
    writes: u32,
    accesses: u32,
}

impl<S: Sync> Schedule<S> {
    pub fn compile(parallelism: Option<NonZeroUsize>, jobs: Vec<ApiJob<S>>) -> Result<Self> {
        let mut pending = Vec::with_capacity(jobs.len());
        for job in jobs {
            let (run, dependencies) = job.into_parts();
            pending.push(PendingJob {
                run: Arc::from(run),
                dependencies: normalize_dependencies(dependencies)?,
            });
        }

        let count = pending.len();
        if count == 0 {
            return Ok(Self {
                pool: ThreadPoolBuilder::new().num_threads(1).build()?,
                clusters: Vec::new().into_boxed_slice(),
                roots: Vec::new().into_boxed_slice(),
                serial_chain_runs: Some(Vec::new().into_boxed_slice()),
                trivial_parallel: true,
                singleton_dag: None,
            });
        }

        let mut strict_candidates = vec![Vec::<JobId>::new(); count];
        let mut relaxed_neighbors = vec![Vec::<JobId>::new(); count];
        let mut relations = vec![Relation::None; count * count];

        for right in 0..count {
            for left in 0..right {
                let relation = relation(&pending[left].dependencies, &pending[right].dependencies);
                set_relation(
                    &mut relations,
                    count,
                    left as JobId,
                    right as JobId,
                    relation,
                );
                if relation == Relation::None {
                    continue;
                }
                if relation == Relation::Relaxed {
                    relaxed_neighbors[left].push(right as JobId);
                    relaxed_neighbors[right].push(left as JobId);
                } else {
                    strict_candidates[right].push(left as JobId);
                }
            }
        }

        let fixed_predecessors = reduce_predecessors_declaration_order(&strict_candidates);
        let fixed_successors = build_successors(&fixed_predecessors);
        let key_heat = build_key_heat(&pending);
        let stats = build_job_stats(&pending, &relaxed_neighbors, &fixed_successors, &key_heat);
        let priority = build_priority_order(&fixed_predecessors, &fixed_successors, &stats);
        let rank = inverse_order(&priority);
        let (cluster_jobs, placements) = build_clusters(
            &pending,
            &relations,
            &relaxed_neighbors,
            &fixed_predecessors,
            &fixed_successors,
            &stats,
            &key_heat,
            &priority,
            &rank,
        );
        let (final_predecessors, key_plans) =
            build_final_predecessors(&pending, &fixed_predecessors, &priority, &rank, &placements);
        let final_successors = build_successors(&final_predecessors);
        let final_heights = build_final_heights(&priority, &final_successors);
        let serial_chain_runs =
            build_serial_chain_runs(&pending, &final_predecessors, &final_successors);
        let (clusters, roots) = build_compiled_clusters(
            cluster_jobs,
            placements,
            pending,
            key_plans,
            final_predecessors,
            final_successors,
            final_heights,
            &stats,
        );
        let trivial_parallel = is_trivial_parallel_schedule(&clusters, &roots);
        let singleton_dag = (!trivial_parallel)
            .then(|| build_singleton_dag(&clusters, &roots))
            .flatten();

        let threads = parallelism
            .or_else(|| thread::available_parallelism().ok())
            .map(NonZeroUsize::get)
            .unwrap_or(1);
        let pool = ThreadPoolBuilder::new().num_threads(threads).build()?;

        Ok(Self {
            pool,
            clusters: clusters.into_boxed_slice(),
            roots: roots.into_boxed_slice(),
            serial_chain_runs,
            trivial_parallel,
            singleton_dag,
        })
    }

    pub fn run(&mut self, state: &S) -> Result<()> {
        if let Some(serial_chain_runs) = self.serial_chain_runs.as_ref() {
            for run in serial_chain_runs.iter() {
                run(state)?;
            }
            return Ok(());
        }

        if self.trivial_parallel {
            return self.pool.install(|| {
                self.clusters.par_iter().try_for_each(|cluster| {
                    let job = &cluster.jobs[0];
                    (job.run)(state)
                })
            });
        }
        if let Some(singleton_dag) = self.singleton_dag.as_ref() {
            return run_singleton_dag(&self.pool, singleton_dag, state);
        }

        self.prepare_run();

        let errors = Mutex::new(Vec::new());
        let cancelled = AtomicBool::new(false);
        let context = RunContext {
            schedule: self,
            state,
            errors: &errors,
            cancelled: &cancelled,
        };

        self.pool.scope(|scope| {
            let mut roots = self.roots.iter().copied();
            if let Some(root) = roots.next() {
                for cluster in roots {
                    try_spawn_cluster(&context, cluster, scope);
                }

                let root_cluster = &self.clusters[root as usize];
                if !root_cluster.queued.swap(true, AcqRel) {
                    run_cluster(&context, root, scope);
                }
            }
        });

        let mut errors = errors.into_inner();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.remove(0))
        }
    }

    fn prepare_run(&self) {
        for cluster in self.clusters.iter() {
            cluster
                .ready_mask
                .store(cluster.ready_mask_initial, Relaxed);
            cluster.done_mask.store(0, Relaxed);
            cluster.queued.store(false, Relaxed);
            for job in cluster.jobs.iter() {
                job.external_wait.store(job.external_wait_initial, Relaxed);
            }
        }
    }
}

fn normalize_dependencies(dependencies: Vec<Dependency>) -> Result<NormalizedDeps> {
    let mut unknown = false;
    let mut accesses = Vec::with_capacity(dependencies.len());

    for dependency in dependencies {
        match dependency {
            Dependency::Unknown => unknown = true,
            Dependency::Read(key, order) => accesses.push(Access {
                key,
                order,
                write: false,
            }),
            Dependency::Write(key, order) => accesses.push(Access {
                key,
                order,
                write: true,
            }),
        }
    }

    if accesses.is_empty() {
        return Ok(if unknown {
            NormalizedDeps::Unknown
        } else {
            NormalizedDeps::Known(Box::new([]))
        });
    }

    accesses.sort_unstable_by(|left, right| left.key.cmp(&right.key));
    let mut merged: Vec<Access> = Vec::with_capacity(accesses.len());
    for access in accesses {
        match merged.last_mut() {
            Some(previous) if previous.key == access.key => {
                let order = max(previous.order, access.order);
                if previous.write && access.write {
                    return Err(ScheduleError::WriteWriteConflict(
                        previous.key.clone(),
                        DependScope::Inner,
                        order,
                    )
                    .into());
                }
                if previous.write || access.write {
                    return Err(ScheduleError::ReadWriteConflict(
                        previous.key.clone(),
                        DependScope::Inner,
                        order,
                    )
                    .into());
                }
                previous.order = order;
            }
            _ => merged.push(access),
        }
    }

    Ok(if unknown {
        NormalizedDeps::Unknown
    } else {
        NormalizedDeps::Known(merged.into_boxed_slice())
    })
}

fn relation(left: &NormalizedDeps, right: &NormalizedDeps) -> Relation {
    match (left, right) {
        (NormalizedDeps::Unknown, _) | (_, NormalizedDeps::Unknown) => Relation::Strict,
        (NormalizedDeps::Known(left), NormalizedDeps::Known(right)) => {
            let mut relation = Relation::None;
            let mut indices = (0usize, 0usize);
            while let (Some(left), Some(right)) = (left.get(indices.0), right.get(indices.1)) {
                match left.key.cmp(&right.key) {
                    Ordering::Less => indices.0 += 1,
                    Ordering::Greater => indices.1 += 1,
                    Ordering::Equal => {
                        if left.write || right.write {
                            match max(left.order, right.order) {
                                Order::Strict => return Relation::Strict,
                                Order::Relax => relation = Relation::Relaxed,
                            }
                        }
                        indices.0 += 1;
                        indices.1 += 1;
                    }
                }
            }
            relation
        }
    }
}

fn reduce_predecessors_declaration_order(candidates: &[Vec<JobId>]) -> Vec<Vec<JobId>> {
    let len = candidates.len();
    let words = len.div_ceil(64);
    let mut predecessors = vec![Vec::<JobId>::new(); len];
    let mut ancestors = vec![vec![0u64; words]; len];

    for right in 0..len {
        for &candidate in candidates[right].iter().rev() {
            let redundant = predecessors[right].iter().any(|&selected| {
                selected == candidate || bit_test(&ancestors[selected as usize], candidate as usize)
            });
            if !redundant {
                predecessors[right].push(candidate);
            }
        }
        predecessors[right].sort_unstable();

        let (before_right, right_and_after) = ancestors.split_at_mut(right);
        let right_ancestors = &mut right_and_after[0];
        for &pred in predecessors[right].iter() {
            bit_set(right_ancestors, pred as usize);
            bit_or(right_ancestors, &before_right[pred as usize]);
        }
    }

    predecessors
}

fn build_key_heat<S>(pending: &[PendingJob<S>]) -> HashMap<Key, KeyHeat> {
    let mut heat = HashMap::<Key, KeyHeat>::new();
    for job in pending.iter() {
        match &job.dependencies {
            NormalizedDeps::Unknown => {
                let entry = heat.entry(Key::Identifier(usize::MAX)).or_default();
                entry.touches += 1;
                entry.writes += 1;
            }
            NormalizedDeps::Known(accesses) => {
                for access in accesses.iter() {
                    let entry = heat.entry(access.key.clone()).or_default();
                    entry.touches += 1;
                    entry.writes += u32::from(access.write);
                }
            }
        }
    }
    heat
}

fn build_job_stats<S>(
    pending: &[PendingJob<S>],
    relaxed_neighbors: &[Vec<JobId>],
    fixed_successors: &[Vec<JobId>],
    key_heat: &HashMap<Key, KeyHeat>,
) -> Vec<JobStats> {
    let len = pending.len();
    let mut stats = vec![JobStats::default(); len];

    for (job, dependencies) in pending.iter().map(|job| &job.dependencies).enumerate() {
        stats[job].relaxed_degree = relaxed_neighbors[job].len() as u32;
        stats[job].strict_fanout = fixed_successors[job].len() as u32;

        match dependencies {
            NormalizedDeps::Unknown => {
                stats[job].writes = 1;
                stats[job].accesses = 1;
                stats[job].hot_access_score = 1;
                stats[job].hot_write_score = 1;
            }
            NormalizedDeps::Known(accesses) => {
                for access in accesses.iter() {
                    stats[job].accesses += 1;
                    let heat = key_heat
                        .get(&access.key)
                        .expect("every normalized access has key heat");
                    stats[job].hot_access_score += heat.touches + heat.writes;
                    if access.write {
                        stats[job].writes += 1;
                        stats[job].hot_write_score += heat.touches + (heat.writes * 2);
                    } else {
                        stats[job].reads += 1;
                        stats[job].hot_read_score += heat.touches + (heat.writes * 2);
                    }
                }
            }
        }
    }

    for job in (0..len).rev() {
        stats[job].strict_height = fixed_successors[job]
            .iter()
            .map(|&succ| stats[succ as usize].strict_height + 1)
            .max()
            .unwrap_or(0);
    }

    stats
}

fn build_priority_order(
    fixed_predecessors: &[Vec<JobId>],
    fixed_successors: &[Vec<JobId>],
    stats: &[JobStats],
) -> Vec<JobId> {
    let len = fixed_predecessors.len();
    let mut remaining = fixed_predecessors
        .iter()
        .map(|preds| preds.len() as u32)
        .collect::<Vec<_>>();
    let mut available = remaining
        .iter()
        .enumerate()
        .filter_map(|(job, &count)| (count == 0).then_some(job as JobId))
        .collect::<Vec<_>>();
    let mut order = Vec::with_capacity(len);

    while !available.is_empty() {
        let best_index = select_best_available(&available, stats);
        let job = available.swap_remove(best_index);
        order.push(job);

        for &successor in fixed_successors[job as usize].iter() {
            let wait = &mut remaining[successor as usize];
            *wait -= 1;
            if *wait == 0 {
                available.push(successor);
            }
        }
    }

    debug_assert_eq!(order.len(), len);
    order
}

fn select_best_available(available: &[JobId], stats: &[JobStats]) -> usize {
    let mut best_index = 0usize;
    for index in 1..available.len() {
        if compare_priority(
            available[index] as usize,
            available[best_index] as usize,
            stats,
        ) == Ordering::Greater
        {
            best_index = index;
        }
    }
    best_index
}

fn compare_priority(left_job: usize, right_job: usize, stats: &[JobStats]) -> Ordering {
    let left = stats[left_job];
    let right = stats[right_job];
    left.strict_height
        .cmp(&right.strict_height)
        .then(left.strict_fanout.cmp(&right.strict_fanout))
        .then(left.relaxed_degree.cmp(&right.relaxed_degree))
        .then(left.hot_write_score.cmp(&right.hot_write_score))
        .then(left.reads.cmp(&right.reads))
        .then(left.writes.cmp(&right.writes))
        .then(left.accesses.cmp(&right.accesses))
        .then_with(|| right_job.cmp(&left_job))
}

fn inverse_order(order: &[JobId]) -> Vec<u32> {
    let mut rank = vec![0u32; order.len()];
    for (index, &job) in order.iter().enumerate() {
        rank[job as usize] = index as u32;
    }
    rank
}

#[allow(clippy::too_many_arguments)]
fn build_clusters<S>(
    pending: &[PendingJob<S>],
    relations: &[Relation],
    relaxed_neighbors: &[Vec<JobId>],
    fixed_predecessors: &[Vec<JobId>],
    fixed_successors: &[Vec<JobId>],
    stats: &[JobStats],
    key_heat: &HashMap<Key, KeyHeat>,
    priority: &[JobId],
    rank: &[u32],
) -> (Vec<Vec<JobId>>, Vec<ClusterPlacement>) {
    let len = pending.len();
    let mut assigned = vec![false; len];
    let mut clusters = Vec::<Vec<JobId>>::new();
    let mut placements = vec![ClusterPlacement::default(); len];

    for &seed in priority.iter() {
        if assigned[seed as usize] {
            continue;
        }

        let mut cluster = vec![seed];
        assigned[seed as usize] = true;

        while cluster.len() < MAX_CLUSTER_JOBS {
            let mut candidates = HashMap::<JobId, i64>::new();
            for &member in cluster.iter() {
                for &candidate in relaxed_neighbors[member as usize].iter() {
                    if assigned[candidate as usize] {
                        continue;
                    }
                    *candidates.entry(candidate).or_default() += pair_cluster_weight(
                        pending, relations, len, member, candidate, stats, key_heat,
                    );
                }
                for &candidate in fixed_predecessors[member as usize].iter() {
                    if assigned[candidate as usize] {
                        continue;
                    }
                    *candidates.entry(candidate).or_default() += pair_cluster_weight(
                        pending, relations, len, member, candidate, stats, key_heat,
                    );
                }
                for &candidate in fixed_successors[member as usize].iter() {
                    if assigned[candidate as usize] {
                        continue;
                    }
                    *candidates.entry(candidate).or_default() += pair_cluster_weight(
                        pending, relations, len, member, candidate, stats, key_heat,
                    );
                }
            }

            let mut best = None::<(JobId, i64)>;
            for (&candidate, &score) in candidates.iter() {
                let score = score + (stats[candidate as usize].strict_height as i64 * 8);
                match best {
                    Some((best_job, best_score))
                        if score < best_score
                            || (score == best_score
                                && rank[candidate as usize] > rank[best_job as usize]) => {}
                    _ if score > 0 => best = Some((candidate, score)),
                    _ => {}
                }
            }

            let Some((best_job, _)) = best else {
                break;
            };
            assigned[best_job as usize] = true;
            cluster.push(best_job);
        }

        cluster.sort_unstable_by_key(|job| rank[*job as usize]);
        let cluster_id = clusters.len() as ClusterId;
        for (local, &job) in cluster.iter().enumerate() {
            placements[job as usize] = ClusterPlacement {
                cluster: cluster_id,
                local: local as LocalIndex,
            };
        }
        clusters.push(cluster);
    }

    (clusters, placements)
}

fn pair_cluster_weight<S>(
    pending: &[PendingJob<S>],
    relations: &[Relation],
    len: usize,
    left: JobId,
    right: JobId,
    stats: &[JobStats],
    key_heat: &HashMap<Key, KeyHeat>,
) -> i64 {
    let relation = get_relation(relations, len, left, right);
    let relation_score = match relation {
        Relation::None => 0,
        Relation::Strict => 48,
        Relation::Relaxed => 24,
    };
    let shared_affinity = shared_hot_key_affinity(
        &pending[left as usize].dependencies,
        &pending[right as usize].dependencies,
        key_heat,
    );
    let right_stats = stats[right as usize];
    relation_score
        + shared_affinity
        + (right_stats.strict_height as i64 * 8)
        + (right_stats.strict_fanout as i64 * 4)
        + (right_stats.writes as i64 * 8)
        + (right_stats.reads as i64 * 2)
}

fn shared_hot_key_affinity(
    left: &NormalizedDeps,
    right: &NormalizedDeps,
    key_heat: &HashMap<Key, KeyHeat>,
) -> i64 {
    match (left, right) {
        (NormalizedDeps::Unknown, _) | (_, NormalizedDeps::Unknown) => 0,
        (NormalizedDeps::Known(left), NormalizedDeps::Known(right)) => {
            let mut score = 0i64;
            let mut indices = (0usize, 0usize);
            while let (Some(left), Some(right)) = (left.get(indices.0), right.get(indices.1)) {
                match left.key.cmp(&right.key) {
                    Ordering::Less => indices.0 += 1,
                    Ordering::Greater => indices.1 += 1,
                    Ordering::Equal => {
                        if left.write || right.write {
                            let heat = key_heat
                                .get(&left.key)
                                .expect("every normalized access has key heat");
                            let footprint = heat.touches as usize;
                            let hotness = (heat.touches + (heat.writes * 2)) as i64;
                            if footprint <= MAX_CLUSTER_JOBS {
                                score += 64 + hotness;
                            } else if left.write && right.write {
                                score += 24 + (heat.writes as i64 / 2);
                            } else {
                                score -= hotness;
                            }
                        }
                        indices.0 += 1;
                        indices.1 += 1;
                    }
                }
            }
            score
        }
    }
}

fn build_final_predecessors<S>(
    pending: &[PendingJob<S>],
    fixed_predecessors: &[Vec<JobId>],
    priority: &[JobId],
    rank: &[u32],
    placements: &[ClusterPlacement],
) -> (Vec<Vec<JobId>>, HashMap<Key, KeyPlan>) {
    let mut predecessors = fixed_predecessors.to_vec();
    let mut key_plans = HashMap::<Key, KeyPlan>::new();

    for &job in priority.iter() {
        match &pending[job as usize].dependencies {
            NormalizedDeps::Unknown => {}
            NormalizedDeps::Known(accesses) => {
                for access in accesses.iter() {
                    let entry = key_plans
                        .entry(access.key.clone())
                        .or_insert_with(|| KeyPlan {
                            events: Vec::new(),
                            any_write: false,
                            all_relax: true,
                            dynamic_cluster: Some(placements[job as usize].cluster),
                        });
                    entry.events.push(AccessEvent {
                        job,
                        write: access.write,
                    });
                    entry.any_write |= access.write;
                    entry.all_relax &= access.order == Order::Relax;
                    match entry.dynamic_cluster {
                        Some(cluster) if cluster == placements[job as usize].cluster => {}
                        _ => entry.dynamic_cluster = None,
                    }
                }
            }
        }
    }

    for plan in key_plans.values_mut() {
        if !(plan.any_write && plan.all_relax) {
            plan.dynamic_cluster = None;
        }
    }

    for plan in key_plans.values() {
        if plan.dynamic_cluster.is_some() {
            continue;
        }

        let mut last_write = None::<JobId>;
        let mut read_batch = Vec::<JobId>::new();

        for event in plan.events.iter().copied() {
            if event.write {
                if let Some(write) = last_write {
                    predecessors[event.job as usize].push(write);
                }
                predecessors[event.job as usize].extend(read_batch.iter().copied());
                read_batch.clear();
                last_write = Some(event.job);
            } else {
                if let Some(write) = last_write {
                    predecessors[event.job as usize].push(write);
                }
                read_batch.push(event.job);
            }
        }
    }

    (
        reduce_predecessors_priority_order(predecessors, priority, rank),
        key_plans,
    )
}

fn reduce_predecessors_priority_order(
    mut predecessors: Vec<Vec<JobId>>,
    priority: &[JobId],
    rank: &[u32],
) -> Vec<Vec<JobId>> {
    let len = predecessors.len();
    let words = len.div_ceil(64);
    let mut ancestors = vec![vec![0u64; words]; len];
    let mut reduced = vec![Vec::<JobId>::new(); len];

    for predecessors in predecessors.iter_mut() {
        predecessors.sort_unstable_by_key(|job| rank[*job as usize]);
        predecessors.dedup();
    }

    for (job_rank, &job) in priority.iter().enumerate() {
        let mut reduced_job = Vec::new();
        for &candidate in predecessors[job as usize].iter().rev() {
            let candidate_rank = rank[candidate as usize] as usize;
            let redundant = reduced_job.iter().any(|&selected| {
                let selected_rank = rank[selected as usize] as usize;
                selected == candidate || bit_test(&ancestors[selected_rank], candidate_rank)
            });
            if !redundant {
                reduced_job.push(candidate);
            }
        }

        reduced_job.sort_unstable_by_key(|pred| rank[*pred as usize]);
        let (before_job, job_and_after) = ancestors.split_at_mut(job_rank);
        let job_ancestors = &mut job_and_after[0];
        for &pred in reduced_job.iter() {
            let pred_rank = rank[pred as usize] as usize;
            bit_set(job_ancestors, pred_rank);
            bit_or(job_ancestors, &before_job[pred_rank]);
        }
        reduced[job as usize] = reduced_job;
    }

    reduced
}

fn build_successors(predecessors: &[Vec<JobId>]) -> Vec<Vec<JobId>> {
    let mut successors = vec![Vec::<JobId>::new(); predecessors.len()];
    for (job, predecessors) in predecessors.iter().enumerate() {
        for &pred in predecessors.iter() {
            successors[pred as usize].push(job as JobId);
        }
    }
    successors
}

fn build_final_heights(priority: &[JobId], successors: &[Vec<JobId>]) -> Vec<u32> {
    let mut heights = vec![0u32; successors.len()];
    for &job in priority.iter().rev() {
        heights[job as usize] = successors[job as usize]
            .iter()
            .map(|&successor| heights[successor as usize] + 1)
            .max()
            .unwrap_or(0);
    }
    heights
}

fn build_serial_chain_runs<S>(
    pending: &[PendingJob<S>],
    predecessors: &[Vec<JobId>],
    successors: &[Vec<JobId>],
) -> Option<Box<[SharedRun<S>]>> {
    if pending.is_empty() {
        return Some(Vec::new().into_boxed_slice());
    }

    if predecessors.iter().any(|preds| preds.len() > 1)
        || successors.iter().any(|succs| succs.len() > 1)
    {
        return None;
    }

    let roots = predecessors
        .iter()
        .enumerate()
        .filter_map(|(job, preds)| preds.is_empty().then_some(job as JobId))
        .collect::<Vec<_>>();
    let [root] = roots.as_slice() else {
        return None;
    };

    let mut order = Vec::with_capacity(pending.len());
    let mut current = Some(*root);
    while let Some(job) = current {
        order.push(job);
        current = successors[job as usize].first().copied();
    }

    if order.len() != pending.len() {
        return None;
    }

    Some(
        order
            .into_iter()
            .map(|job| Arc::clone(&pending[job as usize].run))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_compiled_clusters<S>(
    cluster_jobs: Vec<Vec<JobId>>,
    placements: Vec<ClusterPlacement>,
    pending: Vec<PendingJob<S>>,
    key_plans: HashMap<Key, KeyPlan>,
    final_predecessors: Vec<Vec<JobId>>,
    final_successors: Vec<Vec<JobId>>,
    final_heights: Vec<u32>,
    stats: &[JobStats],
) -> (Vec<Cluster<S>>, Vec<ClusterId>) {
    let cluster_count = cluster_jobs.len();
    let runs_by_job = pending.into_iter().map(|job| job.run).collect::<Vec<_>>();
    let mut local_predecessor_masks = vec![Vec::<u64>::new(); cluster_count];
    let mut external_wait_initials = vec![Vec::<u32>::new(); cluster_count];
    let mut local_successors = vec![Vec::<Vec<LocalIndex>>::new(); cluster_count];
    let mut remote_successors = vec![Vec::<Vec<RemoteSuccessor>>::new(); cluster_count];
    let mut dynamic_conflict_masks = vec![Vec::<u64>::new(); cluster_count];

    for (cluster_id, jobs) in cluster_jobs.iter().enumerate() {
        local_predecessor_masks[cluster_id] = vec![0; jobs.len()];
        external_wait_initials[cluster_id] = vec![0; jobs.len()];
        local_successors[cluster_id] = vec![Vec::new(); jobs.len()];
        remote_successors[cluster_id] = vec![Vec::new(); jobs.len()];
        dynamic_conflict_masks[cluster_id] = vec![0; jobs.len()];
    }

    for (job, predecessors) in final_predecessors.iter().enumerate() {
        let target = placements[job];
        for &pred in predecessors.iter() {
            let source = placements[pred as usize];
            if source.cluster == target.cluster {
                local_predecessor_masks[target.cluster as usize][target.local as usize] |=
                    1u64 << source.local;
                local_successors[source.cluster as usize][source.local as usize].push(target.local);
            } else {
                external_wait_initials[target.cluster as usize][target.local as usize] += 1;
                remote_successors[source.cluster as usize][source.local as usize].push(
                    RemoteSuccessor {
                        cluster: target.cluster,
                        job: target.local,
                    },
                );
            }
        }
    }

    for plan in key_plans.values() {
        let Some(cluster_id) = plan.dynamic_cluster else {
            continue;
        };
        let cluster_id = cluster_id as usize;
        for left in 0..plan.events.len() {
            for right in left + 1..plan.events.len() {
                if !(plan.events[left].write || plan.events[right].write) {
                    continue;
                }
                let left_place = placements[plan.events[left].job as usize];
                let right_place = placements[plan.events[right].job as usize];
                debug_assert_eq!(left_place.cluster, right_place.cluster);
                dynamic_conflict_masks[cluster_id][left_place.local as usize] |=
                    1u64 << right_place.local;
                dynamic_conflict_masks[cluster_id][right_place.local as usize] |=
                    1u64 << left_place.local;
            }
        }
    }

    let mut clusters = Vec::with_capacity(cluster_count);
    let mut roots = Vec::new();
    for (cluster_id, jobs) in cluster_jobs.into_iter().enumerate() {
        let mut cluster_ready_mask = 0u64;
        let mut priority_height = 0u32;
        let mut packed_jobs = Vec::with_capacity(jobs.len());
        let mut packed_local_successors = Vec::new();
        let mut packed_remote_successors = Vec::new();

        for (local, job) in jobs.iter().copied().enumerate() {
            let start_local = packed_local_successors.len() as u32;
            let local_succ = &local_successors[cluster_id][local];
            packed_local_successors.extend(local_succ.iter().copied());
            let end_local = packed_local_successors.len() as u32;

            let start_remote = packed_remote_successors.len() as u32;
            let remote_succ = &remote_successors[cluster_id][local];
            packed_remote_successors.extend(remote_succ.iter().copied());
            let end_remote = packed_remote_successors.len() as u32;

            if local_predecessor_masks[cluster_id][local] == 0
                && external_wait_initials[cluster_id][local] == 0
            {
                cluster_ready_mask |= 1u64 << local;
            }

            priority_height = priority_height.max(final_heights[job as usize]);
            packed_jobs.push(ClusterJob {
                run: Arc::clone(&runs_by_job[job as usize]),
                local_predecessor_mask: local_predecessor_masks[cluster_id][local],
                external_wait_initial: external_wait_initials[cluster_id][local],
                external_wait: AtomicU32::new(external_wait_initials[cluster_id][local]),
                dynamic_conflict_mask: dynamic_conflict_masks[cluster_id][local],
                local_successor_range: start_local..end_local,
                remote_successor_range: start_remote..end_remote,
                priority: LocalPriority {
                    strict_height: final_heights[job as usize],
                    remote_successors: remote_succ.len() as u32,
                    local_successors: local_succ.len() as u32,
                    dynamic_degree: dynamic_conflict_masks[cluster_id][local].count_ones(),
                    hot_write_score: stats[job as usize].hot_write_score,
                    writes: stats[job as usize].writes,
                    reads: stats[job as usize].reads,
                    accesses: stats[job as usize].accesses,
                },
            });
        }

        if cluster_ready_mask != 0 {
            roots.push(cluster_id as ClusterId);
        }

        let mode = classify_cluster_mode(
            cluster_ready_mask,
            &packed_jobs,
            &packed_local_successors,
            &dynamic_conflict_masks[cluster_id],
        );

        clusters.push(Cluster {
            mode,
            ready_mask_initial: cluster_ready_mask,
            ready_mask: AtomicU64::new(cluster_ready_mask),
            done_mask: AtomicU64::new(0),
            queued: AtomicBool::new(false),
            jobs: packed_jobs.into_boxed_slice(),
            local_successors: packed_local_successors.into_boxed_slice(),
            remote_successors: packed_remote_successors.into_boxed_slice(),
            priority_height,
        });
    }

    roots.sort_unstable_by_key(|cluster_id| {
        let cluster = &clusters[*cluster_id as usize];
        (
            Reverse(cluster.priority_height),
            Reverse(cluster.jobs.len()),
            *cluster_id,
        )
    });

    let _ = final_successors;
    (clusters, roots)
}

fn classify_cluster_mode<S>(
    ready_mask_initial: u64,
    jobs: &[ClusterJob<S>],
    local_successors: &[LocalIndex],
    dynamic_conflict_masks: &[u64],
) -> ClusterMode {
    let has_dynamic_conflicts = dynamic_conflict_masks.iter().any(|&mask| mask != 0);
    if has_dynamic_conflicts {
        return ClusterMode::Dynamic;
    }

    let local_roots = jobs
        .iter()
        .filter(|job| job.local_predecessor_mask == 0)
        .count();
    let is_serial = ready_mask_initial.count_ones() <= 1
        && local_roots <= 1
        && jobs.iter().all(|job| {
            let predecessor_count = job.local_predecessor_mask.count_ones();
            predecessor_count <= 1 && local_successors_slice(job, local_successors).len() <= 1
        });

    if is_serial {
        ClusterMode::Serial
    } else {
        ClusterMode::Static
    }
}

fn is_trivial_parallel_schedule<S>(clusters: &[Cluster<S>], roots: &[ClusterId]) -> bool {
    roots.len() == clusters.len()
        && clusters.iter().all(|cluster| {
            cluster.jobs.len() == 1
                && cluster.ready_mask_initial == 1
                && cluster.local_successors.is_empty()
                && cluster.remote_successors.is_empty()
        })
}

fn build_singleton_dag<S>(clusters: &[Cluster<S>], roots: &[ClusterId]) -> Option<SingletonDag<S>> {
    if !clusters.iter().all(|cluster| {
        cluster.jobs.len() == 1
            && cluster.local_successors.is_empty()
            && cluster.jobs[0].local_predecessor_mask == 0
    }) {
        return None;
    }

    let mut nodes = Vec::with_capacity(clusters.len());
    let mut successors = Vec::new();
    for cluster in clusters.iter() {
        let job = &cluster.jobs[0];
        let start = successors.len() as u32;
        for successor in remote_successors(cluster, job).iter().copied() {
            debug_assert_eq!(successor.job, 0);
            successors.push(successor.cluster);
        }
        let end = successors.len() as u32;
        nodes.push(SingletonNode {
            run: Arc::clone(&job.run),
            wait_initial: job.external_wait_initial,
            wait: AtomicU32::new(job.external_wait_initial),
            successor_range: start..end,
        });
    }

    Some(SingletonDag {
        nodes: nodes.into_boxed_slice(),
        successors: successors.into_boxed_slice(),
        roots: roots.to_vec().into_boxed_slice(),
    })
}

fn run_singleton_dag<S: Sync>(pool: &ThreadPool, dag: &SingletonDag<S>, state: &S) -> Result<()> {
    let errors = Mutex::new(Vec::new());
    let cancelled = AtomicBool::new(false);

    pool.scope(|scope| {
        let mut roots = dag.roots.iter().copied();
        if let Some(root) = roots.next() {
            for root in roots {
                spawn_singleton_ready(dag, state, &errors, &cancelled, root, scope);
            }
            run_singleton_chain(dag, state, &errors, &cancelled, root, scope);
        }
    });

    let mut errors = errors.into_inner();
    if errors.is_empty() {
        Ok(())
    } else {
        reset_singleton_dag(dag);
        Err(errors.remove(0))
    }
}

fn reset_singleton_dag<S>(dag: &SingletonDag<S>) {
    for node in dag.nodes.iter() {
        node.wait.store(node.wait_initial, Relaxed);
    }
}

fn try_spawn_cluster<'context, 'schedule, 'state, 'sync, 'scope, S: Sync>(
    context: &'context RunContext<'schedule, 'state, 'sync, S>,
    cluster: ClusterId,
    scope: &RayonScope<'scope>,
) where
    'context: 'scope,
    'schedule: 'scope,
    'state: 'scope,
    'sync: 'scope,
{
    if context.cancelled.load(Acquire) {
        return;
    }

    let cluster_state = &context.schedule.clusters[cluster as usize];
    if cluster_state.ready_mask.load(Acquire) == 0 {
        return;
    }
    if cluster_state.queued.swap(true, AcqRel) {
        return;
    }

    scope.spawn(move |scope| run_cluster(context, cluster, scope));
}

fn run_cluster<'context, 'schedule, 'state, 'sync, 'scope, S: Sync>(
    context: &'context RunContext<'schedule, 'state, 'sync, S>,
    mut cluster_id: ClusterId,
    scope: &RayonScope<'scope>,
) where
    'context: 'scope,
    'schedule: 'scope,
    'state: 'scope,
    'sync: 'scope,
{
    loop {
        let cluster = &context.schedule.clusters[cluster_id as usize];
        let mut ready = cluster.ready_mask.swap(0, AcqRel);
        let mut done_mask = cluster.done_mask.load(Relaxed);
        let mut next_inline_cluster = None;

        while ready != 0 {
            if context.cancelled.load(Acquire) {
                cluster.queued.store(false, Release);
                return;
            }

            let local = select_ready_job(context.schedule, cluster_id, ready, done_mask);
            let bit = 1u64 << local;
            ready &= !bit;

            let job = &cluster.jobs[local as usize];
            let result = (job.run)(context.state);
            if let Err(error) = result {
                context.cancelled.store(true, Release);
                context.errors.lock().push(error);
                cluster.queued.store(false, Release);
                return;
            }

            done_mask |= bit;
            cluster.done_mask.store(done_mask, Release);

            match cluster.mode {
                ClusterMode::Serial => {
                    if let Some(successor) = serial_successor(cluster, job) {
                        let successor_job = &cluster.jobs[successor as usize];
                        let successor_bit = 1u64 << successor;
                        if done_mask & successor_bit == 0
                            && successor_job.external_wait.load(Acquire) == 0
                            && done_mask & successor_job.local_predecessor_mask
                                == successor_job.local_predecessor_mask
                        {
                            ready |= successor_bit;
                        }
                    }
                }
                ClusterMode::Static | ClusterMode::Dynamic => {
                    for &successor in local_successors(cluster, job).iter() {
                        let successor_job = &cluster.jobs[successor as usize];
                        let successor_bit = 1u64 << successor;
                        if done_mask & successor_bit == 0
                            && successor_job.external_wait.load(Acquire) == 0
                            && done_mask & successor_job.local_predecessor_mask
                                == successor_job.local_predecessor_mask
                        {
                            ready |= successor_bit;
                        }
                    }
                }
            }

            wake_remote_successors(
                context,
                remote_successors(cluster, job),
                &mut next_inline_cluster,
                scope,
            );
        }

        if context.cancelled.load(Acquire) {
            cluster.queued.store(false, Release);
            return;
        }

        let remote_ready = cluster.ready_mask.swap(0, AcqRel);
        if remote_ready != 0 {
            ready = remote_ready;
            continue;
        }

        cluster.queued.store(false, Release);
        let remote_ready = cluster.ready_mask.swap(0, AcqRel);
        if remote_ready != 0 {
            if !cluster.queued.swap(true, AcqRel) {
                ready = remote_ready;
                continue;
            }
            cluster.ready_mask.fetch_or(remote_ready, Release);
        }

        if let Some(next_cluster) = next_inline_cluster.take() {
            cluster_id = next_cluster;
            continue;
        }

        return;
    }
}

fn select_ready_job<S: Sync>(
    schedule: &Schedule<S>,
    cluster_id: ClusterId,
    ready_mask: u64,
    done_mask: u64,
) -> LocalIndex {
    let cluster = &schedule.clusters[cluster_id as usize];
    if ready_mask & (ready_mask - 1) == 0 {
        return ready_mask.trailing_zeros() as LocalIndex;
    }
    if cluster.mode != ClusterMode::Dynamic {
        return ready_mask.trailing_zeros() as LocalIndex;
    }

    let mut candidates = ready_mask;
    let mut best = candidates.trailing_zeros() as LocalIndex;
    candidates &= candidates - 1;

    while candidates != 0 {
        let local = candidates.trailing_zeros() as LocalIndex;
        candidates &= candidates - 1;
        if compare_ready_job(schedule, cluster_id, local, best, ready_mask, done_mask)
            == Ordering::Greater
        {
            best = local;
        }
    }

    best
}

fn wake_remote_successors<'context, 'schedule, 'state, 'sync, 'scope, S: Sync>(
    context: &'context RunContext<'schedule, 'state, 'sync, S>,
    successors: &[RemoteSuccessor],
    next_inline_cluster: &mut Option<ClusterId>,
    scope: &RayonScope<'scope>,
) where
    'context: 'scope,
    'schedule: 'scope,
    'state: 'scope,
    'sync: 'scope,
{
    for successor in successors.iter().copied() {
        let target_cluster = &context.schedule.clusters[successor.cluster as usize];
        let target_job = &target_cluster.jobs[successor.job as usize];
        if target_job.external_wait.fetch_sub(1, AcqRel) != 1 {
            continue;
        }

        let target_done = target_cluster.done_mask.load(Acquire);
        let target_bit = 1u64 << successor.job;
        if target_done & target_bit != 0
            || target_done & target_job.local_predecessor_mask != target_job.local_predecessor_mask
        {
            continue;
        }

        if target_cluster.ready_mask.fetch_or(target_bit, AcqRel) & target_bit != 0 {
            continue;
        }

        if target_cluster.queued.swap(true, AcqRel) {
            continue;
        }

        match next_inline_cluster {
            Some(_) => scope.spawn(move |scope| run_cluster(context, successor.cluster, scope)),
            None => *next_inline_cluster = Some(successor.cluster),
        }
    }
}

fn spawn_singleton_ready<'state, 'sync, 'scope, S: Sync>(
    dag: &'scope SingletonDag<S>,
    state: &'state S,
    errors: &'sync Mutex<Vec<anyhow::Error>>,
    cancelled: &'sync AtomicBool,
    job: JobId,
    scope: &RayonScope<'scope>,
) where
    'state: 'scope,
    'sync: 'scope,
{
    if cancelled.load(Acquire) {
        return;
    }

    let node = &dag.nodes[job as usize];
    debug_assert_eq!(node.wait.load(Relaxed), 0);
    scope.spawn(move |scope| run_singleton_chain(dag, state, errors, cancelled, job, scope));
}

fn run_singleton_chain<'state, 'sync, 'scope, S: Sync>(
    dag: &'scope SingletonDag<S>,
    state: &'state S,
    errors: &'sync Mutex<Vec<anyhow::Error>>,
    cancelled: &'sync AtomicBool,
    mut job: JobId,
    scope: &RayonScope<'scope>,
) where
    'state: 'scope,
    'sync: 'scope,
{
    loop {
        if cancelled.load(Acquire) {
            return;
        }

        let node = &dag.nodes[job as usize];
        debug_assert_eq!(node.wait.load(Relaxed), 0);
        let result = (node.run)(state);
        node.wait.store(node.wait_initial, Release);

        match result {
            Ok(()) if !cancelled.load(Acquire) => {
                let mut next_inline = None;
                for &successor in singleton_successors(dag, node).iter() {
                    let successor_node = &dag.nodes[successor as usize];
                    if successor_node.wait.fetch_sub(1, AcqRel) == 1 {
                        if next_inline.is_none() {
                            next_inline = Some(successor);
                        } else {
                            spawn_singleton_ready(dag, state, errors, cancelled, successor, scope);
                        }
                    }
                }

                match next_inline {
                    Some(successor) => job = successor,
                    None => return,
                }
            }
            Ok(()) => return,
            Err(error) => {
                cancelled.store(true, Release);
                errors.lock().push(error);
                return;
            }
        }
    }
}

fn compare_ready_job<S: Sync>(
    schedule: &Schedule<S>,
    cluster_id: ClusterId,
    left: LocalIndex,
    right: LocalIndex,
    ready_mask: u64,
    done_mask: u64,
) -> Ordering {
    let left = ready_score(schedule, cluster_id, left, ready_mask, done_mask);
    let right = ready_score(schedule, cluster_id, right, ready_mask, done_mask);
    left.unlock_balance
        .cmp(&right.unlock_balance)
        .then(left.remote_unlocks.cmp(&right.remote_unlocks))
        .then(left.strict_height.cmp(&right.strict_height))
        .then(left.remote_successors.cmp(&right.remote_successors))
        .then(left.local_successors.cmp(&right.local_successors))
        .then(left.hot_write_score.cmp(&right.hot_write_score))
        .then(left.dynamic_degree.cmp(&right.dynamic_degree))
        .then(left.reads.cmp(&right.reads))
        .then(left.writes.cmp(&right.writes))
        .then(left.accesses.cmp(&right.accesses))
}

fn ready_score<S: Sync>(
    schedule: &Schedule<S>,
    cluster_id: ClusterId,
    local: LocalIndex,
    ready_mask: u64,
    done_mask: u64,
) -> ReadyScore {
    let cluster = &schedule.clusters[cluster_id as usize];
    let job = &cluster.jobs[local as usize];
    let done_after = done_mask | (1u64 << local);
    let immediate_local_unlocks = immediate_local_unlocks(cluster, local, done_after) as i32;
    let immediate_remote_unlocks = immediate_remote_unlocks(schedule, cluster, job) as i32;
    let ready_conflicts = (ready_mask & job.dynamic_conflict_mask).count_ones() as i32;

    ReadyScore {
        unlock_balance: (immediate_local_unlocks * 2) + (immediate_remote_unlocks * 3)
            - ready_conflicts,
        remote_unlocks: immediate_remote_unlocks as u32,
        strict_height: job.priority.strict_height,
        remote_successors: job.priority.remote_successors,
        local_successors: job.priority.local_successors,
        hot_write_score: job.priority.hot_write_score,
        dynamic_degree: job.priority.dynamic_degree,
        reads: job.priority.reads,
        writes: job.priority.writes,
        accesses: job.priority.accesses,
    }
}

fn immediate_local_unlocks<S>(cluster: &Cluster<S>, local: LocalIndex, done_after: u64) -> u32 {
    let job = &cluster.jobs[local as usize];
    local_successors(cluster, job)
        .iter()
        .filter(|&&successor| {
            let successor_job = &cluster.jobs[successor as usize];
            successor_job.external_wait.load(Acquire) == 0
                && done_after & successor_job.local_predecessor_mask
                    == successor_job.local_predecessor_mask
        })
        .count() as u32
}

fn immediate_remote_unlocks<S: Sync>(
    schedule: &Schedule<S>,
    cluster: &Cluster<S>,
    job: &ClusterJob<S>,
) -> u32 {
    remote_successors(cluster, job)
        .iter()
        .filter(|successor| {
            let target_cluster = &schedule.clusters[successor.cluster as usize];
            let target_job = &target_cluster.jobs[successor.job as usize];
            target_job.external_wait.load(Acquire) == 1
                && target_cluster.done_mask.load(Acquire) & target_job.local_predecessor_mask
                    == target_job.local_predecessor_mask
        })
        .count() as u32
}

fn local_successors<'cluster, S>(
    cluster: &'cluster Cluster<S>,
    job: &ClusterJob<S>,
) -> &'cluster [LocalIndex] {
    local_successors_slice(job, &cluster.local_successors)
}

fn remote_successors<'cluster, S>(
    cluster: &'cluster Cluster<S>,
    job: &ClusterJob<S>,
) -> &'cluster [RemoteSuccessor] {
    &cluster.remote_successors
        [job.remote_successor_range.start as usize..job.remote_successor_range.end as usize]
}

fn singleton_successors<'dag, S>(
    dag: &'dag SingletonDag<S>,
    node: &SingletonNode<S>,
) -> &'dag [JobId] {
    &dag.successors[node.successor_range.start as usize..node.successor_range.end as usize]
}

fn local_successors_slice<'slice, S>(
    job: &ClusterJob<S>,
    local_successors: &'slice [LocalIndex],
) -> &'slice [LocalIndex] {
    &local_successors
        [job.local_successor_range.start as usize..job.local_successor_range.end as usize]
}

fn serial_successor<S>(cluster: &Cluster<S>, job: &ClusterJob<S>) -> Option<LocalIndex> {
    let successors = local_successors(cluster, job);
    match successors {
        [successor] => Some(*successor),
        [] => None,
        _ => {
            debug_assert!(false, "serial cluster had branching local successors");
            Some(successors[0])
        }
    }
}

#[inline]
fn set_relation(
    relations: &mut [Relation],
    len: usize,
    left: JobId,
    right: JobId,
    relation: Relation,
) {
    relations[left as usize * len + right as usize] = relation;
    relations[right as usize * len + left as usize] = relation;
}

#[inline]
fn get_relation(relations: &[Relation], len: usize, left: JobId, right: JobId) -> Relation {
    relations[left as usize * len + right as usize]
}

#[inline]
fn bit_set(bits: &mut [u64], index: usize) {
    bits[index / 64] |= 1u64 << (index % 64);
}

#[inline]
fn bit_test(bits: &[u64], index: usize) -> bool {
    bits[index / 64] & (1u64 << (index % 64)) != 0
}

#[inline]
fn bit_or(left: &mut [u64], right: &[u64]) {
    for (left, right) in left.iter_mut().zip(right.iter()) {
        *left |= *right;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::depend::{
        Dependency::{Read, Write},
        Key::Identifier,
    };

    fn noop() -> SharedRun<()> {
        Arc::new(|_| Ok(()))
    }

    fn compile_schedule(jobs: Vec<(SharedRun<()>, Vec<Dependency>)>) -> Schedule<()> {
        let jobs = jobs
            .into_iter()
            .map(|(run, dependencies)| {
                let mut job = ApiJob::new(move |state| run(state));
                if !dependencies.is_empty() {
                    job = job.depend(dependencies);
                }
                job
            })
            .collect();

        Schedule::compile(NonZeroUsize::new(4), jobs).unwrap()
    }

    #[test]
    fn local_relaxed_key_stays_dynamic_inside_a_cluster() {
        let schedule = compile_schedule(vec![
            (noop(), vec![Write(Identifier(0), Order::Relax)]),
            (noop(), vec![Read(Identifier(0), Order::Relax)]),
            (noop(), vec![Write(Identifier(1), Order::Strict)]),
        ]);

        assert_eq!(schedule.clusters.len(), 2);
        let cluster = schedule
            .clusters
            .iter()
            .find(|cluster| cluster.jobs.len() == 2)
            .expect("expected a two-job cluster");
        assert_eq!(cluster.ready_mask_initial.count_ones(), 2);
        assert!(cluster
            .jobs
            .iter()
            .all(|job| job.external_wait_initial == 0));
        assert!(cluster
            .jobs
            .iter()
            .any(|job| job.dynamic_conflict_mask != 0));
    }

    #[test]
    fn wide_local_relaxed_key_is_globally_oriented_once_it_spans_clusters() {
        let mut jobs = Vec::new();
        for _ in 0..(MAX_CLUSTER_JOBS + 1) {
            jobs.push((noop(), vec![Write(Identifier(0), Order::Relax)]));
        }
        let schedule = compile_schedule(jobs);

        assert!(schedule.clusters.len() >= 2);
        let remote_blocked = schedule
            .clusters
            .iter()
            .flat_map(|cluster| cluster.jobs.iter())
            .filter(|job| job.external_wait_initial != 0)
            .count();
        assert!(remote_blocked != 0);
    }
}
