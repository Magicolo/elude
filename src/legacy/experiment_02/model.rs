use crate::legacy::{
    depend::{Dependency, Key, Order, Scope},
    error::Error as ScheduleError,
    experiment::{Job as ApiJob, Run},
};
use anyhow::Result;
use rayon::{prelude::*, ThreadPool, ThreadPoolBuilder};
use std::{
    cmp::{max, Reverse},
    collections::HashMap,
    num::NonZeroUsize,
    ops::Range,
    thread,
};

pub type JobId = u32;

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

impl NormalizedDeps {
    #[inline]
    fn access_count(&self) -> usize {
        match self {
            Self::Unknown => 0,
            Self::Known(accesses) => accesses.len(),
        }
    }
}

struct PendingJob<S> {
    run: Option<Run<S>>,
    dependencies: NormalizedDeps,
    access_count: usize,
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
struct Tracker {
    last_read: Option<u32>,
    last_write: Option<u32>,
    last_strict_read: Option<u32>,
    last_strict_write: Option<u32>,
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
struct GroupAccess {
    has_read: bool,
    has_write: bool,
}

#[derive(Debug, Default)]
struct GroupSummary {
    unknown: bool,
    accesses: HashMap<Key, GroupAccess>,
}

impl GroupSummary {
    #[inline]
    fn is_empty(&self) -> bool {
        !self.unknown && self.accesses.is_empty()
    }

    #[inline]
    fn is_compatible(&self, dependencies: &NormalizedDeps) -> bool {
        match dependencies {
            NormalizedDeps::Unknown => self.is_empty(),
            NormalizedDeps::Known(_) if self.unknown => false,
            NormalizedDeps::Known(accesses) => accesses.iter().all(|access| {
                self.accesses.get(&access.key).is_none_or(|group| {
                    if access.write {
                        !(group.has_read || group.has_write)
                    } else {
                        !group.has_write
                    }
                })
            }),
        }
    }

