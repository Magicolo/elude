use crate::{
    depend::{Dependency, Key, Order, Scope},
    error::Error as ScheduleError,
    experiment::{Job as ApiJob, Run},
};
use anyhow::Result;
use rayon::{ThreadPool, ThreadPoolBuilder, prelude::*};
use std::{cmp::max, collections::HashMap, num::NonZeroUsize, ops::Range, thread};

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
                self.accesses.get(&access.key).map_or(true, |group| {
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
        let mut trackers = HashMap::<Key, Tracker>::new();
        let mut groups = Vec::<GroupSummary>::new();
        let mut grouped_runs = Vec::<Vec<Run<S>>>::new();
        let mut last_unknown = None;

        for job in jobs {
            let (run, dependencies) = job.into_parts();
            let dependencies = normalize_dependencies(dependencies)?;
            let earliest = earliest_group(&dependencies, &trackers, last_unknown, groups.len());
            let mut group = earliest as usize;

            while let Some(summary) = groups.get(group) {
                if summary.is_compatible(&dependencies) {
                    break;
                }
                group += 1;
            }

            if group == groups.len() {
                groups.push(GroupSummary::default());
                grouped_runs.push(Vec::new());
            }

            groups[group].insert(&dependencies);
            grouped_runs[group].push(run);
            update_trackers(&dependencies, group as u32, &mut trackers, &mut last_unknown);
        }

        let total_jobs = grouped_runs.iter().map(Vec::len).sum();
        let mut flattened = Vec::with_capacity(total_jobs);
        let mut compiled_groups = Vec::with_capacity(grouped_runs.len());
        let mut cursor = 0usize;
        for mut runs in grouped_runs {
            let start = cursor as u32;
            cursor += runs.len();
            let end = cursor as u32;
            compiled_groups.push(Group {
                job_range: start..end,
            });
            flattened.append(&mut runs);
        }

        let threads = parallelism
            .or_else(|| thread::available_parallelism().ok())
            .map(NonZeroUsize::get)
            .unwrap_or(1);
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
                    return Err(
                        ScheduleError::WriteWriteConflict(previous.key.clone(), Scope::Inner, order)
                            .into(),
                    );
                }
                if previous.write || access.write {
                    return Err(
                        ScheduleError::ReadWriteConflict(previous.key.clone(), Scope::Inner, order)
                            .into(),
                    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::depend::{
        Dependency::{Read, Unknown, Write},
        Key::Identifier,
    };

    fn noop() -> Run<()> {
        Box::new(|_| Ok(()))
    }

    fn compile_groups(jobs: Vec<(Run<()>, Vec<Dependency>)>) -> Schedule<()> {
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
            error.downcast_ref::<crate::error::Error>(),
            Some(crate::error::Error::ReadWriteConflict(
                Key::Identifier(1),
                crate::depend::Scope::Inner,
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
            error.downcast_ref::<crate::error::Error>(),
            Some(crate::error::Error::WriteWriteConflict(
                Key::Identifier(1),
                crate::depend::Scope::Inner,
                Order::Strict
            ))
        ));
    }

    #[test]
    fn earliest_group_respects_strict_history_only() {
        let mut trackers = HashMap::new();
        let mut last_unknown = None;

        let read_relax = normalize_dependencies(vec![Read(Identifier(0), Order::Relax)]).unwrap();
        let read_strict = normalize_dependencies(vec![Read(Identifier(0), Order::Strict)]).unwrap();
        let write_relax = normalize_dependencies(vec![Write(Identifier(0), Order::Relax)]).unwrap();

        assert_eq!(earliest_group(&read_relax, &trackers, last_unknown, 0), 0);
        update_trackers(&read_strict, 0, &mut trackers, &mut last_unknown);
        assert_eq!(earliest_group(&read_relax, &trackers, last_unknown, 1), 0);
        assert_eq!(earliest_group(&write_relax, &trackers, last_unknown, 1), 1);
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
    fn compile_treats_unknown_as_a_strict_barrier() {
        let schedule = compile_groups(vec![
            (noop(), vec![]),
            (noop(), vec![Unknown]),
            (noop(), vec![]),
        ]);

        assert_eq!(group_lengths(&schedule), vec![1, 1, 1]);
    }
}
