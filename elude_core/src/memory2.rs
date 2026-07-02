use core::{
    alloc::Layout,
    mem::take,
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
    data: NonNull<u8>,
    page: *mut Page,
    slot: u64,
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
    const fn size(self) -> u32 {
        Self::index_to_size(self as u32)
    }

    const fn index(self) -> usize {
        self as usize
    }

    const fn index_to_size(index: u32) -> u32 {
        1u32 << (index + 6)
    }
}

// SAFETY: `Handle` is meant to be transferred between threads. The `page`
// field points to a `Page` behind an `AtomicPtr` whose slots are modified
// atomically, and `data` is a derived offset within that page's buffer. Both
// are thread-safe to access from any thread as long as `Memory` outlives all
// `Handle`s.
unsafe impl Send for Handle {}

impl Handle {
    #[inline]
    pub const fn get(&self) -> NonNull<u8> {
        self.data
    }
}

impl Memory {
    const LAYOUT: Layout =
        unsafe { Layout::from_size_align_unchecked(Self::SIZE as _, Self::SIZE as _) };
    const SIZE: u32 = 4096;

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

    /// Sets the upper bits of the slots to 1 for a given size. This is used to
    /// mark the slots as unavailable for allocation.
    const fn mask(size: u32) -> u64 {
        let shift = Self::SIZE.saturating_div(size);
        !1u64.unbounded_shl(shift).wrapping_sub(1)
    }

    pub fn next(&self, size: Size) -> Handle {
        let mut old = unsafe { self.pages.get_unchecked(size.index()) };
        loop {
            let raw = old.load(Ordering::Acquire);
            if raw.is_null() {
                break;
            }

            let page = unsafe { &*raw };
            let mut index = u32::MAX;
            let mut slot = u64::MAX;
            let slots = page
                .slots
                .try_update(Ordering::Acquire, Ordering::Relaxed, |slots| {
                    index = slots.trailing_ones();
                    slot = 1u64.checked_shl(index)?;
                    Some(slots | slot)
                });
            match slots {
                Ok(_) => {
                    return Handle {
                        data: unsafe { page.data.add(index.saturating_mul(size.size()) as usize) },
                        page: raw,
                        slot,
                    };
                }
                Err(_) => old = &page.next,
            }
        }

        let data = match NonNull::new(unsafe { alloc(Self::LAYOUT) }) {
            Some(data) => data,
            None => handle_alloc_error(Self::LAYOUT),
        };
        let page = Box::into_raw(Box::new(Page {
            data,
            slots: AtomicU64::new(Self::mask(size.size()) | 1),
            next: AtomicPtr::new(null_mut()),
        }));
        loop {
            match old.compare_exchange(null_mut(), page, Ordering::Release, Ordering::Acquire) {
                Ok(_) => {
                    break Handle {
                        data,
                        page,
                        slot: 1,
                    }
                }
                // Another thread added a page under this thread, so add this page further in the
                // linked list.
                Err(new) => old = unsafe { &(*new).next },
            }
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
        let page = unsafe { &*self.page };
        debug_assert_eq!(page.slots.load(Ordering::Relaxed) & self.slot, self.slot);
        page.slots.fetch_and(!self.slot, Ordering::Release);
    }
}

impl Drop for Memory {
    /// # Safety
    ///
    /// All [`Handle`]s returned from [`Memory::next`] must have been dropped
    /// before the `Memory` is dropped. Otherwise `Handle::Drop` will access a
    /// dangling `page` pointer.
    fn drop(&mut self) {
        for (index, head) in self.pages.iter_mut().enumerate() {
            let size = Size::index_to_size(index as u32);
            let mask = Self::mask(size);
            let mut raw = take(head.get_mut());
            while !raw.is_null() {
                let page = unsafe { Box::from_raw(raw) };
                debug_assert_eq!(page.slots.load(Ordering::Relaxed), mask);
                raw = page.next.load(Ordering::Acquire);
                unsafe { dealloc(page.data.as_ptr(), Self::LAYOUT) };
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
        let slots = Memory::SIZE / size.size();
        let mut ptrs: Vec<Handle> = Vec::with_capacity(slots as usize);
        for _ in 0..slots {
            let p = memory.next(size);
            let start = p.data.as_ptr() as usize;
            let end = start + size.size() as usize;
            for prev in &ptrs {
                let s = prev.data.as_ptr() as usize;
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
        let addr = p.data.as_ptr() as usize;
        drop(p);
        let p2 = memory.next(size);
        assert_eq!(p2.data.as_ptr() as usize, addr, "should reuse freed slot");
    }

    #[test]
    fn concurrent_allocations_non_overlapping() {
        let memory = Memory::new();
        let size = Size::_256;
        let slots = Memory::SIZE / size.size();
        let ptrs: Mutex<Vec<(Handle, usize, usize)>> = Mutex::new(Vec::new());

        std::thread::scope(|scope| {
            for _ in 0..slots {
                scope.spawn(|| {
                    let p = memory.next(size);
                    let mut held = ptrs.lock().unwrap();
                    let start = p.data.as_ptr() as usize;
                    let end = start + size.size() as usize;
                    for &(_, s, e) in &*held {
                        assert!(end <= s || start >= e, "concurrent allocation overlap");
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
        let slots = Memory::SIZE / size.size();
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
