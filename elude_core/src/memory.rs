use core::{alloc::Layout, ptr::NonNull};

pub struct Memory {
    pages: Vec<NonNull<usize>>,
    layout: Layout,
}

impl Memory {
    pub fn new() -> Self {
        let size = page_size::get();
        let layout = Layout::from_size_align(size, size).expect("");
        Self {
            pages: Vec::new(),
            layout,
        }
    }

    pub fn try_pop(&mut self) -> Option<NonNull<usize>> {
        todo!()
    }

    pub fn pop(&mut self) -> NonNull<usize> {
        todo!()
    }

    pub fn push(&mut self, page: NonNull<usize>) {}
}
