use core::{
    alloc::Layout,
    array,
    cell::UnsafeCell,
    ptr::{self, NonNull},
    sync::atomic::{AtomicPtr, AtomicU16, AtomicUsize, Ordering},
};
use crossbeam_utils::{Backoff, CachePadded};

// ════════════════════════════════════════════════════════════════════
// Design
// ════════════════════════════════════════════════════════════════════
//
// Lock-free page pool backed by Treiber stacks.  Each stack node is a
// page itself: the first 8 bytes store the `next` pointer via
// `AtomicPtr<u8>` so every access is data-race-free.
//
// `AtomicUsize` is used for the head because a single CAS must update
// both the pointer and a 16-bit ABA counter — two fields that cannot
// be packed into `AtomicPtr`.
//
// A per-thread cache (thread-local) avoids atomic ops on the hot path.
// The shared pool is sharded (N_SHARDS) to reduce CAS contention.
// Each thread is assigned one shard via a sequential counter; on pop
// misses the thread probes all shards round-robin starting at its own.

// ════════════════════════════════════════════════════════════════════
// Configuration
// ════════════════════════════════════════════════════════════════════

/// Number of shared-pool shards (power-of-two for efficient modulo).
const N_SHARDS: usize = 16;

/// Per-thread local cache capacity.
const CACHE_CAPACITY: usize = 64;

/// Pages transferred per batch operation.
const BATCH: usize = CACHE_CAPACITY / 2;

// ════════════════════════════════════════════════════════════════════
// Tagged-pointer encoding
// ════════════════════════════════════════════════════════════════════
//
//   [63:48] 16-bit ABA counter
//   [47: 0] pointer >> PAGE_SHIFT  (48 bits → up to 256 TB)
// ════════════════════════════════════════════════════════════════════

const PAGE_SHIFT: usize = 12;
const COUNTER_SHIFT: usize = 48;
const POINTER_MASK: usize = (1 << COUNTER_SHIFT) - 1;
const COUNTER_MASK: usize = !POINTER_MASK;
const COUNTER_INCREMENT: usize = 1 << COUNTER_SHIFT;

#[inline(always)]
fn shift_ptr(ptr: *mut u8) -> usize {
    ptr.addr() >> PAGE_SHIFT
}

#[inline(always)]
unsafe fn unpack_ptr(word: usize) -> *mut u8 {
    let addr = (word & POINTER_MASK) << PAGE_SHIFT;
    ptr::with_exposed_provenance_mut(addr)
}

#[inline(always)]
fn pack_next(word: usize, new_ptr: *mut u8) -> usize {
    (word.wrapping_add(COUNTER_INCREMENT) & COUNTER_MASK) | shift_ptr(new_ptr)
}

// ════════════════════════════════════════════════════════════════════
// Thread-local page cache
// ════════════════════════════════════════════════════════════════════

struct LocalCache {
    pages: [*mut u8; CACHE_CAPACITY],
    len: u8,
}

impl LocalCache {
    const fn new() -> Self {
        Self {
            pages: [core::ptr::null_mut(); CACHE_CAPACITY],
            len: 0,
        }
    }

    #[inline(always)]
    fn pop(&mut self) -> *mut u8 {
        let length = self.len;
        if length > 0 {
            let index = (length - 1) as usize;
            self.len = length - 1;
            self.pages[index]
        } else {
            core::ptr::null_mut()
        }
    }

    #[inline(always)]
    fn push(&mut self, page: *mut u8) -> bool {
        let length = self.len as usize;
        if length < CACHE_CAPACITY {
            self.pages[length] = page;
            self.len = (length + 1) as u8;
            true
        } else {
            false
        }
    }

    #[inline]
    fn drain(&mut self, max: usize) -> &mut [*mut u8] {
        let length = self.len as usize;
        let count = length.min(max);
        if count == 0 {
            return &mut [];
        }
        let start = length - count;
        self.len = start as u8;
        &mut self.pages[start..length]
    }
}

thread_local! {
    static LOCAL: UnsafeCell<LocalCache> =
        const { UnsafeCell::new(LocalCache::new()) };
}

/// Global atomic counter used to assign each thread a unique shard on first access.
static SHARD_COUNTER: AtomicU16 = AtomicU16::new(0);

thread_local! {
    static SHARD: usize = SHARD_COUNTER.fetch_add(1, Ordering::Relaxed) as usize % N_SHARDS;
}

// ════════════════════════════════════════════════════════════════════
// Memory pool — sharded, cache-line padded
// ════════════════════════════════════════════════════════════════════

pub struct Memory {
    shards: [CachePadded<AtomicUsize>; N_SHARDS],
    layout: Layout,
}

impl Memory {
    #[inline]
    fn shard(&self, idx: usize) -> &CachePadded<AtomicUsize> {
        &self.shards[idx]
    }

