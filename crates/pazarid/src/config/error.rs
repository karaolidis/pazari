use std::{io, path::PathBuf};

use pazari_types::RangeError;
use thiserror::Error;

use crate::broker::RegistryError;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {}: {source}", .path.display())]
    Read { path: PathBuf, source: io::Error },

    #[error("failed to parse {}: {source}", .path.display())]
    Syntax {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("{field}: {source}")]
    Range { field: String, source: RangeError },

    #[error("{field} rejects 0: expected a positive {what}")]
    Zero { field: String, what: &'static str },

    #[error("the file names no guests")]
    Guestless,

    #[error(transparent)]
    Registry(#[from] RegistryError),
}
