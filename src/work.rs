use crate::{
    depend::{Conflict, Dependency, Order},
    error::Error,
    job::{IntoJob, Job},
    utility::short_type_name,
};
use parking_lot::{RwLock, RwLockUpgradableReadGuard};
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::{
    any::{type_name, Any},
    collections::HashSet,
    error,
    num::NonZeroUsize,
    ops::Not,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
};

type Runs = Vec<(RwLock<(Run, State)>, Blockers)>;
type RunResult<T = ()> = Result<T, Box<dyn error::Error + Send + Sync>>;

pub(crate) struct Run {
    run: Box<dyn FnMut(&mut dyn Any) -> RunResult + Send + Sync>,
    dependencies: Vec<Dependency>,
}

#[derive(Debug)]
struct State {
    state: Arc<dyn Any + Send + Sync>,
    done: bool,
    error: Option<Error>,
}

#[derive(Default)]
struct Blockers {
    strong: Vec<(usize, AtomicBool, Error)>,
    weak: Vec<(usize, AtomicBool)>,
}

pub struct Worker {
    prefix: String,
    schedule: bool,
    jobs: Vec<Job>,
    control: bool,
    runs: Runs,
    conflict: Conflict,
    pool: ThreadPool,
}

impl Run {
    pub fn new(
        mut run: impl FnMut(&mut dyn Any) -> RunResult + Send + Sync + 'static,
        dependencies: impl IntoIterator<Item = Dependency>,
    ) -> Self {
        Self {
            run: Box::new(move |state| run(state)),
            dependencies: dependencies.into_iter().collect(),
        }
    }

    pub(crate) fn run(&mut self, state: &mut dyn Any) -> RunResult {
        (self.run)(state)
    }

    #[inline]
    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }
}

impl Worker {
    pub fn new(parallelism: Option<NonZeroUsize>) -> Result<Self, Error> {
        let parallelism = parallelism
            .or_else(|| thread::available_parallelism().ok())
            .map(NonZeroUsize::get)
            .unwrap_or(0);
        Ok(Self {
            prefix: String::new(),
            schedule: true,
            jobs: Vec::new(),
            control: false,
            runs: vec![],
            conflict: Conflict::default(),
            pool: ThreadPoolBuilder::new()
                .num_threads(parallelism)
                .build()
                .map_err(|error| Error::Dynamic(error.into()))?,
        })
    }

    pub fn push<M, J: IntoJob<M>>(&mut self, job: J) -> Result<(), J::Error>
    where
        J::Input: Default,
    {
        self.push_with(J::Input::default(), job)
    }

    pub fn push_with<M, J: IntoJob<M>>(&mut self, input: J::Input, job: J) -> Result<(), J::Error> {
        self.with_prefix::<J, J::Error, _>(|worker| {
            let mut job = job.job(input)?;
            job.name.insert_str(0, &worker.prefix);
            worker.jobs.push(job);
            worker.schedule = true;
            Ok(())
        })
    }

    pub fn clear(&mut self) {
        self.schedule = true;
        self.jobs.clear();
    }

