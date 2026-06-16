use elude::legacy::{
    depend::{
        Dependency::{self, Read, Unknown, Write},
        Key::{self, Identifier},
        Order::{Relax, Strict},
        Scope,
    },
    error::Error as ScheduleError,
    experiment::{CompiledSchedule as _, Job, Scheduler},
};
use std::{
    cmp::max,
    collections::HashMap,
    fmt,
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
        Arc, Mutex,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Relation {
    None,
    Relaxed,
    Strict,
}

#[derive(Clone, Copy, Debug)]
enum Case {
    None,
    Unknown,
    ReadRelaxA,
    ReadStrictA,
    WriteRelaxA,
    WriteStrictA,
    ReadRelaxB,
    ReadStrictB,
    WriteRelaxB,
    WriteStrictB,
}

impl Case {
    const ALL: [Self; 10] = [
        Self::None,
        Self::Unknown,
        Self::ReadRelaxA,
        Self::ReadStrictA,
        Self::WriteRelaxA,
        Self::WriteStrictA,
        Self::ReadRelaxB,
        Self::ReadStrictB,
        Self::WriteRelaxB,
        Self::WriteStrictB,
    ];

    fn dependencies(self) -> Vec<Dependency> {
        match self {
            Self::None => vec![],
            Self::Unknown => vec![Unknown],
            Self::ReadRelaxA => vec![Read(Identifier(0), Relax)],
            Self::ReadStrictA => vec![Read(Identifier(0), Strict)],
            Self::WriteRelaxA => vec![Write(Identifier(0), Relax)],
            Self::WriteStrictA => vec![Write(Identifier(0), Strict)],
            Self::ReadRelaxB => vec![Read(Identifier(1), Relax)],
            Self::ReadStrictB => vec![Read(Identifier(1), Strict)],
            Self::WriteRelaxB => vec![Write(Identifier(1), Relax)],
            Self::WriteStrictB => vec![Write(Identifier(1), Strict)],
        }
    }
}

impl fmt::Display for Case {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::None => "none",
            Self::Unknown => "unknown",
            Self::ReadRelaxA => "read_relax_a",
            Self::ReadStrictA => "read_strict_a",
            Self::WriteRelaxA => "write_relax_a",
            Self::WriteStrictA => "write_strict_a",
            Self::ReadRelaxB => "read_relax_b",
            Self::ReadStrictB => "read_strict_b",
            Self::WriteRelaxB => "write_relax_b",
            Self::WriteStrictB => "write_strict_b",
        };
        f.write_str(label)
    }
}

struct Probe {
    started: Vec<AtomicUsize>,
    finished: Vec<AtomicBool>,
    overlap_count: AtomicUsize,
    overlap_violation: AtomicBool,
    order_violation: AtomicBool,
}

impl Probe {
    fn new(len: usize) -> Self {
        Self {
            started: (0..len).map(|_| AtomicUsize::new(0)).collect(),
            finished: (0..len).map(|_| AtomicBool::new(false)).collect(),
            overlap_count: AtomicUsize::new(0),
            overlap_violation: AtomicBool::new(false),
            order_violation: AtomicBool::new(false),
        }
    }
}

fn pair_relation(left: &[Dependency], right: &[Dependency]) -> Relation {
    let mut relation = Relation::None;
    for left in left.iter() {
        for right in right.iter() {
            relation = max(relation, dependency_relation(left, right));
            if relation == Relation::Strict {
                return relation;
            }
        }
    }
    relation
}

fn dependency_relation(left: &Dependency, right: &Dependency) -> Relation {
    match (left, right) {
        (Unknown, _) | (_, Unknown) => Relation::Strict,
        (Read(left_key, _), Read(right_key, _)) if left_key == right_key => Relation::None,
        (Read(left_key, left_order), Write(right_key, right_order))
        | (Write(left_key, left_order), Read(right_key, right_order))
        | (Write(left_key, left_order), Write(right_key, right_order))
            if left_key == right_key =>
        {
            match max(*left_order, *right_order) {
                Strict => Relation::Strict,
                Relax => Relation::Relaxed,
            }
        }
        _ => Relation::None,
    }
}

