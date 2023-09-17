use std::{
    cell::UnsafeCell,
    error::Error,
    fmt,
    mem::transmute,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering::*},
};

/*
    TODO:
    - Use references for 'mask: Self'
    - Move 'wait' and 'wake' to the 'Lock' trait.
    - Add a 'AtomicUsize' counter to '[L; N]' 'Lock' implementation such that it is used in 'wait/wake'.

    - Implement `StaticAt` for `At1, At2, ...` and `[T; 1], [T; 2], ...`.
    - Implement `DynamicAt` for `AtN<(usize, ...)>`.
    - Tuple locks
    - Nested locks:
        - Given a `struct Boba { a: usize, b: Vec<String>, c: Fett }`, there should be a way to lock nested fields.
            #[derive(Lock)]
            struct Boba { a: usize, b: Vec<String>, c: Fett }
            #[derive(Lock)]
            struct Fett { a: isize }

            let boba = Multex::new([Boba { ... }]);
            let key = (Boba::b::at(1), Boba::c::get(Fett::a));
            let (Some(boba_b), Some(fett_a)) = boba.lock_with(&key, false) else { return; };
*/

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
pub struct Multex<T: ?Sized, L: Lock = usize> {
    state: L::State,
    value: UnsafeCell<T>,
}
pub type MultexN<T: ?Sized, const N: usize> = Multex<T, [usize; N]>;
pub type Multex8<T: ?Sized> = Multex<T, u8>;
pub type Multex8N<T: ?Sized, const N: usize> = Multex<T, [u8; N]>;
pub type Multex16<T: ?Sized> = Multex<T, u16>;
pub type Multex16N<T: ?Sized, const N: usize> = Multex<T, [u16; N]>;
pub type Multex32<T: ?Sized> = Multex<T, u32>;
pub type Multex32N<T: ?Sized, const N: usize> = Multex<T, [u32; N]>;
pub type Multex64<T: ?Sized> = Multex<T, u64>;
pub type Multex64N<T: ?Sized, const N: usize> = Multex<T, [u64; N]>;

pub unsafe trait Lock: Copy {
    type State;
    const MAX: Self;
    const ZERO: Self;
    const BITS: usize;

    fn lock(state: &Self::State, mask: Self, partial: bool, wait: bool) -> Option<Self>;
    fn unlock(state: &Self::State, mask: Self, wake: bool);
    fn is_locked(state: &Self::State, mask: Self, partial: bool) -> bool;
    // TODO: Can this be moved elsewhere?
    fn add(mask: Self, index: usize) -> Result<Self, IndexError>;
    fn has(mask: Self, index: usize) -> bool;
}

#[repr(C)]
struct RawSlice<T: ?Sized>(*mut T, usize);
#[repr(C)]
struct RawVec<T: ?Sized>(*mut T, usize, usize);
#[repr(C)]
struct RawBox<T: ?Sized>(*mut T);

/// Trait that specifies which resources will be locked and what will be returned on a successful lock.
///
/// # Safety
/// An improper implementation of this trait will lead to undefined behavior.
/// - [`Key::value`] __must__ return only resources that were locked (i.e. that correspond to the [`Key::mask`]).
pub unsafe trait Key<T: ?Sized, L: Lock> {
    type Value<'a>
    where
        T: 'a;
    /// Produces a mask where each bit index represents a resource index to lock.
    fn mask(&self) -> L;
    /// Returns the locked resources.
    unsafe fn value<'a>(&self, value: *mut T, mask: L) -> Self::Value<'a>
    where
        T: 'a;
}

pub trait StaticAt<const I: usize> {
    type Item<'a>
    where
        Self: 'a;
    unsafe fn static_at<'a>(items: *mut Self) -> Self::Item<'a>;
}

pub trait DynamicAt {
    type Item<'a>
    where
        Self: 'a;
    unsafe fn dynamic_at<'a>(items: *mut Self, index: usize) -> Option<Self::Item<'a>>;
}

pub struct AtN<L: Lock, I> {
    mask: L,
    indices: I,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum IndexError {
    OutOfBounds(usize),
    Duplicate(usize),
}

pub struct Guard<'a, T, L: Lock>(T, Inner<'a, L>);
/// [`Inner`] should be kept separate from [`Guard`] such that its [`Drop`] implementation is called even if
/// a panic occurs when the value `T` is produced.
struct Inner<'a, L: Lock>(&'a L::State, L);

