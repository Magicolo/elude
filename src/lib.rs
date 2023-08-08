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
    use job::{Barrier, IntoJob, Job};
    use std::{collections::HashMap, marker::PhantomData};
    use work::Worker;

    struct Boba<S, L, P, R>(P, R, PhantomData<S>, PhantomData<L>);
    impl<
            S,
            L: Send + Sync + 'static,
            D: FnOnce(&mut S) -> L,
            R: FnMut(&mut L) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
                + Send
                + Sync
                + 'static,
        > Boba<S, L, D, R>
    {
        pub fn new(declare: D, run: R) -> Self {
            Self(declare, run, PhantomData, PhantomData)
        }
    }

    impl<
            S,
            L: Send + Sync + 'static,
            D: FnOnce(&mut S) -> L,
            R: FnMut(&mut L) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
                + Send
                + Sync
                + 'static,
        > IntoJob<S> for Boba<S, L, D, R>
    {
        type Error = ();

        fn job(self, state: &mut S) -> Result<Job, Self::Error> {
            let state = self.0(state);
            let mut run = self.1;
            todo!()
            // let job = Job {
            //     name: "".into(),
            //     state: Box::new(state),
            //     run: Box::new(move |state| run(state.cast())),
            //     dependencies: vec![],
            // };
            // Ok(job)
        }
    }

    let mut map = HashMap::new();
    map.insert("a", 1u8);
    map.insert("b", 2u8);
    map.insert("c", 3u8);
    let mut worker = Worker::new(map, None)?;
    worker.push(Barrier)?;
    // worker.push(Boba::new(
    //     |map: &mut HashMap<&str, u8>| &mut map["a"],
    //     |a| {
    //         **a += 1;
    //         Ok(())
    //     },
    // ));
    worker.run()?;
    Ok(())
}

pub mod busy_kitchen {
    use std::{
        cell::UnsafeCell,
        num::NonZeroUsize,
        sync::atomic::{AtomicU64, AtomicUsize, Ordering::*},
        thread,
    };

    use parking_lot::Mutex;
    use rayon::{Scope, ThreadPool, ThreadPoolBuilder};

    use crate::{
        depend::{Conflict, Order},
        error::Error,
        job::Job,
        utility::is_sorted_set,
    };

    struct Node {
        pub job: UnsafeCell<Job>,
        pub strong: Strong,
        pub weak: Weak,
    }

    #[derive(Default)]
    struct Strong {
        pub wait: AtomicUsize,
        pub previous: Vec<usize>, // TODO: Node indices could be stored as u32 instead of usize.
        pub next: Vec<usize>,
    }

    struct Weak {
        pub ready: u32,
        pub lock: u32,
        pub cluster: usize,
    }

    struct Cluster {
        pub lock: AtomicU64,
        pub nodes: Vec<usize>, // TODO: Based on average size of a cluster, use SmallVec?
    }

    pub struct Worker {
        schedule: bool,
        reset: bool,
        roots: Vec<usize>,
        nodes: Vec<Node>,
        clusters: Vec<Cluster>,
        conflict: Conflict,
        pool: ThreadPool,
    }

    unsafe impl Sync for Node {}
    impl Node {
        pub fn new(job: Job) -> Self {
            Self {
                job: UnsafeCell::new(job),
                strong: Strong::default(),
                weak: Weak::new(),
            }
        }
    }

    impl Weak {
        pub const fn new() -> Self {
            Self {
                ready: 0,
                lock: 0,
                cluster: usize::MAX,
            }
        }

        pub fn early_reserve(&self, lock: &AtomicU64) -> bool {
            let mut success = false;
            lock.fetch_update(Release, Acquire, |cluster| {
                let (ready, lock) = decompose(cluster);
                debug_assert_eq!(self.ready & ready, 0);
                success = lock & self.lock == 0;
                if success {
                    // Required locks are available. Take them immediately.
                    Some(recompose(ready, lock | self.lock))
                } else {
                    // Locks are unavailable. Set the ready bit.
                    Some(recompose(ready | self.ready, lock))
                }
            })
            // Can't fail since 'fetch_update' always gets 'Some(_)'.
            .unwrap();
            success
        }

        pub fn late_reserve(&self, lock: &AtomicU64) -> Result<u64, u64> {
            lock.fetch_update(Release, Acquire, |cluster| {
                let (ready, lock) = decompose(cluster);
                if ready & self.ready == self.ready && lock & self.lock == 0 {
                    // Required locks are available. Take them and remove the ready bit.
                    Some(recompose(ready & !self.ready, lock | self.lock))
                } else {
                    // Locks are unavailable.
                    None
                }
            })
        }

        pub fn release(&self, lock: &AtomicU64) -> u64 {
            lock.fetch_and(recompose(u32::MAX, !self.lock), Relaxed)
        }
    }

