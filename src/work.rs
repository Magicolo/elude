use crate::{
    depend::{Conflict, Dependency, Order},
    error::Error,
    graph::Graph,
    job::{Job, Run},
    utility::is_sorted_set,
};
use parking_lot::Mutex;
use rayon::{Scope, ThreadPool, ThreadPoolBuilder};
use std::{
    cell::UnsafeCell,
    collections::HashSet,
    marker::PhantomData,
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering::*},
    thread,
};

pub(crate) struct Node<'a, S> {
    pub run: UnsafeCell<Run<'a, S>>,
    pub dependencies: Vec<Dependency>,
    pub strong: Strong,
    pub weak: Weak,
}

pub(crate) struct Strong {
    pub wait: AtomicUsize,
    pub previous: Vec<usize>, // TODO: Node indices could be stored as u32 instead of usize.
    pub next: Vec<usize>,
    pub transitive: HashSet<usize>,
}

pub(crate) struct Weak {
    pub ready: u32,
    pub lock: u32,
    pub cluster: usize,
    pub rivals: Vec<usize>,
}

pub(crate) struct Cluster {
    pub ready: u32,
    pub bits: AtomicU64,
    pub nodes: Vec<usize>, // TODO: Based on average size of a cluster, use SmallVec?
}

// TODO: Could make `Worker<B: Bits>` to change the size of clusters.
// - `Bits` would contain methods for atomic operations.
// - `Bits` would be implemented for `u8, u16, u32` and could be for `u64` for platforms that support AtomicU128.
// - Should it be implemented on the atomic types instead and be called `Atomic`?
pub struct Worker<'a, S> {
    pub(crate) roots: Vec<usize>,
    pub(crate) nodes: Vec<Node<'a, S>>,
    pub(crate) clusters: Vec<Cluster>,
    pub(crate) state: State<S>,
    schedule: bool,
    reset: bool,
    conflict: Conflict,
    pool: ThreadPool,
}
pub struct Build<'a, 'b, S, T, F>(
    pub &'b mut Worker<'a, S>,
    pub Option<Order>,
    pub F,
    pub Vec<Dependency>,
    pub PhantomData<T>,
);

pub(crate) struct State<S>(UnsafeCell<S>);

unsafe impl<S: Send + Sync> Sync for State<S> {}

impl<S> State<S> {
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get(&self) -> &mut S {
        &mut *self.0.get()
    }
}

impl<'a, S> Node<'a, S> {
    #[inline]
    pub fn new(job: Job<'a, S>) -> Self {
        Self {
            run: UnsafeCell::new(job.run),
            dependencies: job.dependencies,
            strong: Strong {
                wait: AtomicUsize::new(0),
                previous: Vec::new(),
                next: Vec::new(),
                transitive: HashSet::new(),
            },
            weak: Weak::new(),
        }
    }
}

unsafe impl<S: Send + Sync> Sync for Node<'_, S> {}

impl Weak {
    #[inline]
    pub const fn new() -> Self {
        Self {
            ready: 0,
            lock: 0,
            cluster: usize::MAX,
            rivals: Vec::new(),
        }
    }

    pub fn early_reserve(&self, bits: &AtomicU64) -> Option<u64> {
        let mut success = false;
        let result = bits.fetch_update(Release, Acquire, |bits| {
            let (ready, lock) = decompose(bits);
            debug_assert_eq!(self.ready & ready, 0);
            success = lock & self.lock == 0;
            if success {
                // Required locks are available. Take them immediately.
                Some(recompose(ready, lock | self.lock))
            } else {
                // Locks are unavailable. Set the ready bit.
                Some(recompose(ready | self.ready, lock))
            }
        });
        match result {
            Ok(bits) if success => Some(bits),
            Ok(_) | Err(_) => None,
        }
    }

    pub fn late_reserve(&self, bits: &AtomicU64) -> Result<u64, u64> {
        bits.fetch_update(Release, Acquire, |bits| {
            let (ready, lock) = decompose(bits);
            if ready & self.ready == self.ready && lock & self.lock == 0 {
                // Required locks are available. Take them and remove the ready bit.
                Some(recompose(ready & !self.ready, lock | self.lock))
            } else {
                // Locks are unavailable.
                None
            }
        })
    }

