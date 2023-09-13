use std::{
    cell::UnsafeCell,
    error::Error,
    fmt,
    mem::transmute,
    ops::{BitAnd, BitOr, Deref, DerefMut, Not},
    sync::atomic::{
        AtomicU16, AtomicU32, AtomicU64, AtomicU8, AtomicUsize,
        Ordering::{self, *},
    },
};

/// A multiple mutually exclusive lock that allows to lock up to 32 resources (or any selection of those resources) with a single
/// atomic operation, thus preventing deadlocks.
///
/// Contrary to most mutexes, the [`Multex`] requires a [`Key`] to lock its resources. [`Key`] is an unsafe trait that determines
/// _which_ resources will be locked and what will be returned on a successful lock. It is not recommended that you implement it
/// yourself, but it is available to advanced users.
///
/// This library provides 2 implementations of the [`Key`] trait:
/// - [`All`] which locks the entire value `T` (just like a regular mutex would).
/// - [`Indices`] which locks resources in a collection based on a selection of indices and returns them as an array.
pub struct Multex<T: ?Sized, M: Mask = usize> {
    state: State<M>,
    value: UnsafeCell<T>,
}
pub type Multex8<T: ?Sized> = Multex<T, u8>;
pub type Multex16<T: ?Sized> = Multex<T, u16>;
pub type Multex32<T: ?Sized> = Multex<T, u32>;
pub type Multex64<T: ?Sized> = Multex<T, u64>;

pub unsafe trait Mask:
    Copy + Eq + BitAnd<Output = Self> + BitOr<Output = Self> + Not<Output = Self> + Send + Sync
{
    type Atomic;
    const MAX: Self;
    const ONE: Self;
    const ZERO: Self;
    const BITS: u32;

    fn fetch_update<F: FnMut(Self) -> Option<Self>>(
        atomic: &Self::Atomic,
        success: Ordering,
        failure: Ordering,
        update: F,
    ) -> Result<Self, Self>;
    fn fetch_and(atomic: &Self::Atomic, value: Self, order: Ordering) -> Self;
    fn load(atomic: &Self::Atomic, order: Ordering) -> Self;
    fn shift_left(value: Self, shift: u32) -> Option<Self>;
}

macro_rules! mask {
    ($v:ty, $a:ty) => {
        unsafe impl Mask for $v {
            type Atomic = $a;
            const MAX: Self = Self::MAX;
            const ZERO: Self = 0;
            const ONE: Self = 1;
            const BITS: u32 = Self::BITS;

            #[inline]
            fn fetch_update<F: FnMut(Self) -> Option<Self>>(
                atomic: &Self::Atomic,
                set_order: Ordering,
                fetch_order: Ordering,
                update: F,
            ) -> Result<Self, Self> {
                atomic.fetch_update(set_order, fetch_order, update)
            }

            #[inline]
            fn fetch_and(atomic: &Self::Atomic, value: Self, order: Ordering) -> Self {
                atomic.fetch_and(value, order)
            }

            #[inline]
            fn load(atomic: &Self::Atomic, order: Ordering) -> Self {
                atomic.load(order)
            }

            #[inline]
            fn shift_left(left: Self, shift: u32) -> Option<Self> {
                left.checked_shl(shift)
            }
        }

        impl<T> Multex<T, $v> {
            #[inline]
            pub const fn new(values: T) -> Self {
                Self {
                    state: State(<$a>::new(0)),
                    value: UnsafeCell::new(values),
                }
            }
        }
    };
}
mask!(u8, AtomicU8);
mask!(u16, AtomicU16);
mask!(u32, AtomicU32);
mask!(u64, AtomicU64);
mask!(usize, AtomicUsize);

// type AtomicMask = AtomicUsize;
// type Mask = usize;

#[repr(transparent)]
struct State<M: Mask>(M::Atomic);

