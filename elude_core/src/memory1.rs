use core::{
    alloc::Layout,
    num::NonZeroUsize,
    ptr::{null_mut, NonNull},
};
use std::alloc::{alloc, dealloc, handle_alloc_error};
/*
    TODO: Unlink pages that have become full from the linked list. Relink them when the `Handle` is dropped.
    - On `next()`, if the page has become full, unlink it.
    - On `Handle::drop()`, relink the page if it was full.
    - This means that the head pointers in `Memory` always have at least one slot available.
*/

pub struct Page {
    data: NonNull<u8>,
    // Each bit represents a chunk in the page.
    slots: u64,
    // Next page of the same size.
    next: *mut Page,
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

impl Memory {
    pub const fn new() -> Self {
        Self {
            pages: [null_mut(); 7],
        }
    }

    pub fn next(&mut self, size: Size) -> Handle {
        let head = unsafe { self.pages.get_unchecked_mut(size.index() as usize) };
        let mut current = &mut *head;
        while let Some(mut raw) = NonNull::new(*current) {
            let page = unsafe { raw.as_mut() };
            let slot = page.next_slot();
            if page.add_slot(slot) {
                return Handle {
                    data: page.get_slot(slot),
                    page: raw,
                };
            } else {
                current = &mut page.next;
            }
        }

        let mut page = Page::new(size);
        let data = page.data();
        page.next = *head;
        page.add_slot(0);
        let page = Box::into_raw(Box::new(page));
        *head = page;
        let page = unsafe { NonNull::new_unchecked(page) };
        Handle { data, page }
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
            slots: Self::mask(size.size()),
            next: null_mut(),
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

    fn find_slot(&self, data: NonNull<u8>) -> Option<u32> {
        data.addr()
            .get()
            .checked_sub(self.data().addr().get())?
            .checked_div(self.size() as usize)?
            .try_into()
            .ok()
    }

    #[inline]
    const fn next_slot(&self) -> u32 {
        self.slots.trailing_ones()
    }

    #[inline]
    const fn add_slot(&mut self, slot: u32) -> bool {
        match 1u64.checked_shl(slot) {
            Some(bit) => {
                self.slots |= bit;
                true
            }
            None => false,
        }
    }

    #[inline]
    const fn remove_slot(&mut self, slot: u32) -> bool {
        match 1u64.checked_shl(slot) {
            Some(bit) => {
                self.slots &= !bit;
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

impl Handle {
    #[inline]
    pub const fn get(&self) -> NonNull<u8> {
        self.data
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let page = unsafe { self.page.as_mut() };
        if let Some(slot) = page.find_slot(self.data) {
            page.remove_slot(slot);
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

#[cfg(test)]
mod tests {
    use super::*;
    use checkito::*;

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

    #[check]
    fn alloc_and_free_roundtrip_reuses_slots() {
        let mut memory = Memory::new();
        for size in [
            Size::_64,
            Size::_128,
            Size::_256,
            Size::_512,
            Size::_1024,
            Size::_2048,
            Size::_4096,
        ] {
            let slots = Page::SIZE / size.size();
            let mut ptrs: Vec<Handle> = Vec::with_capacity(slots as usize);
            for _ in 0..slots {
                ptrs.push(memory.next(size));
            }
            drop(ptrs);
            let p = memory.next(size);
            drop(p);
        }
    }

    #[check(any_size())]
    fn free_then_realloc_each_slot(size: Size) {
        let mut memory = Memory::new();
        let slots = Page::SIZE / size.size();
        for _ in 0..slots {
            let p = memory.next(size);
            drop(p);
        }
        let p = memory.next(size);
        drop(p);
    }
}