unsafe impl<T: Sync, L: Lock> Sync for Multex<T, L> {}

macro_rules! lock {
    ($v:ty, $a:ty) => {
        unsafe impl Lock for $v {
            type State = $a;
            const MAX: Self = Self::MAX;
            const ZERO: Self = 0;
            const BITS: usize = Self::BITS as usize;

            #[inline]
            fn lock(state: &Self::State, mask: Self, partial: bool, wait: bool) -> Option<Self> {
                #[inline]
                fn lock_once(state: &$a, mask: $v) -> Result<$v, $v> {
                    state.fetch_update(Acquire, Relaxed, |state| {
                        if state & mask == 0 {
                            Some(state | mask)
                        } else {
                            None
                        }
                    })
                }

                fn lock_wait(state: &$a, mask: $v) -> $v {
                    loop {
                        match lock_once(state, mask) {
                            Ok(_) => break mask,
                            Err(value) => wait_value(state, value, mask),
                        }
                    }
                }

                if mask == Self::ZERO {
                    Some(mask)
                } else if partial {
                    Some(state.fetch_or(mask, Acquire) ^ mask & mask)
                } else if wait {
                    Some(lock_wait(state, mask))
                } else {
                    lock_once(state, mask).ok()
                }
            }

            #[inline]
            fn unlock(state: &Self::State, mask: Self, wake: bool) {
                if mask == Self::ZERO {
                    return;
                }

                state.fetch_and(!mask, Release);
                if wake {
                    wake_value(state, mask);
                }
            }

            #[inline]
            fn is_locked(state: &Self::State, mask: Self, partial: bool) -> bool {
                if mask == Self::ZERO {
                    false
                } else if partial {
                    state.load(Relaxed) & mask != Self::ZERO
                } else {
                    state.load(Relaxed) & mask == mask
                }
            }

            #[inline]
            fn add(mask: Self, index: usize) -> Result<Self, IndexError> {
                let Some(bit) = (1 as $v).checked_shl(index as _) else { return Err(IndexError::OutOfBounds(index)) };
                let next = mask | bit;
                if mask == next {
                    Err(IndexError::Duplicate(index))
                } else {
                    Ok(next)
                }
            }

            #[inline]
            fn has(mask: Self, index: usize) -> bool {
                let Some(bit) = (1 as $v).checked_shl(index as _) else { return false; };
                mask & bit == bit
            }
        }

        impl<T> Multex<T, $v> {
            #[inline]
            pub const fn new(values: T) -> Self {
                Self {
                    state: <$a>::new(0),
                    value: UnsafeCell::new(values),
                }
            }
        }

        impl<T, const N: usize> Multex<T, [$v; N]> {
            #[inline]
            pub fn new(values: T) -> Self {
                Self {
                    state: ([(); N].map(|_| <$a>::new(0)), AtomicU32::new(0)),
                    value: UnsafeCell::new(values),
                }
            }
        }
    };
}

lock!(u8, AtomicU8);
lock!(u16, AtomicU16);
lock!(u32, AtomicU32);
lock!(u64, AtomicU64);
lock!(usize, AtomicUsize);

unsafe impl<L: Lock + Eq, const N: usize> Lock for [L; N] {
    type State = ([L::State; N], AtomicU32);
    const MAX: Self = [L::MAX; N];
    const ZERO: Self = [L::ZERO; N];
    const BITS: usize = L::BITS * N;

    fn lock(state: &Self::State, mask: Self, partial: bool, wait: bool) -> Option<Self> {
        if mask == Self::ZERO {
            return Some(mask);
        }

        loop {
            let mut masks = Self::ZERO;
            let mut done = true;
            let value = state.1.load(Acquire);
            for (index, pair) in state.0.iter().zip(mask).enumerate() {
                match L::lock(pair.0, pair.1, partial, false) {
                    Some(mask) => masks[index] = mask,
                    None => {
                        // The `masks` ensures that only locks that were taken are unlocked.
                        Self::unlock(state, masks, true);
                        if wait {
                            wait_array(&state.1, value);
                            done = false;
                            break;
                        } else {
                            return None;
                        }
                    }
                }
            }
            if done {
                return Some(masks);
            }
        }
    }

    #[inline]
    fn unlock(state: &Self::State, mask: Self, wake: bool) {
        for (state, mask) in state.0.iter().zip(mask) {
            L::unlock(state, mask, false);
        }
        if wake {
            wake_array(&state.1);
        }
    }

