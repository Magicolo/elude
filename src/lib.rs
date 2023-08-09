pub mod depend;
pub mod error;
pub mod job;
mod utility;
pub mod work;

pub struct Get<T>(T);
pub struct Set<T>(Option<T>);
pub struct Full<T>(T);

#[test]
fn boba() -> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        depend::{Dependency, Key, Order},
        job::{barrier, Job},
        work::Worker,
    };
    use parking_lot::Mutex;

    let jobs = Mutex::new(Vec::new());
    let mut worker = Worker::new(None)?;
    for i in 0..100 {
        let jobs = &jobs;
        worker.push(Job {
            run: Box::new(move |_| {
                jobs.lock().push(i);
                Ok(())
            }),
            state: Box::new(()),
            name: "A".into(),
            dependencies: vec![Dependency::Write(
                Key::Identifier(i % 3),
                if i % 7 == 0 {
                    Order::Strict
                } else {
                    Order::Relax
                },
            )],
        });
        worker.push(barrier());
    }
    worker.update()?;
    worker.run()?;
    Ok(())
}
