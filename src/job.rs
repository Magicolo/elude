use crate::{depend::Dependency, utility::short_type_name};
use std::{
    borrow::Cow,
    error,
    fmt::{self},
};

pub trait State: Send + Sync {}
impl<T: Send + Sync> State for T {}

type Error = Box<dyn error::Error + Send + Sync>;
type Run<'a> = Box<dyn FnMut(&mut dyn State) -> Result<(), Error> + Send + Sync + 'a>;

pub struct Job<'a> {
    pub(crate) run: Run<'a>,
    pub(crate) state: Box<dyn State + 'a>,
    pub(crate) name: Cow<'static, str>,
    pub(crate) dependencies: Vec<Dependency>,
}

impl dyn State {
    pub fn cast<T>(&mut self) -> &mut T {
        todo!()
    }
}

impl Job<'_> {
    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[inline]
    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }
}

impl fmt::Debug for Job<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple(&short_type_name::<Self>())
            .field(&self.name())
            .finish()
    }
}

pub fn barrier<'a>() -> Job<'a> {
    Job {
        name: "barrier".into(),
        dependencies: vec![Dependency::Unknown],
        run: Box::new(|_| Ok(())),
        state: Box::new(()),
    }
}
