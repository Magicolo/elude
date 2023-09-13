use std::{
    cell::UnsafeCell,
    error::Error,
    fmt,
    mem::transmute,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering::*},
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
pub struct Multex<T: ?Sized> {
    state: State,
    value: UnsafeCell<T>,
}
type AtomicMask = AtomicUsize;
type Mask = usize;

#[repr(transparent)]
struct State(AtomicMask);

/// Trait that specifies which resources will be locked and what will be returned on a successful lock.
///
/// # Safety
/// An improper implementation of this trait will lead to undefined behavior.
/// - [`Key::value`] __must__ return only resources that were locked (i.e. that correspond to the [`Key::mask`]).
pub unsafe trait Key<T: ?Sized> {
    type Value<'a>
    where
        T: 'a;
    /// Produces a mask where each bit index represents a resource index to lock.
    fn mask(&self) -> Mask;
    /// Returns the locked resources.
    unsafe fn value<'a>(&self, value: *mut T) -> Self::Value<'a>
    where
        T: 'a;
}

pub struct Indices<const N: usize> {
    mask: Mask,
    indices: [u8; N],
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum IndexError {
    OutOfBounds(usize),
    Duplicate(usize),
}

pub struct All;

pub struct Guard<'a, T>(T, Inner<'a>);
/// [`Inner`] should be kept separate from [`Guard`] such that its [`Drop`] implementation is called even if
/// a panic occurs when the value `T` is produced.
struct Inner<'a>(&'a State, Mask);

unsafe impl<T: Send> Send for Multex<T> {}
unsafe impl<T: Sync> Sync for Multex<T> {}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl Error for IndexError {}

impl State {
    #[inline]
    pub fn lock(&self, mask: Mask) -> Mask {
        loop {
            match self.try_lock(mask) {
                Ok(state) => return state,
                Err(state) => self.wait(state, mask),
            }
        }
    }

    #[inline]
    pub fn try_lock(&self, mask: Mask) -> Result<Mask, Mask> {
        self.0.fetch_update(Acquire, Relaxed, |state| {
            if state & mask == 0 {
                Some(state | mask)
            } else {
                None
            }
        })
    }

    #[inline]
    pub fn unlock(&self, mask: Mask) -> Mask {
        let state = self.0.fetch_and(!mask, Release);
        self.wake(mask);
        state
    }

    #[inline]
    pub fn is_locked(&self, mask: Mask) -> bool {
        self.0.load(Relaxed) & mask != 0
    }

    // TODO: Add support for more platforms.
    #[cfg(target_os = "linux")]
    #[inline]
    pub fn wait(&self, state: Mask, mask: Mask) {
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                self as *const Self,
                libc::FUTEX_WAIT_BITSET | libc::FUTEX_PRIVATE_FLAG,
                state,
                null::<libc::timespec>(),
                null::<u32>(),
                mask,
            )
        };
    }

    #[cfg(target_os = "windows")]
    #[inline]
    pub fn wait(&self, state: Mask, _: Mask) {
        use std::mem::size_of;
        use windows_sys::Win32::System::Threading::WaitOnAddress;

        unsafe {
            WaitOnAddress(
                self as *const _ as *const _,
                &state as *const _ as *const _,
                size_of::<usize>(),
                u32::MAX,
            )
        };
    }

    #[cfg(target_os = "linux")]
    #[inline]
    pub fn wake(&self, mask: Mask) {
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                self as *const Self,
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                2,
                null::<libc::timespec>(),
                null::<u32>(),
                mask,
            )
        };
    }

    #[cfg(target_os = "windows")]
    #[inline]
    pub fn wake(&self, _: Mask) {
        use windows_sys::Win32::System::Threading::WakeByAddressAll;
        unsafe { WakeByAddressAll(self as *const _ as *const _) };
    }
}

impl<'a, T> Guard<'a, T> {
    #[inline]
    pub fn map<U, F: FnOnce(T) -> U>(guard: Self, map: F) -> Guard<'a, U> {
        Guard(map(guard.0), guard.1)
    }
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for Inner<'_> {
    fn drop(&mut self) {
        self.0.unlock(self.1);
    }
}