#[derive(Default)]
struct InternalState {
    order: Option<elude::legacy::depend::Order>,
    read: bool,
    write: bool,
}

fn expected_internal_error(dependencies: &[Dependency]) -> Option<ScheduleError> {
    let mut states = HashMap::<Key, InternalState>::new();

    for dependency in dependencies.iter() {
        let (key, write, order) = match dependency {
            Unknown => continue,
            Read(key, order) => (key, false, *order),
            Write(key, order) => (key, true, *order),
        };

        let state = states.entry(key.clone()).or_default();
        let order = state.order.map_or(order, |previous| max(previous, order));

        if write && state.read {
            return Some(ScheduleError::ReadWriteConflict(
                key.clone(),
                Scope::Inner,
                order,
            ));
        }
        if !write && state.write {
            return Some(ScheduleError::ReadWriteConflict(
                key.clone(),
                Scope::Inner,
                order,
            ));
        }
        if write && state.write {
            return Some(ScheduleError::WriteWriteConflict(
                key.clone(),
                Scope::Inner,
                order,
            ));
        }

        state.order = Some(order);
        state.read |= !write;
        state.write |= write;
    }

    None
}

fn make_probe_job(
    index: usize,
    dependencies: Vec<Dependency>,
    relation: Relation,
    probe: Arc<Probe>,
) -> Job<()> {
    let must_not_overlap = relation != Relation::None;
    let must_follow_first = relation == Relation::Strict && index == 1;
    Job::new(move |_| {
        probe.started[index].fetch_add(1, SeqCst);

        if must_follow_first && !probe.finished[0].load(SeqCst) {
            probe.order_violation.store(true, SeqCst);
        }

        if must_not_overlap && probe.overlap_count.fetch_add(1, SeqCst) != 0 {
            probe.overlap_violation.store(true, SeqCst);
        }

        std::thread::yield_now();
        for _ in 0..10_000 {
            std::hint::spin_loop();
        }

        if must_not_overlap {
            probe.overlap_count.fetch_sub(1, SeqCst);
        }
        probe.finished[index].store(true, SeqCst);
        Ok(())
    })
    .depend(dependencies)
}

fn run_pair_case<T>(left: Vec<Dependency>, right: Vec<Dependency>, label: &str)
where
    T: Scheduler<()>,
{
    let relation = pair_relation(&left, &right);
    let probe = Arc::new(Probe::new(2));
    let mut compiled = T::new()
        .add(make_probe_job(0, left, relation, Arc::clone(&probe)))
        .add(make_probe_job(1, right, relation, Arc::clone(&probe)))
        .schedule()
        .unwrap();

    compiled.run(&()).unwrap();

    assert_eq!(
        probe.started[0].load(SeqCst),
        1,
        "{label}: first job did not run exactly once"
    );
    assert_eq!(
        probe.started[1].load(SeqCst),
        1,
        "{label}: second job did not run exactly once"
    );
    assert!(
        !probe.overlap_violation.load(SeqCst),
        "{label}: conflicting jobs overlapped"
    );
    if relation == Relation::Strict {
        assert!(
            !probe.order_violation.load(SeqCst),
            "{label}: strict conflict did not preserve declaration order"
        );
    }
}

pub fn empty_schedule_runs<T>()
where
    T: Scheduler<()>,
{
    let mut compiled = T::new().schedule().unwrap();
    compiled.run(&()).unwrap();
}

pub fn repeated_runs_execute_each_job_once<T>()
where
    T: Scheduler<()>,
{
    let runs = 4usize;
    let calls = Arc::new((0..8).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());

    let mut scheduler = T::new();
    for index in 0..8usize {
        let calls = Arc::clone(&calls);
        scheduler = scheduler.add(
            Job::new(move |_| {
                calls[index].fetch_add(1, SeqCst);
                Ok(())
            })
            .depend([Write(Identifier(index % 2), Relax)]),
        );
    }

    let mut compiled = scheduler.schedule().unwrap();
    for _ in 0..runs {
        compiled.run(&()).unwrap();
    }

    for (index, call_count) in calls.iter().enumerate() {
        assert_eq!(
            call_count.load(SeqCst),
            runs,
            "job {index} did not run exactly once per iteration"
        );
    }
}

