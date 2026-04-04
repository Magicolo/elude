use crate::{
    depend::{Dependency, Key, Order},
    error::Error as ScheduleError,
    experiment::{Job as ApiJob, Run},
};
use anyhow::Result;
use parking_lot::Mutex;
use rayon::{Scope, ThreadPool, ThreadPoolBuilder};
use std::{
    cmp::{Reverse, max},
    num::NonZeroUsize,
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*},
    thread,
};

pub type PageId = u32;
pub type JobId = u32;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Home {
    pub page: PageId,
    pub slot: u8,
}

impl Home {
    #[inline]
    pub const fn ready_bit(self) -> u32 {
        1u32 << self.slot
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Acquire {
    pub page: PageId,
    pub mask: u32,
}

impl Acquire {
    #[inline]
    pub const fn new(page: PageId, mask: u32) -> Self {
        Self { page, mask }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Access {
    key: Key,
    order: Order,
    write: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NormalizedDeps {
    Unknown,
    Known(Box<[Access]>),
}

struct PendingJob<S> {
    run: Run<S>,
    dependencies: NormalizedDeps,
}

pub struct Node<S> {
    pub run: Run<S>,
    pub home: Home,
    pub strict_wait_initial: u32,
    pub strict_wait: AtomicU32,
    pub successor_range: Range<u32>,
    pub acquire_range: Range<u32>,
}

impl<S> Node<S> {
    pub fn new(
        run: Run<S>,
        home: Home,
        strict_wait_initial: u32,
        successor_range: Range<u32>,
        acquire_range: Range<u32>,
    ) -> Self {
        Self {
            run,
            home,
            strict_wait_initial,
            strict_wait: AtomicU32::new(strict_wait_initial),
            successor_range,
            acquire_range,
        }
    }
}

#[derive(Debug)]
pub struct Page {
    pub state: AtomicU64,
    pub resident_range: Range<u32>,
    pub follower_range: Range<u32>,
}

impl Page {
    pub fn new(resident_range: Range<u32>, follower_range: Range<u32>) -> Self {
        Self {
            state: AtomicU64::new(0),
            resident_range,
            follower_range,
        }
    }
}

pub struct Schedule<S> {
    pool: ThreadPool,
    pub jobs: Box<[Node<S>]>,
    pub pages: Box<[Page]>,
    pub successors: Box<[JobId]>,
    pub acquires: Box<[Acquire]>,
    pub residents: Box<[JobId]>,
    pub followers: Box<[PageId]>,
    pub roots: Box<[JobId]>,
}

struct RunContext<'schedule, 'state, 'sync, S: Sync> {
    schedule: &'schedule Schedule<S>,
    state: &'state S,
    errors: &'sync Mutex<Vec<anyhow::Error>>,
    cancelled: &'sync AtomicBool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Relation {
    None,
    Relaxed,
    Strict,
}

impl<S: Sync> Schedule<S> {
    pub fn compile(parallelism: Option<NonZeroUsize>, jobs: Vec<ApiJob<S>>) -> Result<Self> {
        let mut pending = Vec::with_capacity(jobs.len());
        for job in jobs {
            let (run, dependencies) = job.into_parts();
            pending.push(PendingJob {
                run,
                dependencies: normalize_dependencies(dependencies)?,
            });
        }

        let count = pending.len();
        let mut strict_candidates = vec![Vec::<JobId>::new(); count];
        let mut relaxed_neighbors = vec![Vec::<JobId>::new(); count];

        for right in 0..count {
            for left in 0..right {
                match relation(&pending[left].dependencies, &pending[right].dependencies) {
                    Relation::None => {}
                    Relation::Relaxed => {
                        relaxed_neighbors[left].push(right as JobId);
                        relaxed_neighbors[right].push(left as JobId);
                    }
                    Relation::Strict => strict_candidates[right].push(left as JobId),
                }
            }
        }

        let predecessors = reduce_strict(&strict_candidates);
        let homes = assign_homes(&relaxed_neighbors);
        let pages_count = homes
            .iter()
            .map(|home| home.page as usize + 1)
            .max()
            .unwrap_or(0);

        let mut successors_by_job = vec![Vec::<JobId>::new(); count];
        let mut roots = Vec::new();
        for (job, predecessors) in predecessors.iter().enumerate() {
            if predecessors.is_empty() {
                roots.push(job as JobId);
            }
            for &pred in predecessors.iter() {
                successors_by_job[pred as usize].push(job as JobId);
            }
        }

        let mut acquire_lists = vec![Vec::<Acquire>::new(); count];
        let mut followers_by_page = vec![Vec::<PageId>::new(); pages_count];
        for job in 0..count {
            let home = homes[job];
            let mut raw = Vec::with_capacity(relaxed_neighbors[job].len() + 1);
            raw.push((home.page, home.ready_bit()));
            for &neighbor in relaxed_neighbors[job].iter() {
                let other = homes[neighbor as usize];
                raw.push((other.page, 1u32 << other.slot));
            }
            raw.sort_unstable_by_key(|&(page, _)| page);

            let mut acquires: Vec<Acquire> = Vec::with_capacity(raw.len());
            for (page, mask) in raw {
                match acquires.last_mut() {
                    Some(previous) if previous.page == page => previous.mask |= mask,
                    _ => acquires.push(Acquire::new(page, mask)),
                }
            }
            for acquire in acquires.iter() {
                if acquire.page != home.page {
                    followers_by_page[acquire.page as usize].push(home.page);
                }
            }
            acquire_lists[job] = acquires;
        }

        let mut page_sizes = vec![0u32; pages_count];
        for home in homes.iter() {
            page_sizes[home.page as usize] += 1;
        }

        let mut resident_starts = vec![0u32; pages_count];
        let mut resident_cursor = 0u32;
        for (page, &size) in page_sizes.iter().enumerate() {
            resident_starts[page] = resident_cursor;
            resident_cursor += size;
        }

        let mut residents = vec![0; count];
        for (job, home) in homes.iter().enumerate() {
            let resident = resident_starts[home.page as usize] as usize + home.slot as usize;
            residents[resident] = job as JobId;
        }

        let mut pages = Vec::with_capacity(pages_count);
        let mut followers = Vec::new();
        if pages_count > 0 {
            for page in 0..pages_count {
                let page_followers = &mut followers_by_page[page];
                page_followers.sort_unstable();
                page_followers.dedup();

                let follower_start = followers.len() as u32;
                followers.extend(page_followers.iter().copied());
                let follower_end = followers.len() as u32;
                let resident_start = resident_starts[page];
                let resident_end = resident_start + page_sizes[page];

                pages.push(Page::new(
                    resident_start..resident_end,
                    follower_start..follower_end,
                ));
            }
        }

        let mut successors = Vec::new();
        let mut acquires = Vec::new();
        let mut nodes = Vec::with_capacity(count);
        for (index, pending) in pending.into_iter().enumerate() {
            let successor_start = successors.len() as u32;
            successors.extend(successors_by_job[index].iter().copied());
            let successor_end = successors.len() as u32;

            let acquire_start = acquires.len() as u32;
            acquires.extend(acquire_lists[index].iter().copied());
            let acquire_end = acquires.len() as u32;

            nodes.push(Node::new(
                pending.run,
                homes[index],
                predecessors[index].len() as u32,
                successor_start..successor_end,
                acquire_start..acquire_end,
            ));
        }

        let threads = parallelism
            .or_else(|| thread::available_parallelism().ok())
            .map(NonZeroUsize::get)
            .unwrap_or(1);
        let pool = ThreadPoolBuilder::new().num_threads(threads).build()?;

        Ok(Self {
            pool,
            jobs: nodes.into_boxed_slice(),
            pages: pages.into_boxed_slice(),
            successors: successors.into_boxed_slice(),
            acquires: acquires.into_boxed_slice(),
            residents: residents.into_boxed_slice(),
            followers: followers.into_boxed_slice(),
            roots: roots.into_boxed_slice(),
        })
    }

    pub fn run(&mut self, state: &S) -> Result<()> {
        let errors = Mutex::new(Vec::new());
        let cancelled = AtomicBool::new(false);
        let context = RunContext {
            schedule: self,
            state,
            errors: &errors,
            cancelled: &cancelled,
        };

        self.pool.scope(|scope| {
            for &root in self.roots.iter() {
                attempt_job(&context, root, scope);
            }
        });

        let mut errors = errors.into_inner();
        if errors.is_empty() {
            debug_assert!(self.pages.iter().all(|page| page.state.load(Relaxed) == 0));
            debug_assert!(
                self.jobs
                    .iter()
                    .all(|job| job.strict_wait.load(Relaxed) == job.strict_wait_initial)
            );
            Ok(())
        } else {
            self.reset();
            Err(errors.remove(0))
        }
    }

    fn reset(&self) {
        for page in self.pages.iter() {
            page.state.store(0, Relaxed);
        }
        for job in self.jobs.iter() {
            job.strict_wait.store(job.strict_wait_initial, Relaxed);
        }
    }
}

fn normalize_dependencies(dependencies: Vec<Dependency>) -> Result<NormalizedDeps> {
    let mut unknown = false;
    let mut accesses = Vec::with_capacity(dependencies.len());

    for dependency in dependencies {
        match dependency {
            Dependency::Unknown => unknown = true,
            Dependency::Read(key, order) => accesses.push(Access {
                key,
                order,
                write: false,
            }),
            Dependency::Write(key, order) => accesses.push(Access {
                key,
                order,
                write: true,
            }),
        }
    }

    if accesses.is_empty() {
        return Ok(if unknown {
            NormalizedDeps::Unknown
        } else {
            NormalizedDeps::Known(Box::new([]))
        });
    }

    accesses.sort_unstable_by(|left, right| left.key.cmp(&right.key));
    let mut merged: Vec<Access> = Vec::with_capacity(accesses.len());
    for access in accesses {
        match merged.last_mut() {
            Some(previous) if previous.key == access.key => {
                let order = max(previous.order, access.order);
                if previous.write && access.write {
                    return Err(
                        ScheduleError::WriteWriteConflict(
                            previous.key.clone(),
                            crate::depend::Scope::Inner,
                            order,
                        )
                        .into(),
                    );
                }
                if previous.write || access.write {
                    return Err(
                        ScheduleError::ReadWriteConflict(
                            previous.key.clone(),
                            crate::depend::Scope::Inner,
                            order,
                        )
                        .into(),
                    );
                }
                previous.order = order;
            }
            _ => merged.push(access),
        }
    }

    Ok(if unknown {
        NormalizedDeps::Unknown
    } else {
        NormalizedDeps::Known(merged.into_boxed_slice())
    })
}

fn relation(left: &NormalizedDeps, right: &NormalizedDeps) -> Relation {
    match (left, right) {
        (NormalizedDeps::Unknown, _) | (_, NormalizedDeps::Unknown) => Relation::Strict,
        (NormalizedDeps::Known(left), NormalizedDeps::Known(right)) => {
            let mut relation = Relation::None;
            let mut indices = (0usize, 0usize);
            while let (Some(left), Some(right)) = (left.get(indices.0), right.get(indices.1)) {
                match left.key.cmp(&right.key) {
                    std::cmp::Ordering::Less => indices.0 += 1,
                    std::cmp::Ordering::Greater => indices.1 += 1,
                    std::cmp::Ordering::Equal => {
                        if left.write || right.write {
                            match max(left.order, right.order) {
                                Order::Strict => return Relation::Strict,
                                Order::Relax => relation = Relation::Relaxed,
                            }
                        }
                        indices.0 += 1;
                        indices.1 += 1;
                    }
                }
            }
            relation
        }
    }
}

fn reduce_strict(strict_candidates: &[Vec<JobId>]) -> Vec<Vec<JobId>> {
    let len = strict_candidates.len();
    let words = len.div_ceil(64);
    let mut predecessors = vec![Vec::<JobId>::new(); len];
    let mut ancestors = vec![vec![0u64; words]; len];

    for right in 0..len {
        for &candidate in strict_candidates[right].iter().rev() {
            let redundant = predecessors[right].iter().any(|&selected| {
                selected == candidate || bit_test(&ancestors[selected as usize], candidate as usize)
            });
            if !redundant {
                predecessors[right].push(candidate);
            }
        }
        predecessors[right].sort_unstable();

        let (before_right, right_and_after) = ancestors.split_at_mut(right);
        let right_ancestors = &mut right_and_after[0];
        for &pred in predecessors[right].iter() {
            bit_set(right_ancestors, pred as usize);
            bit_or(right_ancestors, &before_right[pred as usize]);
        }
    }

    predecessors
}

fn assign_homes(relaxed_neighbors: &[Vec<JobId>]) -> Vec<Home> {
    let len = relaxed_neighbors.len();
    let degrees = relaxed_neighbors
        .iter()
        .map(|neighbors| neighbors.len())
        .collect::<Vec<_>>();
    let mut visited = vec![false; len];
    let mut isolated = Vec::<JobId>::new();
    let mut homes = vec![Home { page: 0, slot: 0 }; len];
    let mut next_page = 0u32;

    for job in 0..len {
        if visited[job] {
            continue;
        }
        if relaxed_neighbors[job].is_empty() {
            visited[job] = true;
            isolated.push(job as JobId);
            continue;
        }

        let mut stack = vec![job as JobId];
        let mut component = Vec::new();
        visited[job] = true;
        while let Some(current) = stack.pop() {
            component.push(current);
            for &neighbor in relaxed_neighbors[current as usize].iter() {
                if !visited[neighbor as usize] {
                    visited[neighbor as usize] = true;
                    stack.push(neighbor);
                }
            }
        }

        component.sort_unstable_by_key(|&current| (Reverse(degrees[current as usize]), current));
        for chunk in component.chunks(32) {
            for (slot, &job) in chunk.iter().enumerate() {
                homes[job as usize] = Home {
                    page: next_page,
                    slot: slot as u8,
                };
            }
            next_page += 1;
        }
    }

    for chunk in isolated.chunks(32) {
        for (slot, &job) in chunk.iter().enumerate() {
            homes[job as usize] = Home {
                page: next_page,
                slot: slot as u8,
            };
        }
        next_page += 1;
    }
    homes
}

#[inline]
fn bit_set(bits: &mut [u64], index: usize) {
    bits[index / 64] |= 1u64 << (index % 64);
}

#[inline]
fn bit_test(bits: &[u64], index: usize) -> bool {
    bits[index / 64] & (1u64 << (index % 64)) != 0
}

#[inline]
fn bit_or(left: &mut [u64], right: &[u64]) {
    for (left, right) in left.iter_mut().zip(right.iter()) {
        *left |= *right;
    }
}

fn attempt_job<'context, 'schedule, 'state, 'sync, 'scope, S: Sync>(
    context: &'context RunContext<'schedule, 'state, 'sync, S>,
    job: JobId,
    scope: &Scope<'scope>,
) where
    'context: 'scope,
    'schedule: 'scope,
    'state: 'scope,
    'sync: 'scope,
{
    if context.cancelled.load(Acquire) {
        return;
    }

    let node = &context.schedule.jobs[job as usize];
    if node.strict_wait.load(Acquire) != 0 {
        return;
    }

    let acquires = job_acquires(context.schedule, node);
    let mut reserved = 0usize;
    for acquire in acquires.iter() {
        let ready_clear = if acquire.page == node.home.page {
            node.home.ready_bit()
        } else {
            0
        };
        if reserve_page(
            &context.schedule.pages[acquire.page as usize].state,
            acquire.mask,
            ready_clear,
        ) {
            reserved += 1;
        } else {
            rollback(context.schedule, &acquires[..reserved]);
            mark_ready(
                &context.schedule.pages[node.home.page as usize].state,
                node.home.ready_bit(),
            );
            let failed_page = &context.schedule.pages[acquire.page as usize];
            let failed_lock = failed_page.state.load(Acquire) as u32;
            if failed_lock & acquire.mask == 0 {
                scan_page(context, node.home.page, scope);
            }
            return;
        }
    }

    scope.spawn(move |scope| run_job(context, job, scope));
}

fn run_job<'context, 'schedule, 'state, 'sync, 'scope, S: Sync>(
    context: &'context RunContext<'schedule, 'state, 'sync, S>,
    job: JobId,
    scope: &Scope<'scope>,
) where
    'context: 'scope,
    'schedule: 'scope,
    'state: 'scope,
    'sync: 'scope,
{
    let node = &context.schedule.jobs[job as usize];
    let acquires = job_acquires(context.schedule, node);
    let result = (node.run)(context.state);
    let home_page = node.home.page;

    release(context.schedule, acquires);
    node.strict_wait.store(node.strict_wait_initial, Release);

    match result {
        Ok(()) if !context.cancelled.load(Acquire) => {
            scan_page(context, home_page, scope);
            for acquire in acquires.iter() {
                let page = &context.schedule.pages[acquire.page as usize];
                for &follower in page_followers(context.schedule, page).iter() {
                    if follower != home_page {
                        scan_page(context, follower, scope);
                    }
                }
            }
            for &successor in job_successors(context.schedule, node).iter() {
                let successor_node = &context.schedule.jobs[successor as usize];
                if successor_node.strict_wait.fetch_sub(1, AcqRel) == 1 {
                    attempt_job(context, successor, scope);
                }
            }
        }
        Ok(()) => {}
        Err(error) => {
            context.cancelled.store(true, Release);
            context.errors.lock().push(error);
        }
    }
}

#[inline]
fn reserve_page(state: &AtomicU64, mask: u32, clear_ready: u32) -> bool {
    let mask = u64::from(mask);
    let clear_ready = u64::from(clear_ready) << 32;
    loop {
        let current = state.load(Acquire);
        if current & mask != 0 {
            return false;
        }

        let next = (current | mask) & !clear_ready;

        match state.compare_exchange_weak(current, next, AcqRel, Acquire) {
            Ok(_) => return true,
            Err(_) => {}
        }
    }
}

fn rollback<S>(schedule: &Schedule<S>, acquires: &[Acquire]) {
    for acquire in acquires.iter().rev() {
        release_mask(&schedule.pages[acquire.page as usize].state, acquire.mask);
    }
}

fn release<S>(schedule: &Schedule<S>, acquires: &[Acquire]) {
    for acquire in acquires.iter().rev() {
        release_mask(&schedule.pages[acquire.page as usize].state, acquire.mask);
    }
}

#[inline]
fn release_mask(state: &AtomicU64, mask: u32) {
    let keep_ready = (u64::from(u32::MAX)) << 32;
    state.fetch_and(keep_ready | u64::from(!mask), Release);
}

#[inline]
fn mark_ready(state: &AtomicU64, bit: u32) {
    state.fetch_or((u64::from(bit)) << 32, Release);
}

fn scan_page<'context, 'schedule, 'state, 'sync, 'scope, S: Sync>(
    context: &'context RunContext<'schedule, 'state, 'sync, S>,
    page: PageId,
    scope: &Scope<'scope>,
) where
    'context: 'scope,
    'schedule: 'scope,
    'state: 'scope,
    'sync: 'scope,
{
    if context.cancelled.load(Acquire) {
        return;
    }

    let page = &context.schedule.pages[page as usize];
    let residents =
        &context.schedule.residents[page.resident_range.start as usize..page.resident_range.end as usize];
    let mut pending = (page.state.load(Acquire) >> 32) as u32;
    while pending != 0 {
        let slot = pending.trailing_zeros() as usize;
        pending &= pending - 1;
        if slot < residents.len() {
            attempt_job(context, residents[slot], scope);
        }
    }
}

#[inline]
fn job_acquires<'schedule, S>(
    schedule: &'schedule Schedule<S>,
    node: &Node<S>,
) -> &'schedule [Acquire] {
    &schedule.acquires[node.acquire_range.start as usize..node.acquire_range.end as usize]
}

