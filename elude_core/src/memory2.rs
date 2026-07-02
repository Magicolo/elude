use core::{
    alloc::Layout,
    mem::take,
    num::NonZeroUsize,
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
};
use std::alloc::{alloc, dealloc, handle_alloc_error};

pub struct Page {
    data: NonNull<u8>,
    // Each bit represents a chunk in the page.
    slots: AtomicU64,
    // Next page of the same size.
    next: AtomicPtr<Page>,
}

pub struct Handle {
    page: NonNull<Page>,
    slot: u32,
}

pub struct Memory {
    // First page for each size.
    pages: [AtomicPtr<Page>; 7],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Size {
    _64 = 0,
    _128 = 1,
    _256 = 2,
    _512 = 3,
    _1024 = 4,
    _2048 = 5,
    _4096 = 6,
}

impl Size {
    #[inline]
    const fn size(self) -> u32 {
        Self::index_to_size(self.index())
    }

    #[inline]
    const fn index(self) -> u8 {
        self as u8
    }

    #[inline]
    const fn index_to_size(index: u8) -> u32 {
        1u32 << (index + 6)
    }
}

// SAFETY: `Page` is safe to send between threads because all mutation to its
// fields goes through atomic operations (`AtomicU64` for slots, `AtomicPtr`
// for next). The `data` field is immutable after construction and is only
// read after the page is linked into the global list. Shared access is safe
// because all mutation is atomic and the data pointer is never mutated.
unsafe impl Send for Page {}
unsafe impl Sync for Page {}

// SAFETY: `Handle` is meant to be transferred between threads. The `page`
// field points to a `Page` behind an `AtomicPtr` whose slots are modified
// atomically. It is thread-safe to access from any thread as long as `Memory`
// outlives all `Handle`s.
unsafe impl Send for Handle {}

impl Handle {
    #[inline]
    pub fn get(&self) -> NonNull<u8> {
        unsafe { self.page.as_ref().get_slot(self.slot) }
    }
}

impl Memory {
    pub const fn new() -> Self {
        Self {
            pages: [
                AtomicPtr::new(null_mut()),
                AtomicPtr::new(null_mut()),
                AtomicPtr::new(null_mut()),
                AtomicPtr::new(null_mut()),
                AtomicPtr::new(null_mut()),
                AtomicPtr::new(null_mut()),
                AtomicPtr::new(null_mut()),
            ],
        }
    }

    pub fn next(&self, size: Size) -> Handle {
        let head = unsafe { self.pages.get_unchecked(size.index() as usize) };
        let mut current = head;
        loop {
            let raw = current.load(Ordering::Acquire);
            let page_ptr = match NonNull::new(raw) {
                Some(p) => p,
                None => break,
            };
            let page = unsafe { page_ptr.as_ref() };
            loop {
                let slot = page.next_slot();
                if slot >= 64 {
                    break;
                }
                if page.try_claim_slot(slot) {
                    return Handle {
                        page: page_ptr,
                        slot,
                    };
                }
            }
            current = &page.next;
        }

        let page = Box::new(Page::new(size));
        // Mark slot 0 as used before sharing the page.
        page.slots
            .store(page.slots.load(Ordering::Relaxed) | 1, Ordering::Relaxed);
        let page = Box::into_raw(page);
        loop {
            let head = current.load(Ordering::Relaxed);
            unsafe { (*page).next.store(head, Ordering::Relaxed) };
            if current
                .compare_exchange(head, page, Ordering::Release, Ordering::Acquire)
                .is_ok()
            {
                return Handle {
                    page: unsafe { NonNull::new_unchecked(page) },
                    slot: 0,
                };
            }
        }
    }
}

impl Page {
    const LAYOUT: Layout =
        unsafe { Layout::from_size_align_unchecked(Self::SIZE as _, Self::SIZE as _) };
    const MASK: usize = 0b1111;
    const SIZE: u32 = 4096;

    pub fn new(size: Size) -> Self {
        Self {
            data: match NonNull::new(unsafe { alloc(Self::LAYOUT) }) {
                Some(data) => Self::add_size(data, size.index()),
                None => handle_alloc_error(Self::LAYOUT),
            },
            slots: AtomicU64::new(Self::mask(size.size())),
            next: AtomicPtr::new(null_mut()),
        }
    }

    /// Sets the upper bits of the slots to 1 for a given size. This is used to
    /// mark the slots as unavailable for allocation.
    #[inline]
    const fn mask(size: u32) -> u64 {
        let shift = Self::SIZE.saturating_div(size);
        !1u64.unbounded_shl(shift).wrapping_sub(1)
    }

