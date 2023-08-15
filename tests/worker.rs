use std::error::Error;

use elude::{
    depend::{Dependency::*, Key::*, Order::*},
    job::Job,
    work::Worker,
};

#[test]
fn dependencies_is_empty() -> Result<(), Box<dyn Error>> {
    let mut worker = Worker::new(None)?;
    worker.push(unsafe { Job::new(|| Ok(()), []) });
    let graph = worker.update()?;
    let roots = graph.roots().collect::<Vec<_>>();
    assert_eq!(roots.len(), 1);
    let root = &roots[0];
    assert_eq!(root.index(), 0);
    assert!(root.dependencies().is_empty());
    Ok(())
}

#[test]
fn barrier_forces_sequential() -> Result<(), Box<dyn Error>> {
    let mut worker = Worker::new(None)?;
    worker.extend([Job::ok(), Job::barrier(), Job::ok()]);
    let graph = worker.update()?;
    let a = graph.roots().next().unwrap();
    let b = a.next().next().unwrap();
    let c = b.next().next().unwrap();
    assert_eq!(a.index(), 0);
    assert!(a.dependencies().is_empty());
    assert_eq!(b.index(), 1);
    assert_eq!(b.dependencies(), [Unknown]);
    assert_eq!(c.index(), 2);
    assert!(c.dependencies().is_empty());
    Ok(())
}

#[test]
fn any_read_read_run_in_parallel() -> Result<(), Box<dyn Error>> {
    let mut worker = Worker::new(None)?;
    worker.extend([
        Job::ok().depend([Read(Identifier(0), Relax)]),
        Job::ok().depend([Read(Identifier(1), Strict)]),
        Job::ok().depend([Read(Identifier(0), Strict)]),
        Job::ok().depend([Read(Identifier(1), Relax)]),
    ]);
    let graph = worker.update()?;
    assert!(graph.clusters().next().is_none());
    assert_eq!(
        graph
            .roots()
            .filter(|root| root.next().next().is_none()
                && root.previous().next().is_none()
                && root.rivals().next().is_none())
            .count(),
        4
    );
    Ok(())
}

#[test]
fn relax_read_write_creates_cluster() -> Result<(), Box<dyn Error>> {
    let mut worker = Worker::new(None)?;
    worker.extend([
        Job::ok().depend([Read(Identifier(0), Relax)]),
        Job::ok().depend([Write(Identifier(0), Relax)]),
    ]);
    let graph = worker.update()?;
    let cluster = graph.clusters().next().unwrap();
    assert_eq!(cluster.nodes().count(), 2);
    let roots = graph.roots().collect::<Vec<_>>();
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0].rivals().next().unwrap().index(), 1);
    assert_eq!(roots[1].rivals().next().unwrap().index(), 0);
    Ok(())
}
