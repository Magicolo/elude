#![doc = include_str!("README.md")]

mod model;

use crate::legacy::experiment::Job;
use std::num::NonZeroUsize;

pub struct Scheduler<S: Sync> {
    parallelism: Option<NonZeroUsize>,
    jobs: Vec<Job<S>>,
}

pub struct Compiled<S: Sync>(model::Schedule<S>);

impl<S: Sync> crate::legacy::experiment::Scheduler<S> for Scheduler<S> {
    type Schedule = Compiled<S>;

    const NAME: &'static str = "experiment_02";

    fn with_parallelism(parallelism: Option<NonZeroUsize>) -> Self {
        Self {
            parallelism,
            jobs: Vec::new(),
        }
    }

    fn add(mut self, job: Job<S>) -> Self {
        self.jobs.push(job);
        self
    }

    fn schedule(self) -> anyhow::Result<Self::Schedule> {
        Ok(Compiled(model::Schedule::compile(
            self.parallelism,
            self.jobs,
        )?))
    }
}

impl<S: Sync> crate::legacy::experiment::CompiledSchedule<S> for Compiled<S> {
    fn run(&mut self, state: &S) -> anyhow::Result<()> {
        self.0.run(state)
    }
}
