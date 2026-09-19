use std::{io, path::PathBuf};

use pazari_types::MemInfoError;
use thiserror::Error;

use crate::runtime::qmp::QmpError;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("failed to read {}: {source}", .path.display())]
    Read { path: PathBuf, source: io::Error },

    #[error(transparent)]
    MemInfo(#[from] MemInfoError),

    #[error("the memory device refused a size of {bytes}: {reason}")]
    Device { bytes: u64, reason: String },

    #[error(transparent)]
    Qmp(#[from] QmpError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_names_the_file_it_could_not_use() {
        let error = RuntimeError::Read {
            path: PathBuf::from("/proc/meminfo"),
            source: io::Error::from(io::ErrorKind::NotFound),
        };

        assert_eq!(
            error.to_string(),
            "failed to read /proc/meminfo: entity not found"
        );
    }

    #[test]
    fn a_refused_size_carries_what_was_asked_for() {
        let error = RuntimeError::Device {
            bytes: 1 << 31,
            reason: "the device is not plugged".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            "the memory device refused a size of 2147483648: the device is not plugged"
        );
    }
}
