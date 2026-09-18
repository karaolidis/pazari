use std::fmt;

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
