#![doc = include_str!("README.md")]

use std::num::NonZeroUsize;

pub use crate::legacy::depend::{Dependency, Key, Order};

pub type Run<S> = Box<dyn Fn(&S) -> anyhow::Result<()> + Send + Sync + 'static>;

pub struct Job<S> {
    run: Run<S>,
    dependencies: Vec<Dependency>,
}

impl<S> Job<S> {
    pub fn new<R>(run: R) -> Self
    where
        R: Fn(&S) -> anyhow::Result<()> + Send + Sync + 'static,
    {
        Self {
            run: Box::new(run),
            dependencies: Vec::new(),
        }
    }

    pub fn depend<D: IntoIterator<Item = Dependency>>(mut self, dependencies: D) -> Self {
        self.dependencies.extend(dependencies);
        self
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }

    pub(crate) fn into_parts(self) -> (Run<S>, Vec<Dependency>) {
        (self.run, self.dependencies)
    }
}

pub trait CompiledSchedule<S: Sync> {
    fn run(&mut self, state: &S) -> anyhow::Result<()>;
}

pub trait Scheduler<S: Sync>: Sized {
    type Schedule: CompiledSchedule<S>;

    const NAME: &'static str;

    fn with_parallelism(parallelism: Option<NonZeroUsize>) -> Self;

    fn new() -> Self {
        Self::with_parallelism(None)
    }

    fn add(self, job: Job<S>) -> Self;

    fn schedule(self) -> anyhow::Result<Self::Schedule>;
}
