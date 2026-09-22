//! Configuration loading shared by the pazari binaries.

pub mod error;

use std::{
    fs,
    path::{Path, PathBuf},
};

use doku::Document;
use serde::de::DeserializeOwned;

pub use crate::error::{ConfigError, Positive, Ranged, ordered};

pub trait Settings: Sized {
    type File: DeserializeOwned + Document;

    type Error: From<ConfigError>;

    fn resolve(file: Self::File) -> Result<Self, Self::Error>;

    /// Tidies the printed template. Most schemas need nothing here.
    #[must_use]
    fn polish(template: String) -> String {
        template
    }

    fn load(path: &Path) -> Result<Self, Self::Error> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: PathBuf::from(path),
            source,
        })?;

        Self::parse(path, &text)
    }

    fn parse(path: &Path, text: &str) -> Result<Self, Self::Error> {
        let file = toml::from_str::<Self::File>(text).map_err(|source| ConfigError::Syntax {
            path: PathBuf::from(path),
            source,
        })?;

        Self::resolve(file)
    }

    #[must_use]
    fn template() -> String {
        let formatting = doku::toml::Formatting {
            auto_comments: doku::toml::AutoComments {
                optional: false,
                ..Default::default()
            },
            enums_style: doku::toml::EnumsStyle::Commented,
            ..Default::default()
        };

        let mut template = Self::polish(doku::to_toml_fmt::<Self::File>(&formatting));
        template.push('\n');

        template
    }
}