pub fn exhaustive_pairwise_conflict_matrix<T>()
where
    T: Scheduler<()>,
{
    for left in Case::ALL {
        for right in Case::ALL {
            let label = format!("{left} -> {right}");
            run_pair_case::<T>(left.dependencies(), right.dependencies(), &label);
        }
    }
}

pub fn exhaustive_internal_dependency_matrix<T>()
where
    T: Scheduler<()>,
{
    for left in Case::ALL {
        for right in Case::ALL {
            let mut dependencies = left.dependencies();
            dependencies.extend(right.dependencies());
            let label = format!("internal {left} + {right}");
            let expected = expected_internal_error(&dependencies);

            let started = Arc::new(AtomicBool::new(false));
            let seen = Arc::clone(&started);
            let result = T::new()
                .add(
                    Job::new(move |_| {
                        seen.store(true, SeqCst);
                        Ok(())
                    })
                    .depend(dependencies),
                )
                .schedule();

            match (result, expected) {
                (Ok(mut compiled), None) => {
                    compiled.run(&()).unwrap();
                    assert!(started.load(SeqCst), "{label}: valid job did not run");
                }
                (Err(error), Some(expected)) => {
                    let actual = error.downcast_ref::<ScheduleError>().unwrap_or_else(|| {
                        panic!("{label}: expected scheduler error, got {error:?}")
                    });
                    assert_eq!(
                        format!("{actual:?}"),
                        format!("{expected:?}"),
                        "{label}: wrong internal conflict rejection"
                    );
                    assert!(
                        !started.load(SeqCst),
                        "{label}: invalid job must not have been executed"
                    );
                }
                (Ok(_), Some(expected)) => {
                    panic!("{label}: accepted invalid internal dependency set {expected:?}");
                }
                (Err(error), None) => {
                    panic!("{label}: rejected valid dependency set with {error:?}");
                }
            }
        }
    }
}

pub fn duplicate_reads_within_a_job_are_allowed<T>()
where
    T: Scheduler<()>,
{
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&runs);

    let mut compiled = T::new()
        .add(
            Job::new(move |_| {
                seen.fetch_add(1, SeqCst);
                Ok(())
            })
            .depend([Read(Identifier(0), Relax), Read(Identifier(0), Strict)]),
        )
        .schedule()
        .unwrap();

    compiled.run(&()).unwrap();
    assert_eq!(
        runs.load(SeqCst),
        1,
        "duplicate reads within one job should be accepted"
    );
}

pub fn unknown_dependency_is_a_strict_barrier<T>()
where
    T: Scheduler<()>,
{
    let stage = Arc::new(AtomicUsize::new(0));
    let violation = Arc::new(AtomicBool::new(false));

    let mut scheduler = T::new();
    for index in 0..3usize {
        let stage = Arc::clone(&stage);
        let violation = Arc::clone(&violation);

        let job = Job::new(move |_| {
            let expected = index;
            if stage.load(SeqCst) != expected {
                violation.store(true, SeqCst);
            }

            std::thread::yield_now();
            for _ in 0..5_000 {
                std::hint::spin_loop();
            }

            stage.store(expected + 1, SeqCst);
            Ok(())
        });

        let job = match index {
            1 => job.depend([Unknown]),
            _ => job,
        };

        scheduler = scheduler.add(job);
    }

    let mut compiled = scheduler.schedule().unwrap();
    compiled.run(&()).unwrap();

    assert!(
        !violation.load(SeqCst),
        "unknown dependency did not behave as a strict barrier"
    );
    assert_eq!(
        stage.load(SeqCst),
        3,
        "barrier sequence did not fully execute"
    );
}