    #[inline]
    fn is_locked(state: &Self::State, mask: Self, partial: bool) -> bool {
        for (state, mask) in state.0.iter().zip(mask) {
            if L::is_locked(state, mask, partial) {
                return true;
            }
        }
        false
    }

    #[inline]
    fn add(mut mask: Self, index: usize) -> Result<Self, IndexError> {
        match mask.get_mut(index / L::BITS) {
            Some(value) => {
                *value = L::add(*value, index % L::BITS)?;
                Ok(mask)
            }
            None => Err(IndexError::OutOfBounds(index)),
        }
    }

    #[inline]
    fn has(mask: Self, index: usize) -> bool {
        match mask.get(index / L::BITS) {
            Some(value) => L::has(*value, index % L::BITS),
            None => false,
        }
    }
}

#[cfg(target_os = "linux")]
#[inline]
fn wait_value<S, V, M>(state: &S, value: V, mask: M) {
    use std::mem::size_of;
    debug_assert_eq!(size_of::<S>(), size_of::<V>());
    debug_assert_eq!(size_of::<S>(), size_of::<M>());
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            state as *const Self,
            libc::FUTEX_WAIT_BITSET | libc::FUTEX_PRIVATE_FLAG,
            value,
            null::<libc::timespec>(),
            null::<u32>(),
            lock, // TODO: Convert mask to u32. Deal with overflows. At least 1 bit must be one, otherwise u32::MAX?
        )
    };
}

#[cfg(target_os = "windows")]
#[inline]
fn wait_value<S, V, M>(state: &S, value: V, _: M) {
    use std::mem::size_of;
    debug_assert_eq!(size_of::<S>(), size_of::<V>());
    unsafe {
        windows_sys::Win32::System::Threading::WaitOnAddress(
            state as *const _ as *const _,
            &value as *const _ as *const _,
            size_of::<S>(),
            u32::MAX,
        )
    };
}

#[cfg(target_os = "windows")]
#[inline]
fn wait_array(state: &AtomicU32, value: u32) {
    use std::mem::size_of;
    unsafe {
        windows_sys::Win32::System::Threading::WaitOnAddress(
            state as *const _ as *const _,
            &value as *const _ as *const _,
            size_of::<u32>(),
            u32::MAX,
        )
    };
}

#[cfg(target_os = "linux")]
#[inline]
fn wake_value<S, M>(state: &S, mask: M) {
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            state as *const _,
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
fn wake_value<S, M>(state: &S, _: M) {
    unsafe {
        windows_sys::Win32::System::Threading::WakeByAddressAll(state as *const _ as *const _)
    };
}

#[inline]
fn wake_array(state: &AtomicU32) {
    state.fetch_add(1, Release);
    unsafe {
        windows_sys::Win32::System::Threading::WakeByAddressAll(state as *const _ as *const _)
    };
}

impl DynamicAt for () {
    type Item<'a> = ()
    where
        Self: 'a;

    #[inline]
    unsafe fn dynamic_at<'a>(_: *mut Self, _: usize) -> Option<Self::Item<'a>> {
        Some(())
    }
}

impl<T: DynamicAt> DynamicAt for *mut T {
    type Item<'a> = T::Item<'a>
    where
        Self: 'a;

    #[inline]
    unsafe fn dynamic_at<'a>(items: *mut Self, index: usize) -> Option<Self::Item<'a>> {
        T::dynamic_at(items.read(), index)
    }
}

impl<'b, T: DynamicAt> DynamicAt for &'b mut T {
    type Item<'a> = T::Item<'a>
    where
        Self: 'a, 'b: 'a;

    #[inline]
    unsafe fn dynamic_at<'a>(items: *mut Self, index: usize) -> Option<Self::Item<'a>>
    where
        'b: 'a,
    {
        T::dynamic_at(items.read(), index)
    }
}

impl<T, const N: usize> DynamicAt for [T; N] {
    type Item<'a> = &'a mut T
    where
        Self: 'a;

    #[inline]
    unsafe fn dynamic_at<'a>(items: *mut Self, index: usize) -> Option<Self::Item<'a>> {
        if index < N {
            Some(&mut *items.cast::<T>().add(index))
        } else {
            None
        }
    }
}

impl<T> DynamicAt for [T] {
    type Item<'a> = &'a mut T
    where
        Self: 'a;

    #[inline]
    unsafe fn dynamic_at<'a>(items: *mut Self, index: usize) -> Option<Self::Item<'a>> {
        let raw = transmute::<_, RawSlice<T>>(items);
        if index < raw.1 {
            Some(&mut *raw.0.add(index))
        } else {
            None
        }
    }
}

impl<T, const N: usize> DynamicAt for Box<[T; N]> {
    type Item<'a> = &'a mut T
    where
        Self: 'a;

    #[inline]
    unsafe fn dynamic_at<'a>(items: *mut Self, index: usize) -> Option<Self::Item<'a>> {
        let raw = items.cast::<RawBox<[T; N]>>().read();
        <[T; N] as DynamicAt>::dynamic_at(raw.0, index)
    }
}

impl<T> DynamicAt for Box<[T]> {
    type Item<'a> = &'a mut T
    where
        Self: 'a;

    #[inline]
    unsafe fn dynamic_at<'a>(items: *mut Self, index: usize) -> Option<Self::Item<'a>> {
        let raw = items.cast::<RawBox<[T]>>().read();
        <[T] as DynamicAt>::dynamic_at(raw.0, index)
    }
}

impl<T> DynamicAt for Vec<T> {
    type Item<'a> = &'a mut T
    where
        Self: 'a;

    #[inline]
    unsafe fn dynamic_at<'a>(items: *mut Self, index: usize) -> Option<Self::Item<'a>> {
        let raw = items.cast::<RawVec<T>>().read();
        let slice = transmute::<_, *mut [T]>(RawSlice(raw.0, raw.2));
        <[T] as DynamicAt>::dynamic_at(slice, index)
    }
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl Error for IndexError {}

impl<'a, T, L: Lock> Guard<'a, T, L> {
    #[inline]
    pub fn map<U, F: FnOnce(T) -> U>(guard: Self, map: F) -> Guard<'a, U, L> {
        Guard(map(guard.0), guard.1)
    }

    #[inline]
    pub fn mask(&self) -> L {
        self.1 .1
    }
}

impl<T, L: Lock> Deref for Guard<'_, T, L> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, L: Lock> DerefMut for Guard<'_, T, L> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<L: Lock> Drop for Inner<'_, L> {
    fn drop(&mut self) {
        L::unlock(self.0, self.1, true);
    }
}

