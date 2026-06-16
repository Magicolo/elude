use core::{
    alloc::{Layout, LayoutError},
    any::{TypeId, type_name},
    marker::PhantomData,
    mem::needs_drop,
    ptr::{NonNull, copy, drop_in_place, slice_from_raw_parts_mut},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use slice_dst::SliceWithHeader;
use std::sync::Arc;

pub mod legacy;

pub enum Void {}

#[derive(Debug, Clone, Copy)]
pub struct Row {
    table: u32,
    row: u32,
}

pub struct Meta {
    pub identify: fn() -> TypeId,
    pub name: fn() -> &'static str,
    pub lay: fn(usize) -> Result<Layout, LayoutError>,
    pub add: unsafe fn(NonNull<Void>, usize) -> NonNull<Void>,
    pub copy: unsafe fn(NonNull<Void>, NonNull<Void>, usize),
    pub drop: unsafe fn(NonNull<Void>, usize),
}

pub struct Column<T = Void> {
    meta: &'static Meta,
    pointer: NonNull<T>,
}

pub struct Table(Box<SliceWithHeader<usize, Column>>);

pub struct Store {
    tables: Vec<Table>,
}

impl Store {
    pub const fn new() -> Self {
        Self { tables: Vec::new() }
    }
}

impl Meta {
    pub const fn new<T: 'static>() -> &'static Self {
        &Self {
            identify: TypeId::of::<T>,
            name: type_name::<T>,
            lay: Layout::array::<T>,
            add: |pointer, count| unsafe { pointer.cast::<T>().add(count).cast::<Void>() },
            copy: |source, target, count| {
                if size_of::<T>() > 0 && count > 0 {
                    unsafe {
                        copy(
                            source.as_ptr().cast::<T>(),
                            target.as_ptr().cast::<T>(),
                            count,
                        )
                    }
                }
            },
            drop: |pointer, count| {
                if needs_drop::<T>() && count > 0 {
                    unsafe {
                        drop_in_place(slice_from_raw_parts_mut(
                            pointer.as_ptr().cast::<T>(),
                            count,
                        ))
                    }
                }
            },
        }
    }
}

impl Column {
    pub fn cast<T: 'static>(&self) -> Option<Column<T>> {
        if (self.meta.identify)() == TypeId::of::<T>() {
            Some(Column {
                meta: self.meta,
                pointer: self.pointer.cast(),
            })
        } else {
            None
        }
    }
}

impl<T: 'static> Column<T> {
    pub const fn pointer(&self) -> *mut T {
        self.pointer.as_ptr()
    }
}

impl Table {
    pub fn new(metas: impl IntoIterator<Item = &'static Meta>) -> Self {
        let mut metas = metas.into_iter().collect::<Vec<_>>();
        metas.sort_by_key(|meta| (meta.identify)());
        metas.dedup_by_key(|meta| (meta.identify)());

        Self(SliceWithHeader::new(
            0,
            metas.into_iter().map(|meta| Column {
                meta,
                pointer: NonNull::dangling(),
            }),
        ))
    }

    pub fn count(&self) -> usize {
        self.0.header
    }

    pub fn has(&self, identifier: TypeId) -> bool {
        self.0
            .slice
            .iter()
            .any(|column| (column.meta.identify)() == identifier)
    }

    pub fn columns(&self) -> &[Column] {
        &self.0.slice
    }
}

pub mod schedule {
    use super::*;

    pub struct Barrier;
    pub struct With<F>(F);
    pub struct Each<Q, F>(Q, F);

    pub unsafe trait Run {
        fn run(&self, store: *mut Store);
    }

    pub const fn barrier() -> Barrier {
        Barrier
    }

    pub const fn with<F: Fn(&mut Store) + Send + Sync>(run: F) -> With<F> {
        With(run)
    }

    pub const fn query<Q: query::Query, F: Fn(Q::Item<'_>) + Send + Sync>(
        query: Q,
        run: F,
    ) -> Each<Q, F> {
        Each(query, run)
    }

