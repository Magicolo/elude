use std::{
    cell::UnsafeCell,
    collections::{BTreeMap, HashMap, VecDeque},
    hint::spin_loop,
    ops::{Deref, DerefMut},
    ptr::null,
    sync::atomic::{AtomicU32, Ordering::*},
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

#[repr(transparent)]
struct State(AtomicU32);

pub unsafe trait Mask {
    fn mask(&self) -> u32;
}

pub unsafe trait Key<T: ?Sized>: Mask {
    type Value<'a>
    where
        T: 'a;
    fn value<'a>(&self, value: *mut T) -> Self::Value<'a>;
}

pub struct Indices<const N: usize> {
    mask: u32,
    indices: [u8; N],
}

pub struct All;

pub struct Guard<'a, T>(T, Inner<'a>);
/// [`Inner`] should be kept separate from [`Guard`] such that its [`Drop`] implementation is called even if
/// a panic occurs when the value `T` is produced.
struct Inner<'a>(&'a State, u32);

unsafe impl<T: Send> Send for Multex<T> {}
unsafe impl<T: Sync> Sync for Multex<T> {}

impl State {
    pub fn lock(&self, mask: u32) {
        // Try to lock optimistically as if there was no contention.
        let Err(mut state) = self.0.compare_exchange_weak(0, mask, Acquire, Relaxed) else {
            return;
        };

        let mut spin = 0;
        loop {
            if state & mask == 0 {
                // Try to lock using the previously loaded state.
                let Err(new) = self.0.compare_exchange_weak(state, state | mask, Acquire, Relaxed) else {
                    return;
                };
                state = new;
                // The lock was modified or there was a spurious failure; retry.
                continue;
            }
            if spin < 100 {
                spin += 1;
                state = self.0.load(Relaxed);
                spin_loop();
                continue;
            }
            self.wait(state, mask);
            spin = 0;
            state = self.0.load(Relaxed);
        }
    }

    pub fn try_lock(&self, mask: u32) -> bool {
        // Try to lock optimistically as if there was no contention.
        let Err(mut state) = self.0.compare_exchange_weak(0, mask, Acquire, Relaxed) else {
            return true;
        };

        while state & mask == 0 {
            // Try to lock using the previously loaded state.
            let Err(new) = self.0.compare_exchange_weak(state, state | mask, Acquire, Relaxed) else {
                return true;
            };
            state = new;
            // The lock was modified or there was a spurious failure; retry.
        }
        false
    }

    #[inline]
    pub fn unlock(&self, mask: u32) {
        self.0.fetch_and(!mask, Release);
    }

    // TODO: Add support for more platforms.
    #[inline]
    pub fn wait(&self, previous: u32, mask: u32) {
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                self as *const _,
                libc::FUTEX_WAIT_BITSET | libc::FUTEX_PRIVATE_FLAG,
                previous,
                null::<libc::timespec>(),
                null::<u32>(),
                mask,
            )
        };
    }

    // TODO: Add support for more platforms.
    #[inline]
    pub fn wake(&self, count: usize, mask: u32) {
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                self as *const _,
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                count as u32,
                null::<libc::timespec>(),
                null::<u32>(),
                mask,
            )
        };
    }
}

impl<'a, T> Guard<'a, T> {
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
        self.0.wake(1, self.1);
    }
}

unsafe impl Mask for All {
    #[inline]
    fn mask(&self) -> u32 {
        u32::MAX
    }
}

unsafe impl<T> Key<T> for All {
    type Value<'a> = &'a mut T where T: 'a;

    #[inline]
    fn value<'a>(&self, value: *mut T) -> Self::Value<'a>
    where
        T: 'a,
    {
        unsafe { &mut *value }
    }
}

unsafe impl<const N: usize> Mask for Indices<N> {
    #[inline]
    fn mask(&self) -> u32 {
        self.mask
    }
}

