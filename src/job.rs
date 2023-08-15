use crate::depend::Dependency;
use std::error;

type RunError = Box<dyn error::Error + Send + Sync>;
type RunResult = Result<(), RunError>;

pub trait Run {
    fn run(&mut self) -> RunResult;
}

pub struct Job<'a> {
    pub(crate) run: Box<dyn FnMut() -> RunResult + Send + Sync + 'a>,
    pub(crate) dependencies: Vec<Dependency>,
}

impl<'a> Job<'a> {
    /// # Safety
    /// This library heavily relies on the fact the `dependencies` are exhaustively declared for the `run` closure.
    /// Any omitted dependency may lead to undefined behavior.
    pub unsafe fn new<
        R: FnMut() -> RunResult + Send + Sync + 'a,
        D: IntoIterator<Item = Dependency>,
    >(
        run: R,
        dependencies: D,
    ) -> Self {
        Self {
            run: Box::new(run),
            dependencies: dependencies.into_iter().collect(),
        }
    }

    pub fn with<R: FnMut() -> RunResult + Send + Sync + 'a>(run: R) -> Self {
        unsafe { Self::new(run, []) }
    }

    pub fn ok() -> Self {
        Self::with(|| Ok(()))
    }

    pub fn barrier() -> Self {
        Self::ok().depend([Dependency::Unknown])
    }

    pub fn run(&mut self) -> Result<(), RunError> {
        (self.run)()
    }

    pub fn depend<D: IntoIterator<Item = Dependency>>(mut self, dependencies: D) -> Self {
        self.dependencies.extend(dependencies);
        self
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }
}
