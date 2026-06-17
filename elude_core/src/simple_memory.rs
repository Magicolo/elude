use core::{
    alloc::Layout,
    ptr::NonNull,
};
use parking_lot::Mutex;

pub struct SimpleMemory {
    pool: Mutex<Vec<NonNull<u8>>>,
    layout: Layout,
}

// SAFETY: `Mutex` provides exclusive access to the inner `Vec`, making it
// safe to send and share `SimpleMemory` across threads.
unsafe impl Send for SimpleMemory {}
unsafe impl Sync for SimpleMemory {}

impl SimpleMemory {
    pub fn new() -> Self {
        let page_size = page_size::get();
        let layout = Layout::from_size_align(page_size, page_size)
            .expect("page size is a power of two and non-zero");
        SimpleMemory {
            pool: Mutex::new(Vec::with_capacity(128)),
            layout,
        }
    }

    pub unsafe fn push(&self, page: NonNull<usize>) {
        self.pool.lock().push(page.cast::<u8>());
    }

    pub unsafe fn try_pop(&self) -> Option<NonNull<usize>> {
        let page = self.pool.lock().pop()?;
        Some(page.cast::<usize>())
    }

    pub unsafe fn pop(&self) -> NonNull<usize> {
        if let Some(page) = unsafe { self.try_pop() } {
            return page;
        }
        let ptr = unsafe { std::alloc::alloc(self.layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(self.layout);
        }
        unsafe { NonNull::new_unchecked(ptr).cast::<usize>() }
    }

    pub unsafe fn pop_batch(&self, out: &mut [NonNull<usize>], max: usize) -> usize {
        let mut pool = self.pool.lock();
        let count = pool.len().min(max);
        for i in 0..count {
            out[i] = pool.pop().unwrap().cast::<usize>();
        }
        count
    }

    pub unsafe fn push_batch(&self, pages: &[NonNull<usize>]) {
        let mut pool = self.pool.lock();
        pool.reserve(pages.len());
        for &page in pages {
            pool.push(page.cast::<u8>());
        }
    }
}

impl Drop for SimpleMemory {
    fn drop(&mut self) {
        let pool = self.pool.lock();
        for &ptr in pool.iter() {
            unsafe { std::alloc::dealloc(ptr.as_ptr(), self.layout) };
        }
    }
}
