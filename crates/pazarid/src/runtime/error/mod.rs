use std::{io, path::PathBuf};

use pazari_types::KernelFileError;
use thiserror::Error;

use crate::runtime::qmp::QmpError;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("failed to read {}: {source}", .path.display())]
    Read { path: PathBuf, source: io::Error },

    #[error(transparent)]
    Kernel(#[from] KernelFileError),

    #[error(transparent)]
    Resize(#[from] ResizeError),

    #[error(transparent)]
    Qmp(#[from] QmpError),
}

/// A size the memory device was asked for and did not take.
#[derive(Debug, Error)]
pub enum ResizeError {
    #[error("asked the device for {bytes}, under the guest's boot memory of {boot}")]
    BelowBoot { bytes: u64, boot: u64 },

    #[error("the memory device refused a size of {bytes}: {reason}")]
    Refused { bytes: u64, reason: String },
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
        let error = RuntimeError::from(ResizeError::Refused {
            bytes: 1 << 31,
            reason: "the device is not plugged".to_owned(),
        });

        assert_eq!(
            error.to_string(),
            "the memory device refused a size of 2147483648: the device is not plugged"
        );
    }

    #[test]
    fn an_ask_under_the_boot_memory_names_both_sizes() {
        let error = RuntimeError::from(ResizeError::BelowBoot {
            bytes: 1 << 30,
            boot: 1 << 31,
        });

        assert_eq!(
            error.to_string(),
            "asked the device for 1073741824, under the guest's boot memory of 2147483648"
        );
    }
}