    #[inline]
    pub fn size(&self) -> u32 {
        Size::index_to_size(Self::get_size(self.data))
    }

    #[inline]
    pub fn data(&self) -> NonNull<u8> {
        Self::remove_size(self.data)
    }

    #[inline]
    pub fn get_slot(&self, slot: u32) -> NonNull<u8> {
        unsafe { self.data().add((self.size() * slot) as usize) }
    }

    #[inline]
    fn next_slot(&self) -> u32 {
        self.slots.load(Ordering::Relaxed).trailing_ones()
    }

    #[inline]
    fn try_claim_slot(&self, slot: u32) -> bool {
        match 1u64.checked_shl(slot) {
            Some(bit) => self.slots.fetch_or(bit, Ordering::Acquire) & bit == 0,
            None => false,
        }
    }

    #[inline]
    fn release_slot(&self, slot: u32) -> bool {
        match 1u64.checked_shl(slot) {
            Some(bit) => {
                self.slots.fetch_and(!bit, Ordering::Release);
                true
            }
            None => false,
        }
    }

    #[inline]
    fn add_size(data: NonNull<u8>, index: u8) -> NonNull<u8> {
        match NonZeroUsize::new(data.addr().get() | index as usize) {
            Some(address) => data.with_addr(address),
            None => data,
        }
    }

    #[inline]
    fn get_size(data: NonNull<u8>) -> u8 {
        (data.addr().get() & Self::MASK) as u8
    }

    #[inline]
    fn remove_size(data: NonNull<u8>) -> NonNull<u8> {
        match NonZeroUsize::new(data.addr().get() & !Self::MASK) {
            Some(address) => data.with_addr(address),
            None => data,
        }
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { self.page.as_ref() }.release_slot(self.slot);
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        unsafe { dealloc(self.data().as_ptr(), Self::LAYOUT) };
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        for head in self.pages.iter_mut() {
            let mut raw = take(head.get_mut());
            while !raw.is_null() {
                let page = unsafe { Box::from_raw(raw) };
                raw = page.next.load(Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use checkito::*;
    use core::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    fn any_size() -> impl Generate<Item = Size> {
        checkito::Generate::map(0u32..7, |i| match i {
            0 => Size::_64,
            1 => Size::_128,
            2 => Size::_256,
            3 => Size::_512,
            4 => Size::_1024,
            5 => Size::_2048,
            _ => Size::_4096,
        })
    }

    #[check(any_size())]
    fn alloc_returns_non_overlapping_pointers(size: Size) {
        let memory = Memory::new();
        let slots = Page::SIZE / size.size();
        let mut ptrs: Vec<Handle> = Vec::with_capacity(slots as usize);
        for _ in 0..slots {
            let p = memory.next(size);
            let start = p.get().as_ptr() as usize;
            let end = start + size.size() as usize;
            for prev in &ptrs {
                let s = prev.get().as_ptr() as usize;
                let e = s + size.size() as usize;
                assert!(end <= s || start >= e, "overlapping allocations");
            }
            ptrs.push(p);
        }
        assert!(ptrs.len() == slots as usize);
    }

    #[check(any_size())]
    fn alloc_then_drop_and_realloc_reuses_slot(size: Size) {
        let memory = Memory::new();
        let p = memory.next(size);
        let addr = p.get().as_ptr() as usize;
        drop(p);
        let p2 = memory.next(size);
        assert_eq!(p2.get().as_ptr() as usize, addr, "should reuse freed slot");
    }

    #[test]
    fn concurrent_allocations_non_overlapping() {
        let memory = Memory::new();
        let size = Size::_256;
        let slots = Page::SIZE / size.size();
        let ptrs: Mutex<Vec<(Handle, usize, usize)>> = Mutex::new(Vec::new());

        std::thread::scope(|scope| {
            for _ in 0..slots {
                scope.spawn(|| {
                    let p = memory.next(size);
                    let mut held = ptrs.lock().unwrap();
                    let start = p.get().as_ptr() as usize;
                    let end = start + size.size() as usize;
                    for (_, s, e) in &*held {
                        assert!(end <= *s || start >= *e, "concurrent allocation overlap");
                    }
                    held.push((p, start, end));
                });
            }
        });
    }

    #[test]
    fn concurrent_alloc_all_slots() {
        let memory = Memory::new();
        let size = Size::_128;
        let slots = Page::SIZE / size.size();
        let count = AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for _ in 0..slots {
                scope.spawn(|| {
                    let _p = memory.next(size);
                    count.fetch_add(1, Ordering::Relaxed);
                });
            }
        });

        assert_eq!(count.load(Ordering::Relaxed), slots as usize);
    }
}