    unsafe impl Run for Barrier {
        fn run(&self, store: *mut Store) {}
    }

    unsafe impl<F: Fn(&mut Store) + Send + Sync> Run for With<F> {
        fn run(&self, store: *mut Store) {
            self.0(unsafe { &mut *store });
        }
    }

    unsafe impl<Q: query::Query, F: Fn(Q::Item<'_>) + Send + Sync> Run for Each<Q, F> {
        fn run(&self, store: *mut Store) {}
    }
}

pub mod command {
    use super::*;

    pub struct Schedule<'a>(&'a mut Store);
    pub struct Get<'a, A: query::Access = (), F: query::Filter = ()>(&'a mut Store, A, F);
    pub struct Query<'a, Q: query::Query> {
        store: &'a mut Store,
        index: usize,
        state: Q::State,
        query: Q,
    }
    pub struct Insert<'a, T: template::Template>(&'a mut Store, PhantomData<T>);
    pub struct Remove<'a, F: query::Filter = ()>(&'a mut Store, F);

    impl Store {
        pub fn insert<T: template::Template>(&mut self) -> Insert<'_, T> {
            Insert(self, PhantomData)
        }

        pub fn remove<F: query::Filter>(&mut self, filter: F) -> Remove<'_, F> {
            Remove(self, filter)
        }

        pub fn query<Q: query::Query>(&mut self, query: Q) -> Query<'_, Q> {
            Query {
                store: self,
                index: 0,
                state: query.initialize(),
                query,
            }
        }

        pub fn get<A: query::Access, F: query::Filter>(
            &mut self,
            access: A,
            filter: F,
        ) -> Get<'_, A, F> {
            Get(self, access, filter)
        }

        pub fn schedule(&mut self) -> Schedule<'_> {
            Schedule(self)
        }
    }

    impl<T: template::Template> Insert<'_, T> {
        pub fn one(&mut self, template: T) -> Row {
            todo!()
        }

        pub fn many(
            &mut self,
            templates: impl IntoIterator<Item = T>,
        ) -> impl Iterator<Item = Row> {
            [].into_iter()
        }
    }

    impl<F: query::Filter> Remove<'_, F> {
        pub fn one(&mut self, row: Row) -> bool {
            todo!()
        }

        pub fn many(&mut self, rows: impl IntoIterator<Item = Row>) -> bool {
            todo!()
        }

        pub fn all(&mut self) -> usize {
            todo!()
        }
    }

    impl<'a, A: query::Access, F: query::Filter> Get<'a, A, F> {
        pub fn one(&mut self, row: Row) -> Option<A::One<'_>> {
            todo!()
        }
    }

    impl<Q: query::Query> Query<'_, Q> {
        pub fn all(&mut self) -> impl Iterator<Item = Q::Item<'_>> {
            self.update();
            [].into_iter()
        }

        fn update(&mut self) {
            while let Some(table) = self.store.tables.get(self.index) {
                self.index += 1;
                self.query.update(&mut self.state, table);
            }
        }
    }

    impl Schedule<'_> {
        pub fn push<R: schedule::Run>(&mut self, run: R) {
            todo!()
        }

        pub fn run(&mut self) {
            todo!()
        }
    }
}

pub mod template {
    use super::*;

    pub struct Declare<'a>(&'a mut Vec<Vec<&'static Meta>>);

    pub struct Column<T: ?Sized>(pub T);

    pub trait Template {
        fn declare(context: Declare);
    }

    pub struct Key;

