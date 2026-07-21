use core::{
    alloc::Layout,
    cell::Cell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::{null_mut, NonNull},
    slice::{from_raw_parts, from_raw_parts_mut},
    sync::atomic::{AtomicPtr, Ordering},
};
use crossbeam_utils::{Backoff, CachePadded};
use std::alloc::{alloc, dealloc, handle_alloc_error};

pub struct Handle(NonNull<Node>, Size);
struct Node(Cell<*mut Node>);

struct GlobalPool([GlobalHead; Size::COUNT]);
struct GlobalHead(CachePadded<AtomicPtr<Node>>);

struct LocalPool {
    heads: [LocalHead; Size::COUNT],
    count: Cell<usize>,
}
struct LocalHead {
    head: Cell<*mut Node>,
    count: Cell<usize>,
}

#[repr(u8)]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Size {
    _32 = 0,
    _64 = 1,
    _128 = 2,
    _256 = 3,
    _512 = 4,
    _1024 = 5,
    _2048 = 6,
    _4096 = 7,
}

pub struct Pool(PhantomData<*const ()>);

unsafe impl Sync for Pool {}
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

static GLOBAL: GlobalPool = GlobalPool::new();

thread_local! {
    static LOCAL: LocalPool = const { LocalPool::new() };
}

impl GlobalPool {
    #[inline]
    const fn new() -> Self {
        Self([
            GlobalHead::new(),
            GlobalHead::new(),
            GlobalHead::new(),
            GlobalHead::new(),
            GlobalHead::new(),
            GlobalHead::new(),
            GlobalHead::new(),
            GlobalHead::new(),
        ])
    }

    #[inline]
    fn head(&self, size: Size) -> &GlobalHead {
        &self.0[size as usize]
    }

    #[inline]
    fn try_pop(&self, count: usize, size: Size) -> Option<(NonNull<Node>, NonNull<Node>, usize)> {
        if count == 0 {
            return None;
        }

        let mut result = None;
        let global = self.head(size);
        lock_with(&global.0, |node| match NonNull::new(node) {
            Some(first) => {
                let mut last = first;
                let mut pop = 1;
                while pop < count
                    && let Some(next) = NonNull::new(unsafe { last.as_ref() }.0.get())
                {
                    last = next;
                    pop += 1;
                }
                result = Some((first, last, pop));
                unsafe { last.as_ref() }.0.take()
            }
            None => null_mut(),
        });
        result
    }

    #[inline]
    fn pop(&self, size: Size) -> NonNull<Node> {
        match self.try_pop(1, size) {
            Some((node, _, _)) => node,
            None => size.allocate().cast(),
        }
    }

    #[inline]
    fn push(&self, head: NonNull<Node>, tail: NonNull<Node>, size: Size) {
        let global = self.head(size);
        lock_with(&global.0, |node| {
            unsafe { tail.write(Node::new(node)) };
            head.as_ptr()
        });
    }

    #[inline]
    fn clear(&self) {
        for size in Size::SIZES {
            let global = self.head(size);
            let mut head = None;
            lock_with(&global.0, |node| {
                head = NonNull::new(node);
                null_mut()
            });
            while let Some(node) = head {
                head = NonNull::new(unsafe { node.as_ref() }.0.get());
                size.free(node.cast());
            }
        }
    }
}

impl Drop for GlobalPool {
    fn drop(&mut self) {
        self.clear();
    }
}

impl GlobalHead {
    const fn new() -> Self {
        Self(CachePadded::new(AtomicPtr::new(null_mut())))
    }
}

impl LocalPool {
    const fn new() -> Self {
        Self {
            heads: [
                LocalHead::new(),
                LocalHead::new(),
                LocalHead::new(),
                LocalHead::new(),
                LocalHead::new(),
                LocalHead::new(),
                LocalHead::new(),
                LocalHead::new(),
            ],
            count: Cell::new(0),
        }
    }

    fn increment(&self) {
        self.count.set(self.count.get() + 1)
    }

    fn decrement(&self) -> bool {
        let count = self.count.get();
        self.count.set(count - 1);
        count == 1
    }

    #[inline]
    fn head(&self, size: Size) -> &LocalHead {
        &self.heads[size as usize]
    }

    #[inline]
    fn pop(&self, size: Size) -> NonNull<Node> {
        let limit = size.limit();
        let local = self.head(size);
        let head = local.head.get();
        let count = local.count.get();
        if let Some(node) = NonNull::new(head) {
            debug_assert!(count > 0);
            let next = unsafe { node.as_ref() }.0.take();
            local.head.set(next);
            local.count.set(count - 1);
            node
        } else if let Some((node, _, add)) = GLOBAL.try_pop(limit, size) {
            debug_assert_eq!(count, 0);
            debug_assert!(add > 0);
            if let Some(second) = NonNull::new(unsafe { node.as_ref() }.0.get()) {
                local.head.set(second.as_ptr());
                local.count.set(add - 1);
            }
            node
        } else {
            debug_assert_eq!(count, 0);
            size.allocate().cast()
        }
    }

