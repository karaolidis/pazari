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

#[cfg(test)]
mod tests {
    use crate::{Fraction, RangeError, Swappiness};

    #[test]
    fn a_rejection_names_what_was_rejected() {
        let error = Swappiness::try_new(255).expect_err("255 is over the kernel's limit");

        assert_eq!(error.what, "Swappiness");
        assert_eq!(error.value, "255");
        assert_eq!(error.bounds, "[0, 200]");
        assert_eq!(
            error.to_string(),
            "Swappiness rejects 255: expected [0, 200]"
        );
    }

    #[test]
    fn a_range_error_is_comparable() {
        let first = Fraction::try_new(2.0).expect_err("2.0 is over one");
        let second = Fraction::try_new(2.0).expect_err("2.0 is over one");

        assert_eq!(first, second);
        assert_eq!(
            first,
            RangeError {
                what: "Fraction",
                value: "2.0".to_owned(),
                bounds: "(0, 1]",
            }
        );
    }
}
