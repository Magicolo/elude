use crate::depend::{Key, Order, Scope};
use std::{
    error,
    fmt::{self, Display},
};

#[derive(Debug)]
pub enum Error {
    FailedToRun,
    UnknownConflict(Scope),
    ReadWriteConflict(Key, Scope, Order),
    WriteWriteConflict(Key, Scope, Order),
    Dynamic(Box<dyn error::Error + Send + Sync>),
    All(Vec<Error>),
}

impl Error {
    pub fn all<E: Into<Error>>(errors: impl IntoIterator<Item = E>) -> Self {
        Self::All(errors.into_iter().map(Into::into).collect())
    }

    pub fn merge(self, error: Self) -> Self {
        match (self, error) {
            (Error::All(mut left), Error::All(mut right)) => {
                left.append(&mut right);
                Error::All(left)
            }
            (Error::All(mut left), right) => {
                left.push(right);
                Error::All(left)
            }
            (left, Error::All(mut right)) => {
                right.insert(0, left);
                Error::All(right)
            }
            (left, right) => Error::All(vec![left, right]),
        }
    }

    pub fn flatten(self, recursive: bool) -> Option<Self> {
        fn descend(error: Error, errors: &mut Vec<Error>, recursive: bool) {
            match error {
                Error::All(mut inner) => {
                    if recursive {
                        for error in inner {
                            descend(error, errors, recursive);
                        }
                    } else {
                        errors.append(&mut inner);
                    }
                }
                error => errors.push(error),
            }
        }

        let mut errors = Vec::new();
        descend(self, &mut errors, recursive);

        if errors.is_empty() {
            None
        } else if errors.len() == 1 {
            Some(errors.into_iter().next().unwrap())
        } else {
            Some(Error::All(errors))
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for Error {}
