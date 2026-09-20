use std::{fmt, io, path::PathBuf};

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

    #[error("{field} is {value}, over {ceiling} of {limit}")]
    Order {
        field: String,
        value: String,
        ceiling: String,
        limit: String,
    },

    #[error("the file names no guests")]
    Guestless,

    #[error(transparent)]
    Registry(#[from] RegistryError),
}

impl ConfigError {
    #[must_use]
    pub fn order(
        field: impl Into<String>,
        value: &impl fmt::Debug,
        ceiling: impl Into<String>,
        limit: &impl fmt::Debug,
    ) -> Self {
        Self::Order {
            field: field.into(),
            value: format!("{value:?}"),
            ceiling: ceiling.into(),
            limit: format!("{limit:?}"),
        }
    }
}
