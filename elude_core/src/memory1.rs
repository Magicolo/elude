use core::{
    alloc::Layout,
    cell::RefCell,
    mem::{replace, take},
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    ptr::{null_mut, NonNull},
    slice::{from_raw_parts, from_raw_parts_mut},
    sync::atomic::{AtomicUsize, Ordering},
};
use parking_lot::Mutex;
use std::alloc::{alloc, dealloc, handle_alloc_error};

pub struct Pages([*mut Page; 7]);

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

pub trait Memory {
    fn next(size: Size) -> Handle;
}

pub struct Global;
pub struct Thread;
pub struct Shard;

// SAFETY: `Memory` contains only `[*mut Page; 7]`. Raw pointers are not `Send`,
// but all mutation goes through `&mut self` methods that require exclusive
// access, so it is safe to send `Memory` between threads.
unsafe impl Send for Pages {}

impl Memory for Global {
    fn next(size: Size) -> Handle {
        static PAGES: Mutex<Pages> = Mutex::new(Pages::new());
        PAGES.lock().next(size)
    }
}

impl Memory for Thread {
    fn next(size: Size) -> Handle {
        thread_local! {
            static PAGES: RefCell<Pages> = const { RefCell::new(Pages::new()) };
        }

        PAGES.with_borrow_mut(|pages| pages.next(size))
    }
}

impl Memory for Shard {
    fn next(size: Size) -> Handle {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        static SHARDS: [Mutex<Pages>; 8] = [
            Mutex::new(Pages::new()),
            Mutex::new(Pages::new()),
            Mutex::new(Pages::new()),
            Mutex::new(Pages::new()),
            Mutex::new(Pages::new()),
            Mutex::new(Pages::new()),
            Mutex::new(Pages::new()),
            Mutex::new(Pages::new()),
        ];

        thread_local! {
            static SHARD: &'static Mutex<Pages> = &SHARDS[NEXT.fetch_add(1, Ordering::Relaxed) % SHARDS.len()];
        }

        SHARD.with(|pages| pages.lock().next(size))
    }
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

impl Pages {
    pub const fn new() -> Self {
        Self([null_mut(); 7])
    }

    fn next(&mut self, size: Size) -> Handle {
        let head = unsafe { self.0.get_unchecked_mut(size.index() as usize) };
        match NonNull::new(*head) {
            Some(mut raw) => {
                let page = unsafe { raw.as_mut() };
                debug_assert!(page.slots < u64::MAX);
                let slot = page.slots.trailing_ones();
                debug_assert!(slot < u64::BITS);
                page.slots |= 1u64 << slot;
                if page.slots == u64::MAX {
                    *head = take(&mut page.next);
                }
                Handle::new(page.data, raw, slot, size)
            }
            None => {
                let slots = Page::mask(size.size()) | 1;
                let mut raw = Page::new(NonNull::from_mut(head), slots, size);
                let page = unsafe { raw.as_mut() };
                if slots < u64::MAX {
                    page.next = replace(head, raw.as_ptr());
                }
                Handle::new(page.data, raw, 0, size)
            }
        }
    }
}

impl Page {
    const LAYOUT: Layout =
        unsafe { Layout::from_size_align_unchecked(Self::SIZE as _, Self::SIZE as _) };
    const SIZE: u32 = 4096;

    pub fn new(head: NonNull<*mut Page>, slots: u64, size: Size) -> NonNull<Self> {
        debug_assert!(Self::SIZE / u64::BITS >= MASK as u32);
        unsafe {
            NonNull::new_unchecked(Box::into_raw(Box::new(Page {
                data: match NonNull::new(alloc(Page::LAYOUT)) {
                    Some(data) => add_size(data, size.index()),
                    None => handle_alloc_error(Page::LAYOUT),
                },
                slots,
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
}

impl Handle {
    #[inline]
    fn new(data: NonNull<u8>, page: NonNull<Page>, slot: u32, size: Size) -> Self {
        Self {
            data: add_size(
                unsafe { data.add((size.size() * slot) as usize) },
                size.index(),
            ),
            page,
        }
    }

    #[inline]
    pub fn get(&self) -> NonNull<u8> {
        remove_size(self.data)
    }

    #[inline]
    fn size(&self) -> u8 {
        get_size(self.data)
    }

    #[inline]
    fn len(&self) -> usize {
        1usize << (self.size() + 6)
    }
}

impl Deref for Handle {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { from_raw_parts(self.get().as_ptr(), self.len()) }
    }
}

impl DerefMut for Handle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { from_raw_parts_mut(self.get().as_ptr(), self.len()) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let page = unsafe { self.page.as_mut() };
        let slot = get_slot(self.get(), page.data, self.size());
        let full = page.slots == u64::MAX;
        page.slots &= !(1u64 << slot);
        if full {
            // TODO: This is UB for `Global` and `Shard` since no lock has been taken.
            // - For `Thread`, it is only safe if it can be guaranteed that the `Handle` is
            //   dropped on the same thread that created it, otherwise the `&mut Page`
            //   reference needs synchronization.
            // - For this store design, enforcing the creator thread to release its `Handle`
            //   won't work. We'd like some more relaxed constraints on `Handle`.
            page.next = unsafe { page.head.replace(self.page.as_ptr()) };
        }
    }
}

impl Default for Pages {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        unsafe { dealloc(self.data.as_ptr(), Self::LAYOUT) };
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        for head in self.0.iter_mut() {
            let mut raw = *head;
            while let Some(page) = NonNull::new(raw) {
                let page = unsafe { Box::from_raw(page.as_ptr()) };
                raw = page.next;
            }
        }
    }
}

const MASK: usize = 0b0111;

#[inline]
fn add_size(data: NonNull<u8>, index: u8) -> NonNull<u8> {
    match NonZeroUsize::new(data.addr().get() | index as usize) {
        Some(address) => data.with_addr(address),
        None => data,
    }
}

#[inline]
fn get_size(data: NonNull<u8>) -> u8 {
    (data.addr().get() & MASK) as u8
}

#[inline]
fn remove_size(data: NonNull<u8>) -> NonNull<u8> {
    match NonZeroUsize::new(data.addr().get() & !MASK) {
        Some(address) => data.with_addr(address),
        None => data,
    }
}

#[inline]
fn get_slot(handle: NonNull<u8>, page: NonNull<u8>, index: u8) -> u32 {
    (handle.addr().get() - page.addr().get()) as u32 >> (index + 6)
}