pub fn invalid_internal_conflicts_are_rejected<T>()
where
    T: Scheduler<()>,
{
    fn assert_invalid<T>(
        dependencies: Vec<Dependency>,
        expected: fn(&ScheduleError) -> bool,
        label: &str,
    ) where
        T: Scheduler<()>,
    {
        let started = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&started);

        let result = T::new()
            .add(
                Job::new(move |_| {
                    seen.store(true, SeqCst);
                    Ok(())
                })
                .depend(dependencies),
            )
            .schedule();

        let error = match result {
            Ok(_) => panic!("{label}: scheduler accepted an internally conflicting job"),
            Err(error) => error,
        };

        let schedule_error = error
            .downcast_ref::<ScheduleError>()
            .unwrap_or_else(|| panic!("{label}: expected scheduler error, got {error:?}"));

        assert!(
            expected(schedule_error),
            "{label}: wrong error {schedule_error:?}"
        );
        assert!(
            !started.load(SeqCst),
            "{label}: invalid job must not have been executed"
        );
    }

    assert_invalid::<T>(
        vec![Read(Identifier(0), Relax), Write(Identifier(0), Relax)],
        |error| {
            matches!(
                error,
                ScheduleError::ReadWriteConflict(
                    elude::legacy::depend::Key::Identifier(0),
                    Scope::Inner,
                    Relax
                )
            )
        },
        "read/write conflict within one job",
    );

    assert_invalid::<T>(
        vec![Write(Identifier(1), Relax), Write(Identifier(1), Strict)],
        |error| {
            matches!(
                error,
                ScheduleError::WriteWriteConflict(
                    elude::legacy::depend::Key::Identifier(1),
                    Scope::Inner,
                    Strict
                )
            )
        },
        "write/write conflict within one job",
    );
}

pub fn strict_write_chain_preserves_declaration_order<T>()
where
    T: Scheduler<()>,
{
    let order = Arc::new(Mutex::new(Vec::<usize>::new()));
    let mut scheduler = T::new();
    for index in 0..4usize {
        let order = Arc::clone(&order);
        scheduler = scheduler.add(
            Job::new(move |_| {
                std::thread::yield_now();
                for _ in 0..5_000 {
                    std::hint::spin_loop();
                }
                order.lock().unwrap().push(index);
                Ok(())
            })
            .depend([Write(Identifier(0), Strict)]),
        );
    }

    let mut compiled = scheduler.schedule().unwrap();
    compiled.run(&()).unwrap();

    assert_eq!(
        *order.lock().unwrap(),
        vec![0, 1, 2, 3],
        "strict write chain did not preserve declaration order"
    );
}

pub fn strict_conflict_dominates_when_multiple_dependencies_are_present<T>()
where
    T: Scheduler<()>,
{
    let left = vec![Write(Identifier(0), Relax), Write(Identifier(1), Strict)];
    let right = vec![Write(Identifier(0), Relax), Write(Identifier(1), Strict)];
    run_pair_case::<T>(left, right, "multi-dependency strict dominance");
}

pub fn three_job_mixed_constraints_are_respected<T>()
where
    T: Scheduler<()>,
{
    let overlap = Arc::new(AtomicUsize::new(0));
    let overlap_violation = Arc::new(AtomicBool::new(false));
    let strict_predecessor_finished = Arc::new(AtomicBool::new(false));
    let counts = Arc::new((0..3).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());

    let mut scheduler = T::new();
    for index in 0..3usize {
        let overlap = Arc::clone(&overlap);
        let overlap_violation = Arc::clone(&overlap_violation);
        let strict_predecessor_finished = Arc::clone(&strict_predecessor_finished);
        let counts = Arc::clone(&counts);

        let job = Job::new(move |_| {
            counts[index].fetch_add(1, SeqCst);

            if index <= 1 && overlap.fetch_add(1, SeqCst) != 0 {
                overlap_violation.store(true, SeqCst);
            }
            if index == 2 && !strict_predecessor_finished.load(SeqCst) {
                overlap_violation.store(true, SeqCst);
            }

            std::thread::yield_now();
            for _ in 0..10_000 {
                std::hint::spin_loop();
            }

            if index <= 1 {
                overlap.fetch_sub(1, SeqCst);
            }
            if index == 1 {
                strict_predecessor_finished.store(true, SeqCst);
            }
            Ok(())
        });

        let job = match index {
            0 => job.depend([Write(Identifier(0), Relax)]),
            1 => job.depend([Write(Identifier(0), Relax), Write(Identifier(1), Strict)]),
            2 => job.depend([Write(Identifier(1), Strict)]),
            _ => unreachable!(),
        };

        scheduler = scheduler.add(job);
    }

    let mut compiled = scheduler.schedule().unwrap();
    compiled.run(&()).unwrap();

    assert!(
        !overlap_violation.load(SeqCst),
        "mixed three-job constraints were violated"
    );
    for (index, count) in counts.iter().enumerate() {
        assert_eq!(
            count.load(SeqCst),
            1,
            "job {index} did not run exactly once"
        );
    }
}