unsafe impl<T> Key<T> for All {
    type Value<'a> = &'a mut T where T: 'a;

    #[inline]
    fn mask(&self) -> Mask {
        Mask::MAX
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut T) -> Self::Value<'a>
    where
        T: 'a,
    {
        unsafe { &mut *value }
    }
}

unsafe impl<const N: usize> Key<()> for Indices<N> {
    type Value<'a> = ();

    #[inline]
    fn mask(&self) -> Mask {
        self.mask
    }

    #[inline]
    unsafe fn value<'a>(&self, _: *mut ()) -> Self::Value<'a> {}
}

unsafe impl<T, const N: usize> Key<*mut T> for Indices<N>
where
    Indices<N>: Key<T>,
{
    type Value<'a> = <Self as Key<T>>::Value<'a> where T: 'a;

    #[inline]
    fn mask(&self) -> Mask {
        <Self as Key<T>>::mask(self)
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut *mut T) -> Self::Value<'a>
    where
        T: 'a,
    {
        <Self as Key<T>>::value(self, unsafe { value.read() })
    }
}

unsafe impl<'b, T, const N: usize> Key<&'b mut T> for Indices<N>
where
    Indices<N>: Key<T>,
{
    type Value<'a> = <Self as Key<T>>::Value<'a> where 'b: 'a;

    #[inline]
    fn mask(&self) -> Mask {
        <Self as Key<T>>::mask(self)
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut &'b mut T) -> Self::Value<'a>
    where
        'b: 'a,
    {
        <Self as Key<T>>::value(self, unsafe { value.read() })
    }
}

#[repr(C)]
struct RawSlice<T: ?Sized>(*mut T, usize);
#[repr(C)]
struct RawVec<T: ?Sized>(*mut T, usize, usize);
#[repr(C)]
struct RawBox<T: ?Sized>(*mut T);

unsafe impl<T, const M: usize, const N: usize> Key<[T; M]> for Indices<N> {
    type Value<'a> = [Option<&'a mut T>; N] where T: 'a;

    #[inline]
    fn mask(&self) -> Mask {
        !Mask::MAX.checked_shl(M as _).unwrap_or(0) & self.mask
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut [T; M]) -> Self::Value<'a>
    where
        T: 'a,
    {
        let raw = value.cast::<T>();
        self.indices.map(|index| {
            let index = index as usize;
            if index < M {
                Some(unsafe { &mut *raw.add(index) })
            } else {
                None
            }
        })
    }
}

