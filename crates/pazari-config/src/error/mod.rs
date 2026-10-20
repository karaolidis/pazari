use std::{io, path::PathBuf};

use pazari_types::RangeError;
use thiserror::Error;

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

    #[error("{field} rejects {}: expected an absolute path without `.` or `..`", .path.display())]
    Relative { field: String, path: PathBuf },

    #[error("{field} rejects 0: expected a positive {what}")]
    Zero { field: String, what: &'static str },

    #[error("{field} is {value}, over {limit_field} of {limit}")]
    Order {
        field: String,
        value: String,
        limit_field: String,
        limit: String,
    },
}