pub struct OverlapState {
    entered: AtomicUsize,
    overlap: AtomicBool,
}

impl OverlapState {
    pub fn new() -> Self {
        Self {
            entered: AtomicUsize::new(0),
            overlap: AtomicBool::new(false),
        }
    }
}

pub fn independent_jobs_can_overlap<T>()
where
    T: Scheduler<OverlapState>,
{
    let state = OverlapState::new();
    let mut compiled = T::with_parallelism(NonZeroUsize::new(2))
        .add(Job::new(|state: &OverlapState| {
            let current = state.entered.fetch_add(1, SeqCst) + 1;
            if current > 1 {
                state.overlap.store(true, SeqCst);
            }

            std::thread::yield_now();
            for _ in 0..100_000 {
                std::hint::spin_loop();
                if state.entered.load(SeqCst) > 1 {
                    state.overlap.store(true, SeqCst);
                    break;
                }
            }

            state.entered.fetch_sub(1, SeqCst);
            Ok(())
        }))
        .add(Job::new(|state: &OverlapState| {
            let current = state.entered.fetch_add(1, SeqCst) + 1;
            if current > 1 {
                state.overlap.store(true, SeqCst);
            }

            std::thread::yield_now();
            for _ in 0..100_000 {
                std::hint::spin_loop();
                if state.entered.load(SeqCst) > 1 {
                    state.overlap.store(true, SeqCst);
                    break;
                }
            }

            state.entered.fetch_sub(1, SeqCst);
            Ok(())
        }))
        .schedule()
        .unwrap();

    compiled.run(&state).unwrap();

    assert!(
        state.overlap.load(SeqCst),
        "independent jobs did not overlap on a two-thread scheduler"
    );
}

pub fn shared_reads_can_overlap<T>()
where
    T: Scheduler<OverlapState>,
{
    let state = OverlapState::new();
    let mut compiled = T::with_parallelism(NonZeroUsize::new(2))
        .add(
            Job::new(|state: &OverlapState| {
                let current = state.entered.fetch_add(1, SeqCst) + 1;
                if current > 1 {
                    state.overlap.store(true, SeqCst);
                }

                std::thread::yield_now();
                for _ in 0..100_000 {
                    std::hint::spin_loop();
                    if state.entered.load(SeqCst) > 1 {
                        state.overlap.store(true, SeqCst);
                        break;
                    }
                }

                state.entered.fetch_sub(1, SeqCst);
                Ok(())
            })
            .depend([Read(Identifier(0), Relax)]),
        )
        .add(
            Job::new(|state: &OverlapState| {
                let current = state.entered.fetch_add(1, SeqCst) + 1;
                if current > 1 {
                    state.overlap.store(true, SeqCst);
                }

                std::thread::yield_now();
                for _ in 0..100_000 {
                    std::hint::spin_loop();
                    if state.entered.load(SeqCst) > 1 {
                        state.overlap.store(true, SeqCst);
                        break;
                    }
                }

                state.entered.fetch_sub(1, SeqCst);
                Ok(())
            })
            .depend([Read(Identifier(0), Strict)]),
        )
        .schedule()
        .unwrap();

    compiled.run(&state).unwrap();

    assert!(
        state.overlap.load(SeqCst),
        "shared reads on the same key did not overlap on a two-thread scheduler"
    );
}