    #[inline]
    pub fn release(&self, bits: &AtomicU64) -> u64 {
        bits.fetch_and(recompose(u32::MAX, !self.lock), Relaxed)
    }
}

impl<'a, S> Worker<'a, S> {
    pub fn new(state: S, parallelism: Option<NonZeroUsize>) -> Result<Self, Error> {
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
            state: State(UnsafeCell::new(state)),
            pool: ThreadPoolBuilder::new()
                .num_threads(parallelism)
                .build()
                .map_err(|error| Error::Dynamic(error.into()))?,
        })
    }

    pub fn build(&mut self) -> Build<'a, '_, S, (), ()> {
        Build(self, None, (), vec![], PhantomData)
    }

    pub fn push(&mut self, job: Job<'a, S>) {
        self.nodes.push(Node::new(job));
        self.schedule = true;
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.schedule = true;
    }

    pub fn graph(&self) -> Graph<'a, '_, S> {
        Graph::new(self)
    }

    pub fn update(&mut self) -> Result<Graph<'a, '_, S>, Error> {
        if self.schedule {
            self.schedule()?;
        } else if self.reset {
            self.reset();
        }
        Ok(self.graph())
    }

    pub fn reset(&mut self) {
        for node in self.nodes.iter_mut() {
            *node.strong.wait.get_mut() = node.strong.previous.len();
        }
        for cluster in self.clusters.iter_mut() {
            *cluster.bits.get_mut() = 0;
        }
        self.reset = false;
    }

    pub fn schedule(&mut self) -> Result<Graph<'a, '_, S>, Error> {
        self.roots.clear();
        self.clusters.clear();
        for node in self.nodes.iter_mut() {
            node.strong.wait = AtomicUsize::new(0);
            node.strong.next.clear();
            node.strong.previous.clear();
            node.strong.transitive.clear();
            node.weak.cluster = usize::MAX;
            node.weak.lock = 0;
            node.weak.ready = 0;
            node.weak.rivals.clear();
        }

        for index in 0..self.nodes.len() {
            let (lefts, rights) = self.nodes.split_at_mut(index);
            if let Some((right, _)) = rights.split_first_mut() {
                let right = (lefts.len(), right);
                self.conflict.detect_inner(&right.1.dependencies, true)?;

                resolve(
                    (right.0, &mut *right.1),
                    lefts,
                    &mut self.conflict,
                    &mut self.clusters,
                );
            }
        }

        // for index in 0..self.nodes.len() {
        //     let (lefts, rights) = self.nodes.split_at_mut(index);
        //     if let Some((right, rights)) = rights.split_first_mut() {
        //         for &previous in right.strong.previous.iter() {
        //             if let Some(previous) = lefts.get_mut(previous) {
        //                 sorted_difference(&mut previous.strong.next, &right.weak.rivals);
        //             }
        //         }
        //     }
        // }

        // for index in 0..self.nodes.len() {
        //     let (lefts, rights) = self.nodes.split_at_mut(index);
        //     if let Some((right, rights)) = rights.split_first_mut() {
        //         for &rival in right.weak.rivals.iter() {
        //             if let Some(rival) = lefts
        //                 .get_mut(rival)
        //                 .or_else(|| rights.get_mut(rival - index - 1))
        //             {
        //                 sorted_difference(&mut rival.strong.previous, &right.strong.previous);
        //             }
        //         }
        //     }
        // }

        for (index, node) in self.nodes.iter_mut().enumerate() {
            // TODO: Who resets the ready bits?
            // - Can it be reset after the node is done (similar to strong 'wait')?
            if node.strong.previous.is_empty() {
                self.roots.push(index);
                // if let Some(cluster) = self.clusters.get_mut(node.weak.cluster) {
                //     cluster.ready |= node.weak.ready;
                //     *cluster.bits.get_mut() = recompose(cluster.ready, 0);
                // }

                // if !sorted_intersects(&self.roots, &node.weak.rivals) {
                //     self.roots.push(index);
                // }
            } else {
                *node.strong.wait.get_mut() = node.strong.previous.len();
            }

            debug_assert!(is_sorted_set(&node.strong.next));
            debug_assert!(is_sorted_set(&node.strong.previous));
            debug_assert!(is_sorted_set(&node.weak.rivals));
        }
        debug_assert!(is_sorted_set(&self.roots));

        self.schedule = false;
        self.reset = false;
        Ok(Graph::new(self))
    }
}

