use crate::{
    depend::Dependency,
    identify,
    utility::short_type_name,
    work::{as_mut, Run},
};
use std::{
    any::Any,
    convert::Infallible,
    fmt::{self},
    sync::Arc,
};

pub struct Job {
    identifier: usize,
    pub(crate) name: String,
    pub(crate) state: Arc<dyn Any + Send + Sync>,
    schedule: Box<dyn FnMut(&mut dyn Any) -> Vec<Run>>,
}

pub trait IntoJob<M = ()> {
    type Input;
    type Error;
    fn job(self, input: Self::Input) -> Result<Job, Self::Error>;
}

impl Job {
    #[inline]
    pub const fn identifier(&self) -> usize {
        self.identifier
    }

    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn schedule(&mut self) -> Vec<Run> {
        let state = as_mut(&mut self.state);
        (self.schedule)(state)
    }
}

impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple(&short_type_name::<Self>())
            .field(&self.name())
            .finish()
    }
}

pub struct Barrier;

impl IntoJob for Barrier {
    type Input = ();
    type Error = Infallible;

    fn job(self, _: Self::Input) -> Result<Job, Self::Error> {
        Ok(Job {
            identifier: identify(),
            name: "barrier".into(),
            state: Arc::new(()),
            schedule: Box::new(|_| vec![Run::new(|_| Ok(()), [Dependency::Unknown])]),
        })
    }
}