/// Trait that specifies which resources will be locked and what will be returned on a successful lock.
///
/// # Safety
/// An improper implementation of this trait will lead to undefined behavior.
/// - [`Key::value`] __must__ return only resources that were locked (i.e. that correspond to the [`Key::mask`]).
pub unsafe trait Key<T: ?Sized, M: Mask> {
    type Value<'a>
    where
        T: 'a;
    /// Produces a mask where each bit index represents a resource index to lock.
    fn mask(&self) -> M;
    /// Returns the locked resources.
    unsafe fn value<'a>(&self, value: *mut T) -> Self::Value<'a>
    where
        T: 'a;
}

pub struct Indices<M: Mask, const N: usize> {
    mask: M,
    indices: [u8; N],
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum IndexError {
    OutOfBounds(usize),
    Duplicate(usize),
}

pub struct All;

pub struct Guard<'a, T, M: Mask>(T, Inner<'a, M>);
/// [`Inner`] should be kept separate from [`Guard`] such that its [`Drop`] implementation is called even if
/// a panic occurs when the value `T` is produced.
struct Inner<'a, M: Mask>(&'a State<M>, M);

unsafe impl<T: Sync, M: Mask> Sync for Multex<T, M> {}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl Error for IndexError {}

impl<M: Mask> State<M> {
    #[inline]
    pub fn lock(&self, mask: M) -> M {
        loop {
            match self.try_lock(mask) {
                Ok(state) => return state,
                Err(state) => self.wait(state, mask),
            }
        }
    }

    #[inline]
    pub fn try_lock(&self, mask: M) -> Result<M, M> {
        M::fetch_update(&self.0, Acquire, Relaxed, |state| {
            if state & mask == M::ZERO {
                Some(state | mask)
            } else {
                None
            }
        })
    }

    #[inline]
    pub fn unlock(&self, mask: M) -> M {
        let state = M::fetch_and(&self.0, !mask, Release);
        self.wake(mask);
        state
    }

    #[inline]
    pub fn is_locked(&self, mask: M) -> bool {
        M::load(&self.0, Relaxed) & mask != M::ZERO
    }

    // TODO: Add support for more platforms.
    #[cfg(target_os = "linux")]
    #[inline]
    pub fn wait(&self, state: M, mask: M) {
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                self as *const Self,
                libc::FUTEX_WAIT_BITSET | libc::FUTEX_PRIVATE_FLAG,
                state,
                null::<libc::timespec>(),
                null::<u32>(),
                mask, // TODO: Convert mask to u32. Deal with overflows. At least 1 bit must be one, otherwise u32::MAX?
            )
        };
    }

    #[cfg(target_os = "windows")]
    #[inline]
    pub fn wait(&self, state: M, _: M) {
        use std::mem::size_of;
        unsafe {
            windows_sys::Win32::System::Threading::WaitOnAddress(
                self as *const _ as *const _,
                &state as *const _ as *const _,
                size_of::<usize>(),
                u32::MAX,
            )
        };
    }

    #[cfg(target_os = "linux")]
    #[inline]
    pub fn wake(&self, mask: M) {
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                self as *const Self,
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                2,
                null::<libc::timespec>(),
                null::<u32>(),
                mask, // TODO: Convert to u32.
            )
        };
    }

    #[cfg(target_os = "windows")]
    #[inline]
    pub fn wake(&self, _: M) {
        unsafe {
            windows_sys::Win32::System::Threading::WakeByAddressAll(self as *const _ as *const _)
        };
    }
}

impl<'a, T, M: Mask> Guard<'a, T, M> {
    #[inline]
    pub fn map<U, F: FnOnce(T) -> U>(guard: Self, map: F) -> Guard<'a, U, M> {
        Guard(map(guard.0), guard.1)
    }
}

impl<T, M: Mask> Deref for Guard<'_, T, M> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, M: Mask> DerefMut for Guard<'_, T, M> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<M: Mask> Drop for Inner<'_, M> {
    fn drop(&mut self) {
        self.0.unlock(self.1);
    }
}

unsafe impl<T, M: Mask> Key<T, M> for All {
    type Value<'a> = &'a mut T where T: 'a;

    #[inline]
    fn mask(&self) -> M {
        M::MAX
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut T) -> Self::Value<'a>
    where
        T: 'a,
    {
        unsafe { &mut *value }
    }
}

