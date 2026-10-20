use std::fmt;

use pazari_types::RangeError;

use crate::ConfigError;

/// Names the field a rejected quantity came from.
pub trait Ranged<T> {
    fn at(self, field: impl Into<String>) -> Result<T, ConfigError>;
}

impl<T> Ranged<T> for Result<T, RangeError> {
    fn at(self, field: impl Into<String>) -> Result<T, ConfigError> {
        self.map_err(|source| ConfigError::Range {
            field: field.into(),
            source,
        })
    }
}

/// Names the field a quantity that may not be zero came from.
pub trait Positive<T> {
    fn positive(self, field: impl Into<String>, what: &'static str) -> Result<T, ConfigError>;

    /// [`Positive::positive`] for byte counts, which are most of them.
    fn bytes(self, field: impl Into<String>) -> Result<T, ConfigError>
    where
        Self: Sized,
    {
        self.positive(field, "byte count")
    }
}

impl<T> Positive<T> for Option<T> {
    fn positive(self, field: impl Into<String>, what: &'static str) -> Result<T, ConfigError> {
        self.ok_or_else(|| ConfigError::Zero {
            field: field.into(),
            what,
        })
    }
}

/// Refuses a floor that sits over the ceiling it climbs towards.
pub fn ordered<T: PartialOrd + fmt::Display + Copy>(
    field: impl Into<String>,
    value: T,
    limit_field: impl Into<String>,
    limit: T,
) -> Result<(), ConfigError> {
    if value <= limit {
        return Ok(());
    }

    Err(ConfigError::Order {
        field: field.into(),
        value: value.to_string(),
        limit_field: limit_field.into(),
        limit: limit.to_string(),
    })
}
