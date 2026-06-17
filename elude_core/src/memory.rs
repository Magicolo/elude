use core::{
    alloc::Layout,
    cell::UnsafeCell,
    ptr::{self, NonNull},
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};
use crossbeam_utils::{Backoff, CachePadded};

// ════════════════════════════════════════════════════════════════════
// Design
// ════════════════════════════════════════════════════════════════════
//
// Lock-free page pool backed by a Treiber stack.  Each stack node is a
// page itself: the first 8 bytes store the `next` pointer via
// `AtomicPtr<u8>` so every access is data-race-free.
//
// `AtomicUsize` is used for the head because a single CAS must update
// both the pointer and a 16-bit ABA counter — two fields that cannot
// be packed into `AtomicPtr`.
//
// A per-thread cache (thread-local) avoids atomic ops on the hot path.
// The thread-unsafety guarantee is enforced by `!Send` on the raw
// pointers inside the cache.

// ════════════════════════════════════════════════════════════════════
// Configuration  (tuned for 4 KiB+ page pool)
// ════════════════════════════════════════════════════════════════════

/// Capacity of the per-thread local page cache.  Must be ≥ 1.
/// Kept small (8) to bound wasted memory when threads exit without
/// draining their cache (those pages leak to the OS).
const CACHE_CAPACITY: usize = 8;

/// Pages transferred in one batch between the thread cache and the
/// shared pool.  Must be ≤ CACHE_CAPACITY.  Half the cache is drained
/// on each `flush`, giving room for the incoming page.
const BATCH: usize = 4;

// ════════════════════════════════════════════════════════════════════
// Tagged-pointer encoding
// ════════════════════════════════════════════════════════════════════
//
// The head word is laid out as:
//
//   [63:48] 16-bit ABA counter
//   [47: 0] pointer >> PAGE_SHIFT  (48 bits → up to 256 TB address space)
//
// Page alignment guarantees the lower PAGE_SHIFT bits are always zero,
// so the pointer can be right-shifted to fit.  The counter prevents
// ABA: when a CAS fails because a concurrent push/pop cycled the head
// back to the same pointer, the counter will differ and the CAS fails
// correctly.

/// Page alignment shift (4 KiB pages; larger pages leave extra zeros).
const PAGE_SHIFT: usize = 12;
/// Bits reserved for the ABA counter (upper 16 bits).
const COUNTER_SHIFT: usize = 48;

/// Mask isolating the shifted-pointer field (lower COUNTER_SHIFT bits).
const POINTER_MASK: usize = (1 << COUNTER_SHIFT) - 1;

/// Mask isolating the counter field (upper 16 bits).
const COUNTER_MASK: usize = !POINTER_MASK;

/// One ABA step: add this to the packed word to increment the counter.
const COUNTER_INCREMENT: usize = 1 << COUNTER_SHIFT;

// ── Hot inline helpers ─────────────────────────────────────────────

/// Shift a page-aligned pointer right by `PAGE_SHIFT` to fit into the
/// packed representation.  Safe because `usize → usize` is trivial.
#[inline(always)]
fn shift_ptr(ptr: *mut u8) -> usize {
    ptr.addr() >> PAGE_SHIFT
}

/// Extract the raw pointer from a packed word.  Returns a pointer that
/// may be null — callers must check before dereferencing.
#[inline(always)]
unsafe fn unpack_ptr(word: usize) -> *mut u8 {
    let addr = (word & POINTER_MASK) << PAGE_SHIFT;
    ptr::with_exposed_provenance_mut(addr)
}

/// Build a new packed word: increment the counter and replace the
/// pointer field.  The counter wraps at 16 bits (65 536 steps);
/// this is safe because the CAS comparison will fail if a concurrent
/// push/pop cycled the head to the same pointer with a different count.
#[inline(always)]
fn pack_next(word: usize, new_ptr: *mut u8) -> usize {
    (word.wrapping_add(COUNTER_INCREMENT) & COUNTER_MASK) | shift_ptr(new_ptr)
}

// ════════════════════════════════════════════════════════════════════
// Thread-local page cache
// ════════════════════════════════════════════════════════════════════

