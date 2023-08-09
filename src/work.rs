use crate::{
    depend::{Conflict, Order},
    error::Error,
    job::Job,
    utility::is_sorted_set,
};
use parking_lot::Mutex;
use rayon::{Scope, ThreadPool, ThreadPoolBuilder};
use std::{
    cell::UnsafeCell,
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering::*},
    thread,
};

struct Node<'a> {
    pub job: UnsafeCell<Job<'a>>,
    pub strong: Strong,
    pub weak: Weak,
}

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
    pub bits: AtomicU64,
    pub nodes: Vec<usize>, // TODO: Based on average size of a cluster, use SmallVec?
}

pub struct Worker<'a> {
    schedule: bool,
    reset: bool,
    roots: Vec<usize>,
    nodes: Vec<Node<'a>>,
    clusters: Vec<Cluster>,
    conflict: Conflict,
    pool: ThreadPool,
}

unsafe impl Sync for Node<'_> {}

impl<'a> Node<'a> {
    #[inline]
    pub const fn new(job: Job<'a>) -> Self {
        Self {
            job: UnsafeCell::new(job),
            strong: Strong {
                wait: AtomicUsize::new(0),
                previous: Vec::new(),
                next: Vec::new(),
            },
            weak: Weak::new(),
        }
    }
}

impl Weak {
    #[inline]
    pub const fn new() -> Self {
        Self {
            ready: 0,
            lock: 0,
            cluster: usize::MAX,
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

impl<'a> Worker<'a> {
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

    pub fn push(&mut self, job: Job<'a>) {
        self.nodes.push(Node::new(job));
        self.schedule = true;
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.schedule = true;
    }

    pub fn update(&mut self) -> Result<bool, Error> {
        if self.schedule {
            self.schedule()?;
            Ok(true)
        } else if self.reset {
            self.reset();
            Ok(true)
        } else {
            Ok(false)
        }
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

    pub fn schedule(&mut self) -> Result<(), Error> {
        self.roots.clear();
        self.clusters.clear();
        for node in self.nodes.iter_mut() {
            node.strong.wait = AtomicUsize::new(0);
            node.strong.next.clear();
            node.strong.previous.clear();
            node.weak = Weak::new();
        }

        // TODO: Scheduling scales poorly with the number of jobs (n^2 / 2).
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
                                    Some(right_cluster) if right_cluster.nodes.len() >= 32 => true,
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
                                            bits: AtomicU64::new(0),
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

                    if let Some(strong) = strong {
                        // Remove transitive strong dependencies.
                        // TODO: Implement a better linear 'sorted retain' in a separate pass.

                        // TODO: WHO SETS THE READY BIT?!
                        // - Entry points to a cluster can be reduced with the new 'progress_weak'.
                        // - Can it be set when removing transitive strong dependencies?
                        for &previous in node.strong.previous.iter() {
                            let left = &mut left[previous];
                            if let Ok(next_index) = left.strong.next.binary_search(&right_index) {
                                let Ok(previous_index) =
                                        right.strong.previous.binary_search(&previous)
                                    else {
                                        unreachable!();
                                    };
                                left.strong.next.remove(next_index);
                                right.strong.previous.remove(previous_index);
                            }
                        }

                        if strong {
                            node.strong.next.push(right_index);
                            right.strong.previous.push(left_index);
                        } else {
                            // 'weak.lock' must include its own 'ready' bit.
                            node.weak.lock |= node.weak.ready | right.weak.ready;
                            right.weak.lock |= node.weak.ready | right.weak.ready;
                        }
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

        debug_assert!(is_sorted_set(&self.roots));
        self.schedule = false;
        self.reset = false;
        Ok(())
    }

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

fn progress<'a, 's>(
    worker: &'a Worker,
    node: &'a Node,
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
        let bits = node.weak.release(&cluster.bits);
        // Remove this node's ready bit and look for nodes that could have been conflicting with the current node.
        let mask = node.weak.lock & !node.weak.ready;
        // Try to leverage the ready bits from 'cluster.bits' to spawn more jobs.
        progress_weak(worker, bits, mask, cluster, errors, scope);
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

fn progress_weak<'a, 's>(
    worker: &'a Worker,
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
