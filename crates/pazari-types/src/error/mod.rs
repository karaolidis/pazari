use std::{fmt, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
#[error("{what} rejects {value}: expected {bounds}")]
pub struct RangeError {
    pub what: &'static str,
    pub value: String,
    pub bounds: &'static str,
}

impl RangeError {
    #[must_use]
    pub fn new(what: &'static str, value: &impl fmt::Debug, bounds: &'static str) -> Self {
        Self {
            what,
            value: format!("{value:?}"),
            bounds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
pub enum KernelFileError {
    #[error("{} holds {value:?}, which is not {expected}", .path.display())]
    Malformed {
        path: PathBuf,
        value: String,
        expected: &'static str,
    },

    #[error("{} carries no {key} line", .path.display())]
    Absent { path: PathBuf, key: &'static str },
}