unsafe impl<M: Mask, const N: usize> Key<(), M> for Indices<M, N> {
    type Value<'a> = ();

    #[inline]
    fn mask(&self) -> M {
        self.mask
    }

    #[inline]
    unsafe fn value<'a>(&self, _: *mut ()) -> Self::Value<'a> {}
}

unsafe impl<T, M: Mask, const N: usize> Key<*mut T, M> for Indices<M, N>
where
    Indices<M, N>: Key<T, M>,
{
    type Value<'a> = <Self as Key<T,M>>::Value<'a> where T: 'a;

    #[inline]
    fn mask(&self) -> M {
        <Self as Key<T, M>>::mask(self)
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut *mut T) -> Self::Value<'a>
    where
        T: 'a,
    {
        <Self as Key<T, M>>::value(self, unsafe { value.read() })
    }
}

unsafe impl<'b, T, M: Mask, const N: usize> Key<&'b mut T, M> for Indices<M, N>
where
    Indices<M, N>: Key<T, M>,
{
    type Value<'a> = <Self as Key<T,M>>::Value<'a> where 'b: 'a;

    #[inline]
    fn mask(&self) -> M {
        <Self as Key<T, M>>::mask(self)
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut &'b mut T) -> Self::Value<'a>
    where
        'b: 'a,
    {
        <Self as Key<T, M>>::value(self, unsafe { value.read() })
    }
}

#[repr(C)]
struct RawSlice<T: ?Sized>(*mut T, usize);
#[repr(C)]
struct RawVec<T: ?Sized>(*mut T, usize, usize);
#[repr(C)]
struct RawBox<T: ?Sized>(*mut T);

unsafe impl<T, M: Mask, const O: usize, const N: usize> Key<[T; O], M> for Indices<M, N> {
    type Value<'a> = [Option<&'a mut T>; N] where T: 'a;

    #[inline]
    fn mask(&self) -> M {
        !M::shift_left(M::MAX, O as _).unwrap_or(M::ZERO) & self.mask
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut [T; O]) -> Self::Value<'a>
    where
        T: 'a,
    {
        let raw = value.cast::<T>();
        self.indices.map(|index| {
            let index = index as usize;
            if index < O {
                Some(unsafe { &mut *raw.add(index) })
            } else {
                None
            }
        })
    }
}

unsafe impl<T, M: Mask, const N: usize> Key<[T], M> for Indices<M, N> {
    type Value<'a> = [Option<&'a mut T>; N] where T: 'a;

    #[inline]
    fn mask(&self) -> M {
        self.mask
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut [T]) -> Self::Value<'a>
    where
        T: 'a,
    {
        let raw = unsafe { transmute::<_, RawSlice<T>>(value) };
        self.indices.map(|index| {
            let index = index as usize;
            if index < raw.1 {
                Some(unsafe { &mut *raw.0.add(index) })
            } else {
                None
            }
        })
    }
}

unsafe impl<T, M: Mask, const O: usize, const N: usize> Key<Box<[T; O]>, M> for Indices<M, N> {
    type Value<'a> = <Self as Key<[T; O],M>>::Value<'a> where T: 'a;

    #[inline]
    fn mask(&self) -> M {
        <Self as Key<[T; O], M>>::mask(self)
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut Box<[T; O]>) -> Self::Value<'a>
    where
        T: 'a,
    {
        let raw = unsafe { value.cast::<RawBox<[T; O]>>().read() };
        <Self as Key<[T; O], M>>::value(self, raw.0)
    }
}

unsafe impl<T, M: Mask, const N: usize> Key<Box<[T]>, M> for Indices<M, N> {
    type Value<'a> = <Self as Key<[T],M>>::Value<'a> where T: 'a;

    #[inline]
    fn mask(&self) -> M {
        <Self as Key<[T], M>>::mask(self)
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut Box<[T]>) -> Self::Value<'a>
    where
        T: 'a,
    {
        let raw = unsafe { value.cast::<RawBox<[T]>>().read() };
        <Self as Key<[T], M>>::value(self, raw.0)
    }
}