    impl Declare<'_> {
        pub const fn own(&mut self) -> Declare<'_> {
            Declare(self.0)
        }
    }

    impl Template for () {
        fn declare(_: Declare) {}
    }

    impl<T: Template> Template for (T,) {
        fn declare(context: Declare) {
            T::declare(context);
        }
    }

    impl<T0: Template, T1: Template> Template for (T0, T1) {
        fn declare(mut context: Declare) {
            T0::declare(context.own());
            T1::declare(context.own());
        }
    }

    impl<T0: Template, T1: Template, T2: Template> Template for (T0, T1, T2) {
        fn declare(mut context: Declare) {
            T0::declare(context.own());
            T1::declare(context.own());
            T2::declare(context.own());
        }
    }

    impl<T0: Template, T1: Template, T2: Template, T3: Template> Template for (T0, T1, T2, T3) {
        fn declare(mut context: Declare) {
            T0::declare(context.own());
            T1::declare(context.own());
            T2::declare(context.own());
            T3::declare(context.own());
        }
    }

    impl<T: 'static> Column<T> {
        pub const fn new(value: T) -> Self {
            Self(value)
        }
    }

    impl<T: 'static> Template for Column<T> {
        fn declare(context: Declare) {
            for metas in context.0 {
                metas.push(Meta::new::<T>());
            }
        }
    }

    impl<T: Template> Template for Option<T> {
        fn declare(context: Declare) {
            let mut some = vec![vec![]];
            T::declare(Declare(&mut some));
            for some in some {
                for mut metas in context.0.clone() {
                    metas.extend(some.iter().cloned().clone());
                    context.0.push(metas);
                }
            }
        }
    }
}

pub mod query {
    use super::*;
    use core::slice::{from_raw_parts, from_raw_parts_mut};

    pub trait Query: Filter {
        type Item<'a>
        where
            Self: 'a;
        type State;