unsafe impl<T, const N: usize> Key<[T]> for Indices<N> {
    type Value<'a> = [Option<&'a mut T>; N] where T: 'a;

    #[inline]
    fn mask(&self) -> Mask {
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

unsafe impl<T, const M: usize, const N: usize> Key<Box<[T; M]>> for Indices<N> {
    type Value<'a> = <Self as Key<[T; M]>>::Value<'a> where T: 'a;

    #[inline]
    fn mask(&self) -> Mask {
        <Self as Key<[T; M]>>::mask(self)
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut Box<[T; M]>) -> Self::Value<'a>
    where
        T: 'a,
    {
        let raw = unsafe { value.cast::<RawBox<[T; M]>>().read() };
        <Self as Key<[T; M]>>::value(self, raw.0)
    }
}

unsafe impl<T, const N: usize> Key<Box<[T]>> for Indices<N> {
    type Value<'a> = <Self as Key<[T]>>::Value<'a> where T: 'a;

    #[inline]
    fn mask(&self) -> Mask {
        <Self as Key<[T]>>::mask(self)
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut Box<[T]>) -> Self::Value<'a>
    where
        T: 'a,
    {
        let raw = unsafe { value.cast::<RawBox<[T]>>().read() };
        <Self as Key<[T]>>::value(self, raw.0)
    }
}

unsafe impl<T, const N: usize> Key<Vec<T>> for Indices<N> {
    type Value<'a> = <Self as Key<[T]>>::Value<'a> where T: 'a;

    #[inline]
    fn mask(&self) -> Mask {
        <Self as Key<[T]>>::mask(self)
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut Vec<T>) -> Self::Value<'a>
    where
        T: 'a,
    {
        let raw = unsafe { value.cast::<RawVec<T>>().read() };
        let slice = unsafe { transmute::<_, *mut [T]>(RawSlice(raw.0, raw.2)) };
        <Self as Key<[T]>>::value(self, slice)
    }
}

// TODO: Implement for tuples.

impl<const M: usize> Indices<M> {
    pub fn new(indices: [usize; M]) -> Result<Self, IndexError> {
        let mut mask = 0;
        for index in indices {
            if index >= Mask::BITS as usize {
                // Index out of bounds.
                return Err(IndexError::OutOfBounds(index));
            }

            let bit = 1 << index;
            let next = mask | bit;
            if mask == next {
                // Duplicate index.
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

impl<T> Multex<T> {
    #[inline]
    pub const fn new(values: T) -> Self {
        Self {
            state: State(AtomicMask::new(0)),
            value: UnsafeCell::new(values),
        }
    }

    #[inline]
    pub const fn as_ptr(&self) -> *const T {
        self.value.get()
    }

    #[inline]
    pub const fn as_mut_ptr(&self) -> *mut T {
        self.value.get()
    }

    #[inline]
    pub unsafe fn unlock<K: Key<T>>(&self, key: &K) {
        self.state.unlock(key.mask());
    }

    #[inline]
    pub fn is_locked<K: Key<T>>(&self, key: &K) {
        self.state.is_locked(key.mask());
    }

    #[inline]
    pub fn get_mut<K: Key<T>>(&mut self, key: &K) -> K::Value<'_> {
        unsafe { key.value(self.value.get_mut()) }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[inline]
    pub fn lock<K: Key<T>>(&self, key: &K) -> Guard<'_, K::Value<'_>> {
        let mask = key.mask();
        self.state.lock(mask);
        unsafe { self.guard(mask, key) }
    }

    #[inline]
    pub fn try_lock<K: Key<T>>(&self, key: &K) -> Option<Guard<'_, K::Value<'_>>> {
        let mask = key.mask();
        self.state.try_lock(mask).ok()?;
        Some(unsafe { self.guard(mask, key) })
    }

    #[inline]
    unsafe fn guard<K: Key<T>>(&self, mask: Mask, key: &K) -> Guard<'_, K::Value<'_>> {
        let inner = Inner(&self.state, mask);
        Guard(key.value(self.value.get()), inner)
    }
}

#[test]
fn locks_different_indices() -> Result<(), Box<dyn Error>> {
    let multex = Multex::new([1u8, 2u8, 3u8, 4u8]);
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
    let multex = Multex::new([1u8, 2u8, 3u8, 4u8]);
    let key1 = Indices::new([0, 4])?;
    let key2 = Indices::new([1, 4])?;
    let _guard1 = multex.lock(&key1);
    let _guard2 = multex.lock(&key2);
    Ok(())
}

#[test]
fn locks_all_without_panic() {
    Multex::new(Vec::new()).lock(&All).push(1);
}

#[test]
fn boba() -> Result<(), Box<dyn Error>> {
    use std::thread::scope;
    use std::thread::sleep;
    use std::time::Duration;

    let mut values = [(); 32].map(|_| Vec::new());
    let count = values.len();
    let multex = Multex::new(&mut values);

    const BATCH: usize = 256;
    for b in 0.. {
        scope(|scope| {
            for i in 0..BATCH {
                let index = b * BATCH + i;
                let multex = &multex;
                let key = Indices::new([index % count])?;
                scope.spawn(move || {
                    let mut guard = multex.lock(&key);
                    let Some(value) = &mut guard[0] else { panic!() };
                    value.push(index);
                    println!("ACQUIRE: {index}");
                    sleep(Duration::from_micros(index as u64 % 17));
                    println!("RELEASE: {index}");
                });
            }
            Ok::<_, IndexError>(())
        })?;
        // dbg!(multex.lock(&All).deref());
    }

    Ok(())
}
