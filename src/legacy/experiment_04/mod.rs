#![doc = include_str!("README.md")]

mod model;

use crate::legacy::experiment::Job;
pub use model::{Policy, VariantKind};
use std::num::NonZeroUsize;

pub struct Scheduler<S: Sync> {
    parallelism: Option<NonZeroUsize>,
    policy: Policy,
    jobs: Vec<Job<S>>,
}

pub struct Compiled<S: Sync>(model::Schedule<S>);

impl<S: Sync> Scheduler<S> {
    pub fn with_parallelism_and_policy(parallelism: Option<NonZeroUsize>, policy: Policy) -> Self {
        Self {
            parallelism,
            policy,
            jobs: Vec::new(),
        }
    }

    pub fn with_policy(policy: Policy) -> Self {
        Self::with_parallelism_and_policy(None, policy)
    }
}

impl<S: Sync> crate::legacy::experiment::Scheduler<S> for Scheduler<S> {
    type Schedule = Compiled<S>;

    const NAME: &'static str = "experiment_04";

    fn with_parallelism(parallelism: Option<NonZeroUsize>) -> Self {
        Self::with_parallelism_and_policy(parallelism, Policy::Adaptive)
    }

    fn add(mut self, job: Job<S>) -> Self {
        self.jobs.push(job);
        self
    }

    fn schedule(self) -> anyhow::Result<Self::Schedule> {
        Ok(Compiled(model::Schedule::compile(
            self.parallelism,
            self.policy,
            self.jobs,
        )?))
    }
}

impl<S: Sync> crate::legacy::experiment::CompiledSchedule<S> for Compiled<S> {
    fn run(&mut self, state: &S) -> anyhow::Result<()> {
        self.0.run(state)
    }
}
