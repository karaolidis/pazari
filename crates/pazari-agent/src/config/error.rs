use std::{fmt, io, path::PathBuf};

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
    Range {
        field: &'static str,
        source: RangeError,
    },

    #[error("{field} rejects 0: expected a positive {what}")]
    Zero {
        field: &'static str,
        what: &'static str,
    },

    #[error("{field} is {value}, over {ceiling} of {limit}")]
    Order {
        field: &'static str,
        value: String,
        ceiling: &'static str,
        limit: String,
    },
}

impl ConfigError {
    #[must_use]
    pub fn order(
        field: &'static str,
        value: &impl fmt::Debug,
        ceiling: &'static str,
        limit: &impl fmt::Debug,
    ) -> Self {
        Self::Order {
            field,
            value: format!("{value:?}"),
            ceiling,
            limit: format!("{limit:?}"),
        }
    }
}