/// Fixed-capacity LIFO stack of page pointers, owned by one thread.
///
/// # Safety
///
/// `UnsafeCell` allows interior mutability without runtime borrow
/// checking.  This is sound because:
///
/// 1. Each thread has its own instance via `thread_local!`.
/// 2. `*mut u8` is `!Send`, so the struct cannot leave its thread.
/// 3. No re-entrant access is possible — the `.with()` closure runs
///    synchronously and does not call back into the cache.
struct LocalCache {
    /// Non-null page pointers; slots `[0, len)` are live.
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

    /// Pop one page.  Returns null when empty.
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

    /// Push one page.  Returns `false` when full.
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

    /// Remove up to `max` pages and return a slice (LIFO order:
    /// index 0 = most recently pushed).
    #[inline]
    fn drain(&mut self, max: usize) -> &mut [*mut u8] {
        let length = self.len as usize;
        let count = length.min(max);
        if count == 0 {
            return &mut [];
        }
        debug_assert!(count <= length);
        let start = length - count;
        self.len = start as u8;
        &mut self.pages[start..length]
    }

}

thread_local! {
    static LOCAL: UnsafeCell<LocalCache> =
        const { UnsafeCell::new(LocalCache::new()) };
}

// ════════════════════════════════════════════════════════════════════
// Memory pool — cache-line padded to prevent false sharing
// ════════════════════════════════════════════════════════════════════
//
// `CachePadded` keeps `head` on its own cache line, preventing false
// sharing: a CAS on `head` bounces the cache line across cores;
// without padding the `layout` field (read on every `pop`/`push`)
// would reside on the same line and be invalidated too.

pub struct Memory {
    head: CachePadded<AtomicUsize>,
    layout: Layout,
}

impl Memory {
    pub fn new() -> Self {
        let page_size = page_size::get();
        let layout = Layout::from_size_align(page_size, page_size)
            .expect("page size is a power of two and non-zero");
        Self {
            head: CachePadded::new(AtomicUsize::new(0)),
            layout,
        }
    }

    // ── Public API ─────────────────────────────────────────────────

    /// Return a page to the pool.  On success the page moves into the
    /// thread-local cache; if the cache is full, half of it is flushed
    /// to the shared pool in a single CAS along with this page.
    #[inline]
    pub unsafe fn push(&self, page: NonNull<usize>) {
        unsafe {
            let page_ptr = page.cast::<u8>().as_ptr();

            // Fast path: stash in thread-local cache (no atomics).
            if LOCAL.with(|local| (*local.get()).push(page_ptr)) {
                return;
            }

            // Slow path: flush half the cache + this page to shared pool.
            self.flush(page_ptr);
        }
    }

    /// Pop a page from the pool, or `None` if completely empty.  The
    /// thread-local cache is checked first (no atomics); on a miss,
    /// up to `BATCH` pages are transferred from the shared pool into
    /// the cache and one is returned.
    #[inline]
    pub unsafe fn try_pop(&self) -> Option<NonNull<usize>> {
        unsafe {
            // Fast path: consult thread-local cache.
            let page = LOCAL.with(|local| (*local.get()).pop());
            if !page.is_null() {
                return Some(NonNull::new_unchecked(page).cast::<usize>());
            }

            self.refill()
        }
    }

    /// Pop a page from the pool, or allocate a new one.  This never
    /// returns `None` — the pool is a performance cache, not a
    /// capacity gate.
    #[inline]
    pub unsafe fn pop(&self) -> NonNull<usize> {
        unsafe {
            if let Some(page) = self.try_pop() {
                return page;
            }
            let ptr = std::alloc::alloc(self.layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(self.layout);
            }
            NonNull::new_unchecked(ptr).cast::<usize>()
        }
    }

    // ── Batch flush (cache → shared pool) ──────────────────────────