    impl Worker {
        pub fn new(parallelism: Option<NonZeroUsize>) -> Result<Self, Error> {
            let parallelism = parallelism
                .or_else(|| thread::available_parallelism().ok())
                .map(NonZeroUsize::get)
                .unwrap_or(0);
            Ok(Self {
                schedule: true,
                reset: false,
                roots: Vec::new(),
                nodes: Vec::new(),
                clusters: Vec::new(),
                conflict: Conflict::default(),
                pool: ThreadPoolBuilder::new()
                    .num_threads(parallelism)
                    .build()
                    .map_err(|error| Error::Dynamic(error.into()))?,
            })
        }

        pub fn push(&mut self, job: Job) {
            self.nodes.push(Node::new(job));
            self.schedule = true;
        }

        pub fn schedule(&mut self) -> Result<(), Error> {
            self.roots.clear();
            self.clusters.clear();
            for node in self.nodes.iter_mut() {
                node.strong.next.clear();
                node.strong.previous.clear();
                node.weak = Weak::new();
            }

            for left_index in 0..self.nodes.len() {
                let (left, right) = self.nodes.split_at_mut(left_index);
                if let Some((node, right)) = right.split_first_mut() {
                    self.conflict
                        .detect_inner(node.job.get_mut().dependencies(), true)?;

                    for (i, right) in right.iter_mut().enumerate() {
                        let right_index = left_index + i + 1;
                        debug_assert!(!node.strong.next.contains(&right_index));
                        debug_assert!(!right.strong.previous.contains(&left_index));

                        let strong = match self
                            .conflict
                            .detect_outer(right.job.get_mut().dependencies(), false)
                        {
                            // The nodes have no dependency conflict.
                            Ok(Order::Strict) => None,
                            // The nodes have a weak dependency conflict.
                            Ok(Order::Relax) => {
                                let clusters = self.clusters.len();
                                Some(match self.clusters.get_mut(node.weak.cluster) {
                                    // 'node' and 'right' are already in the same cluster.
                                    Some(_) if node.weak.cluster == right.weak.cluster => false,
                                    // 'node' and 'right' are in different clusters.
                                    Some(_) if right.weak.cluster < clusters => true,
                                    // 'node_cluster' is full.
                                    Some(node_cluster) if node_cluster.nodes.len() >= 32 => true,
                                    // 'node' has a cluster and 'right' doesn't.
                                    Some(node_cluster) => {
                                        right.weak.cluster = node.weak.cluster;
                                        right.weak.ready = 1 << node_cluster.nodes.len();
                                        node_cluster.nodes.push(right_index);
                                        false
                                    }
                                    None => match self.clusters.get_mut(right.weak.cluster) {
                                        // 'right_cluster' is full.
                                        Some(right_cluster) if right_cluster.nodes.len() >= 32 => {
                                            true
                                        }
                                        // 'right' has a cluster and 'node' doesn't.
                                        Some(right_cluster) => {
                                            node.weak.cluster = right.weak.cluster;
                                            node.weak.ready = 1 << right_cluster.nodes.len();
                                            right_cluster.nodes.push(left_index);
                                            false
                                        }
                                        // 'node' and 'right' don't have a cluster.
                                        None => {
                                            node.weak.cluster = clusters;
                                            right.weak.cluster = clusters;
                                            node.weak.ready = 1 << 0;
                                            right.weak.ready = 1 << 1;
                                            self.clusters.push(Cluster {
                                                lock: AtomicU64::new(0),
                                                nodes: vec![left_index, right_index],
                                            });
                                            false
                                        }
                                    },
                                })
                            }
                            // The nodes have a strong dependency conflict.
                            Err(_) => Some(true),
                        };

                        match strong {
                            Some(true) => {
                                // Remove transitive strong dependencies.
                                // TODO: Implement a better linear 'sorted retain' in a separate pass.
                                for &previous in node.strong.previous.iter() {
                                    let left = &mut left[previous];
                                    if let Ok(index) = left.strong.next.binary_search(&right_index)
                                    {
                                        left.strong.next.remove(index);
                                    }
                                }
                                node.strong.next.push(right_index);
                                right.strong.previous.push(left_index);
                            }
                            Some(false) => {
                                // 'weak.lock' must include its own 'ready' bit.
                                node.weak.lock |= node.weak.ready | right.weak.ready;
                                right.weak.lock |= node.weak.ready | right.weak.ready;
                            }
                            None => {}
                        }
                    }

                    if node.strong.previous.is_empty() {
                        self.roots.push(left_index);
                    } else {
                        *node.strong.wait.get_mut() = node.strong.previous.len();
                    }

                    debug_assert!(is_sorted_set(&node.strong.previous));
                    debug_assert!(is_sorted_set(&node.strong.next));
                }
            }

            self.schedule = false;
            self.reset = false;
            Ok(())
        }

