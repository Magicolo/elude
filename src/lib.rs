pub mod depend;
pub mod error;
pub mod graph;
pub mod job;
mod utility;
pub mod work;

pub struct Get<T>(T);
pub struct Set<T>(Option<T>);
pub struct Full<T>(T);

#[test]
fn boba() -> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        depend::{Dependency, Key, Order},
        job::Job,
        work::Worker,
    };
    use parking_lot::Mutex;
    use std::cell::UnsafeCell;
    use std::error::Error;

    struct State<S>(UnsafeCell<S>);
    struct Build<'a, S, T, F>(&'a State<S>, Option<Order>, T, F, Vec<Dependency>);
    unsafe impl<S: Send> Send for State<S> {}
    unsafe impl<S: Sync> Sync for State<S> {}

    impl<S> State<S> {
        pub const fn new(state: S) -> Self {
            Self(UnsafeCell::new(state))
        }

        pub fn read<'a, T: 'a, F: Fn(&'a S) -> T + Send + Sync + 'static>(
            &'a self,
            get: F,
        ) -> Build<'a, S, T, F> {
            Build(self, None, get(unsafe { &*self.0.get() }), get, vec![])
        }

        pub fn write<'a, T: 'a, F: Fn(&'a mut S) -> T + Send + Sync + 'static>(
            &'a self,
            get: F,
        ) -> Build<S, T, F> {
            Build(self, None, get(unsafe { &mut *self.0.get() }), get, vec![])
        }
    }

    impl<S, T, F> Build<'_, S, T, F> {
        pub const fn order(mut self, order: Order) -> Self {
            self.1 = Some(order);
            self
        }

        pub const fn relax(self) -> Self {
            self.order(Order::Relax)
        }

        pub const fn strict(self) -> Self {
            self.order(Order::Strict)
        }
    }

    impl<S, T, F> Build<'_, S, &T, F> {
        fn resolve(&mut self, dependency: impl FnOnce(Key, Order) -> Dependency, order: Order) {
            let key = Key::Address(self.2 as *const T as usize);
            let order = self.1.unwrap_or(order);
            self.4.push(dependency(key, order));
        }
    }

    impl<S, T, F> Build<'_, S, &mut T, F> {
        fn resolve(&mut self, dependency: impl FnOnce(Key, Order) -> Dependency, order: Order) {
            let key = Key::Address(self.2 as *mut T as usize);
            let order = self.1.unwrap_or(order);
            self.4.push(dependency(key, order));
        }
    }

    impl<'a, S, T: 'a, F: FnMut(&'a S) -> &'a T + Send + Sync + 'a> Build<'a, S, &'a T, F> {
        pub fn read<U: 'a, G: FnMut(&'a T) -> U + Send + Sync + 'a>(
            mut self,
            mut get: G,
        ) -> Build<'a, S, U, impl FnMut(&'a S) -> U + Send + Sync + 'a> {
            self.resolve(Dependency::Read, Order::Relax);
            Build(
                self.0,
                None,
                get(self.3(unsafe { &*self.0 .0.get() })),
                move |state| get(self.3(state)),
                self.4,
            )
        }
    }

    impl<'a, S, T: 'a, F: FnMut(&'a mut S) -> &'a mut T + Send + Sync + 'a> Build<'a, S, &'a mut T, F> {
        pub fn read<U: 'a, G: FnMut(&'a T) -> U + Send + Sync + 'a>(
            mut self,
            mut get: G,
        ) -> Build<'a, S, U, impl FnMut(&'a mut S) -> U + Send + Sync + 'a> {
            self.resolve(Dependency::Read, Order::Relax);
            Build(
                self.0,
                None,
                get(self.3(unsafe { &mut *self.0 .0.get() })),
                move |state| get(self.3(state)),
                self.4,
            )
        }

        pub fn write<U: 'a, G: FnMut(&'a mut T) -> U + Send + Sync + 'a>(
            mut self,
            mut get: G,
        ) -> Build<'a, S, U, impl FnMut(&'a mut S) -> U + Send + Sync + 'a> {
            self.resolve(Dependency::Read, Order::Relax);
            Build(
                self.0,
                None,
                get(self.3(unsafe { &mut *self.0 .0.get() })),
                move |state| get(self.3(state)),
                self.4,
            )
        }
    }

    impl<'a, S: Send + Sync + 'a, T: 'a, F: Fn(&'a S) -> &'a T + Send + Sync + 'a>
        Build<'a, S, &'a T, F>
    {
        pub fn job<R: FnMut(&T) -> Result<(), Box<dyn Error + Send + Sync>> + Send + Sync + 'a>(
            mut self,
            mut run: R,
        ) -> Job<'a> {
            self.resolve(Dependency::Read, Order::Strict);
            unsafe { Job::new(move || run(self.3(&*self.0 .0.get())), self.4) }
        }
    }

    impl<'a, S: Send + Sync + 'a, T: 'a, F: Fn(&'a mut S) -> &'a mut T + Send + Sync + 'a>
        Build<'a, S, &'a mut T, F>
    {
        pub fn job<
            R: FnMut(&mut T) -> Result<(), Box<dyn Error + Send + Sync>> + Send + Sync + 'a,
        >(
            mut self,
            mut run: R,
        ) -> Job<'a> {
            self.resolve(Dependency::Write, Order::Strict);
            unsafe { Job::new(move || run(self.3(&mut *self.0 .0.get())), self.4) }
        }
    }

    #[derive(Default)]
    struct Jobs(Mutex<Vec<(&'static str, usize)>>);

    // TODO: 'State::new' should allow for passing references.
    let jobs = Jobs::default();
    let state = State::new(jobs);
    // TODO: 'state' must not be allowed to be sent in more than 1 'worker'. Worker should own that state?
    // TODO: Share the state using 'Arc<UnsafeCell<S>>'.
    let mut worker = Worker::new(None)?;
    for i in 0..100 {
        // worker.push(Job::barrier());
        // worker.push(Job::with(|| Ok(())));
        worker.push(state.read(|Jobs(jobs)| jobs).relax().job(move |jobs| {
            jobs.lock().push(("read", i));
            Ok(())
        }));
        worker.push(state.write(|Jobs(jobs)| jobs).relax().job(move |jobs| {
            jobs.get_mut().push(("write", i));
            Ok(())
        }));
    }
    worker.schedule()?;
    worker.run()?;
    Ok(())
}
