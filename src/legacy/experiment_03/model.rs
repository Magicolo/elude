use crate::legacy::{
    depend::{Dependency, Key, Order, Scope as DependScope},
    error::Error as ScheduleError,
    experiment::{Job as ApiJob, Run},
};
use anyhow::Result;
use parking_lot::Mutex;
use rayon::{Scope as RayonScope, ThreadPool, ThreadPoolBuilder};
use std::{
    cmp::{max, Ordering, Reverse},
    collections::HashMap,
    num::NonZeroUsize,
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU32, Ordering::*},
    thread,
};

type JobId = u32;

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
    run: Run<S>,
    dependencies: NormalizedDeps,
}

pub struct Node<S> {
    pub run: Run<S>,
    pub wait_initial: u32,
    pub wait: AtomicU32,
    pub successor_range: Range<u32>,
}

impl<S> Node<S> {
    fn new(run: Run<S>, wait_initial: u32, successor_range: Range<u32>) -> Self {
        Self {
            run,
            wait_initial,
            wait: AtomicU32::new(wait_initial),
            successor_range,
        }
    }
}

pub struct Schedule<S: Sync> {
    pool: ThreadPool,
    pub jobs: Box<[Node<S>]>,
    pub successors: Box<[JobId]>,
    pub roots: Box<[JobId]>,
}

struct RunContext<'schedule, 'state, 'sync, S: Sync> {
    schedule: &'schedule Schedule<S>,
    state: &'state S,
    errors: &'sync Mutex<Vec<anyhow::Error>>,
    cancelled: &'sync AtomicBool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Relation {
    None,
    Relaxed,
    Strict,
}

#[derive(Clone, Copy, Debug, Default)]
struct JobStats {
    strict_height: u32,
    relaxed_degree: u32,
    writes: u32,
    accesses: u32,
}

#[derive(Clone, Copy, Debug)]
struct AccessEvent {
    job: JobId,
    write: bool,
}

impl<S: Sync> Schedule<S> {
    pub fn compile(parallelism: Option<NonZeroUsize>, jobs: Vec<ApiJob<S>>) -> Result<Self> {
        let mut pending = Vec::with_capacity(jobs.len());
        for job in jobs {
            let (run, dependencies) = job.into_parts();
            pending.push(PendingJob {
                run,
                dependencies: normalize_dependencies(dependencies)?,
            });
        }

        let count = pending.len();
        let mut strict_candidates = vec![Vec::<JobId>::new(); count];
        let mut relaxed_neighbors = vec![Vec::<JobId>::new(); count];

        for right in 0..count {
            for left in 0..right {
                match relation(&pending[left].dependencies, &pending[right].dependencies) {
                    Relation::None => {}
                    Relation::Relaxed => {
                        relaxed_neighbors[left].push(right as JobId);
                        relaxed_neighbors[right].push(left as JobId);
                    }
                    Relation::Strict => strict_candidates[right].push(left as JobId),
                }
            }
        }

        let fixed_predecessors = reduce_predecessors_declaration_order(&strict_candidates);
        let priority = build_priority_order(&pending, &fixed_predecessors, &relaxed_neighbors);
        let rank = inverse_order(&priority);
        let final_predecessors =
            build_final_predecessors(&pending, &fixed_predecessors, &priority, &rank);

        let mut successors_by_job = vec![Vec::<JobId>::new(); count];
        let mut roots = Vec::new();
        for (job, predecessors) in final_predecessors.iter().enumerate() {
            if predecessors.is_empty() {
                roots.push(job as JobId);
            }
            for &pred in predecessors.iter() {
                successors_by_job[pred as usize].push(job as JobId);
            }
        }

        let final_heights = build_final_heights(&priority, &successors_by_job);
        for successors in successors_by_job.iter_mut() {
            successors.sort_unstable_by_key(|&job| {
                (
                    Reverse(final_heights[job as usize]),
                    Reverse(rank[job as usize]),
                    job,
                )
            });
        }
        roots.sort_unstable_by_key(|&job| {
            (
                Reverse(final_heights[job as usize]),
                Reverse(rank[job as usize]),
                job,
            )
        });

        let mut successors = Vec::new();
        let mut nodes = Vec::with_capacity(count);
        for (job, pending) in pending.into_iter().enumerate() {
            let start = successors.len() as u32;
            successors.extend(successors_by_job[job].iter().copied());
            let end = successors.len() as u32;
            nodes.push(Node::new(
                pending.run,
                final_predecessors[job].len() as u32,
                start..end,
            ));
        }

        let threads = parallelism
            .or_else(|| thread::available_parallelism().ok())
            .map(NonZeroUsize::get)
            .unwrap_or(1);
        let pool = ThreadPoolBuilder::new().num_threads(threads).build()?;

        Ok(Self {
            pool,
            jobs: nodes.into_boxed_slice(),
            successors: successors.into_boxed_slice(),
            roots: roots.into_boxed_slice(),
        })
    }

