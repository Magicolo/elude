use checkito::{same::Same, *};
use elude::{
    depend::{
        Dependency::{self, *},
        Key::{self, *},
        Order::{self, *},
    },
    job::Job,
    work::Worker,
};
use parking_lot::Mutex;
use std::borrow::Cow;

#[test]
fn dependencies_is_empty() -> anyhow::Result<()> {
    let mut worker = Worker::new((), None)?;
    worker.push(Job::ok());
    let graph = worker.update()?;
    let roots = graph.roots().collect::<Vec<_>>();
    assert_eq!(roots.len(), 1);
    let root = &roots[0];
    assert_eq!(root.index(), 0);
    assert!(root.dependencies().is_empty());
    Ok(())
}

#[test]
fn barrier_forces_sequential() -> anyhow::Result<()> {
    let mut worker = Worker::new((), None)?;
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
fn any_read_read_run_in_parallel() -> anyhow::Result<()> {
    let mut worker = Worker::new((), None)?;
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
fn relax_read_write_creates_cluster() -> anyhow::Result<()> {
    let mut worker = Worker::new((), None)?;
    worker.extend([
        Job::ok().depend([Read(Identifier(0), Relax)]),
        Job::ok().depend([Write(Identifier(0), Relax)]),
    ]);
    let graph = worker.update()?;
    let cluster = graph.clusters().next().unwrap();
    assert_eq!(cluster.nodes().count(), 2);
    let roots = graph.roots().collect::<Vec<_>>();
    assert_eq!(roots.len(), 2);
    // assert_eq!(roots[0].rivals().next().unwrap().index(), 1);
    // assert_eq!(roots[1].rivals().next().unwrap().index(), 0);
    Ok(())
}

fn identifier() -> impl Generate<Item = Key> {
    usize::generator().map(Identifier)
}
fn address() -> impl Generate<Item = Key> {
    usize::generator().map(Address)
}
fn path() -> impl Generate<Item = Key> {
    letter().collect().map(Cow::Owned).map(Path)
}
fn key() -> impl Generate<Item = Key> {
    (path(), address(), identifier())
        .any()
        .map(|key| key.into())
}
fn order() -> impl Generate<Item = Order> {
    (Same(Strict), Same(Relax)).any().map(|order| order.into())
}
fn read() -> impl Generate<Item = Dependency> {
    (path(), order()).map(|(key, order)| Read(key, order))
}
fn write() -> impl Generate<Item = Dependency> {
    (path(), order()).map(|(key, order)| Write(key, order))
}
fn dependency() -> impl Generate<Item = Dependency> {
    (read(), write()).any().map(|dependency| dependency.into())
}
fn job<'a, S>(run: impl FnMut() + Send + Sync + Clone + 'a) -> impl Generate<Item = Job<'a, S>> {
    dependency().collect::<Vec<_>>().map(move |dependencies| {
        let mut run = run.clone();
        Job::with(move || {
            run();
            Ok(())
        })
        .depend(dependencies)
    })
}
#[test]
fn fett() -> anyhow::Result<()> {
    dependency()
        .collect::<Vec<_>>()
        .collect::<Vec<_>>()
        .check(1000, |dependencies| {
            let indices = Mutex::new(Vec::new());
            let mut worker = Worker::new((), None)?;
            for (i, dependencies) in dependencies.iter().enumerate() {
                let indices = &indices;
                worker.push(
                    Job::with(move || {
                        indices.lock().push(i);
                        Ok(())
                    })
                    .depend(dependencies.iter().cloned()),
                );
            }
            worker.run()?;
            Ok::<_, anyhow::Error>(())
        })?;
    Ok(())
}