    #[inline]
    fn push(&self, node: NonNull<Node>, size: Size) {
        let limit = size.limit();
        let local = self.head(size);
        let head = local.head.get();
        let count = local.count.get();
        unsafe { node.write(Node(Cell::new(head))) };

        debug_assert!(count <= limit);
        if count < limit {
            local.head.set(node.as_ptr());
            local.count.set(count + 1);
        } else {
            let first = node;
            let mut last = node;
            let mut remove = 0;
            while remove <= limit >> 1
                && let Some(next) = NonNull::new(unsafe { last.as_ref() }.0.get())
            {
                last = next;
                remove += 1;
            }
            local.head.set(unsafe { last.as_ref() }.0.take());
            local.count.set(count - remove);
            GLOBAL.push(first, last, size);
        }
    }

    fn clear(&self) {
        for size in Size::SIZES {
            let local = self.head(size);
            let head = local.head.take();
            local.count.set(0);

            if let Some(first) = NonNull::new(head) {
                let mut last = first;
                while let Some(next) = NonNull::new(unsafe { last.as_ref() }.0.get()) {
                    last = next;
                }
                GLOBAL.push(first, last, size);
            }
        }
    }
}

impl Drop for LocalPool {
    fn drop(&mut self) {
        self.clear();
    }
}

impl Node {
    const fn new(next: *mut Node) -> Self {
        Self(Cell::new(next))
    }
}

impl LocalHead {
    const fn new() -> Self {
        Self {
            head: Cell::new(null_mut()),
            count: Cell::new(0),
        }
    }
}

impl Pool {
    pub fn new() -> Self {
        let _ = LOCAL.try_with(|local| local.increment());
        Self(PhantomData)
    }

    pub fn next(&self, size: Size) -> Handle {
        let node = match LOCAL.try_with(|local| local.pop(size)) {
            Ok(node) => node,
            Err(_) => GLOBAL.pop(size),
        };
        Handle(node, size)
    }
}

impl Default for Pool {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let _ = LOCAL.try_with(|local| {
            if local.decrement() {
                local.clear();
            }
        });
    }
}

impl Size {
    const BASE: usize = 4;
    const COUNT: usize = 8;
    #[allow(clippy::identity_op)]
    const LIMITS: [usize; Self::COUNT] = [
        Self::BASE << 3,
        Self::BASE << 3,
        Self::BASE << 2,
        Self::BASE << 2,
        Self::BASE << 1,
        Self::BASE << 1,
        Self::BASE << 0,
        Self::BASE << 0,
    ];
    const SIZES: [Size; Self::COUNT] = [
        Size::_32,
        Size::_64,
        Size::_128,
        Size::_256,
        Size::_512,
        Size::_1024,
        Size::_2048,
        Size::_4096,
    ];

    #[inline]
    fn allocate(self) -> NonNull<u8> {
        let layout = self.layout();
        match NonNull::new(unsafe { alloc(layout) }) {
            Some(data) => data,
            None => handle_alloc_error(layout),
        }
    }

    #[inline]
    fn free(self, data: NonNull<u8>) {
        unsafe { dealloc(data.as_ptr(), self.layout()) };
    }

    #[inline]
    const fn layout(self) -> Layout {
        let size = self.size() as _;
        debug_assert!(size >= 32);
        unsafe { Layout::from_size_align_unchecked(size, size) }
    }

    #[inline]
    fn limit(self) -> usize {
        unsafe { *Self::LIMITS.get_unchecked(self as usize) }
    }

    #[inline]
    const fn size(self) -> u32 {
        1u32 << (self as u8 + 5)
    }
}

impl Handle {
    #[inline]
    pub const fn get(&self) -> NonNull<u8> {
        self.0.cast()
    }

    #[inline]
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(&self) -> usize {
        self.1.size() as _
    }
}

impl Deref for Handle {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { from_raw_parts(self.get().as_ptr(), self.len()) }
    }
}

impl DerefMut for Handle {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { from_raw_parts_mut(self.get().as_ptr(), self.len()) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        match LOCAL.try_with(|local| local.push(self.0, self.1)) {
            Ok(_) => {}
            Err(_) => GLOBAL.push(self.0, self.0, self.1),
        }
    }
}

fn lock_with<T>(atomic: &AtomicPtr<T>, mut with: impl FnMut(*mut T) -> *mut T) {
    let spin = Backoff::new();
    loop {
        let old = atomic.load(Ordering::Relaxed);
        if old.addr() & 1 == 0 {
            let new = old.map_addr(|address| address | 1);
            if atomic
                .compare_exchange_weak(old, new, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                let new = with(old).map_addr(|address| address & !1);
                atomic.store(new, Ordering::Release);
                break;
            }
        }
        spin.spin();
    }
}