impl<'a, S: Send + Sync> Worker<'a, S> {
    pub fn run(&mut self) -> Result<(), Error> {
        // Even if 'run' did not strictly need the '&mut', it must still take it to prevent concurrent runs.
        self.update()?;

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
}

impl<'a, S> Extend<Job<'a, S>> for Worker<'a, S> {
    fn extend<T: IntoIterator<Item = Job<'a, S>>>(&mut self, iter: T) {
        for job in iter {
            self.push(job);
        }
    }
}

fn resolve<S>(
    right: (usize, &mut Node<S>),
    lefts: &mut [Node<S>],
    conflict: &mut Conflict,
    clusters: &mut Vec<Cluster>,
) {
    let Some((left, lefts)) = lefts.split_last_mut() else { return; };
    let left = (lefts.len(), left);
    if right.1.strong.transitive.contains(&left.0) {
        // 'left' and 'right' already have a transitive strong dependency.
        return resolve(right, lefts, conflict, clusters);
    }

    let strong = match conflict.detect_outer(&left.1.dependencies, false) {
        // The nodes have no dependency conflict.
        Ok(Order::Strict) => return resolve(right, lefts, conflict, clusters),
        // The nodes have a weak dependency conflict.
        Ok(Order::Relax) => {
            let count = clusters.len();
            match clusters.get_mut(left.1.weak.cluster) {
                // 'left' and 'right' are already in the same cluster.
                Some(_) if left.1.weak.cluster == right.1.weak.cluster => false,
                // 'left' and 'right' are in different clusters.
                Some(_) if right.1.weak.cluster < count => true,
                // 'node_cluster' is full.
                Some(left_cluster) if left_cluster.nodes.len() >= 32 => true,
                // 'left' has a cluster and 'right' doesn't.
                Some(left_cluster) => {
                    right.1.weak.cluster = left.1.weak.cluster;
                    right.1.weak.ready = 1 << left_cluster.nodes.len();
                    left_cluster.nodes.push(right.0);
                    false
                }
                None => match clusters.get_mut(right.1.weak.cluster) {
                    // 'right_cluster' is full.
                    Some(right_cluster) if right_cluster.nodes.len() >= 32 => true,
                    // 'right' has a cluster and 'left' doesn't.
                    Some(right_cluster) => {
                        left.1.weak.cluster = right.1.weak.cluster;
                        left.1.weak.ready = 1 << right_cluster.nodes.len();
                        right_cluster.nodes.push(left.0);
                        false
                    }
                    // 'left' and 'right' don't have a cluster.
                    None => {
                        left.1.weak.cluster = count;
                        right.1.weak.cluster = count;
                        left.1.weak.ready = 1 << 0;
                        right.1.weak.ready = 1 << 1;
                        clusters.push(Cluster {
                            ready: 0,
                            bits: AtomicU64::new(0),
                            nodes: vec![left.0, right.0],
                        });
                        false
                    }
                },
            }
        }
        // The nodes have a strong dependency conflict.
        Err(_) => true,
    };

    if strong {
        right.1.strong.transitive.insert(left.0);
        right
            .1
            .strong
            .transitive
            .extend(left.1.strong.transitive.iter().copied());
    } else {
        // 'weak.lock' must include its own 'ready' bit.
        left.1.weak.lock |= left.1.weak.ready | right.1.weak.ready;
        right.1.weak.lock |= left.1.weak.ready | right.1.weak.ready;
    }

    resolve((right.0, &mut *right.1), lefts, conflict, clusters);

    // Push the nodes after recursing to preserve ordering.
    if strong {
        left.1.strong.next.push(right.0);
        right.1.strong.previous.push(left.0);
    } else {
        left.1.weak.rivals.push(right.0);
        right.1.weak.rivals.push(left.0);
    }
}