unsafe impl<T: DynamicAt + ?Sized, L: Lock, const N: usize> Key<T, L> for AtN<L, [usize; N]> {
    type Value<'a> = [Option<T::Item<'a>>; N] where T: 'a;

    #[inline]
    fn mask(&self) -> L {
        self.mask
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut T, mask: L) -> Self::Value<'a>
    where
        T: 'a,
    {
        self.indices.map(|index| {
            if L::has(mask, index) {
                T::dynamic_at(value, index)
            } else {
                None
            }
        })
    }
}

unsafe impl<T: DynamicAt + ?Sized, L: Lock> Key<T, L> for AtN<L, (usize,)> {
    type Value<'a> = (Option<T::Item<'a>>,) where T: 'a;

    #[inline]
    fn mask(&self) -> L {
        self.mask
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut T, mask: L) -> Self::Value<'a>
    where
        T: 'a,
    {
        let _0 = if L::has(mask, self.indices.0) {
            T::dynamic_at(value, self.indices.0)
        } else {
            None
        };
        (_0,)
    }
}

unsafe impl<T: DynamicAt + ?Sized, L: Lock> Key<T, L> for AtN<L, (usize, usize)> {
    type Value<'a> = (Option<T::Item<'a>>, Option<T::Item<'a>>) where T: 'a;

    #[inline]
    fn mask(&self) -> L {
        self.mask
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut T, mask: L) -> Self::Value<'a>
    where
        T: 'a,
    {
        let _0 = if L::has(mask, self.indices.0) {
            T::dynamic_at(value, self.indices.0)
        } else {
            None
        };
        let _1 = if L::has(mask, self.indices.1) {
            T::dynamic_at(value, self.indices.1)
        } else {
            None
        };
        (_0, _1)
    }
}

pub struct At1<L: Lock, const I: usize>(L);

impl<L: Lock, const I: usize> At1<L, I> {
    pub fn new() -> Result<Self, IndexError> {
        let mut mask = L::ZERO;
        mask = L::add(mask, I)?;
        Ok(Self(mask))
    }
}

pub struct At2<L: Lock, const I1: usize, const I2: usize>(L);
pub enum One1<T> {
    _1(T),
}
pub enum One2<T1, T2> {
    _1(T1),
    _2(T2),
}