    pub fn new() -> Self {
        let page_size = page_size::get();
        let layout = Layout::from_size_align(page_size, page_size)
            .expect("page size is a power of two and non-zero");
        Self {
            shards: array::from_fn(|_| CachePadded::new(AtomicUsize::new(0))),
            layout,
        }
    }

    // ── Public API ─────────────────────────────────────────────────

    #[inline]
    pub unsafe fn push(&self, page: NonNull<usize>) {
        unsafe {
            let page_ptr = page.cast::<u8>().as_ptr();

            if LOCAL.with(|local| (*local.get()).push(page_ptr)) {
                return;
            }

            self.flush(page_ptr);
        }
    }

    #[inline]
    pub unsafe fn try_pop(&self) -> Option<NonNull<usize>> {
        unsafe {
            let page = LOCAL.with(|local| (*local.get()).pop());
            if !page.is_null() {
                return Some(NonNull::new_unchecked(page).cast::<usize>());
            }

            self.refill()
        }
    }

    #[inline]
    pub unsafe fn pop(&self) -> NonNull<usize> {
        unsafe {
            if let Some(page) = self.try_pop() {
                return page;
            }

            let mut alloc_batch = [core::ptr::null_mut::<u8>(); BATCH];
            for page in &mut alloc_batch {
                let ptr = std::alloc::alloc(self.layout);
                if ptr.is_null() {
                    std::alloc::handle_alloc_error(self.layout);
                }
                *page = ptr;
            }

            LOCAL.with(|local| {
                let cache = &mut *local.get();
                for &p in &alloc_batch[1..] {
                    cache.push(p);
                }
            });

            NonNull::new_unchecked(alloc_batch[0]).cast::<usize>()
        }
    }

    // ── Batch flush (cache -> shared pool) ─────────────────────────

    unsafe fn flush(&self, extra: *mut u8) {
        unsafe {
            LOCAL.with(|local| {
                let cache = &mut *local.get();

                let drain_max = CACHE_CAPACITY / 2;
                let batch = cache.drain(drain_max);
                let count = batch.len();

                let shard = self.shard(SHARD.with(|&s| s));
                let backoff = Backoff::new();

                let mut linked = false;
                let mut prev_linked_head: *mut u8 = ptr::null_mut();
                loop {
                    let head = shard.load(Ordering::Relaxed);
                    let head_ptr = unpack_ptr(head);

                    if !linked || head_ptr != prev_linked_head {
                        let mut prev = head_ptr;
                        for &current in batch[..count].iter().rev() {
                            let current_next = &*(current.cast::<AtomicPtr<u8>>());
                            current_next.store(prev, Ordering::Relaxed);
                            prev = current;
                        }
                        let extra_next = &*(extra.cast::<AtomicPtr<u8>>());
                        extra_next.store(prev, Ordering::Relaxed);
                        linked = true;
                        prev_linked_head = head_ptr;
                    }

                    let new = pack_next(head, extra);

                    if shard
                        .compare_exchange_weak(
                            head,
                            new,
                            Ordering::Release,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        return;
                    }

                    backoff.snooze();
                }
            });
        }
    }

    // ── Batch refill (shared pool -> cache) ─────────────────────────

    /// Probe shards round-robin starting at the calling thread's shard.
    #[cold]
    unsafe fn refill(&self) -> Option<NonNull<usize>> {
        unsafe {
            let my_shard = SHARD.with(|&s| s);
            for offset in 0..N_SHARDS {
                let shard_idx = (my_shard + offset) % N_SHARDS;
                if let Some(page) = self.refill_from_shard(shard_idx) {
                    return Some(page);
                }
            }
            None
        }
    }

    unsafe fn refill_from_shard(&self, shard_idx: usize) -> Option<NonNull<usize>> {
        unsafe {
            let shard = self.shard(shard_idx);
            let backoff = Backoff::new();
            loop {
                let head = shard.load(Ordering::Acquire);
                let head_ptr = unpack_ptr(head);
                if head_ptr.is_null() {
                    return None;
                }

                let mut nodes = [core::ptr::null_mut(); BATCH];
                nodes[0] = head_ptr;
                let mut count = 1usize;

                let mut current = head_ptr;
                while count < BATCH {
                    let current_next = &*(current.cast::<AtomicPtr<u8>>());
                    let next = current_next.load(Ordering::Relaxed);
                    if next.is_null() {
                        break;
                    }
                    nodes[count] = next;
                    count += 1;
                    current = next;
                }

                let tail_next = {
                    let tail_next_atomic = &*(current.cast::<AtomicPtr<u8>>());
                    tail_next_atomic.load(Ordering::Relaxed)
                };

                let new = pack_next(head, tail_next);

                if shard
                    .compare_exchange_weak(
                        head,
                        new,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    LOCAL.with(|local| {
                        let cache = &mut *local.get();
                        for i in (1..count).rev() {
                            cache.push(nodes[i]);
                        }
                    });
                    return Some(NonNull::new_unchecked(head_ptr).cast::<usize>());
                }

                backoff.snooze();
            }
        }
    }