    /// Flush half the thread-local cache plus one extra page to the
    /// shared pool.  The entire batch is linked and pushed in one CAS,
    /// so concurrent pops see either all or none of the flushed pages.
    /// `Release` ordering on the CAS makes all `Relaxed` next-pointer
    /// stores visible to the thread that eventually `Acquire`-loads head.
    unsafe fn flush(&self, extra: *mut u8) {
        unsafe {
            LOCAL.with(|local| {
                let cache = &mut *local.get();

                // Drain half the cache to free room.
                let drain_max = CACHE_CAPACITY / 2;
                let batch = cache.drain(drain_max);
                let count = batch.len();

                let backoff = Backoff::new();
                loop {
                    let head = self.head.load(Ordering::Relaxed);
                    let head_ptr = unpack_ptr(head);

                    // Build a chain: extra → batch[0] → … → batch[n-1] → old_head.
                    // Link backwards from the tail.
                    let mut prev = head_ptr;
                    for &current in batch[..count].iter().rev() {
                        let current_next = &*(current.cast::<AtomicPtr<u8>>());
                        current_next.store(prev, Ordering::Relaxed);
                        prev = current;
                    }
                    // Wire extra → first drained page (or old_head if batch empty).
                    let extra_next = &*(extra.cast::<AtomicPtr<u8>>());
                    extra_next.store(prev, Ordering::Relaxed);

                    let new = pack_next(head, extra);

                    if self
                        .head
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

    // ── Batch refill (shared pool → cache) ─────────────────────────

    /// Pop up to `BATCH` pages from the shared pool in one CAS, cache
    /// all but one, and return one.  `Acquire` on the CAS pairs with
    /// the `Release` in `flush`, ensuring all next-pointer stores from
    /// the pusher are visible before we dereference them.  Pages are
    /// cached in reverse order so that `LocalCache::pop` returns them
    /// in LIFO (stack) order.
    #[cold]
    unsafe fn refill(&self) -> Option<NonNull<usize>> {
        unsafe {
            let backoff = Backoff::new();
            loop {
                let head = self.head.load(Ordering::Acquire);
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
                    // count < BATCH → index in bounds for nodes[count]
                    nodes[count] = next;
                    count += 1;
                    current = next;
                }

                // `current` is the last node we traversed; its next is the
                // new head candidate after removing all nodes.
                let tail_next = {
                    let tail_next_atomic = &*(current.cast::<AtomicPtr<u8>>());
                    tail_next_atomic.load(Ordering::Relaxed)
                };

                let new = pack_next(head, tail_next);

                if self
                    .head
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
                        // Fill in reverse so that pop() returns them
                        // in stack order (nodes[1] first, nodes[count-1] last).
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

    /// Pop up to `max` pages directly from the shared pool (bypassing
    /// the thread cache).  Returns how many were obtained.
    #[allow(dead_code)]
    pub unsafe fn pop_batch(&self, out: &mut [NonNull<usize>], max: usize) -> usize {
        unsafe {
            let backoff = Backoff::new();
            loop {
                let head = self.head.load(Ordering::Acquire);
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

                if self
                    .head
                    .compare_exchange_weak(
                        head,
                        new,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    // Walk the popped chain and fill the output.
                    let mut cursor = head_ptr;
                    for index in 0..count {
                        // index < count, and count ≤ max ≤ out.len() (caller invariant)
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

    /// Push multiple pages to the shared pool in a single CAS.
    /// `pages[0]` becomes the new stack head.
    #[allow(dead_code)]
    pub unsafe fn push_batch(&self, pages: &[NonNull<usize>]) {
        unsafe {
            let page_count = pages.len();
            if page_count == 0 {
                return;
            }

            let backoff = Backoff::new();
            loop {
                let head = self.head.load(Ordering::Relaxed);
                let mut prev = unpack_ptr(head);
                for &page in pages.iter().rev() {
                    let current_page = page.cast::<u8>().as_ptr();
                    let current_next = &*(current_page.cast::<AtomicPtr<u8>>());
                    current_next.store(prev, Ordering::Relaxed);
                    prev = current_page;
                }

                let first = pages[0].cast::<u8>().as_ptr();
                let new = pack_next(head, first);

                if self
                    .head
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

// ── Drop ───────────────────────────────────────────────────────────
//
// Pages held by other threads' local caches are intentionally leaked.
// This is standard for thread-local pools: the OS reclaims the memory
// on process exit.  The alternative (a global registry of all caches)
// would add synchronization overhead to every fast-path push/pop.
impl Drop for Memory {
    fn drop(&mut self) {
        unsafe {
            // Best-effort: drain the current thread's cache.
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

            // Drain the shared free list.
            let head = self.head.load(Ordering::Relaxed);
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

// ════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_try_pop_roundtrip() {
        let pool = Memory::new();
        let page = unsafe { pool.pop() };
        unsafe { pool.push(page) };
        let popped = unsafe { pool.try_pop() }.expect("expected a page");
        assert_eq!(page, popped);
        assert!(unsafe { pool.try_pop() }.is_none());
    }

    #[test]
    fn pop_allocates_when_empty() {
        let pool = Memory::new();
        let page = unsafe { pool.pop() };
        unsafe { pool.push(page) };
    }

    #[test]
    fn lock_free_multi_thread() {
        let pool = Memory::new();
        let count = 4;
        let mut pages = Vec::new();
        for _ in 0..count {
            pages.push(unsafe { pool.pop() });
        }
        for page in &pages {
            unsafe { pool.push(*page) };
        }
        std::thread::scope(|s| {
            for _ in 0..count {
                s.spawn(|| {
                    for _ in 0..100 {
                        let page = unsafe { pool.pop() };
                        unsafe { *(page.cast::<u8>().as_ptr()) = 0xAB };
                        unsafe { pool.push(page) };
                    }
                });
            }
        });
        for _ in 0..count {
            unsafe { pool.try_pop() }.expect("all pages should be returned");
        }
        assert!(unsafe { pool.try_pop() }.is_none());
    }

    #[test]
    fn batch_push_pop_direct() {
        let pool = Memory::new();
        let mut pages = Vec::new();
        for _ in 0..6 {
            pages.push(unsafe { pool.pop() });
        }
        unsafe { pool.push_batch(&pages) };

        // push_batch puts pages[0] as head → pop order is pages[0], pages[1], …
        for expected in pages.iter() {
            let got = unsafe { pool.try_pop() }.expect("expected page");
            assert_eq!(got, *expected);
        }
        assert!(unsafe { pool.try_pop() }.is_none());
    }

    #[test]
    fn batch_pop_from_pool() {
        let pool = Memory::new();
        let total_count = 10usize;

        let mut original_pages = Vec::with_capacity(total_count);
        for _ in 0..total_count {
            original_pages.push(unsafe { pool.pop() });
        }
        // Push all directly to shared pool via push_batch to avoid
        // thread-cache ordering effects.
        unsafe { pool.push_batch(&original_pages) };

        // pop_batch bypasses the thread cache.
        let mut output_buffer = [NonNull::<usize>::dangling(); 4];
        let got = unsafe { pool.pop_batch(&mut output_buffer, 4) };
        assert_eq!(got, 4);
        // All 4 popped pages must be unique and from our set.
        let mut popped_pages = Vec::from(output_buffer);
        popped_pages.sort_by(|a, b| a.as_ptr().cmp(&b.as_ptr()));
        let mut sorted_original: Vec<_> = original_pages.iter().copied().collect();
        sorted_original.sort_by(|a, b| a.as_ptr().cmp(&b.as_ptr()));
        // popped_pages must be a subset of original_pages
        for page in &popped_pages {
            assert!(sorted_original.contains(page));
        }
        // Remaining pages should still be retrievable.
        let mut remaining = 0;
        while unsafe { pool.try_pop() }.is_some() {
            remaining += 1;
        }
        assert_eq!(remaining, total_count - 4);
    }

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
                        // Pop a page.
                        let page = unsafe { pool.pop() };
                        held_pages.push(page);

                        // Sometimes push back in a batch.
                        if held_pages.len() >= 4 {
                            let batch: Vec<_> = held_pages.drain(..4).collect();
                            unsafe { pool.push_batch(&batch) };
                        }
                    }
                    // Return remaining.
                    for page in held_pages {
                        unsafe { pool.push(page) };
                    }
                });
            }
        });

        // All pages must be back in the pool.
        let mut count = 0;
        while unsafe { pool.try_pop() }.is_some() {
            count += 1;
        }
        assert!(count > 0, "expected pages in the pool");
    }
}