impl<L: Lock, const I1: usize, const I2: usize> At2<L, I1, I2> {
    pub fn new() -> Result<Self, IndexError> {
        let mut mask = L::ZERO;
        mask = L::add(mask, I1)?;
        mask = L::add(mask, I2)?;
        Ok(Self(mask))
    }
}

impl<T> DynamicAt for (T,) {
    type Item<'a> = One1<&'a mut T> where Self: 'a;

    unsafe fn dynamic_at<'a>(items: *mut Self, index: usize) -> Option<Self::Item<'a>> {
        match index {
            0 => todo!(),
            _ => None,
        }
    }
}

impl<T1, T2> DynamicAt for (T1, T2) {
    type Item<'a> = One2<&'a mut T1, &'a mut T2> where Self: 'a;

    unsafe fn dynamic_at<'a>(items: *mut Self, index: usize) -> Option<Self::Item<'a>> {
        match index {
            0 => todo!(),
            _ => None,
        }
    }
}

impl<T> StaticAt<0> for (T,) {
    type Item<'a> = &'a mut T where Self: 'a;

    unsafe fn static_at<'a>(value: *mut Self) -> Self::Item<'a>
    where
        Self: 'a,
    {
        todo!()
    }
}

impl<T1, T2> StaticAt<0> for (T1, T2) {
    type Item<'a> = &'a mut T1 where Self: 'a;

    unsafe fn static_at<'a>(value: *mut Self) -> Self::Item<'a>
    where
        Self: 'a,
    {
        todo!()
    }
}

impl<T1, T2> StaticAt<1> for (T1, T2) {
    type Item<'a> = &'a mut T2 where Self: 'a;

    unsafe fn static_at<'a>(value: *mut Self) -> Self::Item<'a>
    where
        Self: 'a,
    {
        todo!()
    }
}

unsafe impl<T: StaticAt<I>, L: Lock, const I: usize> Key<T, L> for At1<L, I> {
    type Value<'a> = (Option<<T as StaticAt<I>>::Item<'a>>,) where T: 'a;

    #[inline]
    fn mask(&self) -> L {
        self.0
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut T, mask: L) -> Self::Value<'a>
    where
        T: 'a,
    {
        let one = if L::has(mask, I) {
            Some(<T as StaticAt<I>>::static_at(value))
        } else {
            None
        };
        (one,)
    }
}

unsafe impl<T: StaticAt<I1> + StaticAt<I2>, L: Lock, const I1: usize, const I2: usize> Key<T, L>
    for At2<L, I1, I2>
{
    type Value<'a> = (
        Option<<T as StaticAt<I1>>::Item<'a>>,
        Option<<T as StaticAt<I2>>::Item<'a>>,
    ) where T: 'a;

    #[inline]
    fn mask(&self) -> L {
        self.0
    }

    #[inline]
    unsafe fn value<'a>(&self, value: *mut T, mask: L) -> Self::Value<'a>
    where
        T: 'a,
    {
        let one = if L::has(mask, I1) {
            Some(<T as StaticAt<I1>>::static_at(value))
        } else {
            None
        };
        let two = if L::has(mask, I2) {
            Some(<T as StaticAt<I2>>::static_at(value))
        } else {
            None
        };
        (one, two)
    }
}

impl<L: Lock, const N: usize> AtN<L, [usize; N]> {
    pub fn new(indices: [usize; N]) -> Result<Self, IndexError> {
        let mut mask = L::ZERO;
        for index in indices {
            mask = L::add(mask, index)?;
        }
        Ok(Self { indices, mask })
    }
}

impl<L: Lock> AtN<L, (usize,)> {
    pub fn new(indices: (usize,)) -> Result<Self, IndexError> {
        let mut mask = L::ZERO;
        mask = L::add(mask, indices.0)?;
        Ok(Self { indices, mask })
    }
}

impl<L: Lock> AtN<L, (usize, usize)> {
    pub fn new(indices: (usize, usize)) -> Result<Self, IndexError> {
        let mut mask = L::ZERO;
        mask = L::add(mask, indices.0)?;
        mask = L::add(mask, indices.1)?;
        Ok(Self { indices, mask })
    }
}

impl<T, L: Lock> Multex<T, L> {
    #[inline]
    pub const fn as_ptr(&self) -> *const T {
        self.value.get()
    }

    #[inline]
    pub const fn as_mut_ptr(&self) -> *mut T {
        self.value.get()
    }