#[inline]
fn job_successors<'schedule, S>(
    schedule: &'schedule Schedule<S>,
    node: &Node<S>,
) -> &'schedule [JobId] {
    &schedule.successors[node.successor_range.start as usize..node.successor_range.end as usize]
}

#[inline]
fn page_followers<'schedule, S>(
    schedule: &'schedule Schedule<S>,
    page: &Page,
) -> &'schedule [PageId] {
    &schedule.followers[page.follower_range.start as usize..page.follower_range.end as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rejects_read_write_aliasing_within_one_job() {
        let error = normalize_dependencies(vec![
            Dependency::Read(Key::Identifier(1), Order::Relax),
            Dependency::Write(Key::Identifier(1), Order::Strict),
            Dependency::Read(Key::Identifier(1), Order::Relax),
        ])
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<crate::error::Error>(),
            Some(crate::error::Error::ReadWriteConflict(
                Key::Identifier(1),
                crate::depend::Scope::Inner,
                Order::Strict
            ))
        ));
    }

    #[test]
    fn normalize_merges_duplicate_reads() {
        let normalized = normalize_dependencies(vec![
            Dependency::Read(Key::Identifier(1), Order::Relax),
            Dependency::Read(Key::Identifier(1), Order::Strict),
            Dependency::Read(Key::Identifier(1), Order::Relax),
        ])
        .unwrap();

        assert_eq!(
            normalized,
            NormalizedDeps::Known(
                vec![Access {
                    key: Key::Identifier(1),
                    order: Order::Strict,
                    write: false,
                }]
                .into_boxed_slice()
            )
        );
    }

    #[test]
    fn normalize_rejects_duplicate_writes_within_one_job() {
        let error = normalize_dependencies(vec![
            Dependency::Write(Key::Identifier(1), Order::Relax),
            Dependency::Write(Key::Identifier(1), Order::Strict),
        ])
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<crate::error::Error>(),
            Some(crate::error::Error::WriteWriteConflict(
                Key::Identifier(1),
                crate::depend::Scope::Inner,
                Order::Strict
            ))
        ));
    }

    #[test]
    fn relation_distinguishes_none_relaxed_and_strict() {
        let none = relation(
            &normalize_dependencies(vec![Dependency::Read(Key::Identifier(0), Order::Relax)]).unwrap(),
            &normalize_dependencies(vec![Dependency::Read(Key::Identifier(0), Order::Strict)]).unwrap(),
        );
        let relaxed = relation(
            &normalize_dependencies(vec![Dependency::Read(Key::Identifier(0), Order::Relax)]).unwrap(),
            &normalize_dependencies(vec![Dependency::Write(Key::Identifier(0), Order::Relax)]).unwrap(),
        );
        let strict = relation(
            &normalize_dependencies(vec![Dependency::Write(Key::Identifier(0), Order::Relax)]).unwrap(),
            &normalize_dependencies(vec![Dependency::Write(Key::Identifier(0), Order::Strict)]).unwrap(),
        );

        assert_eq!(none, Relation::None);
        assert_eq!(relaxed, Relation::Relaxed);
        assert_eq!(strict, Relation::Strict);
    }

    #[test]
    fn strict_reduction_drops_transitive_edges() {
        let reduced = reduce_strict(&[vec![], vec![0], vec![0, 1]]);
        assert_eq!(reduced, vec![vec![], vec![0], vec![1]]);
    }

    #[test]
    fn assign_homes_packs_isolated_jobs_together() {
        let homes = assign_homes(&[vec![], vec![], vec![]]);
        assert_eq!(
            homes,
            vec![
                Home { page: 0, slot: 0 },
                Home { page: 0, slot: 1 },
                Home { page: 0, slot: 2 },
            ]
        );
    }
}
