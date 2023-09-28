use crate::depend::Dependency;

pub(crate) type Run<'a, S> = Box<dyn FnMut(&mut S) -> anyhow::Result<()> + Send + Sync + 'a>;

pub struct Job<'a, S> {
    pub(crate) run: Run<'a, S>,
    pub(crate) dependencies: Vec<Dependency>,
}

impl<'a, S> Job<'a, S> {
    /// # Safety
    /// This library heavily relies on the fact the `dependencies` are exhaustively declared for the `run` closure.
    /// Any omitted dependency may lead to undefined behavior.
    pub unsafe fn new<
        R: FnMut(&mut S) -> anyhow::Result<()> + Send + Sync + 'a,
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

    pub fn with<R: FnMut() -> anyhow::Result<()> + Send + Sync + 'a>(mut run: R) -> Self {
        unsafe { Self::new(move |_| run(), []) }
    }

    pub fn ok() -> Self {
        Self::with(|| Ok(()))
    }

    pub fn barrier() -> Self {
        Self::ok().depend([Dependency::Unknown])
    }

    pub fn depend<D: IntoIterator<Item = Dependency>>(mut self, dependencies: D) -> Self {
        self.dependencies.extend(dependencies);
        self
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }
}