        fn initialize(&self) -> Self::State;
        fn update(&self, state: &mut Self::State, table: &Table) -> bool;
        fn item<'a>(
            &'a self,
            state: &'a Self::State,
            index: usize,
            tables: &'a [Table],
        ) -> Self::Item<'a>;
    }

    pub trait Access: Filter {
        type All<'a>
        where
            Self: 'a;
        type One<'a>
        where
            Self: 'a;
        type State;

        fn initialize(&self, table: &Table) -> Option<Self::State>;
        fn all<'a>(&'a self, state: &'a Self::State, count: usize) -> Self::All<'a>;
        fn one<'a>(&'a self, state: &'a Self::State, index: usize) -> Self::One<'a>;
    }

    pub trait Filter {
        fn filter(&self, table: &Table) -> bool;
    }

    pub struct Not<Q>(Q);

    pub struct All<A>(A);
    pub struct Many<A>(A);
    pub struct One<A>(A);
    pub struct Join<L, R, O> {
        left: L,
        right: R,
        on: O,
    }

    pub struct Write<T>(PhantomData<T>);
    pub struct Read<T>(PhantomData<T>);
    pub struct Has<T>(PhantomData<T>);

    pub const fn all<A: Access>(access: A) -> All<A> {
        All(access)
    }
    pub const fn many<A: Access>(access: A) -> Many<A> {
        Many(access)
    }

    pub const fn one<A: Access>(access: A) -> One<A> {
        One(access)
    }

    pub const fn read<T: 'static>() -> Read<T> {
        Read(PhantomData)
    }

    pub const fn write<T: 'static>() -> Write<T> {
        Write(PhantomData)
    }

    pub const fn join<L: Access, R: Access, O: Fn(L::One<'_>, R::One<'_>) -> bool + Send + Sync>(
        left: L,
        right: R,
        on: O,
    ) -> Join<L, R, O> {
        Join { left, right, on }
    }

    impl Query for () {
        type Item<'a> = ();
        type State = ();

        fn initialize(&self) -> Self::State {}

        fn update(&self, _: &mut Self::State, _: &Table) -> bool {
            false
        }

        fn item<'a>(&'a self, _: &'a Self::State, _: usize, _: &'a [Table]) -> Self::Item<'a> {}
    }

    impl<Q: Query> Query for (Q,) {
        type Item<'a>
            = Q::Item<'a>
        where
            Self: 'a;
        type State = Q::State;

        fn initialize(&self) -> Self::State {
            self.0.initialize()
        }

        fn update(&self, state: &mut Self::State, table: &Table) {
            self.0.update(state, table)
        }

        fn item<'a>(&'a self, state: &'a Self::State) -> Self::Item<'a> {
            self.0.item(state)
        }
    }

    impl<Q0: Query, Q1: Query> Query for (Q0, Q1) {
        type Item<'a>
            = (Q0::Item<'a>, Q1::Item<'a>)
        where
            Self: 'a;
        type State = (Q0::State, Q1::State);

        fn initialize(&self) -> Self::State {
            (self.0.initialize(), self.1.initialize())
        }

        fn update(&self, state: &mut Self::State, table: &Table) {
            self.0.update(&mut state.0, table);
            self.1.update(&mut state.1, table);
        }

        fn item<'a>(&'a self, state: &'a Self::State) -> Self::Item<'a> {
            (self.0.item(&state.0), self.1.item(&state.1))
        }
    }

    impl<Q0: Query, Q1: Query, Q2: Query> Query for (Q0, Q1, Q2) {
        type Item<'a>
            = (Q0::Item<'a>, Q1::Item<'a>, Q2::Item<'a>)
        where
            Self: 'a;
        type State = (Q0::State, Q1::State, Q2::State);

        fn initialize(&self) -> Self::State {
            (
                self.0.initialize(),
                self.1.initialize(),
                self.2.initialize(),
            )
        }

        fn update(&self, state: &mut Self::State, table: &Table) {
            self.0.update(&mut state.0, table);
            self.1.update(&mut state.1, table);
            self.2.update(&mut state.2, table);
        }

        fn item<'a>(&'a self, state: &'a Self::State) -> Self::Item<'a> {
            (
                self.0.item(&state.0),
                self.1.item(&state.1),
                self.2.item(&state.2),
            )
        }
    }

    impl<Q0: Query, Q1: Query, Q2: Query, Q3: Query> Query for (Q0, Q1, Q2, Q3) {
        type Item<'a>
            = (Q0::Item<'a>, Q1::Item<'a>, Q2::Item<'a>, Q3::Item<'a>)
        where
            Self: 'a;
        type State = (Q0::State, Q1::State, Q2::State, Q3::State);

        fn initialize(&self) -> Self::State {
            (
                self.0.initialize(),
                self.1.initialize(),
                self.2.initialize(),
                self.3.initialize(),
            )
        }

        fn update(&self, state: &mut Self::State, table: &Table) {
            self.0.update(&mut state.0, table);
            self.1.update(&mut state.1, table);
            self.2.update(&mut state.2, table);
            self.3.update(&mut state.3, table);
        }

        fn item<'a>(&'a self, state: &'a Self::State) -> Self::Item<'a> {
            (
                self.0.item(&state.0),
                self.1.item(&state.1),
                self.2.item(&state.2),
                self.3.item(&state.3),
            )
        }
    }

    impl<F: Filter> Query for Not<F> {
        type Item<'a>
            = ()
        where
            Self: 'a;
        type State = ();

        fn initialize(&self) -> Self::State {}

        fn update(&self, _: &mut Self::State, _: &Table) {}

        fn item<'a>(&'a self, _: &'a Self::State) -> Self::Item<'a> {}
    }

    impl<T: 'static> Query for Has<T> {
        type Item<'a> = ();
        type State = ();

        fn initialize(&self) -> Self::State {}

        fn update(&self, _: &mut Self::State, _: &Table) {}

        fn item<'a>(&'a self, _: &'a Self::State) -> Self::Item<'a> {}
    }

    impl<A: Access> Query for One<A> {
        type Item<'a> = A::One<'a>;
    }

    impl<A: Access> Query for Many<A> {
        type Item<'a> = A::All<'a>;
    }

    pub mod all {
        use super::*;
        use core::{iter, slice::Iter};

        #[derive(Clone, Copy)]
        pub struct Item<'a, A: Access>(&'a A, &'a [(A::State, &'a Table)]);
        pub struct Iterator<'a, A: Access>(&'a A, Iter<'a, (A::State, &'a Table)>);

        impl<'a, A: Access> IntoIterator for Item<'a, A> {
            type IntoIter = Iterator<'a, A>;
            type Item = A::All<'a>;

            fn into_iter(self) -> Self::IntoIter {
                Iterator(self.0, self.1.iter())
            }
        }

        impl<'a, A: Access> iter::Iterator for Iterator<'a, A> {
            type Item = A::All<'a>;

            fn next(&mut self) -> Option<Self::Item> {
                let (state, table) = self.1.next()?;
                Some(self.0.all(state, table.count()))
            }
        }

        impl<A: Access> Query for All<A> {
            type Item<'a>
                = all::Item<'a, A>
            where
                Self: 'a;
            type State = A::State;

            fn update(&self, state: &mut Self::State, table: &Table) {
                if let Some(access) = self.0.initialize(table) {
                    state.push((access, table.clone()))
                }
            }

            fn item<'a>(
                &'a self,
                index: usize,
                states: &'a [(Self::State, &'a Table)],
            ) -> Self::Item<'a> {
                all::Item(&self.0, states)
            }
        }
    }

    // impl<L: Access, R: Access, O: Fn(L::One<'_>, R::One<'_>) -> bool + Send +
    // Sync> Query     for Join<L, R, O>
    // {
    //     type Item<'a>
    //         = (L::One<'a>, R::One<'a>)
    //     where
    //         Self: 'a;
    // }

    impl Filter for () {
        fn filter(&self, _: &Table) -> bool {
            true
        }
    }

    impl<F: Filter> Filter for (F,) {
        fn filter(&self, table: &Table) -> bool {
            self.0.filter(table)
        }
    }

    impl<F0: Filter, F1: Filter> Filter for (F0, F1) {
        fn filter(&self, table: &Table) -> bool {
            self.0.filter(table) && self.1.filter(table)
        }
    }

    impl<F0: Filter, F1: Filter, F2: Filter> Filter for (F0, F1, F2) {
        fn filter(&self, table: &Table) -> bool {
            self.0.filter(table) && self.1.filter(table) && self.2.filter(table)
        }
    }

    impl<F0: Filter, F1: Filter, F2: Filter, F3: Filter> Filter for (F0, F1, F2, F3) {
        fn filter(&self, table: &Table) -> bool {
            self.0.filter(table)
                && self.1.filter(table)
                && self.2.filter(table)
                && self.3.filter(table)
        }
    }

    impl<F: Filter> Filter for One<F> {
        fn filter(&self, table: &Table) -> bool {
            self.0.filter(table)
        }
    }

    impl<F: Filter> Filter for Many<F> {
        fn filter(&self, table: &Table) -> bool {
            self.0.filter(table)
        }
    }

    impl<F: Filter> Filter for All<F> {
        fn filter(&self, table: &Table) -> bool {
            self.0.filter(table)
        }
    }

    impl<L: Filter, R: Filter, O> Filter for Join<L, R, O> {
        fn filter(&self, table: &Table) -> bool {
            self.left.filter(table) || self.right.filter(table)
        }
    }

    impl<F: Filter> Filter for Not<F> {
        fn filter(&self, table: &Table) -> bool {
            !self.0.filter(table)
        }
    }

    impl<T: 'static> Filter for Has<T> {
        fn filter(&self, table: &Table) -> bool {
            table.has(TypeId::of::<T>())
        }
    }

    impl<F: Filter> Filter for Option<F> {
        fn filter(&self, _: &Table) -> bool {
            true
        }
    }

    impl<T: 'static> Filter for Write<T> {
        fn filter(&self, table: &Table) -> bool {
            table.has(TypeId::of::<T>())
        }
    }

    impl<T: 'static> Filter for Read<T> {
        fn filter(&self, table: &Table) -> bool {
            table.has(TypeId::of::<T>())
        }
    }

    impl Access for () {
        type All<'a> = ();
        type One<'a> = ();
        type State = ();

        fn initialize(&self, _: &Table) -> Option<Self::State> {
            Some(())
        }

        fn all<'a>(&self, _: &'a Self::State, _: usize) -> Self::All<'a> {}

        fn one<'a>(&self, _: &'a Self::State, _: usize) -> Self::One<'a> {}
    }

    impl<A: Access> Access for (A,) {
        type All<'a> = A::All<'a>;
        type One<'a> = A::One<'a>;
        type State = A::State;

        fn initialize(&self, table: &Table) -> Option<Self::State> {
            self.0.initialize(table)
        }

        fn all<'a>(&self, state: &'a Self::State, count: usize) -> Self::All<'a> {
            self.0.all(state, count)
        }

        fn one<'a>(&self, state: &'a Self::State, index: usize) -> Self::One<'a> {
            self.0.one(state, index)
        }
    }

    impl<A0: Access, A1: Access> Access for (A0, A1) {
        type All<'a> = (A0::All<'a>, A1::All<'a>);
        type One<'a> = (A0::One<'a>, A1::One<'a>);
        type State = (A0::State, A1::State);

        fn initialize(&self, table: &Table) -> Option<Self::State> {
            Some((self.0.initialize(table)?, self.1.initialize(table)?))
        }

        fn all<'a>(&self, state: &'a Self::State, count: usize) -> Self::All<'a> {
            (self.0.all(&state.0, count), self.1.all(&state.1, count))
        }

        fn one<'a>(&self, state: &'a Self::State, index: usize) -> Self::One<'a> {
            (self.0.one(&state.0, index), self.1.one(&state.1, index))
        }
    }

    impl<A0: Access, A1: Access, A2: Access> Access for (A0, A1, A2) {
        type All<'a> = (A0::All<'a>, A1::All<'a>, A2::All<'a>);
        type One<'a> = (A0::One<'a>, A1::One<'a>, A2::One<'a>);
        type State = (A0::State, A1::State, A2::State);

        fn initialize(&self, table: &Table) -> Option<Self::State> {
            Some((
                self.0.initialize(table)?,
                self.1.initialize(table)?,
                self.2.initialize(table)?,
            ))
        }

        fn all<'a>(&self, state: &'a Self::State, count: usize) -> Self::All<'a> {
            (
                self.0.all(&state.0, count),
                self.1.all(&state.1, count),
                self.2.all(&state.2, count),
            )
        }

        fn one<'a>(&self, state: &'a Self::State, index: usize) -> Self::One<'a> {
            (
                self.0.one(&state.0, index),
                self.1.one(&state.1, index),
                self.2.one(&state.2, index),
            )
        }
    }

    impl<A0: Access, A1: Access, A2: Access, A3: Access> Access for (A0, A1, A2, A3) {
        type All<'a> = (A0::All<'a>, A1::All<'a>, A2::All<'a>, A3::All<'a>);
        type One<'a> = (A0::One<'a>, A1::One<'a>, A2::One<'a>, A3::One<'a>);
        type State = (A0::State, A1::State, A2::State, A3::State);

        fn initialize(&self, table: &Table) -> Option<Self::State> {
            Some((
                self.0.initialize(table)?,
                self.1.initialize(table)?,
                self.2.initialize(table)?,
                self.3.initialize(table)?,
            ))
        }

        fn all<'a>(&self, state: &'a Self::State, count: usize) -> Self::All<'a> {
            (
                self.0.all(&state.0, count),
                self.1.all(&state.1, count),
                self.2.all(&state.2, count),
                self.3.all(&state.3, count),
            )
        }

        fn one<'a>(&self, state: &'a Self::State, index: usize) -> Self::One<'a> {
            (
                self.0.one(&state.0, index),
                self.1.one(&state.1, index),
                self.2.one(&state.2, index),
                self.3.one(&state.3, index),
            )
        }
    }

    impl<T: 'static> Write<T> {
        pub const fn new() -> Self {
            Self(PhantomData)
        }
    }

    impl<T: 'static> Access for Write<T> {
        type All<'a> = &'a mut [T];
        type One<'a> = &'a mut T;
        type State = *mut T;

        fn initialize(&self, table: &Table) -> Option<Self::State> {
            Some(
                table
                    .columns()
                    .iter()
                    .find_map(|column| column.cast())?
                    .pointer(),
            )
        }

        fn all<'a>(&self, state: &'a Self::State, count: usize) -> Self::All<'a> {
            unsafe { from_raw_parts_mut(*state, count) }
        }

        fn one<'a>(&self, state: &'a Self::State, index: usize) -> Self::One<'a> {
            unsafe { &mut *state.add(index) }
        }
    }

    impl<T: 'static> Read<T> {
        pub const fn new() -> Self {
            Self(PhantomData)
        }
    }

    impl<T: 'static> Access for Read<T> {
        type All<'a> = &'a [T];
        type One<'a> = &'a T;
        type State = *const T;

        fn initialize(&self, table: &Table) -> Option<Self::State> {
            Some(
                table
                    .columns()
                    .iter()
                    .find_map(|column| column.cast())?
                    .pointer(),
            )
        }

        fn all<'a>(&self, state: &'a Self::State, count: usize) -> Self::All<'a> {
            unsafe { from_raw_parts(*state, count) }
        }

        fn one<'a>(&self, state: &'a Self::State, index: usize) -> Self::One<'a> {
            unsafe { &*state.add(index) }
        }
    }

    pub mod dynamic {
        use super::*;

        pub enum Query {
            And(Box<[Query]>),
            One(Access),
            All(Access),
            Many(Access),
            Filter(Filter),
        }

        pub enum Access {
            And(Box<[Access]>),
            Option(Box<Access>),
            Read(TypeId),
            Write(TypeId),
        }

        pub enum Filter {
            And(Box<[Filter]>),
            Not(Box<Filter>),
            Has(TypeId),
        }

        pub struct Item<'a>(&'a [&'a Column]);
        pub struct One<'a>(Item<'a>);
        pub struct Many<'a>(Item<'a>);
        pub struct All<'a>(Item<'a>);

        impl super::Query for Query {
            type Item<'a> = Item<'a>;
        }

        impl super::Filter for Query {
            fn filter(&self, table: &Table) -> bool {
                match self {
                    Query::And(queries) => queries
                        .iter()
                        .all(|query| super::Filter::filter(query, table)),
                    Query::One(access) => super::Filter::filter(access, table),
                    Query::All(access) => super::Filter::filter(access, table),
                    Query::Many(access) => super::Filter::filter(access, table),
                    Query::Filter(filter) => super::Filter::filter(filter, table),
                }
            }
        }

        impl super::Filter for Access {
            fn filter(&self, table: &Table) -> bool {
                match self {
                    Access::And(accesses) => accesses
                        .iter()
                        .all(|access| super::Filter::filter(access, table)),
                    Access::Option(access) => access.filter(table),
                    Access::Read(identifier) | Access::Write(identifier) => table.has(*identifier),
                }
            }
        }

        impl super::Filter for Filter {
            fn filter(&self, table: &Table) -> bool {
                match self {
                    Self::And(accesses) => accesses
                        .iter()
                        .all(|access| super::Filter::filter(access, table)),
                    Self::Not(filter) => !filter.filter(table),
                    Self::Has(identifier) => table.has(*identifier),
                }
            }
        }

        impl super::Access for Access {
            type All<'a> = All<'a>;
            type All<'a> = Many<'a>;
            type One<'a> = One<'a>;
        }
    }
}