    pub fn run(&mut self, state: &S) -> Result<()> {
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
                for root in roots {
                    spawn_ready(&context, root, scope);
                }
                run_ready_chain(&context, root, scope);
            }
        });

        let mut errors = errors.into_inner();
        if errors.is_empty() {
            debug_assert!(self
                .jobs
                .iter()
                .all(|job| job.wait.load(Relaxed) == job.wait_initial));
            Ok(())
        } else {
            self.reset();
            Err(errors.remove(0))
        }
    }

    fn reset(&self) {
        for job in self.jobs.iter() {
            job.wait.store(job.wait_initial, Relaxed);
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

fn build_priority_order<S>(
    pending: &[PendingJob<S>],
    fixed_predecessors: &[Vec<JobId>],
    relaxed_neighbors: &[Vec<JobId>],
) -> Vec<JobId> {
    let len = pending.len();
    let fixed_successors = build_successors(fixed_predecessors);
    let stats = build_job_stats(pending, relaxed_neighbors, &fixed_successors);
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
        let best_index = select_best_available(&available, &stats);
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

fn build_job_stats<S>(
    pending: &[PendingJob<S>],
    relaxed_neighbors: &[Vec<JobId>],
    fixed_successors: &[Vec<JobId>],
) -> Vec<JobStats> {
    let len = pending.len();
    let mut stats = vec![JobStats::default(); len];

    for (job, dependencies) in pending.iter().map(|job| &job.dependencies).enumerate() {
        match dependencies {
            NormalizedDeps::Unknown => {
                stats[job].writes = 1;
                stats[job].accesses = 1;
            }
            NormalizedDeps::Known(accesses) => {
                stats[job].writes = accesses.iter().filter(|access| access.write).count() as u32;
                stats[job].accesses = accesses.len() as u32;
            }
        }
        stats[job].relaxed_degree = relaxed_neighbors[job].len() as u32;
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
        .then(left.relaxed_degree.cmp(&right.relaxed_degree))
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

fn build_final_predecessors<S>(
    pending: &[PendingJob<S>],
    fixed_predecessors: &[Vec<JobId>],
    priority: &[JobId],
    rank: &[u32],
) -> Vec<Vec<JobId>> {
    let mut predecessors = fixed_predecessors.to_vec();
    let mut events_by_key = HashMap::<Key, Vec<AccessEvent>>::new();

    for &job in priority.iter() {
        match &pending[job as usize].dependencies {
            NormalizedDeps::Unknown => {}
            NormalizedDeps::Known(accesses) => {
                for access in accesses.iter() {
                    events_by_key
                        .entry(access.key.clone())
                        .or_default()
                        .push(AccessEvent {
                            job,
                            write: access.write,
                        });
                }
            }
        }
    }

    for events in events_by_key.values() {
        let mut last_write = None::<JobId>;
        let mut read_batch = Vec::<JobId>::new();

        for event in events.iter().copied() {
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

    reduce_predecessors_priority_order(predecessors, priority, rank)
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

fn spawn_ready<'context, 'schedule, 'state, 'sync, 'scope, S: Sync>(
    context: &'context RunContext<'schedule, 'state, 'sync, S>,
    job: JobId,
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

    let node = &context.schedule.jobs[job as usize];
    debug_assert_eq!(node.wait.load(Relaxed), 0);
    scope.spawn(move |scope| run_ready_chain(context, job, scope));
}

fn run_ready_chain<'context, 'schedule, 'state, 'sync, 'scope, S: Sync>(
    context: &'context RunContext<'schedule, 'state, 'sync, S>,
    mut job: JobId,
    scope: &RayonScope<'scope>,
) where
    'context: 'scope,
    'schedule: 'scope,
    'state: 'scope,
    'sync: 'scope,
{
    loop {
        if context.cancelled.load(Acquire) {
            return;
        }

        let node = &context.schedule.jobs[job as usize];
        debug_assert_eq!(node.wait.load(Relaxed), 0);
        let result = (node.run)(context.state);
        node.wait.store(node.wait_initial, Release);

        match result {
            Ok(()) if !context.cancelled.load(Acquire) => {
                let mut next_inline = None;
                for &successor in job_successors(context.schedule, node).iter() {
                    let successor_node = &context.schedule.jobs[successor as usize];
                    if successor_node.wait.fetch_sub(1, AcqRel) == 1 {
                        if next_inline.is_none() {
                            next_inline = Some(successor);
                        } else {
                            spawn_ready(context, successor, scope);
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
                context.cancelled.store(true, Release);
                context.errors.lock().push(error);
                return;
            }
        }
    }
}

fn job_successors<'schedule, S: Sync>(
    schedule: &'schedule Schedule<S>,
    node: &Node<S>,
) -> &'schedule [JobId] {
    &schedule.successors[node.successor_range.start as usize..node.successor_range.end as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::depend::{
        Dependency::{Read, Unknown, Write},
        Key::Identifier,
    };

    fn noop() -> Run<()> {
        Box::new(|_| Ok(()))
    }

    fn compile_schedule(jobs: Vec<(Run<()>, Vec<Dependency>)>) -> Schedule<()> {
        let jobs = jobs
            .into_iter()
            .map(|(run, dependencies)| {
                let mut job = ApiJob::new(run);
                if !dependencies.is_empty() {
                    job = job.depend(dependencies);
                }
                job
            })
            .collect();

        Schedule::compile(NonZeroUsize::new(2), jobs).unwrap()
    }

    fn wait_counts(schedule: &Schedule<()>) -> Vec<u32> {
        schedule.jobs.iter().map(|job| job.wait_initial).collect()
    }

    #[test]
    fn normalize_rejects_read_write_aliasing_within_one_job() {
        let error = normalize_dependencies(vec![
            Read(Identifier(1), Order::Relax),
            Write(Identifier(1), Order::Strict),
            Read(Identifier(1), Order::Relax),
        ])
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<crate::legacy::error::Error>(),
            Some(crate::legacy::error::Error::ReadWriteConflict(
                Key::Identifier(1),
                crate::legacy::depend::Scope::Inner,
                Order::Strict
            ))
        ));
    }

    #[test]
    fn normalize_merges_duplicate_reads() {
        let normalized = normalize_dependencies(vec![
            Read(Identifier(1), Order::Relax),
            Read(Identifier(1), Order::Strict),
            Read(Identifier(1), Order::Relax),
        ])
        .unwrap();

        assert_eq!(
            normalized,
            NormalizedDeps::Known(
                vec![Access {
                    key: Key::Identifier(1),
                    order: Order::Strict,
                    write: false,
                }]
                .into_boxed_slice()
            )
        );
    }

    #[test]
    fn normalize_rejects_duplicate_writes_within_one_job() {
        let error = normalize_dependencies(vec![
            Write(Identifier(1), Order::Relax),
            Write(Identifier(1), Order::Strict),
        ])
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<crate::legacy::error::Error>(),
            Some(crate::legacy::error::Error::WriteWriteConflict(
                Key::Identifier(1),
                crate::legacy::depend::Scope::Inner,
                Order::Strict
            ))
        ));
    }

    #[test]
    fn priority_order_respects_strict_predecessors() {
        let pending = vec![
            PendingJob {
                run: noop(),
                dependencies: normalize_dependencies(vec![Write(Identifier(0), Order::Strict)])
                    .unwrap(),
            },
            PendingJob {
                run: noop(),
                dependencies: normalize_dependencies(vec![Read(Identifier(1), Order::Relax)])
                    .unwrap(),
            },
            PendingJob {
                run: noop(),
                dependencies: normalize_dependencies(vec![Write(Identifier(0), Order::Strict)])
                    .unwrap(),
            },
        ];
        let fixed = reduce_predecessors_declaration_order(&[vec![], vec![], vec![0]]);
        let relaxed = vec![vec![], vec![], vec![]];
        let order = build_priority_order(&pending, &fixed, &relaxed);
        let rank = inverse_order(&order);

        assert!(rank[0] < rank[2]);
    }

    #[test]
    fn compile_keeps_read_batches_free_of_read_read_edges() {
        let schedule = compile_schedule(vec![
            (noop(), vec![Read(Identifier(0), Order::Relax)]),
            (noop(), vec![Read(Identifier(0), Order::Relax)]),
            (noop(), vec![Write(Identifier(0), Order::Relax)]),
        ]);

        let mut waits = wait_counts(&schedule);
        waits.sort_unstable();
        assert!(
            waits == vec![0, 0, 2] || waits == vec![0, 1, 1],
            "expected a sparse orientation with no read-read edge, got {waits:?}"
        );
        assert_eq!(schedule.successors.len(), 2);
    }

    #[test]
    fn compile_treats_unknown_as_a_strict_barrier() {
        let schedule = compile_schedule(vec![
            (noop(), vec![]),
            (noop(), vec![Unknown]),
            (noop(), vec![]),
        ]);
        assert_eq!(wait_counts(&schedule), vec![0, 1, 1]);
    }
}