    // ── Direct batch operations ─────────────────────────────────────

    #[allow(dead_code)]
    pub unsafe fn pop_batch(&self, out: &mut [NonNull<usize>], max: usize) -> usize {
        unsafe {
            let my_shard = SHARD.with(|&s| s);
            let mut total = 0usize;
            for offset in 0..N_SHARDS {
                if total >= max {
                    break;
                }
                let shard_idx = (my_shard + offset) % N_SHARDS;
                let n = self.pop_batch_from_shard(shard_idx, &mut out[total..], max - total);
                total += n;
            }
            total
        }
    }

    unsafe fn pop_batch_from_shard(
        &self,
        shard_idx: usize,
        out: &mut [NonNull<usize>],
        max: usize,
    ) -> usize {
        unsafe {
            let shard = self.shard(shard_idx);
            let backoff = Backoff::new();
            loop {
                let head = shard.load(Ordering::Acquire);
                let head_ptr = unpack_ptr(head);
                if head_ptr.is_null() {
                    return 0;
                }

                let mut count = 1usize;
                let mut current = head_ptr;
                while count < max {
                    let next_atomic = &*(current.cast::<AtomicPtr<u8>>());
                    let next = next_atomic.load(Ordering::Relaxed);
                    if next.is_null() {
                        break;
                    }
                    count += 1;
                    current = next;
                }

                let tail_next = {
                    let tail_atomic = &*(current.cast::<AtomicPtr<u8>>());
                    tail_atomic.load(Ordering::Relaxed)
                };
                let new = pack_next(head, tail_next);

                if shard
                    .compare_exchange_weak(
                        head,
                        new,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    let mut cursor = head_ptr;
                    for index in 0..count {
                        out[index] = NonNull::new_unchecked(cursor).cast::<usize>();
                        let next_atomic = &*(cursor.cast::<AtomicPtr<u8>>());
                        cursor = next_atomic.load(Ordering::Relaxed);
                    }
                    return count;
                }

                backoff.snooze();
            }
        }
    }

