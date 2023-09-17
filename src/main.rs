use std::{
    thread::{scope, sleep},
    time::Duration,
};

use elude::multex::{AtN, Multex64};

const COUNT: usize = 64;
const BATCHES: [usize; 6] = [1, 5, 10, 25, 50, 100];
const OFFSETS: [usize; 9] = [1, 3, 7, 11, 13, 17, 19, 23, 29];

fn main() {
    let multex = Multex64::new([(); COUNT].map(|_| 0));
    let batches = BATCHES.map(|batch| {
        (0..batch)
            .map(|i| AtN::<_, [_; 9]>::new(OFFSETS.map(|offset| (offset + i) % COUNT)).unwrap())
            .collect::<Box<[_]>>()
    });
    for i in 0.. {
        println!("{i}");
        for batch in batches.iter() {
            scope(|scope| {
                let multex = &multex;
                for (i, key) in batch.iter().enumerate() {
                    scope.spawn(move || {
                        let mut guard = multex.lock_with(key, false);
                        for guard in guard.iter_mut() {
                            **guard.as_mut().unwrap() += i;
                        }
                        sleep(Duration::from_nanos(i as u64));
                        drop(guard);
                    });
                }
            });
        }
    }
}