        pub fn run(&mut self) -> Result<(), Error> {
            // Even if 'run' did not strictly need the '&mut', it must still take it to prevent concurrent runs.
            if self.schedule {
                self.schedule()?;
            } else if self.reset {
                self.reset();
            }

            let errors = Mutex::new(Vec::new());
            self.pool.scope(|scope| {
                for &node in self.roots.iter() {
                    let node = &self.nodes[node];
                    progress(self, node, &errors, scope);
                }
            });
            let result = Error::All(errors.into_inner())
                .flatten(true)
                .map_or(Ok(()), Err);
            self.reset = result.is_err();
            result
        }

        fn reset(&mut self) {
            for node in self.nodes.iter_mut() {
                *node.strong.wait.get_mut() = node.strong.previous.len();
            }
            for cluster in self.clusters.iter_mut() {
                *cluster.lock.get_mut() = 0;
            }
            self.reset = false;
        }
    }

    fn progress<'a, 's>(
        worker: &'a Worker,
        node: &'a Node,
        errors: &'a Mutex<Vec<Error>>,
        scope: &Scope<'s>,
    ) where
        'a: 's,
    {
        match worker.clusters.get(node.weak.cluster) {
            Some(cluster) if node.weak.early_reserve(&cluster.lock) => {
                scope.spawn(|scope| progress_local(worker, node, cluster, errors, scope))
            }
            Some(_) => {}
            None => scope.spawn(|scope| progress_foreign(worker, node, errors, scope)),
        }
    }

    #[inline]
    fn progress_local<'a, 's>(
        worker: &'a Worker,
        node: &'a Node,
        cluster: &'a Cluster,
        errors: &'a Mutex<Vec<Error>>,
        scope: &Scope<'s>,
    ) where
        'a: 's,
    {
        if progress_run(node, errors) {
            progress_weak(worker, node, cluster, errors, scope);
            progress_strong(worker, node, errors, scope);
        }
    }

    #[inline]
    fn progress_foreign<'a, 's>(
        worker: &'a Worker,
        node: &'a Node,
        errors: &'a Mutex<Vec<Error>>,
        scope: &Scope<'s>,
    ) where
        'a: 's,
    {
        if progress_run(node, errors) {
            progress_strong(worker, node, errors, scope);
        }
    }

    #[inline]
    fn progress_run<'a, 's>(node: &'a Node, errors: &'a Mutex<Vec<Error>>) -> bool
    where
        'a: 's,
    {
        debug_assert_eq!(node.strong.wait.load(Relaxed), 0);
        debug_assert_eq!(node.weak.lock == 0, node.weak.ready == 0);
        debug_assert!(node.weak.lock >= node.weak.ready);

        let Job { run, state, .. } = unsafe { &mut *node.job.get() };
        if let Err(error) = run(&mut **state).map_err(Error::Dynamic) {
            errors.lock().push(error);
            false
        } else {
            true
        }
    }

    fn progress_weak<'a, 's>(
        worker: &'a Worker,
        node: &'a Node,
        cluster: &'a Cluster,
        errors: &'a Mutex<Vec<Error>>,
        scope: &Scope<'s>,
    ) where
        'a: 's,
    {
        let (ready, _) = decompose(node.weak.release(&cluster.lock));
        // Remove this node's ready bit from the 'lock' mask.
        let mut mask = node.weak.lock & !node.weak.ready;
        let mut ready = ready & mask;
        while ready > 0 {
            // TODO: Verify from which end 'leading_zeros' start to count.
            // TODO: Ensure that 'leading_zeros' favors order of declaration.
            let cluster_index = ready.leading_zeros();
            let node_index = cluster.nodes[cluster_index as usize];
            let node = &worker.nodes[node_index];
            (ready, _) = decompose(match node.weak.late_reserve(&cluster.lock) {
                Ok(locks) => {
                    scope.spawn(|scope| progress_local(worker, node, cluster, errors, scope));
                    locks
                }
                Err(locks) => {
                    // Remove this bit from the mask such that it is not retried.
                    mask &= !(1 << cluster_index);
                    locks
                }
            });
            ready &= mask;
        }
    }

    fn progress_strong<'a, 's>(
        worker: &'a Worker,
        node: &'a Node,
        errors: &'a Mutex<Vec<Error>>,
        scope: &Scope<'s>,
    ) where
        'a: 's,
    {
        for &node_index in node.strong.next.iter() {
            let node = &worker.nodes[node_index];
            if node.strong.wait.fetch_sub(1, Relaxed) == 1 {
                progress(worker, node, errors, scope);
            }
        }
        // Reset the state.
        node.strong.wait.store(node.strong.previous.len(), Relaxed);
    }

    const fn recompose(ready: u32, lock: u32) -> u64 {
        (ready as u64) << 32 | lock as u64
    }

    const fn decompose(cluster: u64) -> (u32, u32) {
        ((cluster >> 32) as u32, cluster as u32)
    }
}