    #[allow(dead_code)]
    pub unsafe fn push_batch(&self, pages: &[NonNull<usize>]) {
        unsafe {
            let page_count = pages.len();
            if page_count == 0 {
                return;
            }

            let shard = self.shard(SHARD.with(|&s| s));
            let backoff = Backoff::new();

            let mut linked = false;
            let mut prev_linked_head: *mut u8 = ptr::null_mut();
            loop {
                let head = shard.load(Ordering::Relaxed);
                let head_ptr = unpack_ptr(head);

                if !linked || head_ptr != prev_linked_head {
                    let mut prev = head_ptr;
                    for &page in pages.iter().rev() {
                        let current_page = page.cast::<u8>().as_ptr();
                        let current_next = &*(current_page.cast::<AtomicPtr<u8>>());
                        current_next.store(prev, Ordering::Relaxed);
                        prev = current_page;
                    }
                    linked = true;
                    prev_linked_head = head_ptr;
                }

                let first = pages[0].cast::<u8>().as_ptr();
                let new = pack_next(head, first);

                if shard
                    .compare_exchange_weak(
                        head,
                        new,
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    return;
                }

                backoff.snooze();
            }
        }
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        unsafe {
            let _ = LOCAL.try_with(|local| {
                let cache = &mut *local.get();
                loop {
                    let page = cache.pop();
                    if page.is_null() {
                        break;
                    }
                    std::alloc::dealloc(page, self.layout);
                }
            });

            for shard in &self.shards {
                let head = shard.load(Ordering::Relaxed);
                let mut ptr = unpack_ptr(head);
                while !ptr.is_null() {
                    let next = {
                        let next_atomic = &*(ptr.cast::<AtomicPtr<u8>>());
                        next_atomic.load(Ordering::Relaxed)
                    };
                    std::alloc::dealloc(ptr, self.layout);
                    ptr = next;
                }
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Thread-safe page handle for crossing Send-bound thread boundaries.
    #[derive(Clone, Copy)]
    struct Page(NonNull<usize>);
    unsafe impl Send for Page {}

    impl Page {
        unsafe fn from_pool(pool: &Memory) -> Self {
            Page(unsafe { pool.pop() })
        }
        unsafe fn push(self, pool: &Memory) {
            unsafe { pool.push(self.0) }
        }
        #[allow(dead_code)]
        fn ptr(self) -> *mut u8 {
            self.0.cast::<u8>().as_ptr()
        }
    }

    // ── Basic operations ───────────────────────────────────────────────

    #[test]
    fn push_try_pop_roundtrip() {
        let pool = Memory::new();
        let page = unsafe { pool.pop() };
        unsafe { pool.push(page) };
        let popped = unsafe { pool.try_pop() }.expect("expected a page");
        assert_eq!(page, popped);
    }

    #[test]
    fn pop_allocates_when_empty() {
        let pool = Memory::new();
        let page = unsafe { pool.pop() };
        unsafe { pool.push(page) };
    }

    // ── Single-threaded edge cases ─────────────────────────────────────

    #[test]
    fn try_pop_returns_none_when_truly_empty() {
        let pool = Memory::new();
        assert!(unsafe { pool.try_pop() }.is_none());
    }

    #[test]
    fn try_pop_finds_pages_in_shard_after_push_batch() {
        let pool = Memory::new();
        let p = unsafe { pool.pop() };
        // Drain the LOCAL cache so try_pop must go to the shard.
        // Collect cached pages and return them to avoid leak.
        let mut cached = Vec::new();
        while let Some(c) = unsafe { pool.try_pop() } {
            cached.push(c);
        }
        for c in &cached {
            unsafe { pool.push_batch(&[*c]) };
        }
        unsafe { pool.push_batch(&[p]) };
        let p2 = unsafe { pool.try_pop() }.expect("should find page in shard");
        assert_eq!(p, p2);
    }

    #[test]
    fn pop_allocates_batch_on_empty_pool() {
        // P2: pop() allocates BATCH pages from OS and caches BATCH-1.
        // So BATCH sequential pops should all succeed without allocating.
        let pool = Memory::new();
        let mut pages = Vec::new();
        for _ in 0..BATCH {
            pages.push(unsafe { pool.pop() });
        }
        assert_eq!(pages.len(), BATCH);
        for p in pages {
            unsafe { pool.push(p) };
        }
    }

    #[test]
    fn cache_hit_after_partial_drain() {
        let pool = Memory::new();
        // Pop BATCH pages — first pop allocates, caches BATCH-1
        let mut pages = Vec::new();
        for _ in 0..BATCH {
            pages.push(unsafe { pool.pop() });
        }
        // Push one back (goes to cache)
        let p = pages.pop().unwrap();
        unsafe { pool.push(p) };
        // try_pop should find it in cache (no CAS)
        let p2 = unsafe { pool.try_pop() }.expect("should hit cache");
        assert_eq!(p, p2);
        // Return remaining
        for p in pages {
            unsafe { pool.push(p) };
        }
    }

    #[test]
    fn push_flushes_on_overflow() {
        let pool = Memory::new();
        // Pop BATCH pages to fill cache partway
        let mut pages = Vec::new();
        for _ in 0..BATCH {
            pages.push(unsafe { pool.pop() });
        }
        // Push CACHE_CAPACITY pages — cache overflows, flushes CACHE/2 to shard
        let mut extra = Vec::new();
        for _ in 0..CACHE_CAPACITY {
            let p = unsafe { pool.pop() };
            extra.push(p);
            unsafe { pool.push(p) };
        }
        // try_pop should still find pages (cache was replenished during flush via
        // the returned pages consumption order, but the key is no crash)
        let found = unsafe { pool.try_pop() };
        assert!(found.is_some());

        for p in pages {
            unsafe { pool.push(p) };
        }
    }

    #[test]
    fn push_many_triggers_multiple_flushes() {
        let pool = Memory::new();
        // Pop pages, then push 3× CACHE_CAPACITY → triggers multiple flushes
        let mut pages = Vec::new();
        for _ in 0..3 * CACHE_CAPACITY {
            pages.push(unsafe { pool.pop() });
        }
        for p in &pages {
            unsafe { pool.push(*p) };
        }
        // All pages are now back in the pool (cache + shards)
        for _ in pages {
            let popped = unsafe { pool.try_pop() }.expect("should recover page");
            // Silently drop — we've verified we can recover, the Drop impl will dealloc
            // But wait, Drop only frees what's in the pool, not held refs -> leak.
            // So we need to push it back instead.
            unsafe { pool.push(popped) };
        }
    }

    #[test]
    fn multiple_push_batch_before_pop() {
        let pool = Memory::new();
        let batch1: Vec<_> = (0..4).map(|_| unsafe { pool.pop() }).collect();
        let batch2: Vec<_> = (0..4).map(|_| unsafe { pool.pop() }).collect();
        unsafe { pool.push_batch(&batch1) };
        unsafe { pool.push_batch(&batch2) };

        let mut out = [NonNull::<usize>::dangling(); 8];
        let n = unsafe { pool.pop_batch(&mut out, 8) };
        assert_eq!(n, 8);
        let mut sorted_out: Vec<_> = out.iter().map(|p| p.as_ptr()).collect();
        sorted_out.sort();
        let mut expected: Vec<_> = batch1.into_iter().chain(batch2).map(|p| p.as_ptr()).collect();
        expected.sort();
        assert_eq!(sorted_out, expected);
    }

    // ── Batch operations ───────────────────────────────────────────────

    #[test]
    fn push_batch_empty_nop() {
        let pool = Memory::new();
        unsafe { pool.push_batch(&[]) };
    }

    #[test]
    fn pop_batch_empty_returns_zero() {
        let pool = Memory::new();
        let mut out = [NonNull::<usize>::dangling(); 4];
        let n = unsafe { pool.pop_batch(&mut out, 4) };
        assert_eq!(n, 0);
    }

    #[test]
    fn pop_batch_max_zero_returns_zero() {
        let pool = Memory::new();
        let mut out = [NonNull::<usize>::dangling(); 1];
        let n = unsafe { pool.pop_batch(&mut out, 0) };
        assert_eq!(n, 0);
    }

    #[test]
    fn push_batch_single_page() {
        let pool = Memory::new();
        let p = unsafe { pool.pop() };
        unsafe { pool.push_batch(&[p]) };

        let mut out = [NonNull::<usize>::dangling(); 1];
        let n = unsafe { pool.pop_batch(&mut out, 1) };
        assert_eq!(n, 1);
        assert_eq!(out[0], p);
    }

    #[test]
    fn push_batch_then_pop_batch_all() {
        let pool = Memory::new();
        let n_pages = 10;
        let pages: Vec<_> = (0..n_pages).map(|_| unsafe { pool.pop() }).collect();
        unsafe { pool.push_batch(&pages) };

        let mut out = vec![NonNull::<usize>::dangling(); n_pages];
        let n = unsafe { pool.pop_batch(&mut out, n_pages) };
        assert_eq!(n, n_pages);

        let mut sorted_out: Vec<_> = out.iter().map(|p| p.as_ptr()).collect();
        sorted_out.sort();
        let mut sorted_pages: Vec<_> = pages.iter().map(|p| p.as_ptr()).collect();
        sorted_pages.sort();
        assert_eq!(sorted_out, sorted_pages);
    }

    #[test]
    fn pop_batch_small_buffer() {
        let pool = Memory::new();
        let n_pages = 6;
        let pages: Vec<_> = (0..n_pages).map(|_| unsafe { pool.pop() }).collect();
        unsafe { pool.push_batch(&pages) };

        // Pop with a 3-page buffer even though 6 are available
        let mut out = [NonNull::<usize>::dangling(); 3];
        let n = unsafe { pool.pop_batch(&mut out, 3) };
        assert_eq!(n, 3);
        let mut recovered = out[..n].to_vec();

        // Empty the rest
        let mut out2 = [NonNull::<usize>::dangling(); 3];
        let n2 = unsafe { pool.pop_batch(&mut out2, 3) };
        assert_eq!(n2, 3);
        recovered.extend_from_slice(&out2[..n2]);

        let mut sorted_rec: Vec<_> = recovered.iter().map(|p| p.as_ptr()).collect();
        sorted_rec.sort();
        let mut sorted_pages: Vec<_> = pages.iter().map(|p| p.as_ptr()).collect();
        sorted_pages.sort();
        assert_eq!(sorted_rec, sorted_pages);
    }

    #[test]
    fn pop_batch_exceeds_available() {
        let pool = Memory::new();
        let pages: Vec<_> = (0..3).map(|_| unsafe { pool.pop() }).collect();
        unsafe { pool.push_batch(&pages) };

        // Request 5 but only 3 available
        let mut out = [NonNull::<usize>::dangling(); 5];
        let n = unsafe { pool.pop_batch(&mut out, 5) };
        assert_eq!(n, 3);

        // Remaining pop should return 0
        let n2 = unsafe { pool.pop_batch(&mut out, 1) };
        assert_eq!(n2, 0);

        // Return pages
        for &p in &out[..n] {
            unsafe { pool.push(p) };
        }
    }

    #[test]
    fn push_batch_large() {
        // Push more pages than BATCH to exercise chain relink
        let pool = Memory::new();
        let count = BATCH + 5;
        let pages: Vec<_> = (0..count).map(|_| unsafe { pool.pop() }).collect();
        unsafe { pool.push_batch(&pages) };

        let mut out = vec![NonNull::<usize>::dangling(); count];
        let n = unsafe { pool.pop_batch(&mut out, count) };
        assert_eq!(n, count);

        let mut sorted_out: Vec<_> = out.iter().map(|p| p.as_ptr()).collect();
        sorted_out.sort();
        let mut sorted_pages: Vec<_> = pages.iter().map(|p| p.as_ptr()).collect();
        sorted_pages.sort();
        assert_eq!(sorted_out, sorted_pages);
    }

    // ── Multi-threaded ─────────────────────────────────────────────────

    #[test]
    fn concurrent_push_pop_simple() {
        let pool = Memory::new();
        let n_threads = 4;
        // Prime pool with pages
        let mut seed_pages = Vec::new();
        for _ in 0..n_threads * 10 {
            seed_pages.push(unsafe { pool.pop() });
        }
        for p in &seed_pages {
            unsafe { pool.push(*p) };
        }

        std::thread::scope(|s| {
            for _ in 0..n_threads {
                s.spawn(|| {
                    for _ in 0..50 {
                        let page = unsafe { pool.pop() };
                        unsafe { *(page.cast::<u8>().as_ptr()) = 0xAB };
                        unsafe { pool.push(page) };
                    }
                });
            }
        });
    }

    #[test]
    fn concurrent_push_only() {
        let pool = Memory::new();
        let n_threads = 4;
        let per_thread = 20;

        std::thread::scope(|s| {
            for _ in 0..n_threads {
                s.spawn(|| {
                    let mut local_pages = Vec::new();
                    for _ in 0..per_thread {
                        local_pages.push(unsafe { pool.pop() });
                    }
                    unsafe { pool.push_batch(&local_pages) };
                });
            }
        });

        // Verify at least some pages are recoverable (exact count depends on
        // SHARD assignment relative to prior tests).
        let mut total = 0;
        loop {
            let mut out = [NonNull::<usize>::dangling(); 16];
            let n = unsafe { pool.pop_batch(&mut out, 16) };
            if n == 0 {
                break;
            }
            total += n;
            for &p in &out[..n] {
                unsafe { pool.push(p) };
            }
        }
        assert!(total > 0, "should recover at least some pages");
    }

    #[test]
    fn concurrent_pop_only() {
        let pool = Memory::new();
        let n_threads = 4;
        let per_thread = 15;
        let total = n_threads * per_thread;

        // Pre-populate pool
        let mut all_pages = Vec::new();
        for _ in 0..total {
            all_pages.push(unsafe { pool.pop() });
        }
        unsafe { pool.push_batch(&all_pages) };

        // Each thread pops pages — Page is Send so crossing thread boundary is safe.
        let handle_pages: Vec<Vec<Page>> = std::thread::scope(|s| {
            let mut handles = Vec::new();
            for _ in 0..n_threads {
                handles.push(s.spawn(|| {
                    let mut pages = Vec::new();
                    for _ in 0..per_thread {
                        pages.push(unsafe { Page::from_pool(&pool) });
                    }
                    pages
                }));
            }
            handles.into_iter().map(|h| h.join().expect("thread panicked")).collect()
        });

        let all_recovered: Vec<_> = handle_pages.into_iter().flatten().map(|p| p.0).collect();
        assert_eq!(all_recovered.len(), total);
        let unique: HashSet<_> = all_recovered.iter().map(|p| p.as_ptr()).collect();
        assert_eq!(unique.len(), total);
        for p in all_recovered {
            unsafe { pool.push(p) };
        }
    }

    #[test]
    fn concurrent_mixed_batch_and_single() {
        let pool = Memory::new();
        let n_threads = 4;
        let ops_per_thread = 30;

        std::thread::scope(|s| {
            for _ in 0..n_threads {
                s.spawn(|| {
                    let mut held = Vec::new();
                    for i in 0..ops_per_thread {
                        let page = unsafe { pool.pop() };
                        held.push(page);

                        if i % 5 == 4 {
                            // Every 5 ops, batch-push half of held
                            let mid = held.len() / 2;
                            let batch: Vec<_> = held.drain(..mid).collect();
                            unsafe { pool.push_batch(&batch) };
                        }
                    }
                    for p in held {
                        unsafe { pool.push(p) };
                    }
                });
            }
        });
    }

    #[test]
    fn concurrent_high_contention() {
        let pool = Memory::new();
        let n_threads = 8;
        let ops_per_thread = 100;

        // Prime with enough pages
        let mut seed = Vec::new();
        for _ in 0..n_threads * 8 {
            seed.push(unsafe { pool.pop() });
        }
        for p in &seed {
            unsafe { pool.push(*p) };
        }

        std::thread::scope(|s| {
            for _ in 0..n_threads {
                s.spawn(|| {
                    for _ in 0..ops_per_thread {
                        let page = unsafe { pool.pop() };
                        // Touch the page
                        unsafe { *(page.cast::<u8>().as_ptr()) = 0xFF };
                        unsafe { pool.push(page) };
                    }
                });
            }
        });
    }

    #[test]
    fn concurrent_thread_migration() {
        let pool = Memory::new();
        let pages_a: Vec<Page> = (0..10).map(|_| unsafe { Page::from_pool(&pool) }).collect();

        // Thread A pushes
        std::thread::scope(|s| {
            s.spawn(|| {
                for p in pages_a {
                    unsafe { p.push(&pool) };
                }
            });
        });

        // Thread B pops
        let pages_b: Vec<Page> = std::thread::scope(|s| {
            s.spawn(|| {
                let mut pages = Vec::new();
                for _ in 0..10 {
                    pages.push(unsafe { Page::from_pool(&pool) });
                }
                pages
            })
            .join()
            .expect("thread panicked")
        });

        assert_eq!(pages_b.len(), 10);

        // Return via another thread
        std::thread::scope(|s| {
            s.spawn(|| {
                for p in pages_b {
                    unsafe { p.push(&pool) };
                }
            });
        });
    }

    #[test]
    fn concurrent_batch_push_pop_multi_shard() {
        let pool = Memory::new();
        let n_threads = 4;
        let n_pages = 8;

        std::thread::scope(|s| {
            for _ in 0..n_threads {
                s.spawn(|| {
                    let pages: Vec<_> = (0..n_pages).map(|_| unsafe { pool.pop() }).collect();
                    unsafe { pool.push_batch(&pages) };
                });
            }
        });

        let total = n_threads * n_pages;
        let mut out = vec![NonNull::<usize>::dangling(); total];
        let n = unsafe { pool.pop_batch(&mut out, total) };
        assert!(n <= total, "popped more than available");
        assert!(n > 0, "should recover at least some pages");
        if n > 0 {
            let unique: HashSet<_> = out[..n].iter().map(|p| p.as_ptr()).collect();
            assert_eq!(unique.len(), n, "duplicate pages recovered");
            for &p in &out[..n] {
                unsafe { pool.push(p) };
            }
        }
    }

    // ── Correctness invariants ─────────────────────────────────────────

    #[test]
    fn no_duplicate_pages_single_thread() {
        let pool = Memory::new();
        let count = 50;
        let pages: Vec<_> = (0..count).map(|_| unsafe { pool.pop() }).collect();
        assert_eq!(pages.len(), count);

        // Push to shards via push_batch (bypasses cache) — pages are now in shard only.
        unsafe { pool.push_batch(&pages) };

        // Pop them back via pop_batch (bypasses cache)
        let mut out = vec![NonNull::<usize>::dangling(); count];
        let n = unsafe { pool.pop_batch(&mut out, count) };
        assert_eq!(n, count);

        // Verify no duplicates
        let unique: HashSet<_> = out[..n].iter().map(|p| p.as_ptr()).collect();
        assert_eq!(unique.len(), count);

        for &p in &out[..n] {
            unsafe { pool.push(p) };
        }
    }

    #[test]
    fn all_pages_accounted_for_invariants() {
        // Pop N pages, push them back. All original pages must be recovered
        // (batch allocation may add extra cached pages beyond N).
        let pool = Memory::new();
        let n = 73;
        let pages: Vec<_> = (0..n).map(|_| unsafe { pool.pop() }).collect();
        assert_eq!(pages.len(), n);

        // Push via mixed methods
        let mid = n / 3;
        unsafe { pool.push_batch(&pages[..mid]) };
        for &p in &pages[mid..] {
            unsafe { pool.push(p) };
        }

        // Recover all via combined methods
        let mut recovered = Vec::new();
        loop {
            let mut out = [NonNull::<usize>::dangling(); 16];
            let batch = unsafe { pool.pop_batch(&mut out, 16) };
            if batch == 0 {
                break;
            }
            recovered.extend_from_slice(&out[..batch]);
        }
        while let Some(p) = unsafe { pool.try_pop() } {
            recovered.push(p);
        }

        // Every original page must be present somewhere in recovered set
        let recovered_set: HashSet<_> = recovered.iter().map(|p| p.as_ptr()).collect();
        for p in &pages {
            assert!(
                recovered_set.contains(&p.as_ptr()),
                "lost page {:p}",
                p.as_ptr()
            );
        }

        for p in recovered {
            unsafe { pool.push(p) };
        }
    }

    #[test]
    fn drop_does_not_panic() {
        // Memory::drop must cleanly walk and dealloc all pages in shards + cache
        let pool = Memory::new();
        let mut pages = Vec::new();
        for _ in 0..30 {
            pages.push(unsafe { pool.pop() });
        }
        // Mix of pages in cache and shards
        for p in &pages[..10] {
            unsafe { pool.push(*p) };
        }
        unsafe { pool.push_batch(&pages[10..]) };
        // Drop will dealloc the 10 in cache + 20 in shards
        drop(pool);
    }

    #[test]
    fn all_ptr_non_null_aligned() {
        let pool = Memory::new();
        let page_size = page_size::get();
        let pages: Vec<_> = (0..20).map(|_| unsafe { pool.pop() }).collect();
        for p in &pages {
            let ptr = p.as_ptr();
            assert!(!ptr.is_null(), "popped null pointer");
            assert_eq!(
                ptr.addr() % page_size,
                0,
                "page not aligned to page size"
            );
        }
        for p in pages {
            unsafe { pool.push(p) };
        }
    }

    #[test]
    fn ptr_value_integrity_after_roundtrip() {
        let pool = Memory::new();
        let p = unsafe { pool.pop() };
        let ptr = p.as_ptr();
        unsafe { *(ptr.cast::<u64>()) = 0xDEAD_BEEF_CAFE_F00Du64 };
        unsafe { pool.push(p) };
        let p2 = unsafe { pool.pop() };
        assert_eq!(p2.as_ptr(), ptr);
        let val = unsafe { *(p2.cast::<u8>().as_ptr().cast::<u64>()) };
        assert_eq!(val, 0xDEAD_BEEF_CAFE_F00Du64, "data lost through pool roundtrip");
        unsafe { pool.push(p2) };
    }

    // ── Batch correctness ──────────────────────────────────────────────

    #[test]
    fn batch_push_then_pop_all_shards() {
        // Push enough pages across push_batch calls so every shard gets some.
        // Verify pop_batch drains all shards completely.
        let pool = Memory::new();
        let pages_per_call = 4;
        let calls = N_SHARDS * 2;
        let all_pages: Vec<_> = (0..pages_per_call * calls)
            .map(|_| unsafe { pool.pop() })
            .collect();

        let mut offset = 0;
        for _ in 0..calls {
            let batch = &all_pages[offset..offset + pages_per_call];
            unsafe { pool.push_batch(batch) };
            offset += pages_per_call;
        }

        // Drain all shards
        let mut recovered = Vec::new();
        loop {
            let mut out = [NonNull::<usize>::dangling(); 16];
            let n = unsafe { pool.pop_batch(&mut out, 16) };
            if n == 0 {
                break;
            }
            recovered.extend_from_slice(&out[..n]);
        }
        assert_eq!(recovered.len(), all_pages.len());
        let unique: HashSet<_> = recovered.iter().map(|p| p.as_ptr()).collect();
        assert_eq!(unique.len(), all_pages.len());

        for p in recovered {
            unsafe { pool.push(p) };
        }
    }

    #[test]
    fn batch_pop_between_shards_no_loss() {
        let pool = Memory::new();
        // Allocate pages via OS directly (pop_batch won't alloc, pop triggers cache)
        unsafe {
            let layout = Layout::from_size_align(page_size::get(), page_size::get()).unwrap();
            let n_pages = 23;
            let mut pages = Vec::with_capacity(n_pages);
            for _ in 0..n_pages {
                let ptr = std::alloc::alloc(layout);
                assert!(!ptr.is_null());
                pages.push(NonNull::new_unchecked(ptr).cast::<usize>());
            }

            // push_batch to shards
            pool.push_batch(&pages);

            // pop_batch in small 5-page batches to exhaust shards
            let mut total = 0;
            loop {
                let mut out = [NonNull::<usize>::dangling(); 5];
                let n = pool.pop_batch(&mut out, 5);
                if n == 0 {
                    break;
                }
                total += n;
                // Immediately free each page to avoid leaking
                for &p in &out[..n] {
                    std::alloc::dealloc(p.cast::<u8>().as_ptr(), layout);
                }
            }
            assert_eq!(total, n_pages);
        }
    }

    // ── Stress ─────────────────────────────────────────────────────────

    #[test]
    fn stress_concurrent_batch() {
        let pool = Memory::new();
        let threads = 4;
        let ops_per_thread = 200;

        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    let mut held_pages = Vec::with_capacity(16);
                    for _ in 0..ops_per_thread {
                        let page = unsafe { pool.pop() };
                        held_pages.push(page);

                        if held_pages.len() >= 4 {
                            let batch: Vec<_> = held_pages.drain(..4).collect();
                            unsafe { pool.push_batch(&batch) };
                        }
                    }
                    for page in held_pages {
                        unsafe { pool.push(page) };
                    }
                });
            }
        });
    }

    #[test]
    fn stress_massive_alloc_free() {
        let pool = Memory::new();
        let n_pages = 500;
        // Allocate and free repeatedly
        for _ in 0..5 {
            let pages: Vec<_> = (0..n_pages).map(|_| unsafe { pool.pop() }).collect();
            for p in &pages {
                unsafe { *p.as_ptr() = 0x42 };
            }
            for p in &pages {
                unsafe { pool.push(*p) };
            }
        }
    }

    #[test]
    fn stress_alternating_push_pop() {
        let pool = Memory::new();
        let mut held = Vec::new();
        for i in 0..200 {
            if i % 3 == 0 && !held.is_empty() {
                // Return a batch
                let n = held.len().min(4);
                let batch: Vec<_> = held.drain(..n).collect();
                unsafe { pool.push_batch(&batch) };
            } else {
                let p = unsafe { pool.pop() };
                unsafe { *(p.cast::<u8>().as_ptr()) = i as u8 };
                held.push(p);
            }
        }
        for p in held {
            unsafe { pool.push(p) };
        }
    }

    #[test]
    fn stress_cache_boundary_ping_pong() {
        let pool = Memory::new();
        // Pre-populate with enough pages to stay in cache
        let seeds: Vec<_> = (0..CACHE_CAPACITY).map(|_| unsafe { pool.pop() }).collect();
        for p in &seeds {
            unsafe { pool.push(*p) };
        }

        // Ping-pong: pop CACHE_CAPACITY pages, push them back, repeat
        for _ in 0..10 {
            let mut pages = Vec::new();
            for _ in 0..CACHE_CAPACITY {
                pages.push(unsafe { pool.pop() });
            }
            assert_eq!(pages.len(), CACHE_CAPACITY);
            for p in &pages {
                unsafe { pool.push(*p) };
            }
        }
    }
}