unsafe impl<T, M: Mask, const N: usize> Key<Vec<T>, M> for Indices<M, N> {
    type Value<'a> = <Self as Key<[T],M>>::Value<'a> where T: 'a;

    #[inline]
    fn mask(&self) -> M {
        <Self as Key<[T], M>>::mask(self)
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut Vec<T>) -> Self::Value<'a>
    where
        T: 'a,
    {
        let raw = unsafe { value.cast::<RawVec<T>>().read() };
        let slice = unsafe { transmute::<_, *mut [T]>(RawSlice(raw.0, raw.2)) };
        <Self as Key<[T], M>>::value(self, slice)
    }
}

// TODO: Implement for tuples.

impl<M: Mask, const N: usize> Indices<M, N> {
    pub fn new(indices: [usize; N]) -> Result<Self, IndexError> {
        let mut mask = M::ZERO;
        for index in indices {
            let Some(bit) = M::shift_left(M::ONE, index as _) else {
                return Err(IndexError::OutOfBounds(index))
            };
            let next = mask | bit;
            if mask == next {
                return Err(IndexError::Duplicate(index));
            } else {
                mask = next;
            }
        }
        Ok(Self {
            indices: indices.map(|index| index as u8),
            mask,
        })
    }
}

impl<T, M: Mask> Multex<T, M> {
    #[inline]
    pub const fn as_ptr(&self) -> *const T {
        self.value.get()
    }

    #[inline]
    pub const fn as_mut_ptr(&self) -> *mut T {
        self.value.get()
    }

    #[inline]
    pub unsafe fn unlock<K: Key<T, M>>(&self, key: &K) {
        self.state.unlock(key.mask());
    }

    #[inline]
    pub fn is_locked<K: Key<T, M>>(&self, key: &K) {
        self.state.is_locked(key.mask());
    }

    #[inline]
    pub fn get_mut<K: Key<T, M>>(&mut self, key: &K) -> K::Value<'_> {
        unsafe { key.value(self.value.get_mut()) }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[inline]
    pub fn lock<K: Key<T, M>>(&self, key: &K) -> Guard<'_, K::Value<'_>, M> {
        let mask = key.mask();
        self.state.lock(mask);
        unsafe { self.guard(mask, key) }
    }

    #[inline]
    pub fn try_lock<K: Key<T, M>>(&self, key: &K) -> Option<Guard<'_, K::Value<'_>, M>> {
        let mask = key.mask();
        self.state.try_lock(mask).ok()?;
        Some(unsafe { self.guard(mask, key) })
    }

    #[inline]
    unsafe fn guard<K: Key<T, M>>(&self, mask: M, key: &K) -> Guard<'_, K::Value<'_>, M> {
        let inner = Inner(&self.state, mask);
        Guard(key.value(self.value.get()), inner)
    }
}

#[test]
fn locks_different_indices() -> Result<(), Box<dyn Error>> {
    let multex = Multex8::new([1u8, 2u8, 3u8, 4u8]);
    let key1 = Indices::new([0])?;
    let key2 = Indices::new([1])?;
    let mut guard1 = multex.lock(&key1);
    let mut guard2 = multex.lock(&key2);
    let Some(value1) = guard1[0].as_mut() else {
        panic!()
    };
    let Some(value2) = guard2[0].as_mut() else {
        panic!()
    };
    assert_eq!(**value1, 1u8);
    assert_eq!(**value2, 2u8);
    Ok(())
}

#[test]
fn does_not_contend_on_out_of_bounds_indices() -> Result<(), Box<dyn Error>> {
    let multex = Multex16::new([1u8, 2u8, 3u8, 4u8]);
    let key1 = Indices::new([0, 4])?;
    let key2 = Indices::new([1, 4])?;
    let _guard1 = multex.lock(&key1);
    let _guard2 = multex.lock(&key2);
    Ok(())
}

#[test]
fn locks_all_without_panic() {
    Multex32::new(Vec::new()).lock(&All).push(1);
}