    #[inline]
    pub fn lock(&self) -> Guard<'_, &mut T, L> {
        match L::lock(&self.state, L::MAX, false, true) {
            Some(mask) => unsafe { self.guard(mask) },
            None => unreachable!(),
        }
    }

    #[inline]
    pub fn try_lock(&self) -> Option<Guard<&mut T, L>> {
        let mask = L::lock(&self.state, L::MAX, false, false)?;
        Some(unsafe { self.guard(mask) })
    }

    #[inline]
    pub fn lock_with<K: Key<T, L>>(&self, key: &K, partial: bool) -> Guard<'_, K::Value<'_>, L> {
        match L::lock(&self.state, key.mask(), partial, true) {
            Some(mask) => unsafe { self.guard_with(mask, key) },
            None => unreachable!(),
        }
    }

    #[inline]
    pub fn try_lock_with<K: Key<T, L>>(
        &self,
        key: &K,
        partial: bool,
    ) -> Option<Guard<'_, K::Value<'_>, L>> {
        let mask = L::lock(&self.state, key.mask(), partial, false)?;
        Some(unsafe { self.guard_with(mask, key) })
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        L::unlock(&self.state, L::MAX, true);
    }

    #[inline]
    pub unsafe fn unlock_with<K: Key<T, L>>(&self, mask: L) {
        L::unlock(&self.state, mask, true);
    }

    #[inline]
    pub fn is_locked(&self, partial: bool) -> bool {
        L::is_locked(&self.state, L::MAX, partial)
    }

    #[inline]
    pub fn is_locked_with<K: Key<T, L>>(&self, key: &K, partial: bool) -> bool {
        L::is_locked(&self.state, key.mask(), partial)
    }

    #[inline]
    pub fn get_mut<K: Key<T, L>>(&mut self, key: &K) -> K::Value<'_> {
        unsafe { key.value(self.value.get_mut(), key.mask()) }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[inline]
    unsafe fn guard(&self, mask: L) -> Guard<&mut T, L> {
        let inner = Inner(&self.state, mask);
        Guard(unsafe { &mut *self.value.get() }, inner)
    }

    #[inline]
    unsafe fn guard_with<K: Key<T, L>>(&self, mask: L, key: &K) -> Guard<K::Value<'_>, L> {
        let inner = Inner(&self.state, mask);
        Guard(key.value(self.value.get(), mask), inner)
    }
}

#[test]
fn locks_different_indices() -> Result<(), Box<dyn Error>> {
    let multex = Multex8::new([1u8, 2u8, 3u8, 4u8]);
    let key1 = AtN::<_, [_; 1]>::new([0])?;
    let key2 = AtN::<_, [_; 1]>::new([1])?;
    let mut guard1 = multex.lock_with(&key1, false);
    let mut guard2 = multex.lock_with(&key2, false);
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

// #[test]
// fn does_not_contend_on_out_of_bounds_indices() -> Result<(), Box<dyn Error>> {
//     let multex = Multex16::new([1u8, 2u8, 3u8, 4u8]);
//     let key1 = Indices::new([0, 4])?;
//     let key2 = Indices::new([1, 4])?;
//     let _guard1 = multex.lock_with(&key1, false);
//     let _guard2 = multex.lock_with(&key2, false);
//     Ok(())
// }

#[test]
fn locks_all_without_panic() {
    Multex32::new(Vec::new()).lock().push(1);
}

#[test]
fn boba() {
    let multex = Multex8::new((1u8, 2u16));
    let key1 = At1::<_, 1>::new().unwrap();
    let key2 = At2::<_, 1, 0>::new().unwrap();
    let key3 = AtN::<_, [_; 2]>::new([0, 1]).unwrap();
    let key4 = AtN::<_, (_, _)>::new((0, 1)).unwrap();
    let mut guard1 = multex.lock_with(&key1, false);
    let mut guard2 = multex.lock_with(&key2, false);
    let mut guard3 = multex.lock_with(&key3, false);
    let mut guard4 = multex.lock_with(&key4, false);
    **guard1.deref_mut().0.as_mut().unwrap() += 1;
    **guard2.deref_mut().1.as_mut().unwrap() += 1;
    match guard3[0].as_mut().unwrap() {
        One2::_1(_1) => **_1 += 1,
        One2::_2(_2) => **_2 += 2,
    }
    match guard4.deref_mut().0.as_mut().unwrap() {
        One2::_1(_1) => **_1 += 1,
        One2::_2(_2) => **_2 += 2,
    }
}
