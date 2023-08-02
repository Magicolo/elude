use std::sync::atomic::{AtomicUsize, Ordering};

pub mod depend;
pub mod error;
pub mod job;
mod utility;
pub mod work;

pub trait Fold<T> {
    fn fold(self, item: T) -> Self;
}

impl<T> Fold<T> for () {
    fn fold(self, _: T) -> Self {}
}

impl<T> Fold<T> for Vec<T> {
    fn fold(mut self, item: T) -> Self {
        self.push(item);
        self
    }
}

fn identify() -> usize {
    static COUNTER: AtomicUsize = AtomicUsize::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[test]
fn boba() -> Result<(), Box<dyn std::error::Error>> {
    use job::Barrier;
    use work::Worker;

    let mut worker = Worker::new(None)?;
    worker.push(Barrier)?;
    worker.run()?;
    Ok(())
}
