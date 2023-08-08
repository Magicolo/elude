use crate::{depend::Dependency, utility::short_type_name};
use std::{
    convert::Infallible,
    error,
    fmt::{self},
};

pub trait State: Send + Sync {}
impl<T: Send + Sync> State for T {}

type Run =
    Box<dyn FnMut(&mut dyn State) -> Result<(), Box<dyn error::Error + Send + Sync>> + Send + Sync>;

pub struct Job {
    pub(crate) run: Run,
    pub(crate) state: Box<dyn State>,
    pub(crate) name: String,
    pub(crate) dependencies: Vec<Dependency>,
}

pub trait IntoJob<S> {
    type Error;
    fn job(self, state: &mut S) -> Result<Job, Self::Error>;
}

unsafe impl Sync for Job {}

impl dyn State {
    pub fn cast<T>(&mut self) -> &mut T {
        todo!()
    }
}

impl Job {
    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[inline]
    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
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

impl<S> IntoJob<S> for Barrier {
    type Error = Infallible;

    fn job(self, _: &mut S) -> Result<Job, Self::Error> {
        Ok(Job {
            name: "barrier".into(),
            dependencies: vec![Dependency::Unknown],
            run: Box::new(|_| Ok(())),
            state: Box::new(()),
        })
    }
}
