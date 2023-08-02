use std::{
    any::TypeId,
    borrow::Cow,
    collections::{HashMap, HashSet},
    result,
};

use crate::error::Error;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Inner,
    Outer,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Identifier(usize),
    Type(TypeId),
    Path(Cow<'static, str>),
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Order {
    #[default]
    Relax,
    Strict,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dependency {
    Unknown,
    Read(Key, Order),
    Write(Key, Order),
}

#[derive(Debug, Default)]
pub struct Conflict {
    unknown: bool,
    orders: HashMap<Key, Order>,
    reads: HashSet<Key>,
    writes: HashSet<Key>,
}

/// # Safety
/// This library heavily relies on the correct implementation of this trait.
/// Any omitted dependency may lead to undefined behavior.
pub unsafe trait Depend {
    type Dependencies: Iterator<Item = Dependency>;
    fn depend(&self) -> Self::Dependencies;
}

impl Dependency {
    pub const fn read_at(identifier: usize, order: Order) -> Self {
        Self::Read(Key::Identifier(identifier), order)
    }

    pub fn read<T: 'static>(order: Order) -> Self {
        Self::Read(Key::Type(TypeId::of::<T>()), order)
    }

    pub const fn write_at(identifier: usize, order: Order) -> Self {
        Self::Write(Key::Identifier(identifier), order)
    }

    pub fn write<T: 'static>(order: Order) -> Self {
        Self::Write(Key::Type(TypeId::of::<T>()), order)
    }

    pub fn order(self, order: Order) -> Self {
        match self {
            Self::Unknown => Self::Unknown,
            Self::Read(key, _) => Self::Read(key, order),
            Self::Write(key, _) => Self::Write(key, order),
        }
    }

    pub fn relax(self) -> Self {
        self.order(Order::Relax)
    }

    pub fn strict(self) -> Self {
        self.order(Order::Strict)
    }
}

impl Conflict {
    pub fn detect_inner(&mut self, dependencies: &[Dependency], all: bool) -> Result<(), Error> {
        self.clear();

        let mut errors = Vec::new();
        for dependency in dependencies.iter() {
            match self.conflict(Scope::Inner, dependency, true) {
                Ok(_) => {}
                Err(error) if all => errors.push(error),
                Err(error) => return Err(error),
            }
        }
        Error::all(errors).flatten(true).map_or(Ok(()), Err)
    }

    /// - `Ok(Strict)` means that execution is strictly allowed without any concern for the given `dependencies`.
    /// - `Ok(Relax)` means that execution may proceed as long as the given `dependencies` are not executing (no ordering is imposed).
    /// - `Err(error)` means that execution can not proceed as long as the given `dependencies` have not completely executed.
    pub fn detect_outer(&mut self, dependencies: &[Dependency], all: bool) -> Result<Order, Error> {
        let mut allow = Order::Strict;
        let mut errors = Vec::new();

        for dependency in dependencies.iter() {
            match self.conflict(Scope::Outer, dependency, false) {
                Ok(order) => allow = allow.min(order),
                Err(error) if all => errors.push(error),
                Err(error) => return Err(error),
            }
        }

        Error::all(errors).flatten(true).map_or(Ok(allow), Err)
    }

    pub fn clear(&mut self) {
        self.unknown = false;
        self.orders.clear();
        self.reads.clear();
        self.writes.clear();
    }

    fn conflict(
        &mut self,
        scope: Scope,
        dependency: &Dependency,
        fill: bool,
    ) -> result::Result<Order, Error> {
        use self::{Dependency::*, Error::*, Order::*, Scope::*};

        let order = match dependency {
            Read(key, order) | Write(key, order) => match self.orders.get_mut(key) {
                Some(previous) => {
                    let current = (*previous).max(*order);
                    if fill {
                        *previous = current;
                    }
                    current
                }
                None => {
                    if fill {
                        self.orders.insert(key.clone(), *order);
                    }
                    *order
                }
            },
            Unknown => {
                if fill {
                    self.unknown = true;
                }
                Strict
            }
        };

        match (dependency, scope, order) {
            (Unknown, Inner, _) => Ok(Strict),
            (Unknown, Outer, _) => Err(UnknownConflict(scope)),

            (Read(key, _), Outer, Relax) if self.writes.contains(key) => Ok(Relax),
            (Read(key, _), Outer, Relax) if fill && self.reads.insert(key.clone()) => Ok(Strict),
            (Read(_, _), Outer, Relax) => Ok(Strict),

            (Read(key, _), Inner, _) | (Read(key, _), _, Strict) if self.writes.contains(key) => {
                Err(ReadWriteConflict(key.clone(), scope, order))
            }
            (Read(key, _), Inner, _) | (Read(key, _), _, Strict)
                if fill && self.reads.insert(key.clone()) =>
            {
                Ok(Strict)
            }
            (Read(_, _), Inner, _) | (Read(_, _), _, Strict) => Ok(Strict),

            (Write(key, _), Outer, Relax) if self.reads.contains(key) => Ok(Relax),
            (Write(key, _), Outer, Relax) if fill && self.writes.insert(key.clone()) => Ok(Strict),
            (Write(key, _), Outer, Relax) if self.writes.contains(key) => Ok(Relax),
            (Write(_, _), Outer, Relax) => Ok(Strict),

            (Write(key, _), Inner, _) | (Write(key, _), _, Strict) if self.reads.contains(key) => {
                Err(ReadWriteConflict(key.clone(), scope, order))
            }
            (Write(key, _), Inner, _) | (Write(key, _), _, Strict)
                if fill && self.writes.insert(key.clone()) =>
            {
                Ok(Strict)
            }
            (Write(key, _), Inner, _) | (Write(key, _), _, Strict) if self.writes.contains(key) => {
                Err(WriteWriteConflict(key.clone(), scope, order))
            }
            (Write(_, _), Inner, _) | (Write(_, _), _, Strict) => Ok(Strict),
        }
    }
}
