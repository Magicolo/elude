use core::{alloc::Layout, num::TryFromIntError, ptr::NonNull};
use parking_lot::Mutex;
use std::alloc::{alloc, dealloc};
use thiserror::Error;

pub static MEMORY: Mutex<Memory> = Mutex::new(Memory::new());

pub struct Page {
    data: NonNull<u8>,
    // Each bit represents a chunk in the page.
    slots: u32,
    // Next page of the same size.
    next: u32,
}

pub struct Pointer {
    data: NonNull<u8>,
    page: u32,
    bit: u32,
}

pub struct Memory {
    pages: Vec<Page>,
    // First page for each size.
    heads: [u32; 6],
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Allocation failed")]
    AllocationFailed,
    #[error("Page index overflow: {0}")]
    PageIndexOverflow(TryFromIntError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Size {
    _128 = 0,
    _256 = 1,
    _512 = 2,
    _1024 = 3,
    _2048 = 4,
    _4096 = 5,
}

unsafe impl Send for Memory {}

impl Size {
    const fn size(self) -> u32 {
        Self::index_to_size(self as u32)
    }

    const fn index(self) -> usize {
        self as usize
    }

    const fn index_to_size(index: u32) -> u32 {
        1u32 << (index + 7)
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
            pages: Vec::new(),
            heads: [u32::MAX; 6],
        }
    }

    /// Sets the upper bits of the slots to 1 for a given size. This is used to
    /// mark the slots as unavailable for allocation.
    const fn mask(size: Size) -> u32 {
        let shift = Self::SIZE.saturating_div(size.size());
        !1u32.unbounded_shl(shift).wrapping_sub(1)
    }

    pub fn take(&mut self, size: Size) -> Result<Pointer, Error> {
        let count = self.pages.len();
        let mut last = 0;
        let mut next = unsafe { self.heads.get_unchecked_mut(size.index()) };
        let mut pages = self.pages.as_mut_slice();
        while let (_, tail) = pages.split_at_mut(next.saturating_sub(last) as usize)
            && let Some((page, tail)) = tail.split_first_mut()
        {
            let slot = page.slots.trailing_ones();
            match 1u32.checked_shl(slot) {
                Some(bit) => {
                    page.slots |= bit;
                    return Ok(Pointer {
                        data: unsafe { page.data.add(slot.saturating_mul(size.size()) as usize) },
                        page: *next,
                        bit,
                    });
                }
                None => {
                    last = next.saturating_add(1);
                    next = &mut page.next;
                    pages = tail;
                }
            }
        }

        let data = unsafe { alloc(Self::LAYOUT) };
        match NonNull::new(data) {
            Some(data) => match u32::try_from(count) {
                Ok(index) => {
                    *next = index;
                    self.pages.push(Page {
                        data,
                        slots: Self::mask(size) | 1,
                        next: u32::MAX,
                    });
                    Ok(Pointer {
                        data,
                        page: index,
                        bit: 1,
                    })
                }
                Err(error) => Err(Error::PageIndexOverflow(error)),
            },
            None => Err(Error::AllocationFailed),
        }
    }

    /// Returns true if the pointer was successfully returned to the memory
    /// pool.
    ///
    /// # Safety
    /// The pointer must have been previously allocated by this memory pool and
    /// not already returned.
    pub unsafe fn put(&mut self, pointer: Pointer) -> bool {
        if let Some(page) = self.pages.get_mut(pointer.page as usize) {
            debug_assert_eq!(page.slots & pointer.bit, pointer.bit);
            page.slots &= !pointer.bit;
            true
        } else {
            false
        }
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        for page in self.pages.drain(..) {
            unsafe { dealloc(page.data.as_ptr(), Self::LAYOUT) };
        }
    }
}
