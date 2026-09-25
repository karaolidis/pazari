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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{KernelFileError, Psi, RangeError, Swappiness};

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
        let first = Psi::try_new(101.0).expect_err("101% is over one hundred");
        let second = Psi::try_new(101.0).expect_err("101% is over one hundred");

        assert_eq!(first, second);
        assert_eq!(
            first,
            RangeError {
                what: "Psi",
                value: "101.0".to_owned(),
                bounds: "(0, 100]",
            }
        );
    }

    #[test]
    fn a_kernel_file_names_itself_and_what_it_was_missing() {
        let path = PathBuf::from("/proc/pressure/memory");

        assert_eq!(
            KernelFileError::Absent {
                path: path.clone(),
                key: "full avg10",
            }
            .to_string(),
            "/proc/pressure/memory carries no full avg10 line"
        );
        assert_eq!(
            KernelFileError::Malformed {
                path,
                value: "wat".to_owned(),
                expected: "a pressure percentage",
            }
            .to_string(),
            "/proc/pressure/memory holds \"wat\", which is not a pressure percentage"
        );
    }
}