#[test]
fn boba() {
    use query::*;
    use schedule::*;

    struct Empty;
    struct Player {
        health: usize,
    }
    struct Position(f64, f64);
    struct Velocity(f64, f64);

    let mut store = Store::new();
    let row = store.insert().one(template::Column(Empty));
    let item = store.get(read::<Empty>(), ()).one(row);
    let items = store.query(one(read::<Empty>())).all().collect::<Vec<_>>();
    assert!(store.remove(()).one(row));

    let mut schedule = store.schedule();
    schedule.push(query(
        many((write::<Position>(), read::<Velocity>())),
        |(positions, velocities)| {
            for (position, velocity) in positions.iter_mut().zip(velocities.iter()) {
                position.0 += velocity.0;
                position.1 += velocity.1;
            }
        },
    ));

    let exit = AtomicBool::new(false);
    schedule.push(query(all(read::<Player>()), |players| {
        if players
            .iter()
            .flat_map(|players| players.iter())
            .all(|player| player.health == 0)
        {
            exit.store(true, Ordering::Relaxed);
        }
    }));

    while !exit.load(Ordering::Relaxed) {
        schedule.run();
    }
}
/*
fn main() {
    #[derive(Query)]
    pub struct Position {
        pub x: f64,
        pub y: f64,
    }

    #[derive(Query)]
    pub struct Velocity {
        pub x: f64,
        pub y: f64,
    }

    #[derive(Query)]
    pub struct Target(pub Key);

    let mut store = Store::new();
    let mut scheduler = store.scheduler();
    scheduler.query::<One<(Write<Position>, Read<Velocity>)>>(|position, velocity| {
        *position.x += *velocity.x;
        *position.y += *velocity.y;
    });
    scheduler.query::<Many<(Write<Position::X>, Read<Velocity::X>)>>(|positions, velocities| {
        for (position, velocity) in (positions, velocities) {
            *position += *velocity;
        }
    });
    scheduler.query::<Many<(Write<Position::Y>, Read<Velocity::Y>)>>(|positions, velocities| {
        for (position, velocity) in (positions, velocities) {
            *position += *velocity;
        }
    });
    scheduler.query::<One<(Write<Position::X>, Read<Velocity::X>)>>(|position, velocity| {
        *position += *velocity;
    });
    scheduler.query::<One<(Write<Position::Y>, Read<Velocity::Y>)>>(|position, velocity| {
        *position += *velocity;
    });
    scheduler.query::<And<(
        One<(Write<Target>, Read<Position>, Not<Has<Prey>>)>,
        All<(Key, Read<Position>, Has<Prey>)>,
    )>>(|target: &mut Target, position, others| {
        let mut closest = None;
        let mut distance = f32::MAX;
        for (key, other) in others {
            let current = position.distance(*other);
            if current < distance {
                closest = Some(key);
                distance = current;
            }
        }
        target.0 = closest;
    });
    scheduler.query::<And<(
        One<Write<Velocity::Y>>,
        Global<Read<Physics>>,
    )>>(|velocity: &mut f64, physics: &Physics| {
        *velocity += physics.gravity;
    });
    scheduler.query_with(
        query::Dynamic::and([
            query::Dynamic::one([query::Dynamic::write::<Target>(), query::Dynamic::read::<Position>(), query::Dynamic::not(query::Dynamic::has::<Prey>())]),
            query::Dynamic::all([query::Dynamic::read::<Position>(), query::Dynamic::has::<Prey>()]),
        ]),
        |dynamic: &[Dynamic]| {
            if let [Dynamic::Column(targets), Dynamic::Column(positions), Dynamic::Columns(others)] = dynamic
                && let Some(targets) = targets.cast::<Target>()
                && let Some(positions) = positions.cast::<Position>()
                && let Some(others) = others.cast::<Prey>()
            {
                for (target, position) in targets.iter().zip(positions) {
                    let mut closest = None;
                    let mut distance = f32::MAX;
                    for other in others.iter().flatten() {
                        let current = position.distance(*other);
                        if current < distance {
                            closest = Some(*other);
                            distance = current;
                        }
                    }
                    target.0 = closest;
                }
            }
        }
    );
    let mut schedule = scheduler.schedule();
    schedule.run();
}
*/
