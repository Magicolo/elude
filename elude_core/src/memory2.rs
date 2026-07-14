use core::{
    alloc::Layout,
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    ptr::{null_mut, NonNull},
    slice::{from_raw_parts, from_raw_parts_mut},
    sync::atomic::{AtomicPtr, Ordering},
};
use crossbeam_utils::CachePadded;
use std::alloc::{alloc, dealloc, handle_alloc_error};

struct Node {
    data: NonNull<u8>,
    next: AtomicPtr<Node>,
}
pub struct Handle(NonNull<Node>);

#[repr(u8)]
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

static HEADS: [CachePadded<AtomicPtr<Node>>; 8] = [
    CachePadded::new(AtomicPtr::new(null_mut())),
    CachePadded::new(AtomicPtr::new(null_mut())),
    CachePadded::new(AtomicPtr::new(null_mut())),
    CachePadded::new(AtomicPtr::new(null_mut())),
    CachePadded::new(AtomicPtr::new(null_mut())),
    CachePadded::new(AtomicPtr::new(null_mut())),
    CachePadded::new(AtomicPtr::new(null_mut())),
    CachePadded::new(AtomicPtr::new(null_mut())),
];

#[inline]
pub fn try_next(size: Size) -> Option<Handle> {
    try_pop_with(unsafe { HEADS.get_unchecked(size as usize) }).map(Handle)
}

#[inline]
pub fn next(size: Size) -> Handle {
    match try_next(size) {
        Some(handle) => handle,
        None => Handle(Node::new(size)),
    }
}

fn try_pop_with(head: &AtomicPtr<Node>) -> Option<NonNull<Node>> {
    let mut old = head.load(Ordering::Acquire);
    while let Some(node) = NonNull::new(old) {
        let next = unsafe { node.as_ref() }.next.load(Ordering::Relaxed);
        match head.compare_exchange_weak(old, next, Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => return Some(node),
            Err(new) => old = new,
        }
    }
    None
}

fn push_with(node: NonNull<Node>, head: &AtomicPtr<Node>) {
    let mut old = head.load(Ordering::Acquire);
    loop {
        unsafe { node.as_ref() }.next.store(old, Ordering::Relaxed);
        match head.compare_exchange_weak(old, node.as_ptr(), Ordering::Release, Ordering::Acquire) {
            Ok(_) => break,
            Err(new) => old = new,
        }
    }
}

impl Node {
    fn new(size: Size) -> NonNull<Self> {
        let layout = layout_size(size as _);
        let node = match NonNull::new(unsafe { alloc(layout) }) {
            Some(data) => Self {
                data: add_size(data, size as _),
                next: AtomicPtr::new(null_mut()),
            },
            None => handle_alloc_error(layout),
        };
        unsafe { NonNull::new_unchecked(Box::into_raw(Box::new(node))) }
    }

    unsafe fn free(node: NonNull<Self>) {
        drop(unsafe { Box::from_raw(node.as_ptr()) });
    }

    #[inline]
    fn layout(&self) -> Layout {
        layout_size(self.size())
    }

    #[inline]
    fn data(&self) -> NonNull<u8> {
        remove_size(self.data)
    }

    #[inline]
    fn size(&self) -> u8 {
        get_size(self.data)
    }

    #[inline]
    fn head(&self) -> &AtomicPtr<Node> {
        unsafe { HEADS.get_unchecked(self.size() as usize) }
    }
}

impl Handle {
    #[inline]
    pub fn get(&self) -> NonNull<u8> {
        self.node().data()
    }

    #[inline]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        index_to_size(self.node().size()) as _
    }

    #[inline]
    const fn node(&self) -> &Node {
        unsafe { self.0.as_ref() }
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
        push_with(self.0, unsafe { self.0.as_ref() }.head());
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        unsafe { dealloc(self.data().as_ptr(), self.layout()) };
    }
}

const MASK: usize = 0b1111;

#[inline]
const fn layout_size(index: u8) -> Layout {
    let size = index_to_size(index) as _;
    debug_assert!(size >= 32);
    unsafe { Layout::from_size_align_unchecked(size, size) }
}

#[inline]
fn add_size<T>(data: NonNull<T>, index: u8) -> NonNull<T> {
    // Leave the first bit for tagging.
    let index = (index as usize) << 1;
    match NonZeroUsize::new(data.addr().get() | index) {
        Some(address) => data.with_addr(address),
        None => data,
    }
}

#[inline]
fn get_size<T>(data: NonNull<T>) -> u8 {
    (data.addr().get() & MASK) as u8 >> 1
}

#[inline]
fn remove_size<T>(data: NonNull<T>) -> NonNull<T> {
    match NonZeroUsize::new(data.addr().get() & !MASK) {
        Some(address) => data.with_addr(address),
        None => data,
    }
}

#[inline]
const fn index_to_size(index: u8) -> u32 {
    1u32 << (index + 5)
}
