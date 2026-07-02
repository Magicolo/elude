use core::{
    alloc::Layout,
    mem::take,
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
};
use std::alloc::{alloc, dealloc, handle_alloc_error};

pub static MEMORY: Memory = Memory::new();

pub struct Page {
    data: NonNull<u8>,
    // Each bit represents a chunk in the page.
    slots: AtomicU64,
    // Next page of the same size.
    next: AtomicPtr<Page>,
}

pub struct Pointer {
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

impl Pointer {
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

    pub fn take(&self, size: Size) -> Pointer {
        let mut old = unsafe { self.pages.get_unchecked(size.index()) };
        loop {
            let raw = old.load(Ordering::Relaxed);
            if raw.is_null() {
                break;
            }

            let page = unsafe { &*raw };
            let mut index = u32::MAX;
            let mut slot = u64::MAX;
            let slots = page
                .slots
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |slots| {
                    index = slots.trailing_ones();
                    slot = 1u64.checked_shl(index)?;
                    Some(slots | slot)
                });
            match slots {
                Ok(_) => {
                    return Pointer {
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
            match old.compare_exchange(null_mut(), page, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => {
                    break Pointer {
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

impl Drop for Pointer {
    fn drop(&mut self) {
        let page = unsafe { &*self.page };
        debug_assert_eq!(page.slots.load(Ordering::Relaxed) & self.slot, self.slot);
        page.slots.fetch_and(!self.slot, Ordering::Relaxed);
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        for (index, head) in self.pages.iter_mut().enumerate() {
            let size = Size::index_to_size(index as u32);
            let mask = Self::mask(size);
            let mut raw = take(head.get_mut());
            while !raw.is_null() {
                let page = unsafe { &*raw };
                debug_assert_eq!(page.slots.load(Ordering::Relaxed), mask);
                raw = page.next.load(Ordering::Relaxed);
                unsafe { dealloc(page.data.as_ptr(), Self::LAYOUT) };
                drop(unsafe { Box::from_raw(raw) });
            }
        }
    }
}