    #[inline]
    fn insert(&mut self, dependencies: &NormalizedDeps) {
        match dependencies {
            NormalizedDeps::Unknown => self.unknown = true,
            NormalizedDeps::Known(accesses) => {
                for access in accesses.iter() {
                    let entry = self.accesses.entry(access.key.clone()).or_default();
                    if access.write {
                        entry.has_write = true;
                    } else {
                        entry.has_read = true;
                    }
                }
            }
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Relation {
    None,
    Relaxed,
    Strict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub job_range: Range<u32>,
}

pub struct Schedule<S: Sync> {
    pool: ThreadPool,
    threads: usize,
    jobs: Box<[Run<S>]>,
    pub groups: Box<[Group]>,
}

impl<S: Sync> Schedule<S> {
    pub fn compile(parallelism: Option<NonZeroUsize>, jobs: Vec<ApiJob<S>>) -> Result<Self> {
        let threads = parallelism
            .or_else(|| thread::available_parallelism().ok())
            .map(NonZeroUsize::get)
            .unwrap_or(1);

        let mut pending = Vec::with_capacity(jobs.len());
        for job in jobs {
            let (run, dependencies) = job.into_parts();
            let dependencies = normalize_dependencies(dependencies)?;
            let access_count = dependencies.access_count();
            pending.push(PendingJob {
                run: Some(run),
                dependencies,
                access_count,
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

        let predecessors = reduce_strict(&strict_candidates);
        let mut successors = vec![Vec::<JobId>::new(); count];
        for (job, predecessors) in predecessors.iter().enumerate() {
            for &predecessor in predecessors.iter() {
                successors[predecessor as usize].push(job as JobId);
            }
        }

        let grouped_jobs = select_group_layout(
            &pending,
            &predecessors,
            &successors,
            &relaxed_neighbors,
            threads,
        );

        let total_jobs = grouped_jobs.iter().map(Vec::len).sum();
        let mut flattened = Vec::with_capacity(total_jobs);
        let mut compiled_groups = Vec::with_capacity(grouped_jobs.len());
        let mut cursor = 0u32;
        for jobs in grouped_jobs {
            let start = cursor;
            for job in jobs {
                flattened.push(
                    pending[job as usize]
                        .run
                        .take()
                        .expect("job run must be moved exactly once"),
                );
                cursor += 1;
            }
            compiled_groups.push(Group {
                job_range: start..cursor,
            });
        }

        let pool = ThreadPoolBuilder::new().num_threads(threads).build()?;

        Ok(Self {
            pool,
            threads,
            jobs: flattened.into_boxed_slice(),
            groups: compiled_groups.into_boxed_slice(),
        })
    }

    pub fn run(&self, state: &S) -> Result<()> {
        self.pool.install(|| {
            for group in self.groups.iter() {
                let jobs = &self.jobs[group.job_range.start as usize..group.job_range.end as usize];
                if self.threads == 1 || jobs.len() <= 1 {
                    for run in jobs.iter() {
                        run(state)?;
                    }
                } else {
                    jobs.par_iter().try_for_each(|run| run(state))?;
                }
            }
            Ok(())
        })
    }
}

fn select_group_layout<S>(
    pending: &[PendingJob<S>],
    predecessors: &[Vec<JobId>],
    successors: &[Vec<JobId>],
    relaxed_neighbors: &[Vec<JobId>],
    threads: usize,
) -> Vec<Vec<JobId>> {
    let first_fit = compile_first_fit_layout(pending);
    let frontier = compile_frontier_layout(pending, predecessors, successors, relaxed_neighbors);
    let frontier_max_width = frontier.iter().map(Vec::len).max().unwrap_or(0);
    if frontier_max_width <= threads
        && layout_score(&frontier, threads) < layout_score(&first_fit, threads)
    {
        frontier
    } else {
        first_fit
    }
}

fn compile_first_fit_layout<S>(pending: &[PendingJob<S>]) -> Vec<Vec<JobId>> {
    let mut trackers = HashMap::<Key, Tracker>::new();
    let mut groups = Vec::<GroupSummary>::new();
    let mut grouped_jobs = Vec::<Vec<JobId>>::new();
    let mut last_unknown = None;

    for (job, pending) in pending.iter().enumerate() {
        let earliest = earliest_group(&pending.dependencies, &trackers, last_unknown, groups.len());
        let mut group = earliest as usize;

        while let Some(summary) = groups.get(group) {
            if summary.is_compatible(&pending.dependencies) {
                break;
            }
            group += 1;
        }

        if group == groups.len() {
            groups.push(GroupSummary::default());
            grouped_jobs.push(Vec::new());
        }

        groups[group].insert(&pending.dependencies);
        grouped_jobs[group].push(job as JobId);
        update_trackers(
            &pending.dependencies,
            group as u32,
            &mut trackers,
            &mut last_unknown,
        );
    }

    grouped_jobs
}

fn compile_frontier_layout<S>(
    pending: &[PendingJob<S>],
    predecessors: &[Vec<JobId>],
    successors: &[Vec<JobId>],
    relaxed_neighbors: &[Vec<JobId>],
) -> Vec<Vec<JobId>> {
    let count = pending.len();
    let strict_heights = compute_strict_heights(successors);
    let relaxed_sets = build_relaxed_sets(relaxed_neighbors);
    let mut remaining_relaxed_degree = relaxed_neighbors
        .iter()
        .map(|neighbors| neighbors.len() as u32)
        .collect::<Vec<_>>();
    let mut indegree = predecessors
        .iter()
        .map(|predecessors| predecessors.len() as u32)
        .collect::<Vec<_>>();
    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(job, &indegree)| (indegree == 0).then_some(job as JobId))
        .collect::<Vec<_>>();
    let mut scheduled = vec![false; count];
    let mut grouped_jobs = Vec::<Vec<JobId>>::new();
    let mut scheduled_count = 0usize;

    while scheduled_count < count {
        debug_assert!(
            !ready.is_empty(),
            "strict dependency graph must always expose at least one ready job"
        );

        let mut group = build_group(
            &mut ready,
            pending,
            &relaxed_sets,
            &remaining_relaxed_degree,
            &strict_heights,
        );
        group.sort_unstable();
        scheduled_count += group.len();

        for &job in group.iter() {
            scheduled[job as usize] = true;
        }

        let mut newly_ready = Vec::new();
        for &job in group.iter() {
            for &neighbor in relaxed_neighbors[job as usize].iter() {
                if !scheduled[neighbor as usize] {
                    remaining_relaxed_degree[neighbor as usize] -= 1;
                }
            }
            for &successor in successors[job as usize].iter() {
                let successor_indegree = &mut indegree[successor as usize];
                *successor_indegree -= 1;
                if *successor_indegree == 0 {
                    newly_ready.push(successor);
                }
            }
        }

        ready.extend(newly_ready);
        grouped_jobs.push(group);
    }

    grouped_jobs
}

fn build_group<S>(
    ready: &mut Vec<JobId>,
    pending: &[PendingJob<S>],
    relaxed_sets: &[Vec<u64>],
    remaining_relaxed_degree: &[u32],
    strict_heights: &[u32],
) -> Vec<JobId> {
    let seed_index = ready
        .iter()
        .enumerate()
        .max_by_key(|&(_, &job)| {
            seed_priority(job, pending, remaining_relaxed_degree, strict_heights)
        })
        .map(|(index, _)| index)
        .expect("ready set must not be empty");
    let seed = ready.swap_remove(seed_index);

    let mut summary = GroupSummary::default();
    summary.insert(&pending[seed as usize].dependencies);
    let mut group = vec![seed];

    loop {
        let candidate_index = ready
            .iter()
            .enumerate()
            .filter(|&(_, &job)| summary.is_compatible(&pending[job as usize].dependencies))
            .max_by_key(|&(_, &job)| {
                group_candidate_score(
                    job,
                    &group,
                    pending,
                    relaxed_sets,
                    remaining_relaxed_degree,
                    strict_heights,
                )
            })
            .map(|(index, _)| index);

        let Some(candidate_index) = candidate_index else {
            break;
        };

        let candidate = ready.swap_remove(candidate_index);
        summary.insert(&pending[candidate as usize].dependencies);
        group.push(candidate);
    }

    group
}

fn seed_priority<S>(
    job: JobId,
    pending: &[PendingJob<S>],
    remaining_relaxed_degree: &[u32],
    strict_heights: &[u32],
) -> (u32, u32, usize, Reverse<JobId>) {
    (
        strict_heights[job as usize],
        remaining_relaxed_degree[job as usize],
        pending[job as usize].access_count,
        Reverse(job),
    )
}

fn group_candidate_score<S>(
    job: JobId,
    group: &[JobId],
    pending: &[PendingJob<S>],
    relaxed_sets: &[Vec<u64>],
    remaining_relaxed_degree: &[u32],
    strict_heights: &[u32],
) -> (Reverse<u32>, u32, u32, usize, Reverse<JobId>) {
    let overlap = group
        .iter()
        .map(|&member| {
            bit_intersection_count(&relaxed_sets[job as usize], &relaxed_sets[member as usize])
        })
        .sum();

    (
        Reverse(remaining_relaxed_degree[job as usize]),
        overlap,
        strict_heights[job as usize],
        pending[job as usize].access_count,
        Reverse(job),
    )
}

fn build_relaxed_sets(relaxed_neighbors: &[Vec<JobId>]) -> Vec<Vec<u64>> {
    let words = relaxed_neighbors.len().div_ceil(64);
    let mut sets = vec![vec![0u64; words]; relaxed_neighbors.len()];
    for (job, neighbors) in relaxed_neighbors.iter().enumerate() {
        for &neighbor in neighbors.iter() {
            bit_set(&mut sets[job], neighbor as usize);
        }
    }
    sets
}

fn compute_strict_heights(successors: &[Vec<JobId>]) -> Vec<u32> {
    let mut heights = vec![0u32; successors.len()];
    for job in (0..successors.len()).rev() {
        heights[job] = successors[job]
            .iter()
            .map(|&successor| heights[successor as usize] + 1)
            .max()
            .unwrap_or(0);
    }
    heights
}

fn layout_score(groups: &[Vec<JobId>], threads: usize) -> (usize, usize, usize) {
    let threads = threads.max(1);
    let waves = groups
        .iter()
        .map(|group| group.len().div_ceil(threads))
        .sum::<usize>();
    let group_count = groups.len();
    let max_width = groups.iter().map(Vec::len).max().unwrap_or(0);
    (waves, group_count, max_width)
}

fn earliest_group(
    dependencies: &NormalizedDeps,
    trackers: &HashMap<Key, Tracker>,
    last_unknown: Option<u32>,
    group_count: usize,
) -> u32 {
    match dependencies {
        NormalizedDeps::Unknown => group_count as u32,
        NormalizedDeps::Known(accesses) => {
            let mut earliest = last_unknown.map_or(0, |group| group + 1);
            for access in accesses.iter() {
                let tracker = trackers.get(&access.key).copied().unwrap_or_default();
                let predecessor = if access.write {
                    if access.order == Order::Strict {
                        max_option(tracker.last_read, tracker.last_write)
                    } else {
                        max_option(tracker.last_strict_read, tracker.last_strict_write)
                    }
                } else if access.order == Order::Strict {
                    tracker.last_write
                } else {
                    tracker.last_strict_write
                };

                if let Some(group) = predecessor {
                    earliest = earliest.max(group + 1);
                }
            }
            earliest
        }
    }
}

#[inline]
fn max_option(left: Option<u32>, right: Option<u32>) -> Option<u32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn update_trackers(
    dependencies: &NormalizedDeps,
    group: u32,
    trackers: &mut HashMap<Key, Tracker>,
    last_unknown: &mut Option<u32>,
) {
    match dependencies {
        NormalizedDeps::Unknown => *last_unknown = Some(group),
        NormalizedDeps::Known(accesses) => {
            for access in accesses.iter() {
                let tracker = trackers.entry(access.key.clone()).or_default();
                if access.write {
                    tracker.last_write = Some(group);
                    if access.order == Order::Strict {
                        tracker.last_strict_write = Some(group);
                    }
                } else {
                    tracker.last_read = Some(group);
                    if access.order == Order::Strict {
                        tracker.last_strict_read = Some(group);
                    }
                }
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
    let mut merged = Vec::<Access>::with_capacity(accesses.len());
    for access in accesses {
        match merged.last_mut() {
            Some(previous) if previous.key == access.key => {
                let order = max(previous.order, access.order);
                if previous.write && access.write {
                    return Err(ScheduleError::WriteWriteConflict(
                        previous.key.clone(),
                        Scope::Inner,
                        order,
                    )
                    .into());
                }
                if previous.write || access.write {
                    return Err(ScheduleError::ReadWriteConflict(
                        previous.key.clone(),
                        Scope::Inner,
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
                    std::cmp::Ordering::Less => indices.0 += 1,
                    std::cmp::Ordering::Greater => indices.1 += 1,
                    std::cmp::Ordering::Equal => {
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

fn reduce_strict(strict_candidates: &[Vec<JobId>]) -> Vec<Vec<JobId>> {
    let len = strict_candidates.len();
    let words = len.div_ceil(64);
    let mut predecessors = vec![Vec::<JobId>::new(); len];
    let mut ancestors = vec![vec![0u64; words]; len];

    for right in 0..len {
        for &candidate in strict_candidates[right].iter().rev() {
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
        for &predecessor in predecessors[right].iter() {
            bit_set(right_ancestors, predecessor as usize);
            bit_or(right_ancestors, &before_right[predecessor as usize]);
        }
    }

    predecessors
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

#[inline]
fn bit_intersection_count(left: &[u64], right: &[u64]) -> u32 {
    left.iter()
        .zip(right.iter())
        .map(|(left, right)| (left & right).count_ones())
        .sum()
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

    fn compile_groups(jobs: Vec<(Run<()>, Vec<Dependency>)>) -> Schedule<()> {
        compile_groups_with_parallelism(2, jobs)
    }

    fn compile_groups_with_parallelism(
        parallelism: usize,
        jobs: Vec<(Run<()>, Vec<Dependency>)>,
    ) -> Schedule<()> {
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

        Schedule::compile(NonZeroUsize::new(parallelism), jobs).unwrap()
    }

    fn group_lengths(schedule: &Schedule<()>) -> Vec<u32> {
        schedule
            .groups
            .iter()
            .map(|group| group.job_range.end - group.job_range.start)
            .collect()
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
    fn relation_distinguishes_none_relaxed_and_strict() {
        let none = relation(
            &normalize_dependencies(vec![Read(Identifier(0), Order::Relax)]).unwrap(),
            &normalize_dependencies(vec![Read(Identifier(0), Order::Strict)]).unwrap(),
        );
        let relaxed = relation(
            &normalize_dependencies(vec![Read(Identifier(0), Order::Relax)]).unwrap(),
            &normalize_dependencies(vec![Write(Identifier(0), Order::Relax)]).unwrap(),
        );
        let strict = relation(
            &normalize_dependencies(vec![Write(Identifier(0), Order::Relax)]).unwrap(),
            &normalize_dependencies(vec![Write(Identifier(0), Order::Strict)]).unwrap(),
        );

        assert_eq!(none, Relation::None);
        assert_eq!(relaxed, Relation::Relaxed);
        assert_eq!(strict, Relation::Strict);
    }

    #[test]
    fn strict_reduction_drops_transitive_edges() {
        let reduced = reduce_strict(&[vec![], vec![0], vec![0, 1]]);
        assert_eq!(reduced, vec![vec![], vec![0], vec![1]]);
    }

    #[test]
    fn compile_packs_independent_relaxed_conflicts_into_layers() {
        let schedule = compile_groups(vec![
            (noop(), vec![Write(Identifier(0), Order::Relax)]),
            (noop(), vec![Write(Identifier(1), Order::Relax)]),
            (noop(), vec![Write(Identifier(0), Order::Relax)]),
            (noop(), vec![Write(Identifier(1), Order::Relax)]),
        ]);

        assert_eq!(group_lengths(&schedule), vec![2, 2]);
    }

    #[test]
    fn compile_can_outperform_online_first_fit_on_crown_conflicts() {
        let schedule = compile_groups_with_parallelism(
            8,
            vec![
                (
                    noop(),
                    vec![
                        Write(Identifier(0), Order::Relax),
                        Write(Identifier(1), Order::Relax),
                    ],
                ),
                (
                    noop(),
                    vec![
                        Write(Identifier(2), Order::Relax),
                        Write(Identifier(4), Order::Relax),
                    ],
                ),
                (
                    noop(),
                    vec![
                        Write(Identifier(2), Order::Relax),
                        Write(Identifier(3), Order::Relax),
                    ],
                ),
                (
                    noop(),
                    vec![
                        Write(Identifier(0), Order::Relax),
                        Write(Identifier(5), Order::Relax),
                    ],
                ),
                (
                    noop(),
                    vec![
                        Write(Identifier(4), Order::Relax),
                        Write(Identifier(5), Order::Relax),
                    ],
                ),
                (
                    noop(),
                    vec![
                        Write(Identifier(1), Order::Relax),
                        Write(Identifier(3), Order::Relax),
                    ],
                ),
            ],
        );

        assert_eq!(group_lengths(&schedule), vec![3, 3]);
    }

    #[test]
    fn compile_treats_unknown_as_a_strict_barrier() {
        let schedule = compile_groups(vec![
            (noop(), vec![]),
            (noop(), vec![Unknown]),
            (noop(), vec![]),
        ]);

        assert_eq!(group_lengths(&schedule), vec![1, 1, 1]);
    }
}
