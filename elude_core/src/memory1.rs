use core::{
    alloc::Layout,
    cell::RefCell,
    mem::{replace, take},
    num::NonZeroUsize,
    ptr::{null_mut, NonNull},
};
use std::alloc::{alloc, dealloc, handle_alloc_error};

pub struct Page {
    data: NonNull<u8>,
    // Each bit represents a chunk in the page.
    slots: u64,
    // Next page of the same size.
    next: *mut Page,
    // Head of the linked list for this size.
    head: NonNull<*mut Page>,
}

pub struct Handle {
    data: NonNull<u8>,
    page: NonNull<Page>,
}

pub struct Memory {
    // First page for each size.
    pages: [*mut Page; 7],
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

// SAFETY: `Memory` contains only `[*mut Page; 7]`. Raw pointers are not `Send`,
// but all mutation goes through `&mut self` methods that require exclusive
// access, so it is safe to send `Memory` between threads.
unsafe impl Send for Memory {}

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

thread_local! {
    static MEMORY: RefCell<Memory> = const { RefCell::new(Memory::new()) };
}

impl Memory {
    pub const fn new() -> Self {
        Self {
            pages: [null_mut(); 7],
        }
    }

    pub fn next(size: Size) -> Handle {
        MEMORY.with_borrow_mut(|memory| {
            let head = unsafe { memory.pages.get_unchecked_mut(size.index() as usize) };
            match NonNull::new(*head) {
                Some(mut raw) => {
                    let page = unsafe { raw.as_mut() };
                    let slot = page.slots.trailing_ones();
                    page.slots |= 1u64 << slot;
                    if page.slots == u64::MAX {
                        *head = take(&mut page.next);
                    }
                    Handle {
                        data: unsafe { page.data().add((size.size() * slot) as usize) },
                        page: raw,
                    }
                }
                None => {
                    let mut raw = Page::new(NonNull::from_mut(head), size);
                    let page = unsafe { raw.as_mut() };
                    if page.slots < u64::MAX {
                        page.next = replace(head, raw.as_ptr());
                    }
                    Handle {
                        data: page.data(),
                        page: raw,
                    }
                }
            }
        })
    }
}

impl Page {
    const LAYOUT: Layout =
        unsafe { Layout::from_size_align_unchecked(Self::SIZE as _, Self::SIZE as _) };
    const MASK: usize = 0b1111;
    const SIZE: u32 = 4096;

    pub fn new(head: NonNull<*mut Page>, size: Size) -> NonNull<Self> {
        unsafe {
            NonNull::new_unchecked(Box::into_raw(Box::new(Page {
                data: match NonNull::new(alloc(Page::LAYOUT)) {
                    Some(data) => Self::add_size(data, size.index()),
                    None => handle_alloc_error(Page::LAYOUT),
                },
                slots: Page::mask(size.size()) | 1,
                next: null_mut(),
                head,
            })))
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
    pub fn size(&self) -> u8 {
        Self::get_size(self.data)
    }

    #[inline]
    pub fn data(&self) -> NonNull<u8> {
        Self::remove_size(self.data)
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

impl Handle {
    #[inline]
    pub const fn get(&self) -> NonNull<u8> {
        self.data
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let page = unsafe { self.page.as_mut() };
        let slot = (self.data.addr().get() - page.data().addr().get()) as u32 >> (page.size() + 6);
        let full = page.slots == u64::MAX;
        page.slots &= !(1u64 << slot);
        if full {
            page.next = unsafe { page.head.replace(self.page.as_ptr()) };
        }
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
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
            let mut raw = *head;
            while let Some(page) = NonNull::new(raw) {
                let page = unsafe { Box::from_raw(page.as_ptr()) };
                raw = page.next;
            }
        }
    }
}
