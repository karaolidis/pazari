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