    #[inline]
    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    pub fn update(&mut self) -> Result<bool, Error> {
        if self.schedule {
            self.runs = self
                .jobs
                .iter_mut()
                .flat_map(|job| {
                    job.schedule().into_iter().map(|run| {
                        (
                            RwLock::new((
                                run,
                                State {
                                    state: job.state.clone(),
                                    done: self.control,
                                    error: None,
                                },
                            )),
                            Blockers::default(),
                        )
                    })
                })
                .collect();
            self.schedule()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn schedule(&mut self) -> Result<(), Error> {
        // TODO: This algorithm scales poorly (n^2 / 2, where n is the number of runs) and a lot of the work is redundant
        // as shown by the refining part. This could be optimized.
        let mut runs = &mut self.runs[..];
        while let Some((tail, rest)) = runs.split_last_mut() {
            let (run, _) = tail.0.get_mut();
            self.conflict.detect_inner(&run.dependencies, true)?;

            let index = rest.len();
            for (i, rest) in rest.iter_mut().enumerate() {
                let pair = rest.0.get_mut();
                match self.conflict.detect_outer(&pair.0.dependencies, true) {
                    Ok(Order::Strict) => {}
                    Ok(Order::Relax) => {
                        tail.1.weak.push((i, self.control.into()));
                        rest.1.weak.push((index, self.control.into()));
                    }
                    Err(error) => tail.1.strong.push((i, self.control.into(), error)),
                }
            }

            runs = rest;
        }

        self.refine_strong_blockers();
        // self.refine_weak_blockers();
        self.schedule = false;
        Ok(())
    }

    pub fn run(&mut self) -> Result<(), Error> {
        self.update()?;

        let Self {
            control,
            runs,
            pool,
            ..
        } = self;

        *control = control.not();

        let index = AtomicUsize::new(0);
        let success = AtomicBool::new(true);
        let control = *control;
        pool.scope(|scope| {
            for _ in 0..pool.current_num_threads() {
                scope.spawn(|_| {
                    success.fetch_and(Self::progress(&index, runs, control), Ordering::Relaxed);
                });
            }
        });

        if success.into_inner() {
            Ok(())
        } else {
            // Err(Error::FailedToRun)
            Error::All(
                runs.iter_mut()
                    .filter_map(|(run, _)| run.get_mut().1.error.take())
                    .collect(),
            )
            .flatten(true)
            .map_or(Err(Error::FailedToRun), Err)
        }
    }

    /// The synchronization mechanism is in 2 parts:
    /// 1. An `AtomicUsize` is used to reserve an index in the `runs` vector. It ensures that each run is executed only once.
    /// 2. A `Mutex` around the run and its state that will force the blocked threads to wait until this run is done. This choice
    /// of synchronization prevents sneaky interleaving of threads and is very straightforward to implement.
    /// - This mechanism can not produce a dead lock as long as the `blockers` are all indices `< index` (which they are by design).
    /// Since runs are executed in order, for any index that is reserved, all indices smaller than that index represent a run that
    /// is done or in progress (not idle) which is important to prevent a spin loop when waiting for `blockers` to finish.
    /// - This mechanism has a lookahead that is equal to the degree of parallelism which is currently the number of logical CPUs by default.
    fn progress(index: &AtomicUsize, runs: &Runs, control: bool) -> bool {
        loop {
            // `Ordering` doesn't matter here, only atomicity.
            let index = index.fetch_add(1, Ordering::Relaxed);
            match step(index, runs, control, false) {
                Some(true) => continue,
                Some(false) => loop {
                    match step(index, runs, control, true) {
                        Some(true) => break,
                        Some(false) => {
                            if rayon::yield_now().is_none() {
                                thread::yield_now()
                            }
                        }
                        None => return false,
                    }
                },
                None => return false,
            }
        }

        fn step(index: usize, runs: &Runs, control: bool, lock: bool) -> Option<bool> {
            let (run, blockers) = match runs.get(index) {
                Some(run) => run,
                None => return Some(true),
            };

            match if lock {
                Some(run.read())
            } else {
                run.try_read()
            } {
                Some(guard) if guard.1.done == control => return Some(true),
                Some(guard) if guard.1.error.is_some() => return None,
                _ => {}
            }

            let mut ready = true;
            for (blocker, done, _) in blockers.strong.iter() {
                debug_assert!(*blocker < index);

                if done.load(Ordering::Acquire) == control {
                    continue;
                }

                // Do not try to `progress` here since if `blocker < index`, it is expected that the blocker index will be
                // locked by its responsible thread imminently. So this lock should be kept for the least amount of time.
                match if lock {
                    Some(runs[*blocker].0.read())
                } else {
                    runs[*blocker].0.try_read()
                } {
                    Some(guard) if guard.1.done == control => {
                        done.store(control, Ordering::Release)
                    }
                    Some(guard) if guard.1.error.is_some() => return None,
                    Some(_) | None => ready = false,
                };
            }

            // TODO: Reuse a vector?
            let mut guards = Vec::new();
            for (blocker, done) in blockers.weak.iter() {
                if done.load(Ordering::Acquire) == control {
                    continue;
                }

                match runs[*blocker].0.try_read() {
                    Some(guard) if guard.1.done => done.store(control, Ordering::Release),
                    Some(guard) if guard.1.error.is_some() => return None,
                    Some(guard) if ready => guards.push(guard),
                    guard => {
                        drop(guard);
                        if !guards.is_empty() {
                            guards.clear();
                            ready = false;
                        } else if step(*blocker, runs, control, false)? {
                            done.store(control, Ordering::Release);
                        } else {
                            ready = false;
                        }
                    }
                };
            }

            if ready {
                let guard = run.upgradable_read();
                if guard.1.done == control {
                    return Some(true);
                } else if guard.1.error.is_some() {
                    return None;
                }

                let mut guard = RwLockUpgradableReadGuard::upgrade(guard);
                let input = as_mut(&mut guard.1.state);
                let result = guard.0.run(input);
                guards.clear();
                match result {
                    Ok(_) => {
                        guard.1.done = control;
                        Some(true)
                    }
                    Err(error) => {
                        guard.1.error = Some(Error::Dynamic(error));
                        None
                    }
                }
            } else {
                Some(false)
            }
        }
    }

    /// Remove transitive pre blockers.
    fn refine_strong_blockers(&mut self) {
        let mut runs = &mut self.runs[..];
        let mut set = HashSet::new();
        while let Some(((_, tail), rest)) = runs.split_last_mut() {
            for &(blocker, _, _) in tail.strong.iter() {
                // `rest[blocker]` ensures that `blocker < rest.len()` which is important when running.
                let strong = rest[blocker].1.strong.iter();
                set.extend(strong.map(|&(blocker, _, _)| blocker));
            }
            tail.strong.retain(|(blocker, _, _)| !set.contains(blocker));
            set.clear();
            runs = rest;
        }
    }

    /// Removes the post blockers that have pre blockers later than the current run.
    // fn refine_weak_blockers(&mut self) {
    //     let mut runs = &mut self.runs[..];
    //     let mut index = 0;
    //     while let Some((head, rest)) = runs.split_first_mut() {
    //         let (_, state) = head.get_mut();
    //         index += 1;
    //         state.weak_blockers.retain(|&(blocker, _)| {
    //             let (_, next) = rest[blocker - index].get_mut();
    //             next.strong_blockers
    //                 .iter()
    //                 .all(|&(blocker, _, _)| blocker < index)
    //         });
    //         runs = rest;
    //     }
    // }

    fn with_prefix<T, E, F: FnOnce(&mut Self) -> Result<(), E>>(
        &mut self,
        with: F,
    ) -> Result<(), E> {
        let count = self.prefix.len();
        let prefix = if count == 0 {
            format!("{}::", type_name::<T>())
        } else {
            format!("{}::", short_type_name::<T>())
        };
        self.prefix.push_str(&prefix);
        with(self)?;
        self.prefix.truncate(count);
        Ok(())
    }
}

#[inline]
pub(crate) fn as_mut<'a, T: ?Sized>(state: &mut Arc<T>) -> &'a mut T {
    unsafe { &mut *(Arc::as_ptr(&state) as *mut T) }
}