fn progress<'a, 's, S: Send + Sync>(
    worker: &'a Worker<S>,
    node: &'a Node<S>,
    errors: &'a Mutex<Vec<Error>>,
    scope: &Scope<'s>,
) where
    'a: 's,
{
    match worker.clusters.get(node.weak.cluster) {
        Some(cluster) => {
            if let Some(bits) = node.weak.early_reserve(&cluster.bits) {
                // Spawn this job as soon as possible.
                scope.spawn(|scope| progress_local(worker, node, cluster, errors, scope));
                // Remove this node's ready bit and lock bits to filter out nodes that would conflict with the current node.
                let mask = !node.weak.lock & !node.weak.ready;
                // Try to leverage the ready from 'cluster.bits' to spawn more jobs.
                progress_weak(worker, bits, mask, cluster, errors, scope);
            }
        }
        None => scope.spawn(|scope| progress_foreign(worker, node, errors, scope)),
    }
}

#[inline]
fn progress_local<'a, 's, S: Send + Sync>(
    worker: &'a Worker<S>,
    node: &'a Node<S>,
    cluster: &'a Cluster,
    errors: &'a Mutex<Vec<Error>>,
    scope: &Scope<'s>,
) where
    'a: 's,
{
    if progress_run(worker, node, errors) {
        let bits = node.weak.release(&cluster.bits);
        // Remove this node's ready bit and look for nodes that could have been conflicting with the current node.
        let mask = node.weak.lock & !node.weak.ready;
        // Try to leverage the ready bits from 'cluster.bits' to spawn more jobs.
        progress_weak(worker, bits, mask, cluster, errors, scope);
        progress_strong(worker, node, errors, scope);
    }
}

#[inline]
fn progress_foreign<'a, 's, S: Send + Sync>(
    worker: &'a Worker<S>,
    node: &'a Node<S>,
    errors: &'a Mutex<Vec<Error>>,
    scope: &Scope<'s>,
) where
    'a: 's,
{
    if progress_run(worker, node, errors) {
        progress_strong(worker, node, errors, scope);
    }
}

#[inline]
fn progress_run<'a, 's, S>(
    worker: &'a Worker<S>,
    node: &'a Node<S>,
    errors: &'a Mutex<Vec<Error>>,
) -> bool
where
    'a: 's,
{
    debug_assert_eq!(node.strong.wait.load(Relaxed), 0);
    debug_assert_eq!(node.weak.lock == 0, node.weak.ready == 0);
    debug_assert!(node.weak.lock >= node.weak.ready);

    let run = unsafe { &mut *node.run.get() };
    let state = unsafe { &mut *worker.state.0.get() };
    if let Err(error) = run(state).map_err(Error::Dynamic) {
        errors.lock().push(error);
        false
    } else {
        true
    }
}

fn progress_strong<'a, 's, S: Send + Sync>(
    worker: &'a Worker<S>,
    node: &'a Node<S>,
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

    // Reset the 'wait' counter after scheduling others.
    node.strong.wait.store(node.strong.previous.len(), Relaxed);
}

fn progress_weak<'a, 's, S: Send + Sync>(
    worker: &'a Worker<S>,
    bits: u64,
    mut mask: u32,
    cluster: &'a Cluster,
    errors: &'a Mutex<Vec<Error>>,
    scope: &Scope<'s>,
) where
    'a: 's,
{
    let (mut ready, _) = decompose(bits);
    ready &= mask;
    while ready > 0 {
        let cluster_index = ready.trailing_zeros();
        let node_index = cluster.nodes[cluster_index as usize];
        let node = &worker.nodes[node_index];
        (ready, _) = decompose(match node.weak.late_reserve(&cluster.bits) {
            Ok(bits) => {
                scope.spawn(|scope| progress_local(worker, node, cluster, errors, scope));
                bits
            }
            Err(bits) => bits,
        });
        // Remove this bit from the mask such that it is not retried.
        mask &= !node.weak.ready;
        ready &= mask;
    }
}

const fn recompose(ready: u32, lock: u32) -> u64 {
    (ready as u64) << 32 | lock as u64
}

const fn decompose(bits: u64) -> (u32, u32) {
    ((bits >> 32) as u32, bits as u32)
}
