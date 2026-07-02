use core::{
    alloc::Layout,
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
}

pub struct Handle {
    page: NonNull<Page>,
    slot: u32,
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
    const LAYOUT: Layout =
        unsafe { Layout::from_size_align_unchecked(Self::SIZE as _, Self::SIZE as _) };
    const SIZE: u32 = 4096;

    pub const fn new() -> Self {
        Self {
            pages: [null_mut(); 7],
        }
    }

    /// Sets the upper bits of the slots to 1 for a given size. This is used to
    /// mark the slots as unavailable for allocation.
    #[inline]
    const fn mask(size: u32) -> u64 {
        let shift = Self::SIZE.saturating_div(size);
        !1u64.unbounded_shl(shift).wrapping_sub(1)
    }

    pub fn next(&mut self, size: Size) -> Handle {
        let head = unsafe { self.pages.get_unchecked_mut(size.index() as usize) };
        let mut current = &mut *head;
        while let Some(mut raw) = NonNull::new(*current) {
            let page = unsafe { raw.as_mut() };
            let slot = page.next_slot();
            if page.add_slot(slot) {
                return Handle { page: raw, slot };
            } else {
                current = &mut page.next;
            }
        }

        let data = match NonNull::new(unsafe { alloc(Self::LAYOUT) }) {
            Some(data) => Page::add_size(data, size.index()),
            None => handle_alloc_error(Self::LAYOUT),
        };
        let page = Box::into_raw(Box::new(Page {
            data,
            slots: Self::mask(size.size()) | 1,
            next: *head,
        }));
        *head = page;

        Handle {
            page: unsafe { NonNull::new_unchecked(page) },
            slot: 1,
        }
    }
}

impl Page {
    const MASK: usize = 0b1111;

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
    pub fn get(&self) -> NonNull<u8> {
        unsafe { self.page.as_ref().get_slot(self.slot) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { self.page.as_mut() }.remove_slot(self.slot);
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        for head in self.pages.iter_mut() {
            let mut raw = *head;
            while !raw.is_null() {
                let page = unsafe { Box::from_raw(raw) };
                raw = page.next;
                unsafe { dealloc(page.data.as_ptr(), Self::LAYOUT) };
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
            let slots = Memory::SIZE / size.size();
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
        let slots = Memory::SIZE / size.size();
        for _ in 0..slots {
            let p = memory.next(size);
            drop(p);
        }
        let p = memory.next(size);
        drop(p);
    }
}
