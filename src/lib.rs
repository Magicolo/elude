pub mod depend;
pub mod error;
pub mod graph;
pub mod job;
mod utility;
pub mod work;

pub struct Get<T>(T);
pub struct Set<T>(Option<T>);
pub struct Full<T>(T);

use crate::{
    depend::{Dependency, Key, Order},
    job::Job,
};
use job::RunResult;
use std::marker::PhantomData;
use work::Build;

// impl<S> State<S> {
//     pub fn read<T, F: Fn(&S) -> &T + Send + Sync>(&mut self, get: F) -> Build<'_, S, &T, F> {
//         Build(self, None, get, vec![], PhantomData)
//     }

//     pub fn write<T, F: Fn(&mut S) -> &mut T + Send + Sync>(
//         &mut self,
//         get: F,
//     ) -> Build<'_, S, &mut T, F> {
//         Build(self, None, get, vec![], PhantomData)
//     }
// }

impl<'a, 'b, S> Build<'a, 'b, S, (), ()> {
    pub fn read<'c, T, F: Fn(&S) -> &T + Send + Sync>(self, get: F) -> Build<'a, 'b, S, &'c T, F> {
        Build(self.0, None, get, self.3, PhantomData)
    }

    pub fn write<'c, T, F: Fn(&mut S) -> &mut T + Send + Sync>(
        self,
        get: F,
    ) -> Build<'a, 'b, S, &'c mut T, F> {
        Build(self.0, None, get, self.3, PhantomData)
    }
}

impl<S, T, F> Build<'_, '_, S, T, F> {
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

impl<S, T, F: Fn(&S) -> &T> Build<'_, '_, S, &T, F> {
    fn resolve(&mut self, dependency: impl FnOnce(Key, Order) -> Dependency, order: Order) {
        let value = self.2(unsafe { self.0.state.get() });
        let key = Key::Address(value as *const T as usize);
        let order = self.1.unwrap_or(order);
        self.3.push(dependency(key, order));
    }
}

// impl<S, T, F: Fn(&mut S) -> &mut T> Build<'_, S, &mut T, F> {
//     fn resolve(&mut self, dependency: impl FnOnce(Key, Order) -> Dependency, order: Order) {
//         let value = self.2(unsafe { self.0.get() });
//         let key = Key::Address(value as *mut T as usize);
//         let order = self.1.unwrap_or(order);
//         self.3.push(dependency(key, order));
//     }
// }

impl<'a, S: Send + Sync + 'a, T, F: Fn(&S) -> &T + Send + Sync + 'a> Build<'a, '_, S, &T, F> {
    pub fn job<R: FnMut(&T) -> RunResult + Send + Sync + 'a>(mut self, mut run: R) {
        self.resolve(Dependency::Read, Order::Strict);
        let Self(worker, _, map, dependencies, _) = self;
        let job = unsafe { Job::new(move |state| run(map(state)), dependencies) };
        worker.push(job);
    }
}

// impl<'a, S: Send + Sync + 'a, T, F: Fn(&mut S) -> &mut T + Send + Sync + 'a>
//     Build<'_, S, &mut T, F>
// {
//     pub fn job<R: FnMut(&mut T) -> RunResult + Send + Sync + 'a>(mut self, mut run: R) -> Job<'a> {
//         self.resolve(Dependency::Write, Order::Strict);
//         let Self(state, _, map, dependencies, _) = self;
//         let state = state.clone();
//         unsafe { Job::new(move || run(map(state.get())), dependencies) }
//     }
// }

#[test]
fn boba() -> Result<(), Box<dyn std::error::Error>> {
    use parking_lot::Mutex;
    use work::Worker;

    #[derive(Default, Debug)]
    struct Jobs(Mutex<Vec<(&'static str, usize)>>);

    loop {
        // TODO: 'State::new' should allow for passing references.
        let mut jobs = Jobs::default();
        // TODO: 'state' must not be allowed to be sent in more than 1 'worker'. Worker should own that state?
        // TODO: Share the state using 'Arc<UnsafeCell<S>>'.
        let mut worker = Worker::new(&mut jobs, None)?;
        for i in 0..32 {
            // worker.push(Job::barrier());
            // worker.push(Job::with(|| Ok(())));
            worker
                .build()
                .read(|Jobs(jobs)| jobs)
                .relax()
                .job(move |jobs| {
                    jobs.lock().push(("read", i));
                    Ok(())
                });
            // worker2.push(state.write(|Jobs(jobs)| jobs).relax().job(move |jobs| {
            //     jobs.lock().push(("read", i));
            //     Ok(())
            // }));
            // state.read(|a| a).job(|_| Ok(()));
            // worker.push(state.read(|Jobs(jobs)| jobs).relax().job(move |jobs| {
            //     jobs.lock().push(("read", i));
            //     Ok(())
            // }));
            // worker.push(state.write(|Jobs(jobs)| jobs).relax().job(move |jobs| {
            //     jobs.get_mut().push(("write", i));
            //     Ok(())
            // }));
            // worker2.push(state.write(|Jobs(jobs)| jobs).relax().job(move |jobs| {
            //     jobs.get_mut().push(("write", i));
            //     Ok(())
            // }));
        }
        worker.schedule()?;
        worker.run()?;
        drop(worker);
        jobs.0.get_mut().push(("a", 1));
    }
}
