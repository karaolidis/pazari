//! Configuration loading shared by the pazari binaries.

pub mod error;
pub mod validate;

use std::{
    fs,
    path::{Path, PathBuf},
};

use doku::Document;
use serde::de::DeserializeOwned;

pub use crate::{
    error::ConfigError,
    validate::{Positive, Ranged, ordered},
};

pub trait Settings: Sized {
    type File: DeserializeOwned + Document;

    type Error: From<ConfigError>;

    fn resolve(file: Self::File) -> Result<Self, Self::Error>;

    /// Tidies the printed template.
    #[must_use]
    fn polish(template: &str) -> String {
        // doku pads the blank comment lines between enum variant fields.
        let mut lines = template.lines().map(str::trim_end).collect::<Vec<_>>();

        spaced(&mut lines);

        lines.join("\n")
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

        let mut template = Self::polish(&doku::to_toml_fmt::<Self::File>(&formatting));
        template.push('\n');

        template
    }
}

/// Puts a table's own comment under the blank line that separates it from the
/// field before it, rather than over it, which doku does for optional tables
/// and which reads as though the comment belonged to that field.
fn spaced(lines: &mut Vec<&str>) {
    let mut index = 1;

    while index + 1 < lines.len() {
        let header = lines[index].is_empty()
            && lines[index + 1].starts_with('[')
            && lines[index - 1].starts_with('#');

        if header {
            let start = lines[..index]
                .iter()
                .rposition(|line| !line.starts_with('#'))
                .map_or(0, |last| last + 1);

            if start > 0 {
                lines.remove(index);
                lines.insert(start, "");
            }
        }

        index += 1;
    }
}