unsafe impl<const N: usize> Key<()> for Indices<N> {
    type Value<'a> = ();

    #[inline]
    fn value<'a>(&self, _: *mut ()) -> Self::Value<'a> {}
}

macro_rules! key {
    ($t:ty, [$($c:ident)?], [$($r:tt)?]) => {
        unsafe impl<T $(, const $c: usize)?, const N: usize> Key<$t> for Indices<N> {
            type Value<'a> = [Option<&'a mut T>; N] where T: 'a;
            #[inline]
            fn value<'a>(&self, value: *mut $t) -> Self::Value<'a> {
                self.indices.map(|index| unsafe { &mut *value }.get_mut($($r)?(index as usize)))
            }
        }
    };
}

key!([T; M], [M], []);
key!([T], [], []);
key!(Box<[T]>, [], []);
key!(Vec<T>, [], []);
key!(VecDeque<T>, [], []);
key!(HashMap<usize, T>, [], [&]);
key!(BTreeMap<usize, T>, [], [&]);

impl<const M: usize> Indices<M> {
    pub fn new(indices: [usize; M]) -> Option<Self> {
        let mut mask = 0;
        for index in indices {
            if index >= u32::BITS as usize {
                // Index out of bounds.
                return None;
            }

            let bit = 1 << index;
            let next = mask | bit;
            if mask == next {
                // Duplicate index.
                return None;
            } else {
                mask = next;
            }
        }
        if mask == 0 {
            // Mask must not be empty.
            None
        } else {
            Some(Self {
                indices: indices.map(|index| index as u8),
                mask,
            })
        }
    }
}

impl<T> Multex<T> {
    #[inline]
    pub const fn new(values: T) -> Self {
        Self {
            state: State(AtomicU32::new(0)),
            value: UnsafeCell::new(values),
        }
    }

    #[inline]
    pub fn lock<K: Key<T>>(&self, key: &K) -> Guard<'_, K::Value<'_>> {
        let mask = key.mask();
        self.state.lock(mask);
        let inner = Inner(&self.state, mask);
        Guard(key.value(self.value.get()), inner)
    }

    #[inline]
    pub fn try_lock<K: Key<T>>(&self, key: &K) -> Option<Guard<'_, K::Value<'_>>> {
        let mask = key.mask();
        if self.state.try_lock(mask) {
            let inner = Inner(&self.state, mask);
            Some(Guard(key.value(self.value.get()), inner))
        } else {
            None
        }
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

#[test]
fn locks_different_indices() {
    let multex = Multex::new([1u8, 2u8, 3u8, 4u8]);
    let key1 = Indices::new([0]).unwrap();
    let key2 = Indices::new([1]).unwrap();
    let mut guard1 = multex.lock(&key1);
    let mut guard2 = multex.lock(&key2);
    let Some(value1) = guard1[0].as_mut() else { panic!() };
    let Some(value2) = guard2[0].as_mut() else { panic!() };
    assert_eq!(**value1, 1u8);
    assert_eq!(**value2, 2u8);
}

#[test]
fn boba() {
    use std::thread::scope;
    use std::thread::sleep;
    use std::time::Duration;

    let values = [(); 11].map(|_| Vec::new());
    let count = values.len();
    let multex = Multex::new(values);
    scope(|scope| {
        for i in 0..100 {
            let multex = &multex;
            let key = Indices::new([i % count]).unwrap();
            scope.spawn(move || {
                let mut guard = multex.lock(&key);
                let Some(value) = &mut guard[0] else { panic!() };
                value.push(i);
                println!("ACQUIRE: {i}");
                sleep(Duration::from_millis(100));
                println!("RELEASE: {i}");
                drop(guard);
            });
        }
    });
    dbg!(multex.into_inner());
}

fn fett() {
    type A = Multex<Vec<Multex<[usize; 32]>>>;
    let a = A::new(Vec::new());
    a.lock(&All).push(Multex::new([0; 32]));
}
